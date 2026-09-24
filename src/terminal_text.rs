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
            byte if byte < 0x20 && !matches!(byte, b'\n' | b'\r' | b'\t') => i += 1,
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
