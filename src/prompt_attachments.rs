//! Phone prompts with attachments: images and files sent from the phone, kept
//! privately on this PC and handed to the harness together with the prompt.
//!
//! Harnesses that turn a pasted image path into an attachment of their own
//! (Codex, Claude Code, OpenCode, Grok) get each image that way, and the bridge
//! waits for the harness's `[Image …]` marker before anything else is typed.
//! Every other harness, and every non-image file, is referenced by its path in
//! the prompt; a shell gets the quoted paths after the command. Nothing is
//! submitted until the whole prompt is in the composer, and then Enter is sent
//! once.
//!
//! A harness that is still working takes the prompt too: the keystrokes are the
//! same ones the phone's text input already sends at any time, and agent TUIs
//! queue them for the next turn. A pane whose process has exited has nowhere to
//! put them, and a composer that already holds text the user typed on the PC is
//! never pasted over.
use crate::prompt_image::{input_guard, normalize, validate_prompt};
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// At most this many attachments ride on one prompt.
pub const MAX_ATTACHMENTS: usize = 4;
/// Decoded bytes of all attachments of one prompt together.
pub const MAX_TOTAL: usize = 16 * 1024 * 1024;
/// The request body: every attachment as base64, plus the prompt and names.
pub const MAX_BODY: usize = MAX_TOTAL / 3 * 4 + 4 + 64 * 1024;
/// How long the phone has to deliver that body.
pub const UPLOAD_DEADLINE: Duration = Duration::from_secs(180);
/// Characters of a stored file name (the extension included).
const MAX_NAME_CHARS: usize = 80;
/// The upload folder's capacity. Uploads are kept for the conversation that
/// uses them, so a full folder refuses new uploads instead of deleting any.
const STORE_BYTES: u64 = 512 * 1024 * 1024;
const STORE_ENTRIES: usize = 1024;
/// How long a harness has to show a pasted image as its own attachment.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(3);

pub fn route(path: &str) -> Option<&str> {
    crate::prompt_image::harness_route(path, "/attachment-prompt")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Image,
    File,
}

/// One attachment, validated: an image is re-encoded to PNG from its decoded
/// pixels, a file is kept byte for byte. `name` is already safe to store.
#[derive(Debug)]
pub struct Attachment {
    pub kind: Kind,
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Body<'a> {
    #[serde(default, borrow)]
    text: Cow<'a, str>,
    #[serde(default, borrow)]
    request_id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    attachments: Vec<Item<'a>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item<'a> {
    #[serde(borrow)]
    kind: Cow<'a, str>,
    #[serde(default, borrow)]
    name: Cow<'a, str>,
    #[serde(borrow)]
    data_base64: Cow<'a, str>,
}

/// The prompt, its request ID and its attachments, from the phone's JSON.
/// Strings are borrowed from the body: an upload is never copied whole.
pub fn parse(body: &str) -> Result<(String, String, Vec<Attachment>), String> {
    let body: Body = serde_json::from_str(body).map_err(|_| "bad_json")?;
    let request = body.request_id.as_deref().ok_or("invalid_request_id")?;
    validate_prompt(&body.text, request)?;
    if body.attachments.is_empty() {
        return Err("missing_attachments".into());
    }
    if body.attachments.len() > MAX_ATTACHMENTS {
        return Err("too_many_attachments".into());
    }
    let mut total = 0usize;
    let mut attachments = Vec::with_capacity(body.attachments.len());
    for item in &body.attachments {
        let kind = match item.kind.as_ref() {
            "image" => Kind::Image,
            "file" => Kind::File,
            _ => return Err("invalid_attachment_kind".into()),
        };
        if item.name.len() > 1024 {
            return Err("invalid_attachment_name".into());
        }
        // Checked before decoding, so a body over the limit is refused cheaply.
        total += item.data_base64.len() / 4 * 3;
        if total > MAX_TOTAL + 2 * MAX_ATTACHMENTS {
            return Err("attachments_too_large".into());
        }
        let bytes = match kind {
            Kind::Image => normalize(&item.data_base64)?,
            Kind::File => decode(&item.data_base64)?,
        };
        attachments.push(Attachment { kind, name: safe_name(&item.name, kind), bytes });
    }
    Ok((body.text.into_owned(), request.to_string(), attachments))
}

/// Strict base64: only the canonical encoding of the bytes it decodes to.
fn decode(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.is_empty() {
        return Err("empty_attachment".into());
    }
    if encoded.len() > (MAX_TOTAL + 2) / 3 * 4 {
        return Err("attachments_too_large".into());
    }
    STANDARD.decode(encoded).map_err(|_| "invalid_attachment_encoding".into())
}

/// A file name that is safe to store and to paste: letters and digits of any
/// script, `.`, `-` and `_`, never hidden, never option-like, bounded, with
/// its extension kept. An image is stored as the PNG it was re-encoded to.
pub fn safe_name(raw: &str, kind: Kind) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut cleaned = String::new();
    for c in base.chars() {
        let c = if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' };
        if c == '_' && cleaned.ends_with('_') {
            continue;
        }
        cleaned.push(c);
    }
    let cleaned = cleaned.trim_start_matches(['.', '-', '_']).trim_end_matches(['.', '_']);
    let (stem, extension) = match cleaned.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty() && !ext.is_empty() && ext.chars().count() <= 16 =>
        {
            (stem, Some(ext))
        }
        _ => (cleaned, None),
    };
    let fallback = match kind {
        Kind::Image => "image",
        Kind::File => "file",
    };
    let stem = if stem.is_empty() { fallback } else { stem };
    let extension = match kind {
        Kind::Image => Some("png"),
        Kind::File => extension,
    };
    let room = MAX_NAME_CHARS - extension.map_or(0, |ext| ext.chars().count() + 1);
    let stem: String = stem.chars().take(room).collect();
    let stem = stem.trim_end_matches(['.', '_']);
    let stem = if stem.is_empty() { fallback } else { stem };
    match extension {
        Some(ext) => format!("{stem}.{ext}"),
        None => stem.to_string(),
    }
}

