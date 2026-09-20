//! Private, bounded image staging and Codex's native bracketed-paste attachment.
use gtk4::gdk_pixbuf::prelude::*;
use sha2::{Digest, Sha256};
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

pub const MAX_IMAGE: usize = 2 * 1024 * 1024;
pub const MAX_BODY: usize = 3 * 1024 * 1024;
static INPUT: OnceLock<[Mutex<()>; 64]> = OnceLock::new();
pub fn input_guard(session: &str) -> Result<MutexGuard<'static, ()>, String> {
    let index = Sha256::digest(session.as_bytes())[0] as usize % 64;
    INPUT.get_or_init(|| std::array::from_fn(|_| Mutex::new(())))[index]
        .try_lock()
        .map_err(|_| "terminal_input_busy_try_again".into())
}

pub fn route(path: &str) -> Option<&str> {
    let id = path
        .strip_prefix("/api/v1/harnesses/")?
        .strip_suffix("/image-prompt")?;
    (!id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then_some(id)
}

pub fn validate_prompt(text: &str, request: &str) -> Result<(), String> {
    if text.len() > 4096
        || text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("invalid_prompt".into());
    }
    if request.len() != 32 || !request.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid_request_id".into());
    }
    Ok(())
}

fn normalize(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() > (MAX_IMAGE + 2) / 3 * 4 || encoded.as_bytes().contains(&0) {
        return Err("image_too_large".into());
    }
    let bytes = gtk4::glib::base64_decode(encoded);
    if bytes.is_empty() || bytes.len() > MAX_IMAGE || crate::ws::base64(&bytes) != encoded {
        return Err("invalid_image_encoding".into());
    }
    // Only JPEG/PNG. Reject SVG, animated formats and arbitrary files regardless of MIME.
    if !bytes.starts_with(b"\xff\xd8\xff") && !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("unsupported_image_type".into());
    }
    let bad_size = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = bad_size.clone();
    let loader = gtk4::gdk_pixbuf::PixbufLoader::new();
    loader.connect_size_prepared(move |loader, width, height| {
        if width <= 0 || height <= 0 || width > 2048 || height > 2048 {
            flag.set(true);
            loader.set_size(1, 1);
        }
    });
    let written = loader.write(&bytes);
    let closed = loader.close();
    if written.and(closed).is_err() || bad_size.get() {
        return Err("invalid_or_oversized_image".into());
    }
    // Re-encode just decoded pixels; no EXIF, filename, embedded scripts or ancillary data.
    let image = loader.pixbuf().ok_or("invalid_image")?;
    let pixels = gtk4::gdk_pixbuf::Pixbuf::new(
        gtk4::gdk_pixbuf::Colorspace::Rgb,
        image.has_alpha(),
        8,
        image.width(),
        image.height(),
    )
    .ok_or("image_encoding_failed")?;
    image.copy_area(0, 0, image.width(), image.height(), &pixels, 0, 0);
    let output = pixels
        .save_to_bufferv("png", &[])
        .map_err(|_| "image_encoding_failed")?;
    if output.len() > 16 * 1024 * 1024 {
        return Err("image_too_large".into());
    }
    Ok(output)
}

fn stage(directory: &Path, key: &str, image: &[u8]) -> Result<PathBuf, String> {
    static STORAGE: Mutex<()> = Mutex::new(());
    let _guard = STORAGE.lock().map_err(|_| "storage_unavailable")?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(|_| "image_storage_unavailable")?;
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(directory)
        .map_err(|_| "unsafe_image_storage")?;
    let meta = dir.metadata().map_err(|_| "image_storage_unavailable")?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("unsafe_image_storage".into());
    }
    // Retain images for conversation resume. Never silently prune an image an agent may use.
    let mut total = 0u64;
    let mut count = 0;
    for entry in std::fs::read_dir(directory).map_err(|_| "image_storage_unavailable")? {
        let entry = entry.map_err(|_| "image_storage_unavailable")?;
        total += entry
            .metadata()
            .map_err(|_| "image_storage_unavailable")?
            .len();
        count += 1;
        if count >= 1024 {
            return Err("image_storage_full".into());
        }
    }
    if total + image.len() as u64 > 64 * 1024 * 1024 {
        return Err("image_storage_full".into());
    }
    let name = std::ffi::CString::new(format!("{key}.png")).unwrap();
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
                "request_already_attempted_check_terminal_do_not_resend"
            } else {
                "image_storage_unavailable"
            }
            .into(),
        );
    }
    // File creation is also the durable no-replay marker, retained even on uncertain failure.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(image)
        .and_then(|_| file.sync_all())
        .map_err(|_| "image_storage_write_failed")?;
    dir.sync_all().map_err(|_| "image_storage_write_failed")?;
    Ok(directory.join(format!("{key}.png")))
}

fn composer_line(screen: &str) -> Option<&str> {
    screen
        .trim_end()
        .lines()
        .rev()
        .take(16)
        .find_map(|line| line.split_once('›').map(|(_, content)| content))
}

fn empty_composer(screen: &str) -> bool {
    let Some(line) = composer_line(screen) else {
        return false;
    };
    let plain = crate::tmux::strip_terminal_escapes(line);
    // Codex 0.154: placeholder is dim, actual draft text isn't. Unknown UI fails closed.
    plain.trim().is_empty()
        || (line.contains("\x1b[2m") && plain.trim() == "Ask Codex to do anything")
}

