//! Get out of the way of Omarchy's screensaver.
//!
//! The screensaver is an ordinary full-screen terminal window, one per
//! monitor, with the class `org.omarchy.screensaver`. The overlay is a
//! layer-shell surface on the Overlay layer, which Hyprland always draws above
//! normal windows, so it would cover the screensaver. The daemon follows
//! Hyprland's event socket and hides the overlay while any screensaver window
//! is open, then shows it again when the last one closes if it was on screen
//! before.
//!
//! The screensaver quits as soon as its window is not the active one. A
//! window that opens while the overlay holds the keyboard never gets focus,
//! and when the overlay lets go Hyprland focuses the window that had it
//! before, so after hiding the daemon focuses the screensaver itself.
//!
//! Hyprland also skips the screensaver's fullscreen rule in that case
//! (`noInitialFocus` while a layer surface holds the keyboard). The float
//! rule still matches, so the terminal stays at its default size, about
//! 700×500. The daemon sets fullscreen itself. `action = "set"` leaves a
//! window that is already fullscreen alone.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// The class every Omarchy screensaver window is opened with.
pub const SCREENSAVER_CLASS: &str = "org.omarchy.screensaver";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// The first screensaver window opened.
    Started,
    /// The last screensaver window closed.
    Ended,
}

/// The screensaver windows that are open, from Hyprland's event lines.
#[derive(Debug, Default)]
pub struct Tracker {
    windows: HashSet<String>,
}

impl Tracker {
    /// Feed one event line (`openwindow>>ADDR,WORKSPACE,CLASS,TITLE` or
    /// `closewindow>>ADDR`); returns a change when the screensaver as a whole
    /// starts or ends.
    pub fn feed(&mut self, line: &str) -> Option<Change> {
        let (event, data) = line.split_once(">>")?;
        match event {
            "openwindow" => {
                let mut fields = data.splitn(4, ',');
                let address = fields.next()?;
                let _workspace = fields.next()?;
                let class = fields.next()?;
                if class != SCREENSAVER_CLASS {
                    return None;
                }
                let first = self.windows.is_empty();
                self.windows.insert(address.to_string());
                first.then_some(Change::Started)
            }
            "closewindow" => {
                (self.windows.remove(data.trim()) && self.windows.is_empty()).then_some(Change::Ended)
            }
            _ => None,
        }
    }

    /// The event connection was lost: whatever was open may have closed
    /// unseen, so report the screensaver as over rather than keep the overlay
    /// hidden forever.
    pub fn reset(&mut self) -> Option<Change> {
        let was_on = !self.windows.is_empty();
        self.windows.clear();
        was_on.then_some(Change::Ended)
    }
}

/// Address of a screensaver window this `openwindow` line opened.
fn opened_address(line: &str) -> Option<&str> {
    let (event, data) = line.split_once(">>")?;
    if event != "openwindow" {
        return None;
    }
    let mut fields = data.splitn(4, ',');
    let address = fields.next()?;
    let _workspace = fields.next()?;
    let class = fields.next()?;
    (class == SCREENSAVER_CLASS).then_some(address)
}

fn class_selector() -> String {
    // No escaped dots: a backslash is not a valid escape in the Lua string,
    // and an unescaped `.` still matches the class.
    format!("class:^({SCREENSAVER_CLASS})$")
}

/// `address:0x…` for a Hyprland window address, with or without a `0x` prefix.
fn address_selector(address: &str) -> Option<String> {
    let bare = address.trim().trim_start_matches("0x");
    if bare.is_empty() || !bare.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("address:0x{bare}"))
}

/// Force fullscreen. `action = "set"` does not toggle it back off.
fn fullscreen_lua(window: &str) -> String {
    format!("hl.dsp.window.fullscreen({{ mode = \"fullscreen\", action = \"set\", window = \"{window}\" }})")
}

fn dispatch_lua(expression: &str) -> bool {
    Command::new("hyprctl")
        .args(["dispatch", expression])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn fullscreen_selector(window: &str) {
    if dispatch_lua(&fullscreen_lua(window)) {
        return;
    }
    // Hyprland before its Lua config: focus that window, then set both the
    // internal and the client mode to fullscreen (2). This is not a toggle.
    let _ = Command::new("hyprctl").args(["dispatch", "focuswindow", window]).output();
    let _ = Command::new("hyprctl").args(["dispatch", "fullscreenstate", "2", "2"]).output();
}

/// Fullscreen one screensaver window as soon as it opens. The screensaver
/// measures the terminal once, at startup. Blocks on `hyprctl`.
fn fullscreen_window(address: &str) {
    // The class selector covers the usual one-monitor case even when the
    // address is not selectable yet. The address covers every monitor.
    fullscreen_selector(&class_selector());
    if let Some(window) = address_selector(address) {
        fullscreen_selector(&window);
    }
}

/// Focus a screensaver window. Blocks on `hyprctl`; call it off the UI thread.
pub fn focus_screensaver() {
    let window = class_selector();
    let lua = format!("hl.dsp.focus({{ window = \"{window}\" }})");
    if !dispatch_lua(&lua) {
        let _ = Command::new("hyprctl").args(["dispatch", "focuswindow", &window]).output();
    }
}

/// Addresses of screensaver windows, from `hyprctl clients`.
fn screensaver_addresses() -> Vec<String> {
    let Ok(output) = Command::new("hyprctl").args(["clients", "-j"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    let Some(clients) = value.as_array() else {
        return Vec::new();
    };
    clients
        .iter()
        .filter_map(|client| {
            if client.get("class")?.as_str()? != SCREENSAVER_CLASS {
                return None;
            }
            Some(client.get("address")?.as_str()?.to_string())
        })
        .collect()
}

/// Focus the screensaver and force every one of its windows fullscreen.
///
/// Hiding the overlay hands focus back to the previous tiled window, and
/// with `on_focus_under_fullscreen = 1` that window takes fullscreen away.
/// Call again after the overlay has unmapped. Blocks on `hyprctl`.
pub fn settle_screensaver() {
    focus_screensaver();
    fullscreen_selector(&class_selector());
    for address in screensaver_addresses() {
        if let Some(window) = address_selector(&address) {
            fullscreen_selector(&window);
        }
    }
}

/// Hyprland's event socket, when this session runs under Hyprland.
fn event_socket() -> Option<PathBuf> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime).join("hypr").join(signature).join(".socket2.sock"))
}