/// How a harness takes an attachment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Delivery {
    /// The harness turns a pasted image path into its own `[Image …]`
    /// attachment; other files are named by path in the prompt.
    Native,
    /// Every attachment is named by its path in the prompt.
    Path,
    /// A shell: the prompt is a command, and the quoted paths follow it.
    Shell,
}

/// The delivery for the program running in the terminal right now: `command`
/// is its foreground command, so a custom launcher that starts `claude` gets
/// Claude's, and a harness that exited back to its shell gets the shell's.
/// `launch` is the card's launch command: OpenCode's `--mini` interface (the
/// launcher's default) does not turn a pasted path into an attachment.
pub fn delivery(agent_type: &str, command: &str, launch: &str) -> Delivery {
    match command {
        "opencode" if launch.split_whitespace().any(|arg| arg == "--mini") => Delivery::Path,
        "codex" | "claude" | "opencode" | "grok" => Delivery::Native,
        "bash" | "zsh" | "fish" | "sh" | "dash" | "ksh" | "nu" => Delivery::Shell,
        _ if agent_type == "shell" => Delivery::Shell,
        _ => Delivery::Path,
    }
}

/// Where the phone's uploads are kept: private, outside every workspace.
pub fn store_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("upload_storage_unavailable")?;
    Ok(PathBuf::from(home).join(".local/state/super-desktop/uploads"))
}

/// The folder of one request: bound to the device's credential, the terminal
/// and the request ID, so it doubles as the no-replay marker.
pub fn request_key(owner: &str, session: &str, request: &str) -> String {
    let digest = Sha256::digest(format!("{owner}\0{session}\0{request}"));
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Write the attachments into a new private folder for this request. The
/// folder's creation is the durable marker of the attempt: the same request
/// can never be staged, or submitted, twice.
pub fn stage(root: &Path, key: &str, attachments: &[Attachment]) -> Result<Vec<PathBuf>, String> {
    static STORAGE: Mutex<()> = Mutex::new(());
    let _guard = STORAGE.lock().map_err(|_| "upload_storage_unavailable")?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
        .map_err(|_| "upload_storage_unavailable")?;
    let dir = open_dir(None, root).map_err(|_| "unsafe_upload_storage")?;
    let meta = dir.metadata().map_err(|_| "upload_storage_unavailable")?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("unsafe_upload_storage".into());
    }
    let incoming: u64 = attachments.iter().map(|a| a.bytes.len() as u64).sum();
    let (used, entries) = usage(root)?;
    if entries >= STORE_ENTRIES || used + incoming > STORE_BYTES {
        return Err("upload_storage_full".into());
    }
    let name = std::ffi::CString::new(key).map_err(|_| "invalid_request_id")?;
    if unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        return Err(
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
                "request_already_attempted_check_terminal_do_not_resend"
            } else {
                "upload_storage_unavailable"
            }
            .into(),
        );
    }
    let folder = open_dir(Some(&dir), Path::new(key)).map_err(|_| "unsafe_upload_storage")?;
    let mut used_names: Vec<String> = Vec::new();
    let mut paths = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let name = unique_name(&attachment.name, &used_names);
        used_names.push(name.clone());
        let c_name = std::ffi::CString::new(name.as_str()).map_err(|_| "invalid_attachment_name")?;
        let fd = unsafe {
            libc::openat(
                folder.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err("upload_storage_write_failed".into());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(&attachment.bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "upload_storage_write_failed")?;
        paths.push(root.join(key).join(&name));
    }
    folder.sync_all().map_err(|_| "upload_storage_write_failed")?;
    dir.sync_all().map_err(|_| "upload_storage_write_failed")?;
    Ok(paths)
}

fn open_dir(parent: Option<&File>, path: &Path) -> std::io::Result<File> {
    match parent {
        None => OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path),
        Some(parent) => {
            let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
}

/// Bytes stored and request folders kept, one level deep.
fn usage(root: &Path) -> Result<(u64, usize), String> {
    let mut bytes = 0u64;
    let mut entries = 0usize;
    for entry in std::fs::read_dir(root).map_err(|_| "upload_storage_unavailable")? {
        let entry = entry.map_err(|_| "upload_storage_unavailable")?;
        entries += 1;
        let meta = entry.metadata().map_err(|_| "upload_storage_unavailable")?;
        if meta.is_dir() {
            for file in std::fs::read_dir(entry.path()).map_err(|_| "upload_storage_unavailable")? {
                let file = file.map_err(|_| "upload_storage_unavailable")?;
                bytes += file.metadata().map_err(|_| "upload_storage_unavailable")?.len();
            }
        } else {
            bytes += meta.len();
        }
    }
    Ok((bytes, entries))
}

/// `name`, or `name-2`, `name-3`… when an earlier attachment of the same
/// prompt already took it.
fn unique_name(name: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) => (stem, format!(".{ext}")),
        None => (name, String::new()),
    };
    (2..)
        .map(|n| format!("{stem}-{n}{ext}"))
        .find(|candidate| !taken.iter().any(|t| t == candidate))
        .expect("an unused name")
}

