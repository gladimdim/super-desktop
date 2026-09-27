//! Terminal text helpers that must stay GTK-free: compiled into both the
//! application and `super-desktop-client`.

/// Remove terminal control sequences while preserving the visible text.
/// Handles CSI/OSC/DCS strings as well as two-byte ESC commands; tmux `-e`
/// currently emits SGR CSI sequences, while the wider handling keeps the
/// plain fallback safe if tmux expands what it preserves later.
pub fn strip_terminal_escapes(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x1b if i + 1 < bytes.len() => {
                i += 1;
                match bytes[i] {
                    b'[' => {
                        i += 1;
                        while i < bytes.len() {
                            let byte = bytes[i];
                            i += 1;
                            if (0x40..=0x7e).contains(&byte) {
                                break;
                            }
                        }
                    }
                    b']' | b'P' | b'^' | b'_' => {
                        i += 1;
                        while i < bytes.len() {
                            if bytes[i] == 0x07 {
                                i += 1;
                                break;
                            }
                            if bytes[i] == 0x1b
                                && i + 1 < bytes.len()
                                && bytes[i + 1] == b'\\'
                            {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    _ => i += 1,
                }
            }
            0x1b => i += 1,
            byte @ (b'\n' | b'\r' | b'\t') => {
                out.push(byte);
                i += 1;
            }
            byte if byte < 0x20 => i += 1,
            _ => {
                let start = i;
                i += ordinary_prefix(&bytes[i..]);
                out.extend_from_slice(&bytes[start..i]);
            }
        }
    }
    // An unknown ESC command can consume the leading byte of a UTF-8
    // character. Keep the lossy repair in that case, but reuse valid output.
    String::from_utf8(out)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

/// Scan ordinary text in fixed blocks so LLVM can use the host target's
/// baseline SIMD. C0 bytes (including whitespace) stay in the scalar parser;
/// all UTF-8 bytes and DEL keep their existing treatment.
fn ordinary_prefix(bytes: &[u8]) -> usize {
    if bytes.first().is_some_and(|&byte| byte < 0x20) {
        return 0;
    }
    let mut rest = bytes;
    while rest.len() >= 32 {
        let minimum = rest[..32].iter().fold(u8::MAX, |min, &byte| min.min(byte));
        if minimum < 0x20 {
            break;
        }
        rest = &rest[32..];
    }
    bytes.len() - rest.len() + rest.iter().position(|&byte| byte < 0x20).unwrap_or(rest.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_scan_matches_byte_scan_at_every_boundary() {
        for offset in 0..32 {
            for length in 0..=96 {
                let mut storage = vec![b'x'; offset + length];
                assert_eq!(ordinary_prefix(&storage[offset..]), length);
                for position in 0..length {
                    for byte in [0, 7, 9, 10, 13, 27, 31, 32, 127, 128, 255] {
                        storage[offset + position] = byte;
                        let expected = if byte < 0x20 { position } else { length };
                        assert_eq!(ordinary_prefix(&storage[offset..]), expected);
                    }
                    storage[offset + position] = b'x';
                }
            }
        }
    }

    #[test]
    fn terminal_text_preserves_controls_unicode_and_truncated_sequences() {
        for padding in 0..65 {
            let prefix = "x".repeat(padding);
            for (input, expected) in [
                ("", ""),
                ("Готово ✓ 日本語\u{7f}", "Готово ✓ 日本語\u{7f}"),
                ("a\0\x07\x08\x0b\x1fc\t\r\n", "ac\t\r\n"),
                ("\x1b[31mred\x1b[0m", "red"),
                ("\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x07", "link"),
                ("\x1bPdiscard\x1b\\ok\x1b^discard\x07!\x1b_discard\x07", "ok!"),
                ("\x1b7saved\x1b8", "saved"),
                ("text\x1b", "text"),
                ("text\x1b[31", "text"),
                ("text\x1b]unterminated", "text"),
                ("text\x1bPunterminated\x1b", "text"),
                ("\x1bé", "�"),
                ("\x1b[é", ""),
            ] {
                assert_eq!(strip_terminal_escapes(&format!("{prefix}{input}")), format!("{prefix}{expected}"));
            }
        }
    }
}
