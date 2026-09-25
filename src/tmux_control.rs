//! A private control client for a phone terminal. It never resizes the desktop
//! pane, and closes only its own client when the phone disconnects.
//!
//! A *watching* client (`open_watching`) also receives tmux's `%output`
//! notifications. Their payload is never kept: the reader thread only bumps an
//! [`Activity`] counter so the phone stream knows the pane changed and should
//! capture again, instead of re-capturing on a fixed timer.
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub struct Control {
    child: Child,
    input: ChildStdin,
    lines: Receiver<String>,
    pane: String,
    session: String,
    healthy: bool,
}

/// Change counters raised by tmux notifications (and by phone input).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityState {
    /// `%output` / `%extended-output` for a pane of the session.
    pub output: u64,
    /// Window/pane layout or selection changed (the pane grid may differ).
    pub layout: u64,
    /// Input was sent to this session by the bridge.
    pub input: u64,
    /// The control client ended (`%exit`, EOF).
    pub closed: bool,
}

/// Shared between a control client's reader thread and the stream waiting on it.
#[derive(Default)]
pub struct Activity {
    state: Mutex<ActivityState>,
    changed: Condvar,
}

impl Activity {
    fn bump(&self, change: impl FnOnce(&mut ActivityState)) {
        if let Ok(mut state) = self.state.lock() {
            change(&mut state);
            self.changed.notify_all();
        }
    }
    /// Phone input for this session: capture promptly.
    pub fn poke(&self) {
        self.bump(|s| s.input = s.input.wrapping_add(1));
    }
    pub fn snapshot(&self) -> ActivityState {
        self.state.lock().map(|s| *s).unwrap_or(ActivityState { closed: true, ..Default::default() })
    }
    /// Wait until anything differs from `seen`, or `timeout` passes.
    pub fn wait_changed(&self, seen: ActivityState, timeout: Duration) -> ActivityState {
        let Ok(state) = self.state.lock() else {
            return ActivityState { closed: true, ..seen };
        };
        match self.changed.wait_timeout_while(state, timeout, |s| *s == seen) {
            Ok((state, _)) => *state,
            Err(_) => ActivityState { closed: true, ..seen },
        }
    }
}

/// What one control-mode line outside a command block means to the reader.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    Output,
    Layout,
    Exit,
    Other,
}

pub(crate) fn classify(line: &str) -> Notice {
    let name = line.split(' ').next().unwrap_or("");
    match name {
        "%output" | "%extended-output" => Notice::Output,
        "%layout-change" | "%window-pane-changed" | "%session-window-changed"
        | "%session-changed" | "%window-add" | "%window-close" | "%unlinked-window-add"
        | "%unlinked-window-close" | "%client-session-changed" => Notice::Layout,
        "%exit" => Notice::Exit,
        _ => Notice::Other,
    }
}

/// Forward command blocks (and `%exit`) to the response channel; turn every
/// other notification into an activity bump. Lines are read as bytes: pane
/// output is not guaranteed to be valid UTF-8 at chunk boundaries.
fn read_lines(output: impl std::io::Read, sender: SyncSender<String>, activity: Option<Arc<Activity>>) {
    let mut reader = BufReader::new(output);
    let mut block: Option<String> = None;
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if raw.last() == Some(&b'\n') {
            raw.pop();
        }
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
        let line = String::from_utf8_lossy(&raw).into_owned();
        if let Some(tag) = block.as_deref() {
            if line.strip_prefix("%end ").or_else(|| line.strip_prefix("%error ")) == Some(tag) {
                block = None;
            }
        } else if let Some(tag) = line.strip_prefix("%begin ") {
            block = Some(tag.to_string());
        } else {
            match classify(&line) {
                Notice::Output => {
                    if let Some(activity) = &activity {
                        activity.bump(|s| s.output = s.output.wrapping_add(1));
                    }
                    continue;
                }
                Notice::Layout => {
                    if let Some(activity) = &activity {
                        activity.bump(|s| s.layout = s.layout.wrapping_add(1));
                    }
                    continue;
                }
                Notice::Exit => {}
                Notice::Other => continue,
            }
        }
        if sender.send(line).is_err() {
            break;
        }
    }
    if let Some(activity) = &activity {
        activity.bump(|s| s.closed = true);
    }
}

