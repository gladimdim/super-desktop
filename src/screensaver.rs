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

/// Focus a screensaver window. Blocks on `hyprctl`; call it off the UI thread.
pub fn focus_screensaver() {
    // No escaped dots: a backslash is not a valid escape in the Lua string,
    // and an unescaped `.` still matches the class.
    let window = format!("class:^({SCREENSAVER_CLASS})$");
    let lua = format!("hl.dsp.focus({{ window = \"{window}\" }})");
    let done = Command::new("hyprctl")
        .args(["dispatch", &lua])
        .output()
        .is_ok_and(|output| output.status.success());
    // Hyprland before its Lua config takes the old dispatcher.
    if !done {
        let _ = Command::new("hyprctl").args(["dispatch", "focuswindow", &window]).output();
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
    fn screensaver_ends_when_the_event_stream_is_lost() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.reset(), None);
        tracker.feed("openwindow>>5a1f,1,org.omarchy.screensaver,Screensaver");
        assert_eq!(tracker.reset(), Some(Change::Ended));
        assert_eq!(tracker.feed("closewindow>>5a1f"), None);
    }
}
