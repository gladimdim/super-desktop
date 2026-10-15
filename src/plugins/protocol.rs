//! Messages between SUPER DESKTOP and `sd-qml-host`, and the mapping of GTK
//! input to the Qt values the host feeds to QML.
//!
//! One JSON object per SOCK_SEQPACKET packet. Unknown message types and
//! fields are ignored on both sides, so either side can grow.
use serde_json::{json, Value};

/// What the host sends.
#[derive(Debug, Clone, PartialEq)]
pub enum FromHost {
    Ready,
    /// QML failed to load; the host exits after this.
    Error(Value),
    /// A new shared-memory frame buffer (the fd rides on the packet).
    Buffer { width: i32, height: i32, stride: i32, scale: f64 },
    Frame { seq: u64 },
    Log { level: String, message: String },
    Call { id: u64, method: String, params: Value },
    /// QML asked for another pointer shape (a Qt::CursorShape value).
    Cursor { shape: i64 },
    /// No QML item took a `scroll` (the overlay's own deltas, echoed): what
    /// is around the surface may scroll instead.
    ScrollIgnored { dx: f64, dy: f64 },
    Unknown,
}

pub fn parse(bytes: &[u8]) -> FromHost {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return FromHost::Unknown;
    };
    let int = |key: &str| value.get(key).and_then(Value::as_i64).unwrap_or(0);
    match value.get("t").and_then(Value::as_str).unwrap_or("") {
        "ready" => FromHost::Ready,
        "error" => FromHost::Error(value.get("errors").cloned().unwrap_or(Value::Array(Vec::new()))),
        "buffer" => FromHost::Buffer {
            width: int("width") as i32,
            height: int("height") as i32,
            stride: int("stride") as i32,
            scale: value.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
        },
        "frame" => FromHost::Frame { seq: int("seq") as u64 },
        "log" => FromHost::Log {
            level: value.get("level").and_then(Value::as_str).unwrap_or("info").to_owned(),
            message: value.get("message").and_then(Value::as_str).unwrap_or("").to_owned(),
        },
        "call" => FromHost::Call {
            id: int("id") as u64,
            method: value.get("method").and_then(Value::as_str).unwrap_or("").to_owned(),
            params: value.get("params").cloned().unwrap_or(Value::Null),
        },
        "cursor" => FromHost::Cursor { shape: int("shape") },
        "scrollIgnored" => {
            let float = |key: &str| value.get(key).and_then(Value::as_f64).unwrap_or(0.0);
            FromHost::ScrollIgnored { dx: float("dx"), dy: float("dy") }
        }
        _ => FromHost::Unknown,
    }
}

/// Qt::CursorShape → CSS cursor name for GDK. `None` means the default.
pub fn cursor_name(shape: i64) -> Option<&'static str> {
    Some(match shape {
        1 => "n-resize",      // UpArrow
        2 => "crosshair",
        3 => "wait",
        4 => "text",          // IBeam
        5 => "ns-resize",     // SizeVer
        6 => "ew-resize",     // SizeHor
        7 => "nesw-resize",   // SizeBDiag
        8 => "nwse-resize",   // SizeFDiag
        9 => "move",          // SizeAll
        10 => "none",         // Blank
        11 => "row-resize",   // SplitV
        12 => "col-resize",   // SplitH
        13 => "pointer",      // PointingHand
        14 => "not-allowed",  // Forbidden
        15 => "help",         // WhatsThis
        16 => "progress",     // Busy
        17 => "grab",         // OpenHand
        18 => "grabbing",     // ClosedHand
        19 => "copy",         // DragCopy
        20 => "alias",        // DragLink
        _ => return None,
    })
}

pub fn resize(width: i32, height: i32) -> Value {
    json!({"t": "resize", "width": width, "height": height})
}

pub fn ack(seq: u64) -> Value {
    json!({"t": "ack", "seq": seq})
}

pub fn visible(value: bool) -> Value {
    json!({"t": "visible", "value": value})
}