impl Control {
    /// Command-only client (no pane output), e.g. for phone input.
    pub fn open(session: &str) -> Result<Self, String> {
        Self::spawn(session, None)
    }

    /// Client that also reports pane output/layout changes through `Activity`.
    pub fn open_watching(session: &str) -> Result<(Self, Arc<Activity>), String> {
        let activity = Arc::new(Activity::default());
        let control = Self::spawn(session, Some(activity.clone()))?;
        Ok((control, activity))
    }

    fn spawn(session: &str, activity: Option<Arc<Activity>>) -> Result<Self, String> {
        let flags = if activity.is_some() { "ignore-size" } else { "ignore-size,no-output" };
        let mut child = Command::new("tmux")
            .args([
                "-C",
                "attach-session",
                "-f",
                flags,
                "-t",
                &format!("={session}"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::sync_channel(512);
        std::thread::spawn(move || read_lines(output, sender, activity));
        let mut client = Self {
            child,
            input,
            lines,
            pane: String::new(),
            session: session.to_string(),
            healthy: true,
        };
        client.response()?; // attach-session acknowledgement
        let pane = client.command("display-message -p '#{pane_id}'")?;
        let pane = pane.trim();
        if !pane.starts_with('%') || !pane[1..].chars().all(|c| c.is_ascii_digit()) {
            return Err("Invalid tmux pane".into());
        }
        client.pane = pane.to_string();
        Ok(client)
    }

    fn response(&mut self) -> Result<String, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut block: Option<String> = None;
        let mut output = String::new();
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| "tmux control connection lost or timed out".to_string())?;
            if let Some(tag) = block.as_ref() {
                if line == format!("%end {tag}") {
                    return Ok(output);
                }
                if line == format!("%error {tag}") {
                    return Err(output);
                }
                if output.len() + line.len() > 1 << 20 {
                    return Err("tmux response too large".into());
                }
                output.push_str(&line);
                output.push('\n');
            } else if let Some(tag) = line.strip_prefix("%begin ") {
                block = Some(tag.to_string());
            } else if line.starts_with("%exit") {
                return Err("Terminal session ended".into());
            }
        }
    }

    /// False once a command failed. The reply stream may then be out of step
    /// with tmux (a late answer would be read as the next command's), so the
    /// caller must open a new client instead of reusing this one.
    pub fn is_healthy(&self) -> bool {
        self.healthy
    }

    fn command(&mut self, command: &str) -> Result<String, String> {
        if !self.healthy {
            return Err("tmux connection must be reopened".into());
        }
        self.healthy = false;
        writeln!(self.input, "{command}").map_err(|e| e.to_string())?;
        self.input.flush().map_err(|e| e.to_string())?;
        let result = self.response();
        self.healthy = result.is_ok();
        result
    }

    pub fn send(&mut self, text: &str, enter: bool) -> Result<(), String> {
        if !text.is_empty() {
            // Hex bytes keep all user input out of the tmux command language.
            let bytes = text
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            self.command(&format!("send-keys -t {} -H {bytes}", self.pane))?;
        }
        if enter {
            self.command(&format!("send-keys -t {} Enter", self.pane))?;
        }
        Ok(())
    }

    pub fn capture(&mut self) -> Result<String, String> {
        self.command(&format!("capture-pane -p -e -S -300 -t {}", self.pane))
    }