pub fn submit(
    session: &str,
    owner: &str,
    body: &serde_json::Value,
    authorized: impl Fn() -> bool,
) -> Result<(), String> {
    let text = body["text"].as_str().ok_or("invalid_prompt")?;
    let request = body["requestId"].as_str().ok_or("invalid_request_id")?;
    validate_prompt(text, request)?;
    let image = normalize(body["imageBase64"].as_str().ok_or("missing_image")?)?;
    let _input = input_guard(session)?;
    let state = crate::state::load_state();
    if !state
        .terminals
        .iter()
        .any(|t| t.session_name == session && t.agent_type == "codex")
    {
        return Err("image_prompts_require_codex".into());
    }
    let status = crate::tmux::inspect_status(session, "codex");
    if status.status != "IDLE" || status.cmd != "codex" {
        return Err("wait_for_idle_terminal".into());
    }
    let mut control = crate::tmux_control::Control::open(session)?;
    if !empty_composer(&control.capture()?) {
        return Err("clear_remote_draft_or_close_menu_first".into());
    }
    let home = std::env::var_os("HOME").ok_or("image_storage_unavailable")?;
    let key = format!(
        "{:x}",
        Sha256::digest(format!("{owner}\0{session}\0{request}"))
    );
    if !authorized() {
        return Err("device_revoked".into());
    }
    let path = stage(
        &PathBuf::from(home).join(".local/state/super-desktop/prompt-images"),
        &key,
        &image,
    )?;
    if !authorized() {
        return Err("device_revoked".into());
    }
    prepare_composer(&mut control, &path, text, &authorized)?;
    control.send("", true)?;
    Ok(())
}

fn prepare_composer(
    control: &mut crate::tmux_control::Control,
    path: &Path,
    text: &str,
    authorized: impl Fn() -> bool,
) -> Result<(), String> {
    if path.to_string_lossy().chars().any(char::is_control) {
        return Err("invalid_image_storage_path".into());
    }
    control.send(&format!("\x1b[200~{}\x1b[201~", path.display()), false)?;
    let started = Instant::now();
    loop {
        if !authorized() {
            return Err("device_revoked".into());
        }
        let screen = control.capture()?;
        let attached = composer_line(&screen)
            .map(crate::tmux::strip_terminal_escapes)
            .is_some_and(|s| s.trim() == "[Image #1]");
        if attached {
            break;
        }
        if started.elapsed() > Duration::from_secs(3) {
            return Err("attachment_not_confirmed_check_remote_draft".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !text.is_empty() {
        control.send(&format!("\x1b[200~{text}\x1b[201~"), false)?;
    }
    std::thread::sleep(Duration::from_millis(200));
    if !authorized() {
        return Err("device_revoked_check_remote_draft".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_paths_controls_and_invalid_requests() {
        assert_eq!(
            route("/api/v1/harnesses/sd_term_1/image-prompt"),
            Some("sd_term_1")
        );
        assert!(route("/api/v1/harnesses/../image-prompt").is_none());
        assert!(validate_prompt("hello\nworld", &"a".repeat(32)).is_ok());
        assert!(validate_prompt("hello\x1b[201~", &"a".repeat(32)).is_err());
        assert!(validate_prompt("hello", "../bad").is_err());
        assert!(normalize("not base64").is_err());
        assert!(normalize(&crate::ws::base64(b"<svg/>")).is_err());
    }
    #[test]
    fn composer_requires_empty_native_prompt() {
        assert!(empty_composer(
            "\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n"
        ));
        assert!(!empty_composer("› already typing\n"));
        assert!(!empty_composer("› [Image #1]\n"));
        assert!(!empty_composer("Choose a model\n"));
        assert!(!empty_composer(
            "› \x1b[2m[Pasted text #1 +10 lines]\x1b[0m\n"
        ));
    }
    #[test]
    fn validates_real_pixels_and_stages_without_replay() {
        let pixbuf =
            gtk4::gdk_pixbuf::Pixbuf::new(gtk4::gdk_pixbuf::Colorspace::Rgb, false, 8, 2, 2)
                .unwrap();
        pixbuf.fill(0xff0000ff);
        let png = pixbuf.save_to_bufferv("png", &[]).unwrap();
        let bytes = normalize(&crate::ws::base64(&png)).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "sd-prompt-image-{}",
            crate::tmux::unique_session_name()
        ));
        let key = "a".repeat(64);
        let path = stage(&dir, &key, &bytes).unwrap();
        assert!(stage(&dir, &key, &bytes)
            .unwrap_err()
            .contains("already_attempted"));
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    #[ignore = "Requires an explicitly created disposable Codex tmux session; never submits a prompt"]
    fn native_codex_attachment_probe() {
        let session = std::env::var("SD_IMAGE_PROBE_SESSION").unwrap();
        assert!(session.starts_with("sd_image_attachment_probe"));
        let mut control = crate::tmux_control::Control::open(&session).unwrap();
        assert!(
            empty_composer(&control.capture().unwrap()),
            "Probe requires an empty composer"
        );
        let pixbuf =
            gtk4::gdk_pixbuf::Pixbuf::new(gtk4::gdk_pixbuf::Colorspace::Rgb, false, 8, 2, 2)
                .unwrap();
        pixbuf.fill(0xff0000ff);
        let dir = std::env::temp_dir().join(format!(
            "sd-image-probe-{}",
            crate::tmux::unique_session_name()
        ));
        let path = stage(
            &dir,
            &"b".repeat(64),
            &pixbuf.save_to_bufferv("png", &[]).unwrap(),
        )
        .unwrap();
        prepare_composer(
            &mut control,
            &path,
            "attachment integration check - not submitted",
            || true,
        )
        .unwrap();
        let screen = crate::tmux::strip_terminal_escapes(&control.capture().unwrap());
        assert!(screen.contains("[Image #1]"));
        assert!(screen.contains("attachment integration check - not submitted"));
        control.send("\x15", false).unwrap(); // Clear only the draft created in this disposable test.
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
