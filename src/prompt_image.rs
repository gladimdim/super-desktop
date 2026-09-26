//! Image validation, the per-terminal input lock, and the original
//! single-image route (`/image-prompt`), which older phone apps still use.
//! Staging and delivery live in `prompt_attachments`.
use gtk4::gdk_pixbuf::prelude::*;
use sha2::{Digest, Sha256};
use std::sync::{Mutex, MutexGuard, OnceLock};

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

pub(crate) fn normalize(encoded: &str) -> Result<Vec<u8>, String> {
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

fn composer_line(screen: &str) -> Option<&str> {
    screen
        .trim_end()
        .lines()
        .rev()
        .take(16)
        .find_map(|line| line.split_once('›').map(|(_, content)| content))
}

pub(crate) fn empty_composer(screen: &str) -> bool {
    let Some(line) = composer_line(screen) else {
        return false;
    };
    let plain = crate::tmux::strip_terminal_escapes(line);
    // Codex 0.154: placeholder is dim, actual draft text isn't. Unknown UI fails closed.
    plain.trim().is_empty()
        || (line.contains("\x1b[2m") && plain.trim() == "Ask Codex to do anything")
}

/// The single-image route: one image and the prompt, delivered like any
/// other attachment prompt.
pub fn submit(
    session: &str,
    owner: &str,
    body: &serde_json::Value,
    authorized: impl Fn() -> bool,
) -> Result<(), String> {
    use crate::prompt_attachments::{Attachment, Kind};
    let text = body["text"].as_str().ok_or("invalid_prompt")?;
    let request = body["requestId"].as_str().ok_or("invalid_request_id")?;
    validate_prompt(text, request)?;
    let image = normalize(body["imageBase64"].as_str().ok_or("missing_image")?)?;
    let attachment = Attachment { kind: Kind::Image, name: "image.png".into(), bytes: image };
    crate::prompt_attachments::submit(session, owner, text, request, &[attachment], authorized)
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
    fn validates_real_pixels() {
        let pixbuf =
            gtk4::gdk_pixbuf::Pixbuf::new(gtk4::gdk_pixbuf::Colorspace::Rgb, false, 8, 2, 2)
                .unwrap();
        pixbuf.fill(0xff0000ff);
        let png = pixbuf.save_to_bufferv("png", &[]).unwrap();
        let bytes = normalize(&crate::ws::base64(&png)).unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
    }
}