    /// Session names this client can address safely inside a quoted target.
    fn safe_session(&self) -> Option<&str> {
        let session = self.session.as_str();
        (!session.is_empty()
            && session.len() <= 128
            && session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        .then_some(session)
    }

    /// The same inventory row `tmux::pane_snapshot` reads for this session,
    /// over this connection instead of a new `tmux` process.
    pub fn pane_snapshot(&mut self) -> Result<crate::tmux::PaneSnapshot, String> {
        let session = self.safe_session().ok_or("Invalid session")?.to_string();
        let format = crate::tmux::pane_snapshot_format();
        let text = self.command(&format!("list-panes -t '={session}:' -F '{format}'"))?;
        Ok(crate::tmux::parse_pane_snapshot(&text))
    }

    /// `tmux::pane_grid` over this connection.
    pub fn pane_grid(&mut self) -> Result<Option<crate::desktop_protocol::TerminalSize>, String> {
        let session = self.safe_session().ok_or("Invalid session")?.to_string();
        let text = self.command(&format!(
            "display-message -p -t '={session}:' '#{{pane_width}} #{{pane_height}}'"
        ))?;
        Ok(crate::tmux::parse_pane_grid(&text))
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_forwards_blocks_and_counts_notifications() {
        // A captured line that looks like a notification stays part of the
        // command's answer; outside blocks, notifications only bump counters.
        let text = b"%begin 1 2 0\n%end 1 2 0\n%output %1 hi\\015\n%begin 3 4 1\n%output %1 not a notice\nplain\n%end 3 4 1\n%layout-change @1 x\n%extended-output %1 5 : \xff\xfe\n%session-renamed x\n%exit\n";
        let (sender, lines) = mpsc::sync_channel(16);
        let activity = Arc::new(Activity::default());
        read_lines(&text[..], sender, Some(activity.clone()));
        let forwarded: Vec<String> = lines.try_iter().collect();
        assert_eq!(forwarded, [
            "%begin 1 2 0", "%end 1 2 0", "%begin 3 4 1", "%output %1 not a notice",
            "plain", "%end 3 4 1", "%exit",
        ]);
        let state = activity.snapshot();
        assert_eq!((state.output, state.layout, state.closed), (2, 1, true));
    }

    #[test]
    fn activity_wait_returns_on_change_or_timeout() {
        let activity = Arc::new(Activity::default());
        let seen = activity.snapshot();
        let started = Instant::now();
        assert_eq!(activity.wait_changed(seen, Duration::from_millis(20)), seen);
        assert!(started.elapsed() >= Duration::from_millis(20));
        let poker = activity.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            poker.poke();
        });
        let next = activity.wait_changed(seen, Duration::from_secs(5));
        assert_eq!(next.input, 1);
        worker.join().unwrap();
    }

    #[test]
    fn watching_client_sees_output_and_queries_status_without_processes() {
        let session = format!("sd_term_ctltest_{}", std::process::id());
        struct Session(String);
        impl Drop for Session {
            fn drop(&mut self) {
                let _ = Command::new("tmux").args(["kill-session", "-t", &self.0]).output();
            }
        }
        let made = Command::new("tmux")
            .args(["new-session", "-d", "-x", "91", "-y", "27", "-s", &session, "cat"])
            .output()
            .unwrap();
        assert!(made.status.success());
        let _guard = Session(session.clone());
        let (mut control, activity) = Control::open_watching(&session).unwrap();
        let seen = activity.snapshot();
        control.send("watch me", true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut state = seen;
        while state.output == seen.output && Instant::now() < deadline {
            state = activity.wait_changed(state, Duration::from_millis(200));
        }
        assert!(state.output > seen.output, "no %output notification arrived");
        // Commands still work while notifications stream in.
        assert!(control.capture().unwrap().contains("watch me"));
        let snapshot = control.pane_snapshot().unwrap();
        let crate::tmux::PaneLookup::Row(row) = snapshot.lookup(&session) else {
            panic!("session row missing")
        };
        assert!(!row.dead && !row.pid.is_empty());
        // Same row as the process-based inventory.
        let listed = crate::tmux::pane_snapshot().unwrap();
        let crate::tmux::PaneLookup::Row(expected_row) = listed.lookup(&session) else {
            panic!("inventory row missing")
        };
        assert_eq!((&row.pid, row.height, &row.cwd), (&expected_row.pid, expected_row.height, &expected_row.cwd));
        let pane = |row: &crate::tmux::PaneRow| row.stamp.as_ref().map(|s| (s.pane_id.clone(), s.width));
        assert!(pane(row).is_some(), "the control client's row is stamped too");
        assert_eq!(pane(row), pane(expected_row));
        let expected = crate::tmux::pane_grid(&session);
        assert!(expected.is_some());
        assert_eq!(control.pane_grid().unwrap(), expected);
    }
}
