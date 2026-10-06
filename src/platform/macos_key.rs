//! macOS virtual key codes used by the native global shortcut.
pub fn parse(combo: &str) -> Result<(u32, u32), String> {
    let mut parts: Vec<_> = combo.split('+').map(str::trim).collect();
    let key = parts.pop().unwrap_or_default().to_ascii_uppercase();
    let mut modifiers = 0;
    for part in parts {
        modifiers |= match part.to_ascii_uppercase().as_str() {
            "SUPER" | "CMD" | "META" => 1 << 8,
            "SHIFT" => 1 << 9,
            "ALT" | "OPTION" => 1 << 11,
            "CTRL" | "CONTROL" => 1 << 12,
            _ => return Err(format!("Unknown shortcut modifier: {part}")),
        };
    }
    let code =
        match key.as_str() {
            "A" => 0,
            "S" => 1,
            "D" => 2,
            "F" => 3,
            "H" => 4,
            "G" => 5,
            "Z" => 6,
            "X" => 7,
            "C" => 8,
            "V" => 9,
            "B" => 11,
            "Q" => 12,
            "W" => 13,
            "E" => 14,
            "R" => 15,
            "Y" => 16,
            "T" => 17,
            "1" => 18,
            "2" => 19,
            "3" => 20,
            "4" => 21,
            "6" => 22,
            "5" => 23,
            "9" => 25,
            "7" => 26,
            "8" => 28,
            "0" => 29,
            "O" => 31,
            "U" => 32,
            "I" => 34,
            "P" => 35,
            "RETURN" => 36,
            "L" => 37,
            "J" => 38,
            "K" => 40,
            "N" => 45,
            "M" => 46,
            "TAB" => 48,
            "SPACE" => 49,
            "BACKSPACE" => 51,
            "ESCAPE" => 53,
            "F1" => 122,
            "F2" => 120,
            "F3" => 99,
            "F4" => 118,
            "F5" => 96,
            "F6" => 97,
            "F7" => 98,
            "F8" => 100,
            "F9" => 101,
            "F10" => 109,
            "F11" => 103,
            "F12" => 111,
            _ => return Err(
                "Use a Latin letter, number, Space, Tab, Return, Escape or F1–F12 for the shortcut"
                    .into(),
            ),
        };
    if modifiers & !(1 << 9) == 0 && !(modifiers == 0 && key.starts_with('F') && key.len() > 1) {
        return Err("Hold Command, Control or Option with the key".into());
    }
    Ok((code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_shortcut_maps_default_and_rejects_bare_typing_keys() {
        assert_eq!(parse("CTRL + ALT + space"), Ok((49, 6144)));
        assert_eq!(parse("SUPER + SHIFT + Q"), Ok((12, 768)));
        assert_eq!(parse("F5"), Ok((96, 0)));
        for invalid in ["Q", "SHIFT + Q", "SHIFT + F5", "CTRL + unknown", "wat + Q"] {
            assert!(parse(invalid).is_err(), "{invalid}");
        }
    }
}