/// Follow Hyprland's events for the life of the daemon, calling `notify` on
/// every screensaver start and end. Blocks; run it on its own thread. Returns
/// at once outside Hyprland.
pub fn watch(notify: impl Fn(Change)) {
    let Some(path) = event_socket() else {
        return;
    };
    let mut tracker = Tracker::default();
    loop {
        if let Ok(stream) = UnixStream::connect(&path) {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                // Before the UI thread hides the overlay, so the screensaver
                // measures a fullscreen terminal.
                if let Some(address) = opened_address(&line) {
                    fullscreen_window(address);
                }
                if let Some(change) = tracker.feed(&line) {
                    notify(change);
                }
            }
        }
        if let Some(change) = tracker.reset() {
            notify(change);
        }
        // Hyprland restarting, or not up yet: try again shortly.
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screensaver_on_every_monitor_is_one_start_and_one_end() {
        let mut tracker = Tracker::default();
        assert_eq!(
            tracker.feed("openwindow>>5a1f,1,org.omarchy.screensaver,Screensaver"),
            Some(Change::Started)
        );
        assert_eq!(tracker.feed("openwindow>>5b2e,2,org.omarchy.screensaver,Screensaver"), None);
        assert_eq!(tracker.feed("closewindow>>5a1f"), None);
        assert_eq!(tracker.feed("closewindow>>5b2e"), Some(Change::Ended));
    }

    #[test]
    fn screensaver_ignores_other_windows_and_events() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.feed("openwindow>>77,1,Alacritty,org.omarchy.screensaver"), None);
        assert_eq!(tracker.feed("closewindow>>77"), None);
        assert_eq!(tracker.feed("activewindow>>org.omarchy.screensaver,x"), None);
        assert_eq!(tracker.feed("garbage"), None);
        assert_eq!(tracker.feed("openwindow>>5a1f,1,org.omarchy.screensaver,Title, with, commas"), Some(Change::Started));
        assert_eq!(tracker.feed("closewindow>>77"), None);
    }

    #[test]
    fn screensaver_open_line_yields_its_address() {
        assert_eq!(opened_address("openwindow>>5a1f,1,org.omarchy.screensaver,Screensaver"), Some("5a1f"));
        assert_eq!(opened_address("openwindow>>5a1f,1,org.omarchy.screensaver,Title, with, commas"), Some("5a1f"));
        assert_eq!(opened_address("openwindow>>77,1,Alacritty,org.omarchy.screensaver"), None);
        assert_eq!(opened_address("closewindow>>5a1f"), None);
        assert_eq!(opened_address("garbage"), None);
    }

    #[test]
    fn screensaver_fullscreen_set_targets_one_window_and_does_not_toggle() {
        assert_eq!(address_selector("5a1f"), Some("address:0x5a1f".to_string()));
        assert_eq!(address_selector("0x5a1f"), Some("address:0x5a1f".to_string()));
        assert_eq!(address_selector("  0x5A1F "), Some("address:0x5A1F".to_string()));
        assert_eq!(address_selector(""), None);
        assert_eq!(address_selector("not hex"), None);
        assert_eq!(address_selector("5a1f;dispatch"), None);
        let lua = fullscreen_lua("address:0x5a1f");
        assert!(lua.contains("action = \"set\""), "{lua}");
        assert!(lua.contains("mode = \"fullscreen\""), "{lua}");
        assert!(lua.contains("window = \"address:0x5a1f\""), "{lua}");
        assert!(!lua.contains("toggle"), "{lua}");
    }

    #[test]
    fn screensaver_ends_when_the_event_stream_is_lost() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.reset(), None);
        tracker.feed("openwindow>>5a1f,1,org.omarchy.screensaver,Screensaver");
        assert_eq!(tracker.reset(), Some(Change::Ended));
        assert_eq!(tracker.feed("closewindow>>5a1f"), None);
    }
}
