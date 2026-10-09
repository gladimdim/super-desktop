//! Image validation, the per-terminal input lock, and the original
//! single-image route (`/image-prompt`), which older phone apps still use.
//! Staging and delivery live in `prompt_attachments`.
use gtk4::gdk_pixbuf::prelude::*;
use base64::{engine::general_purpose::STANDARD, Engine};
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
    harness_route(path, "/image-prompt")
}

/// The terminal id in `/api/v1/harnesses/<id><suffix>`, when it is a plain id
/// of at most 128 ASCII letters, digits, `_` and `-`.
pub fn harness_route<'a>(path: &'a str, suffix: &str) -> Option<&'a str> {
    let id = path
        .strip_prefix("/api/v1/harnesses/")?
        .strip_suffix(suffix)?;
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
    if !crate::control::is_hex(request, 32) {
        return Err("invalid_request_id".into());
    }
    Ok(())
}

pub(crate) fn normalize(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() > (MAX_IMAGE + 2) / 3 * 4 || encoded.as_bytes().contains(&0) {
        return Err("image_too_large".into());
    }
    // Decode and check canonical padding/trailing bits in the same pass.
    let bytes = STANDARD.decode(encoded).map_err(|_| "invalid_image_encoding")?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE {
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

/// Codex's idle animation paints braille "sparkles" across the composer row, in and
/// around its own placeholder (`›⠁Ask Codex to do anything⡀   ⠂ …`). They are
/// decoration the user never typed, so they are removed before the row is read.
fn without_sparkles(plain: &str) -> String {
    plain
        .chars()
        .filter(|c| !('\u{2800}'..='\u{28ff}').contains(c))
        .collect()
}

pub(crate) fn empty_composer(screen: &str) -> bool {
    let Some(line) = composer_line(screen) else {
        return false;
    };
    let plain = without_sparkles(&crate::tmux::strip_terminal_escapes(line));
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
    fn b64(bytes: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

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
        assert!(normalize(&b64(b"<svg/>")).is_err());
    }
    #[test]
    fn composer_requires_empty_native_prompt() {
        assert!(empty_composer(
            "\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n"
        ));
        // Codex's idle animation sparkles in and around its own placeholder: the
        // row is still empty. This is the line a live 0.154 card draws.
        assert!(empty_composer(
            "\x1b[1m›\x1b[0m\x1b[38;2;112;111;109m\x1b[48;2;44;44;43m⠁\x1b[2m\x1b[39mAsk Codex to do anything\
             \x1b[0m\x1b[38;2;124;122;120m\x1b[48;2;44;44;43m⡀\x1b[39m    \x1b[38;2;80;79;77m⠂\x1b[39m\n"
        ));
        assert!(empty_composer("\x1b[1m›\x1b[0m ⠁  ⡀   ⠂ ⣿\n"));
        // Text among the sparkles is still the user's draft.
        assert!(!empty_composer("› ⠁ already typing ⡀\n"));
        assert!(!empty_composer("› already typing\n"));
        assert!(!empty_composer("› [Image #1]\n"));
        assert!(!empty_composer("Choose a model\n"));
        assert!(!empty_composer(
            "› \x1b[2m[Pasted text #1 +10 lines]\x1b[0m\n"
        ));
    }
    #[test]
    fn image_encoding_remains_strict_before_pixel_decoding() {
        for bad in ["", "YQ", "YQ=", "YR==", "YWJ=", "YQ===", "Y Q==", "YQ==\n", "-_==", "éAAA"] {
            assert_eq!(normalize(bad).unwrap_err(), "invalid_image_encoding", "{bad:?}");
        }
        assert_eq!(normalize("YQ\0=").unwrap_err(), "image_too_large");
        let oversized = "A".repeat((MAX_IMAGE + 2) / 3 * 4 + 4);
        assert_eq!(normalize(&oversized).unwrap_err(), "image_too_large");
        // Same encoded length as the largest permitted image, but padding
        // determines whether the decoded bytes actually fit the limit.
        let too_many_bytes = b64(&vec![0; MAX_IMAGE + 1]);
        assert_eq!(normalize(&too_many_bytes).unwrap_err(), "invalid_image_encoding");
    }

    #[test]
    fn validates_real_pixels() {
        let pixbuf =
            gtk4::gdk_pixbuf::Pixbuf::new(gtk4::gdk_pixbuf::Colorspace::Rgb, false, 8, 2, 2)
                .unwrap();
        pixbuf.fill(0xff0000ff);
        let png = pixbuf.save_to_bufferv("png", &[]).unwrap();
        let bytes = normalize(&b64(&png)).unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
    }
}