pub fn focus(value: bool) -> Value {
    json!({"t": "focus", "value": value})
}

pub fn theme(theme: &Value) -> Value {
    json!({"t": "theme", "theme": theme})
}

/// A click on one of the plugin's card buttons, for `sd.cardButtonClicked`.
pub fn card_button(button: &str, card: &Value) -> Value {
    json!({"t": "event", "name": "cardButton", "button": button, "card": card})
}

pub fn reply(id: u64, result: Result<Value, Value>) -> Value {
    match result {
        Ok(result) => json!({"t": "reply", "id": id, "result": result, "error": Value::Null}),
        Err(error) => json!({"t": "reply", "id": id, "result": Value::Null, "error": error}),
    }
}

pub fn pointer(kind: &str, x: f64, y: f64, button: u32, mods: u32) -> Value {
    json!({"t": "pointer", "kind": kind, "x": x, "y": y, "button": button, "mods": mods})
}

pub fn scroll(x: f64, y: f64, dx: f64, dy: f64, mods: u32) -> Value {
    json!({"t": "scroll", "x": x, "y": y, "dx": dx, "dy": dy, "mods": mods})
}

pub fn key(press: bool, key: u32, text: &str, mods: u32) -> Value {
    json!({"t": "key", "kind": if press { "press" } else { "release" }, "key": key, "text": text, "mods": mods})
}

/// GDK button number → Qt::MouseButton bit.
pub fn qt_button(gdk_button: u32) -> u32 {
    match gdk_button {
        1 => 0x1,       // LeftButton
        2 => 0x4,       // MiddleButton
        3 => 0x2,       // RightButton
        8 => 0x8,       // BackButton
        9 => 0x10,      // ForwardButton
        _ => 0,
    }
}

// GdkModifierType bits.
const GDK_SHIFT: u32 = 1;
const GDK_CONTROL: u32 = 1 << 2;
const GDK_ALT: u32 = 1 << 3;
const GDK_SUPER: u32 = 1 << 26;
const GDK_META: u32 = 1 << 28;

/// GDK modifier bits → Qt::KeyboardModifiers bits.
pub fn qt_modifiers(gdk: u32) -> u32 {
    let mut qt = 0;
    if gdk & GDK_SHIFT != 0 {
        qt |= 0x0200_0000;
    }
    if gdk & GDK_CONTROL != 0 {
        qt |= 0x0400_0000;
    }
    if gdk & GDK_ALT != 0 {
        qt |= 0x0800_0000;
    }
    if gdk & (GDK_SUPER | GDK_META) != 0 {
        qt |= 0x1000_0000;
    }
    qt
}

pub const QT_KEY_UNKNOWN: u32 = 0x01ff_ffff;