/// Whether the harness's composer holds text the user typed on the PC (a
/// draft that the phone's prompt must not be mixed into), when this harness's
/// composer is known. `None` for a harness whose composer cannot be read.
pub fn composer_has_draft(command: &str, screen: &str) -> Option<bool> {
    match command {
        "codex" => Some(!crate::prompt_image::empty_composer(screen)),
        "opencode" => Some(
            crate::tmux::extract_composer_draft(screen).is_some_and(|draft| !draft.trim().is_empty()),
        ),
        // Claude Code, Grok and Hermes (a Python process) draw `❯ ` then the draft.
        "claude" | "grok" | "python" => {
            let line = screen
                .trim_end()
                .lines()
                .rev()
                .take(16)
                .find_map(|line| line.split_once('❯').map(|(_, rest)| rest))?;
            Some(has_bright_text(line))
        }
        _ => None,
    }
}

/// Whether `line` shows any character outside a dim (placeholder) style.
fn has_bright_text(line: &str) -> bool {
    let mut dim = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                let mut params = String::new();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        if c == 'm' {
                            for code in params.split(';') {
                                match code {
                                    "" | "0" | "22" => dim = false,
                                    "2" => dim = true,
                                    _ => {}
                                }
                            }
                        }
                        break;
                    }
                    params.push(c);
                }
            }
            continue;
        }
        if !c.is_whitespace() && !dim {
            return true;
        }
    }
    false
}

/// How many `[Image #1]` / `[Image 1]` attachment markers a screen shows.
pub fn image_markers(screen: &str) -> usize {
    let plain = crate::terminal_text::strip_terminal_escapes(screen);
    plain
        .match_indices("[Image ")
        .filter(|(at, marker)| {
            let rest = &plain[at + marker.len()..];
            let rest = rest.strip_prefix('#').unwrap_or(rest);
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            digits > 0 && rest[digits..].starts_with(']')
        })
        .count()
}

/// A path as the prompt names it: bare, or in quotes when it has whitespace.
fn prompt_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.chars().any(char::is_whitespace) {
        format!("\"{text}\"")
    } else {
        text.into_owned()
    }
}

fn shell_quote(path: &Path) -> String {
    crate::harness_metadata::shell_quote(&path.to_string_lossy())
}

/// The text pasted into the composer after any native image attachments: the
/// user's prompt, then the paths the harness should read. A prompt never
/// starts with a path: a leading `/` opens slash-command menus.
pub fn prompt_text(delivery: Delivery, text: &str, attachments: &[Attachment], paths: &[PathBuf]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !text.is_empty() {
        parts.push(text.to_string());
    }
    for (attachment, path) in attachments.iter().zip(paths) {
        match delivery {
            Delivery::Native if attachment.kind == Kind::Image => {}
            Delivery::Shell => parts.push(shell_quote(path)),
            _ => {
                if parts.is_empty() {
                    parts.push("Attached:".into());
                }
                parts.push(prompt_path(path));
            }
        }
    }
    parts.join(" ")
}

fn paste(control: &mut crate::tmux_control::Control, text: &str) -> Result<(), String> {
    if text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err("invalid_prompt".into());
    }
    control.send(&format!("\x1b[200~{text}\x1b[201~"), false)
}