/// GDK keyval → Qt::Key.
pub fn qt_key(keyval: u32) -> u32 {
    let upper = |cp: u32| char::from_u32(cp).map(|c| c.to_uppercase().next().unwrap_or(c) as u32).unwrap_or(QT_KEY_UNKNOWN);
    match keyval {
        0x20..=0x7e | 0xa0..=0xff => upper(keyval),
        // Unicode keyvals.
        0x0100_0000..=0x0110_ffff => upper(keyval - 0x0100_0000),
        0xff1b => 0x0100_0000, // Escape
        0xff09 => 0x0100_0001, // Tab
        0xfe20 => 0x0100_0002, // ISO_Left_Tab → Backtab
        0xff08 => 0x0100_0003, // BackSpace
        0xff0d => 0x0100_0004, // Return
        0xff8d => 0x0100_0005, // KP_Enter
        0xff63 => 0x0100_0006, // Insert
        0xffff => 0x0100_0007, // Delete
        0xff13 => 0x0100_0008, // Pause
        0xff61 => 0x0100_0009, // Print
        0xff50 => 0x0100_0010, // Home
        0xff57 => 0x0100_0011, // End
        0xff51 => 0x0100_0012, // Left
        0xff52 => 0x0100_0013, // Up
        0xff53 => 0x0100_0014, // Right
        0xff54 => 0x0100_0015, // Down
        0xff55 => 0x0100_0016, // Page_Up
        0xff56 => 0x0100_0017, // Page_Down
        0xffe1 | 0xffe2 => 0x0100_0020, // Shift
        0xffe3 | 0xffe4 => 0x0100_0021, // Control
        0xffeb | 0xffec | 0xffe7 | 0xffe8 => 0x0100_0022, // Super/Meta
        0xffe9 | 0xffea => 0x0100_0023, // Alt
        0xffe5 => 0x0100_0024, // Caps_Lock
        0xffbe..=0xffd5 => 0x0100_0030 + (keyval - 0xffbe), // F1..F24
        0xff67 => 0x0100_0055, // Menu
        // Keypad digits and operators type like the main keys.
        0xffb0..=0xffb9 => 0x30 + (keyval - 0xffb0),
        0xffaa => 0x2a,
        0xffab => 0x2b,
        0xffad => 0x2d,
        0xffae => 0x2e,
        0xffaf => 0x2f,
        _ => QT_KEY_UNKNOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_protocol_parses_host_messages() {
        assert_eq!(parse(br#"{"t":"ready","api":1}"#), FromHost::Ready);
        assert_eq!(parse(br#"{"t":"frame","seq":7}"#), FromHost::Frame { seq: 7 });
        assert_eq!(
            parse(br#"{"t":"buffer","width":128,"height":800,"stride":512,"scale":2}"#),
            FromHost::Buffer { width: 128, height: 800, stride: 512, scale: 2.0 }
        );
        assert!(matches!(parse(br#"{"t":"call","id":3,"method":"overlay.hide"}"#),
            FromHost::Call { id: 3, ref method, params: Value::Null } if method == "overlay.hide"));
        assert_eq!(parse(br#"{"t":"cursor","shape":13}"#), FromHost::Cursor { shape: 13 });
        assert_eq!(cursor_name(13), Some("pointer"));
        assert_eq!(cursor_name(0), None);
        assert_eq!(parse(br#"{"t":"something-new"}"#), FromHost::Unknown);
        assert_eq!(parse(b"not json"), FromHost::Unknown);
    }

    #[test]
    fn plugins_protocol_card_button_event() {
        let card = json!({"id": "sd_term_1", "kind": "terminal"});
        assert_eq!(card_button("left", &card), json!({"t": "event", "name": "cardButton", "button": "left", "card": card}));
    }

    #[test]
    fn plugins_protocol_maps_buttons_and_modifiers() {
        assert_eq!(qt_button(1), 1);
        assert_eq!(qt_button(3), 2);
        assert_eq!(qt_button(2), 4);
        assert_eq!(qt_modifiers(GDK_SHIFT | GDK_CONTROL), 0x0600_0000);
        assert_eq!(qt_modifiers(GDK_ALT), 0x0800_0000);
        assert_eq!(qt_modifiers(GDK_SUPER), 0x1000_0000);
        assert_eq!(qt_modifiers(2), 0); // Caps Lock is not a modifier in Qt
    }

    #[test]
    fn plugins_protocol_maps_keys() {
        assert_eq!(qt_key(u32::from(b'a')), u32::from(b'A'));
        assert_eq!(qt_key(u32::from(b'A')), u32::from(b'A'));
        assert_eq!(qt_key(u32::from(b' ')), 0x20);
        assert_eq!(qt_key(0xff0d), 0x0100_0004);
        assert_eq!(qt_key(0xff1b), 0x0100_0000);
        assert_eq!(qt_key(0xff51), 0x0100_0012);
        assert_eq!(qt_key(0xffbe), 0x0100_0030);
        assert_eq!(qt_key(0xffc9), 0x0100_003b); // F12
        assert_eq!(qt_key(0x0100_0444), 0x0424); // Cyrillic ф → Ф
        assert_eq!(qt_key(0xfd01), QT_KEY_UNKNOWN);
    }
}