/// Put the whole prompt into the composer, without submitting it.
pub fn prepare(
    control: &mut crate::tmux_control::Control,
    delivery: Delivery,
    text: &str,
    attachments: &[Attachment],
    paths: &[PathBuf],
    authorized: &dyn Fn() -> bool,
) -> Result<(), String> {
    if delivery == Delivery::Native {
        for (attachment, path) in attachments.iter().zip(paths) {
            if attachment.kind != Kind::Image {
                continue;
            }
            let before = image_markers(&control.capture()?);
            paste(control, &path.to_string_lossy())?;
            let started = Instant::now();
            loop {
                if !authorized() {
                    return Err("device_revoked_check_remote_draft".into());
                }
                if image_markers(&control.capture()?) > before {
                    break;
                }
                if started.elapsed() > ATTACH_TIMEOUT {
                    return Err("attachment_not_confirmed_check_remote_draft".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    let payload = prompt_text(delivery, text, attachments, paths);
    if !payload.is_empty() {
        paste(control, &payload)?;
    }
    // Paste detection settles before Enter (Codex needs it).
    std::thread::sleep(Duration::from_millis(200));
    if !authorized() {
        return Err("device_revoked_check_remote_draft".into());
    }
    Ok(())
}

/// Stage the attachments and submit them with the prompt as one turn.
pub fn submit(
    session: &str,
    owner: &str,
    text: &str,
    request: &str,
    attachments: &[Attachment],
    authorized: impl Fn() -> bool,
) -> Result<(), String> {
    let _input = input_guard(session)?;
    let state = crate::state::load_state();
    let terminal = state
        .terminals
        .iter()
        .find(|t| t.session_name == session)
        .ok_or("no_such_session")?;
    if !crate::tmux::session_alive(session) {
        return Err("no_such_session".into());
    }
    let status = crate::tmux::inspect_status(session, &terminal.agent_type);
    // A working harness is not a closed door: typed input is queued by every
    // agent TUI, and a prompt with attachments is the same keystrokes. Only a
    // pane whose process is gone has nowhere to put them. What still protects
    // the terminal is the composer check below: nothing is pasted over text
    // the user is holding on the PC.
    if status.status == "EXITED" {
        return Err("no_such_session".into());
    }
    let delivery = delivery(&terminal.agent_type, &status.cmd, &terminal.command);
    // A shell would run the file itself: it needs a command to go with it.
    if delivery == Delivery::Shell && text.trim().is_empty() {
        return Err("type_a_command_for_the_attachment".into());
    }
    let mut control = crate::tmux_control::Control::open(session)?;
    if composer_has_draft(&status.cmd, &control.capture()?) == Some(true) {
        return Err("clear_remote_draft_or_close_menu_first".into());
    }
    if !authorized() {
        return Err("device_revoked".into());
    }
    let paths = stage(&store_dir()?, &request_key(owner, session, request), attachments)?;
    if !authorized() {
        return Err("device_revoked".into());
    }
    prepare(&mut control, delivery, text, attachments, &paths, &authorized)?;
    control.send("", true)?;
    if !text.is_empty() {
        crate::prompt_history::record(session, text);
    }
    Ok(())
}

/// At most this many files go into one desktop insert (a card's 📎 panel).
pub const MAX_LOCAL_ATTACHMENTS: usize = 20;

/// Whether a file on this PC is handed to a native harness as an image: the
/// formats every native harness turns into an `[Image …]` attachment.
pub fn local_kind(path: &Path) -> Kind {
    let image = path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"));
    if image { Kind::Image } else { Kind::File }
}

/// What the desktop panel types after any native images: the paths of the
/// other files, never submitted. Agents get `Attached:` first when nothing
/// is in the composer yet (a leading `/` opens slash-command menus), and a
/// space otherwise; a shell gets the quoted paths. A trailing space lets the
/// user keep typing.
pub fn local_insert_text(delivery: Delivery, items: &[(Kind, PathBuf)], composer_has_text: bool) -> String {
    let paths: Vec<String> = items
        .iter()
        .filter(|(kind, _)| !(delivery == Delivery::Native && *kind == Kind::Image))
        .map(|(_, path)| if delivery == Delivery::Shell { shell_quote(path) } else { prompt_path(path) })
        .collect();
    if paths.is_empty() {
        return String::new();
    }
    let lead = match delivery {
        Delivery::Shell => " ",
        _ if composer_has_text => " ",
        _ => "Attached: ",
    };
    format!("{lead}{} ", paths.join(" "))
}

/// Type files that are already on this PC into a local card's composer, as
/// the 📎 panel's Insert does. Images go natively to the harnesses that take
/// them; everything else is named by its path, where it is (nothing is
/// copied). Nothing is submitted: the user finishes the prompt and presses
/// Enter. Returns how many files were inserted.
pub fn insert_local(session: &str, paths: &[PathBuf]) -> Result<usize, String> {
    if paths.is_empty() {
        return Ok(0);
    }
    if paths.len() > MAX_LOCAL_ATTACHMENTS {
        return Err("too_many_attachments".into());
    }
    if let Some(gone) = paths.iter().find(|path| !path.exists()) {
        let name = gone.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Err(format!("missing:{name}"));
    }
    let _input = input_guard(session)?;
    let state = crate::state::load_state();
    let terminal = state
        .terminals
        .iter()
        .find(|t| t.session_name == session)
        .ok_or("no_such_session")?;
    if !crate::tmux::session_alive(session) {
        return Err("no_such_session".into());
    }
    let status = crate::tmux::inspect_status(session, &terminal.agent_type);
    if status.status == "EXITED" {
        return Err("no_such_session".into());
    }
    let delivery = delivery(&terminal.agent_type, &status.cmd, &terminal.command);
    let mut control = crate::tmux_control::Control::open(session)?;
    // Scrolled back in the card: keys would drive copy mode, not the program.
    if control.pane_format("#{pane_in_mode}")?.trim() == "1" {
        control.cancel_copy_mode()?;
    }
    let items: Vec<(Kind, PathBuf)> = paths.iter().map(|p| (local_kind(p), p.clone())).collect();
    let mut has_text = composer_has_draft(&status.cmd, &control.capture()?) == Some(true);
    if delivery == Delivery::Native {
        for (_, path) in items.iter().filter(|(kind, _)| *kind == Kind::Image) {
            let before = image_markers(&control.capture()?);
            paste(&mut control, &path.to_string_lossy())?;
            let started = Instant::now();
            while image_markers(&control.capture()?) <= before {
                if started.elapsed() > ATTACH_TIMEOUT {
                    return Err("image_not_confirmed".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            has_text = true;
        }
    }
    let text = local_insert_text(delivery, &items, has_text);
    if !text.is_empty() {
        paste(&mut control, &text)?;
    }
    Ok(items.len())
}

#[cfg(test)]
mod tests {
    fn b64(bytes: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    use super::*;

    fn png() -> String {
        // Big enough for every harness: Grok ignores a pasted 2×2 image.
        let pixbuf =
            gtk4::gdk_pixbuf::Pixbuf::new(gtk4::gdk_pixbuf::Colorspace::Rgb, false, 8, 64, 64).unwrap();
        pixbuf.fill(0xff0000ff);
        b64(&pixbuf.save_to_bufferv("png", &[]).unwrap())
    }

    fn body(attachments: serde_json::Value) -> String {
        serde_json::json!({"requestId": "a".repeat(32), "text": "Look", "attachments": attachments}).to_string()
    }

    #[test]
    fn routes_only_plain_terminal_ids() {
        assert_eq!(route("/api/v1/harnesses/sd_term_1/attachment-prompt"), Some("sd_term_1"));
        assert!(route("/api/v1/harnesses/../attachment-prompt").is_none());
        assert!(route("/api/v1/harnesses//attachment-prompt").is_none());
        assert!(route("/api/v1/harnesses/sd_term_1/image-prompt").is_none());
    }

    #[test]
    fn parses_images_and_files_and_refuses_the_rest() {
        let (text, request, attachments) = parse(&body(serde_json::json!([
            {"kind": "image", "name": "Screenshot 2026-09-26.jpg", "dataBase64": png()},
            {"kind": "file", "name": "report.pdf", "dataBase64": b64(b"%PDF-1.7")},
        ])))
        .unwrap();
        assert_eq!((text.as_str(), request.len()), ("Look", 32));
        assert_eq!(attachments[0].kind, Kind::Image);
        assert_eq!(attachments[0].name, "Screenshot_2026-09-26.png");
        assert!(attachments[0].bytes.starts_with(b"\x89PNG"));
        assert_eq!(attachments[1].name, "report.pdf");
        assert_eq!(attachments[1].bytes, b"%PDF-1.7");

        let refuse = |attachments: serde_json::Value| parse(&body(attachments)).unwrap_err();
        assert_eq!(refuse(serde_json::json!([])), "missing_attachments");
        let five: Vec<_> = (0..5)
            .map(|_| serde_json::json!({"kind": "file", "name": "a", "dataBase64": "YQ=="}))
            .collect();
        assert_eq!(refuse(serde_json::json!(five)), "too_many_attachments");
        assert_eq!(
            refuse(serde_json::json!([{"kind": "binary", "name": "a", "dataBase64": "YQ=="}])),
            "invalid_attachment_kind"
        );
        // An image must really be one; SVG and friends are refused.
        assert!(parse(&body(serde_json::json!([
            {"kind": "image", "name": "x.svg", "dataBase64": b64(b"<svg/>")}
        ])))
        .is_err());
        // Non-canonical or empty base64.
        assert_eq!(
            refuse(serde_json::json!([{"kind": "file", "name": "a", "dataBase64": "Y Q=="}])),
            "invalid_attachment_encoding"
        );
        assert_eq!(
            refuse(serde_json::json!([{"kind": "file", "name": "a", "dataBase64": ""}])),
            "empty_attachment"
        );
        // Over the combined limit.
        let big = "A".repeat((MAX_TOTAL / 3 + 8) * 4);
        assert_eq!(
            refuse(serde_json::json!([{"kind": "file", "name": "a", "dataBase64": big}])),
            "attachments_too_large"
        );
        // The prompt's own rules still apply.
        let bad = body(serde_json::json!([{"kind": "file", "name": "a", "dataBase64": "YQ=="}]))
            .replace("\"Look\"", "\"x\\u001b[201~\"");
        assert_eq!(parse(&bad).unwrap_err(), "invalid_prompt");
        assert_eq!(parse("{").unwrap_err(), "bad_json");
        // Only the canonical encoding of any bytes is accepted.
        assert!(decode("YQ==").is_ok() && decode("YWI=").is_ok() && decode("YWJj").is_ok());
        assert!(decode("YR==").is_err() && decode("YWJ=").is_err() && decode("YQ").is_err());
        assert!(decode("Y===").is_err() && decode("YQ=a").is_err() && decode("-_==").is_err());
    }

    #[test]
    fn decoding_accepts_only_canonical_bytes_including_all_tails() {
        for len in 1..=129 {
            let bytes: Vec<_> = (0..len).map(|i| (i * 197 + len) as u8).collect();
            let encoded = b64(&bytes);
            assert_eq!(decode(&encoded).unwrap(), bytes);
        }
        // Check all ASCII substitutions, including padding, NUL, whitespace,
        // bad alphabet bytes and nonzero unused bits, against canonical re-encoding.
        for encoded in ["AA==", "AAA=", "AAAA", "+/8=", "YWJjZA==", "YWJjZGU="] {
            for at in 0..encoded.len() {
                for byte in 0..=127 {
                    let mut candidate = encoded.as_bytes().to_vec();
                    candidate[at] = byte;
                    let candidate = String::from_utf8(candidate).unwrap();
                    // GLib takes a C string; the previous validator rejected NUL
                    // before calling it, as the new decoder must do too.
                    let expected = if byte == 0 {
                        None
                    } else {
                        let decoded = gtk4::glib::base64_decode(&candidate);
                        (b64(&decoded) == candidate).then_some(decoded)
                    };
                    assert_eq!(decode(&candidate).ok(), expected, "{candidate:?}");
                }
            }
        }
        for bad in ["", "YQ", "YQ=", "YQ===", "YQ==\n", "YQ==YQ==", "éAAA", "\u{a0}AAA"] {
            assert!(decode(bad).is_err(), "{bad:?}");
        }
        let encoded = "A".repeat((MAX_TOTAL + 2) / 3 * 4 + 4);
        assert_eq!(decode(&encoded).unwrap_err(), "attachments_too_large");
    }

    #[test]
    fn stored_names_are_plain_and_bounded() {
        assert_eq!(safe_name("../../etc/passwd", Kind::File), "passwd");
        assert_eq!(safe_name(".bashrc", Kind::File), "bashrc");
        assert_eq!(safe_name("-rf .txt", Kind::File), "rf.txt");
        assert_eq!(safe_name("звіт за вересень.pdf", Kind::File), "звіт_за_вересень.pdf");
        assert_eq!(safe_name("a$(rm -rf ~)`b`.sh", Kind::File), "a_rm_-rf_b.sh");
        assert_eq!(safe_name("", Kind::File), "file");
        assert_eq!(safe_name("", Kind::Image), "image.png");
        assert_eq!(safe_name("photo.heic", Kind::Image), "photo.png");
        assert_eq!(safe_name("Makefile", Kind::File), "Makefile");
        let long = safe_name(&format!("{}.tar.gz", "x".repeat(300)), Kind::File);
        assert_eq!(long.chars().count(), MAX_NAME_CHARS);
        assert!(long.ends_with(".gz"));
        assert_eq!(unique_name("a.png", &["a.png".into()]), "a-2.png");
        assert_eq!(unique_name("a", &["a".into(), "a-2".into()]), "a-3");
    }

    #[test]
    fn staging_is_private_and_never_replayed() {
        let root = std::env::temp_dir().join(format!(
            "sd-prompt-attachments-{}",
            crate::tmux::unique_session_name()
        ));
        let attachments = vec![
            Attachment { kind: Kind::File, name: "notes.txt".into(), bytes: b"one".to_vec() },
            Attachment { kind: Kind::File, name: "notes.txt".into(), bytes: b"two".to_vec() },
        ];
        let key = request_key("owner", "sd_term_1", &"a".repeat(32));
        assert_eq!(key.len(), 16);
        assert_ne!(key, request_key("other device", "sd_term_1", &"a".repeat(32)));
        let paths = stage(&root, &key, &attachments).unwrap();
        assert_eq!(paths[0], root.join(&key).join("notes.txt"));
        assert_eq!(paths[1], root.join(&key).join("notes-2.txt"));
        assert_eq!(std::fs::read(&paths[1]).unwrap(), b"two");
        assert_eq!(std::fs::metadata(&paths[0]).unwrap().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(root.join(&key)).unwrap().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
        // The same request can never be staged, and so submitted, twice.
        assert!(stage(&root, &key, &attachments).unwrap_err().contains("already_attempted"));
        // A storage folder anyone else can read is refused.
        std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert_eq!(stage(&root, "b".repeat(16).as_str(), &attachments).unwrap_err(), "unsafe_upload_storage");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn each_harness_gets_attachments_its_own_way() {
        assert_eq!(delivery("codex", "codex", "codex"), Delivery::Native);
        assert_eq!(delivery("claude", "claude", "claude --settings x"), Delivery::Native);
        assert_eq!(delivery("grok", "grok", "grok --minimal"), Delivery::Native);
        assert_eq!(delivery("opencode", "opencode", "opencode"), Delivery::Native);
        // OpenCode's mini interface (the launcher's default) keeps a pasted path as text.
        assert_eq!(delivery("opencode", "opencode", "/x/opencode --mini --auto"), Delivery::Path);
        // A custom launcher running Claude, and a harness that exited to its shell.
        assert_eq!(delivery("my-claude", "claude", "my-claude"), Delivery::Native);
        assert_eq!(delivery("codex", "bash", "codex"), Delivery::Shell);
        assert_eq!(delivery("hermes", "python", "hermes"), Delivery::Path);
        assert_eq!(delivery("pi", "pi", "pi"), Delivery::Path);
        assert_eq!(delivery("shell", "htop", "bash"), Delivery::Shell);

        let attachments = vec![
            Attachment { kind: Kind::Image, name: "a.png".into(), bytes: vec![] },
            Attachment { kind: Kind::File, name: "b.pdf".into(), bytes: vec![] },
        ];
        let paths = vec![PathBuf::from("/u/k/a.png"), PathBuf::from("/u/k/it's b.pdf")];
        // Native images are attached on their own; only the file is named.
        assert_eq!(prompt_text(Delivery::Native, "Compare", &attachments, &paths), "Compare \"/u/k/it's b.pdf\"");
        // Never a leading path: `/` would open a slash-command menu.
        assert_eq!(
            prompt_text(Delivery::Path, "", &attachments, &paths),
            "Attached: /u/k/a.png \"/u/k/it's b.pdf\""
        );
        assert_eq!(prompt_text(Delivery::Native, "", &attachments, &paths), "Attached: \"/u/k/it's b.pdf\"");
        assert_eq!(
            prompt_text(Delivery::Shell, "wc -c", &attachments, &paths),
            "wc -c '/u/k/a.png' '/u/k/it'\\''s b.pdf'"
        );
        assert_eq!(prompt_text(Delivery::Native, "", &attachments[..1], &paths[..1]), "");
    }

    #[test]
    fn attachment_markers_and_drafts_are_read_from_the_screen() {
        assert_eq!(image_markers("› [Image #1] [Image #2] look"), 2);
        assert_eq!(image_markers("┃  \x1b[1m[Image 1]\x1b[0m"), 1);
        assert_eq!(image_markers("[Image #] [Image x] [Image 3"), 0);

        // Empty composers, exactly as the installed harnesses draw them.
        assert_eq!(composer_has_draft("claude", "\x1b[39m❯\u{a0}\n"), Some(false));
        assert_eq!(composer_has_draft("grok", "\x1b[0m❯\n"), Some(false));
        assert_eq!(composer_has_draft("python", "\x1b[38;5;230m❯ \x1b[39m\n"), Some(false));
        assert_eq!(
            composer_has_draft("codex", "\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n"),
            Some(false)
        );
        // A placeholder in a dim style is not a draft; typed text is.
        assert_eq!(composer_has_draft("claude", "❯ \x1b[2mTry \"fix lint\"\x1b[22m\n"), Some(false));
        assert_eq!(composer_has_draft("claude", "❯ half a prompt\n"), Some(true));
        assert_eq!(composer_has_draft("grok", "❯ \x1b[2mhint\x1b[0m typed\n"), Some(true));
        // No composer to read: unknown, never a guess.
        assert_eq!(composer_has_draft("pi", "────\n\n────\n"), None);
        assert_eq!(composer_has_draft("claude", "Choose a model\n"), None);
    }

    /// Put an image and a file into a live harness's composer the way `submit`
    /// does, and never press Enter.
    ///
    /// Driven by `tests/harness_phone_matrix.py --attachments`, which launches
    /// each installed harness in a private tmux server exactly as a card and
    /// passes its session here; the draft this makes is cleared afterwards.
    #[test]
    #[ignore = "Needs a disposable harness session (SD_ATTACHMENT_PROBE_SESSION); never submits"]
    fn native_attachment_probe() {
        let session = std::env::var("SD_ATTACHMENT_PROBE_SESSION").unwrap();
        let agent = std::env::var("SD_ATTACHMENT_PROBE_AGENT").unwrap();
        let out = std::env::var("SD_ATTACHMENT_PROBE_OUT").unwrap();
        let status = crate::tmux::inspect_status(&session, &agent);
        let launch = std::env::var("SD_ATTACHMENT_PROBE_LAUNCH").unwrap_or_default();
        let delivery = delivery(&agent, &status.cmd, &launch);
        let mut control = crate::tmux_control::Control::open(&session).unwrap();
        let empty = composer_has_draft(&status.cmd, &control.capture().unwrap());
        let root = std::env::temp_dir().join(format!(
            "sd-attachment-probe-{}",
            crate::tmux::unique_session_name()
        ));
        let (_, _, attachments) = parse(&serde_json::json!({
            "requestId": "c".repeat(32), "text": "",
            "attachments": [
                {"kind": "image", "name": "probe.png", "dataBase64": png()},
                {"kind": "file", "name": "probe notes.txt", "dataBase64": b64(b"probe\n")},
            ],
        }).to_string())
        .unwrap();
        let paths = stage(&root, &"d".repeat(16), &attachments).unwrap();
        let prepared = prepare(
            &mut control,
            delivery,
            "attachment check - not submitted",
            &attachments,
            &paths,
            &|| true,
        );
        let screen = crate::terminal_text::strip_terminal_escapes(&control.capture().unwrap());
        std::fs::write(
            &out,
            serde_json::json!({
                "command": status.cmd,
                "delivery": format!("{delivery:?}"),
                "draftBefore": empty,
                "prepared": prepared.as_ref().map(|_| "ok").unwrap_or_else(|e| e.as_str()),
                "imageMarkers": image_markers(&screen),
                "textShown": screen.contains("attachment check - not submitted"),
                "screen": screen,
            })
            .to_string(),
        )
        .unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        prepared.unwrap();
    }

    #[test]
    fn local_files_are_typed_by_path_or_natively_without_submitting() {
        let dir = std::env::temp_dir().join(format!("sd-local-kind-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("shots.png")).unwrap();
        std::fs::write(dir.join("Photo.JPG"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert_eq!(local_kind(&dir.join("Photo.JPG")), Kind::Image);
        assert_eq!(local_kind(&dir.join("notes.txt")), Kind::File);
        // A folder named like an image is still a folder.
        assert_eq!(local_kind(&dir.join("shots.png")), Kind::File);
        std::fs::remove_dir_all(&dir).unwrap();

        let items = vec![
            (Kind::Image, PathBuf::from("/tmp/a.png")),
            (Kind::File, PathBuf::from("/tmp/my notes.txt")),
            (Kind::File, PathBuf::from("/tmp/it's.md")),
        ];
        // Native: images went in on their own; the rest follows a space.
        assert_eq!(
            local_insert_text(Delivery::Native, &items, true),
            " \"/tmp/my notes.txt\" /tmp/it's.md "
        );
        // An empty composer never starts with a path.
        assert_eq!(
            local_insert_text(Delivery::Path, &items, false),
            "Attached: /tmp/a.png \"/tmp/my notes.txt\" /tmp/it's.md "
        );
        assert_eq!(local_insert_text(Delivery::Path, &items[..1], true), " /tmp/a.png ");
        // A shell gets every path quoted for the shell.
        assert_eq!(
            local_insert_text(Delivery::Shell, &items, false),
            r#" '/tmp/a.png' '/tmp/my notes.txt' '/tmp/it'\''s.md' "#
        );
        assert_eq!(local_insert_text(Delivery::Native, &items[..1], false), "");
    }

    #[test]
    fn local_insert_types_into_a_private_tmux_card() {
        let root = std::env::temp_dir().join(format!("sd-local-insert-{}", std::process::id()));
        std::fs::create_dir_all(root.join("tmux")).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "prompt_attachments::tests::local_insert_tmux_inner", "--nocapture"])
            .env("SD_LOCAL_INSERT_ROOT", &root)
            .env("TMUX_TMPDIR", root.join("tmux"))
            .env("HOME", &root)
            .env("XDG_STATE_HOME", root.join("state"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn local_insert_tmux_inner() {
        let Some(root) = std::env::var_os("SD_LOCAL_INSERT_ROOT").map(PathBuf::from) else {
            return;
        };
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::process::Command::new("tmux").arg("kill-server").output();
            }
        }
        let _cleanup = Cleanup;
        let tmux = |args: &[&str]| {
            let output = std::process::Command::new("tmux").args(args).output().unwrap();
            assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8(output.stdout).unwrap()
        };
        let wait = |what: &dyn Fn() -> bool, why: &str| {
            let until = Instant::now() + Duration::from_secs(5);
            while !what() {
                assert!(Instant::now() < until, "timed out: {why}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let quote = |path: &Path| shlex::try_quote(path.to_str().unwrap()).unwrap().into_owned();
        let files = root.join("files");
        std::fs::create_dir_all(&files).unwrap();
        let doc = files.join("design notes.md");
        let odd = files.join("it's.txt");
        let shot = files.join("shot.png");
        for path in [&doc, &odd, &shot] {
            std::fs::write(path, b"x").unwrap();
        }

        // A shell card: the quoted paths are typed, and nothing is run.
        let shell_out = root.join("shell-received");
        let shell = "sd_term_local_shell";
        tmux(&["-f", "/dev/null", "new-session", "-d", "-s", shell,
            &format!("stty raw -echo; printf READY; exec cat > {}", quote(&shell_out))]);
        // A stand-in for Claude Code: a program named `claude` that shows its
        // prompt, takes a pasted image path, answers with Claude's marker and
        // keeps everything else it is sent.
        let claude_out = root.join("claude-received");
        let stub = root.join("claude");
        std::fs::copy("/usr/bin/bash", &stub).unwrap();
        let script = format!(
            "stty raw -echo; printf '❯ '; IFS= read -r -d '~' a; IFS= read -r -d '~' b; \
             printf '%s' \"$b\" > {out}; printf ' [Image #1] '; exec cat >> {out}",
            out = quote(&claude_out)
        );
        let claude = "sd_term_local_claude";
        tmux(&["new-session", "-d", "-s", claude, &format!("exec {} -c {}", quote(&stub), quote(Path::new(&script)))]);
        let card = |session: &str, agent: &str| serde_json::json!({
            "id": session, "session_name": session, "agent_type": agent,
            "command": agent, "x": 0, "y": 0, "created_at": 1.0,
        });
        let state = serde_json::json!({"notes": [], "terminals": [card(shell, "shell"), card(claude, "claude")]});
        std::fs::write(crate::state::get_state_path(), state.to_string()).unwrap();
        wait(&|| tmux(&["capture-pane", "-p", "-t", shell]).contains("READY"), "shell ready");
        wait(&|| tmux(&["display-message", "-p", "-t", claude, "#{pane_current_command}"]).trim() == "claude", "claude stub");

        assert_eq!(insert_local(shell, &[doc.clone(), odd.clone()]), Ok(2));
        let expected = format!("\x1b[200~ {} {} \x1b[201~", shell_quote(&doc), shell_quote(&odd));
        wait(&|| std::fs::read(&shell_out).unwrap_or_default() == expected.as_bytes(), "shell paste");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(std::fs::read(&shell_out).unwrap(), expected.as_bytes(), "no Enter, nothing else");

        // Claude: the image first, natively; the document by path after it.
        assert_eq!(insert_local(claude, &[shot.clone(), doc.clone()]), Ok(2));
        let expected = format!("{}\x1b[201\x1b[200~ \"{}\" \x1b[201~", shot.display(), doc.display());
        wait(&|| std::fs::read(&claude_out).unwrap_or_default() == expected.as_bytes(), "claude paste");

        // Gone files and gone terminals are refused before anything is typed.
        assert_eq!(insert_local(shell, &[files.join("missing.txt")]), Err("missing:missing.txt".into()));
        assert_eq!(insert_local("sd_term_nope", &[doc.clone()]), Err("no_such_session".into()));
        let many: Vec<PathBuf> = (0..=MAX_LOCAL_ATTACHMENTS).map(|_| doc.clone()).collect();
        assert_eq!(insert_local(shell, &many), Err("too_many_attachments".into()));
        assert_eq!(std::fs::read(&shell_out).unwrap(), format!("\x1b[200~ {} {} \x1b[201~", shell_quote(&doc), shell_quote(&odd)).as_bytes());
    }
}
