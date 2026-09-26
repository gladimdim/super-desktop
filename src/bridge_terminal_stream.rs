//! `WS /api/v1/harnesses/<id>/stream`: one terminal's screen for the phone.
//!
//! The stream's private tmux control client receives `%output` notifications;
//! they only mark the pane dirty (their payload is discarded). A dirty pane is
//! re-captured over that same connection, at most `MIN_FRAME` apart, with a
//! `SAFETY_POLL` capture in case a notification is missed. A capture woken by
//! phone input alone that finds the screen unchanged (the program has not
//! redrawn yet) does not start a new `MIN_FRAME` wait, so the redraw's own
//! notification is captured at once instead of up to a frame later. Status and grid
//! queries also use the control connection, and run only after output or
//! layout notifications (or at a slow cadence) instead of spawning
//! `list-panes`/`show-options`/`display-message` every half second.
use super::*;
use crate::desktop_protocol::TerminalSize;
use crate::tmux::PaneLookup;
use crate::tmux_control::{Activity, ActivityState};
use std::sync::{Arc, Weak};
use std::time::Instant;

/// Coalesce captures to about 30 per second while output streams.
pub(super) const MIN_FRAME: Duration = Duration::from_millis(33);
/// Capture at least this often even without notifications.
pub(super) const SAFETY_POLL: Duration = Duration::from_secs(1);
/// An unchanged frame is resent this often (unchanged since before).
const HEARTBEAT: Duration = Duration::from_secs(5);
/// Status after output at most this often (as before); without output or
/// layout notifications, at the slow cadence only.
const STATUS_AFTER_OUTPUT: Duration = Duration::from_millis(500);
const STATUS_IDLE: Duration = Duration::from_secs(2);
const GRID_IDLE: Duration = Duration::from_secs(10);

static WAKERS: Mutex<Vec<(String, Weak<Activity>)>> = Mutex::new(Vec::new());

/// Phone input reached `session`: its streams capture now. Streams of other
/// sessions are not woken (this used to be one global signal).
pub(super) fn wake(session: &str) {
    let wakers: Vec<Arc<Activity>> = WAKERS
        .lock()
        .map(|w| w.iter().filter(|(s, _)| s == session).filter_map(|(_, a)| a.upgrade()).collect())
        .unwrap_or_default();
    for activity in wakers {
        activity.poke();
    }
}

struct Registration(Weak<Activity>);
impl Registration {
    fn new(session: &str, activity: &Arc<Activity>) -> Self {
        let weak = Arc::downgrade(activity);
        if let Ok(mut wakers) = WAKERS.lock() {
            wakers.retain(|(_, a)| a.strong_count() > 0);
            wakers.push((session.to_string(), weak.clone()));
        }
        Registration(weak)
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut wakers) = WAKERS.lock() {
            wakers.retain(|(_, a)| !Weak::ptr_eq(a, &self.0));
        }
    }
}

/// `?ansiOnly=1` (or `true`): the client derives plain text itself.
pub(super) fn ansi_only(query: &str) -> bool {
    query.split('&').any(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        key == "ansiOnly" && matches!(value, "1" | "true")
    })
}

pub(super) struct Frame<'a> {
    pub id: &'a str,
    pub agent_type: &'a str,
    pub status: &'a SessionStatus,
    pub title: Option<&'a str>,
    pub tag: u8,
    pub ansi: Option<&'a str>,
    pub grid: Option<TerminalSize>,
    pub ansi_only: bool,
    pub updated_at: &'a str,
}

/// One serialized stream frame. Without `ansi_only` the frame is unchanged
/// from earlier bridges (`tail` plain + `tailAnsi`). With it, `tail` is
/// omitted whenever `tailAnsi` is present.
pub(super) fn frame_json(frame: &Frame) -> String {
    let mut document = serde_json::json!({
        "id": frame.id,
        "agentType": frame.agent_type,
        "status": frame.status.status,
        "sessionTitle": frame.title,
        "label": frame.status.label,
        "tag": frame.tag,
        "tagColor": tag_color(frame.tag),
        // `tailAnsi` is the exact tmux styling for clients that render ANSI SGR.
        "tailAnsi": frame.ansi,
        "tailFormat": frame.ansi.is_some().then_some("ansi-sgr"),
        // The pane's own grid, so a client can lay this text out at the width
        // it was rendered at instead of guessing from the lines.
        "columns": frame.grid.map(|grid| grid.columns),
        "rows": frame.grid.map(|grid| grid.rows),
        "updatedAt": frame.updated_at,
    });
    if !(frame.ansi_only && frame.ansi.is_some()) {
        // `tail` stays plain for existing launchers.
        document["tail"] = serde_json::json!(frame.ansi.map(strip_terminal_escapes));
    }
    document.to_string()
}

/// Content of the last frame sent: status, label, title, styled tail, grid.
type SentFrame = (&'static str, &'static str, Option<String>, Option<String>, Option<TerminalSize>);

/// Status, title and (on request) grid over the control connection.
fn query_status(
    control: &mut crate::tmux_control::Control,
    id: &str,
    agent: &str,
    screen: &mut dyn FnMut() -> Option<String>,
) -> (SessionStatus, Option<String>) {
    match control.pane_snapshot().map(|snapshot| {
        match snapshot.lookup(id) {
            PaneLookup::Row(row) => {
                let metadata = crate::harness_metadata::inspect_option(agent, &row.metadata_option);
                let status = crate::tmux::status_for_pane(id, agent, row, &|| metadata.clone(), screen);
                let oc_id = (agent == "opencode").then(|| resolve_own_opencode_id(id, None)).flatten();
                let title = crate::card_status::card_title(agent, metadata.as_ref(), oc_id.as_deref(), &status.pid);
                Some((status, title))
            }
            PaneLookup::Missing => Some((crate::tmux::exited_status(agent), None)),
            PaneLookup::Unknown => None,
        }
    }) {
        Ok(Some(result)) => result,
        // Ambiguous row or a failed control command: the per-session path.
        _ => {
            let text = screen().unwrap_or_default();
            let status = inspect_status_with_screen(id, agent, &text);
            let title = session_title(id, agent, &status.pid);
            (status, title)
        }
    }
}

/// Push changed terminal snapshots; the first frame doubles as the "attached"
/// signal, and the stream ends after an `EXITED` frame.
pub(super) fn stream(stream: &mut Connection, id: &str, ansi_only: bool) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let deadline = Instant::now() + Duration::from_secs(STREAM_MAX_SECS);
    let (agent_type, tag) = session_meta(id);
    let Ok((mut control, activity)) = crate::tmux_control::Control::open_watching(id) else {
        let _ = crate::ws::write_close(stream, 1011, "terminal unavailable");
        return;
    };
    let _registration = Registration::new(id, &activity);
    let mut seen = activity.snapshot();
    let mut status: Option<SessionStatus> = None;
    let mut title: Option<String> = None;
    let mut grid: Option<TerminalSize> = None;
    let mut status_at = Instant::now();
    let mut grid_at = Instant::now();
    let (mut output_dirty, mut layout_dirty) = (true, true);
    // Last sent content, compared before anything is serialized.
    let mut sent: Option<SentFrame> = None;
    let mut sent_at = Instant::now();
    // What `MIN_FRAME` is measured from, and whether this pass was woken by
    // phone input alone.
    let mut throttle_from = Instant::now();
    let mut input_probe = false;
    while Instant::now() < deadline {
        if !ws_client_alive(stream) {
            return;
        }
        let captured = control.capture().ok();
        let alive = captured.is_some();
        let now = Instant::now();
        if !alive {
            status = Some(SessionStatus {
                status: "EXITED",
                label: "○ EXITED",
                pid: String::new(),
                cmd: String::new(),
                cwd: String::new(),
            });
        } else if status.is_none()
            || (output_dirty && now.duration_since(status_at) >= STATUS_AFTER_OUTPUT)
            || now.duration_since(status_at) >= STATUS_IDLE
        {
            let ansi = captured.as_deref().unwrap_or("");
            let (fresh, fresh_title) =
                query_status(&mut control, id, &agent_type, &mut || Some(strip_terminal_escapes(ansi)));
            status = Some(fresh);
            title = fresh_title;
            status_at = now;
            output_dirty = false;
        }
        if alive && (layout_dirty || now.duration_since(grid_at) >= GRID_IDLE) {
            if let Ok(fresh) = control.pane_grid() {
                grid = fresh;
            }
            grid_at = now;
            layout_dirty = false;
        }
        let current_status = status.as_ref().expect("status is set above");
        let changed = match &sent {
            Some((s, l, t, a, g)) => {
                *s != current_status.status || *l != current_status.label || *t != title || *a != captured || *g != grid
            }
            None => true,
        };
        if changed || sent_at.elapsed() >= HEARTBEAT {
            if changed {
                sent = Some((current_status.status, current_status.label, title.clone(), captured, grid));
            }
            let ansi = sent.as_ref().and_then(|s| s.3.as_deref());
            let text = frame_json(&Frame {
                id,
                agent_type: &agent_type,
                status: current_status,
                title: title.as_deref(),
                tag,
                ansi,
                grid,
                ansi_only,
                updated_at: &utc_now_iso(),
            });
            if crate::ws::write_text(stream, &text).is_err() {
                return;
            }
            sent_at = Instant::now();
        }
        if !alive {
            let _ = crate::ws::write_close(stream, 1000, "session ended");
            return;
        }
        let captured_at = Instant::now();
        if changed || !input_probe {
            throttle_from = captured_at;
        }
        let next = wait_for_change(&activity, seen, captured_at, throttle_from, deadline);
        input_probe = next.input != seen.input && next.output == seen.output && next.layout == seen.layout;
        output_dirty |= next.output != seen.output || next.input != seen.input;
        layout_dirty |= next.layout != seen.layout;
        seen = next;
    }
    let _ = crate::ws::write_close(stream, 1000, "reconnect");
}

/// Sleep until the pane is dirty (then no sooner than `MIN_FRAME` after
/// `throttle_from`, the last capture that counted) or `SAFETY_POLL` passes
/// since the last capture. Returns the latest activity.
fn wait_for_change(activity: &Activity, seen: ActivityState, captured_at: Instant, throttle_from: Instant, deadline: Instant) -> ActivityState {
    let poll_at = (captured_at + SAFETY_POLL).min(deadline);
    loop {
        let now = Instant::now();
        let current = activity.snapshot();
        if current != seen {
            let earliest = throttle_from + MIN_FRAME;
            if now < earliest {
                std::thread::sleep(earliest - now);
            }
            return activity.snapshot();
        }
        if now >= poll_at {
            return current;
        }
        activity.wait_changed(seen, poll_at - now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn status() -> SessionStatus {
        SessionStatus { status: "IDLE", label: "● IDLE", pid: "1".into(), cmd: "sh".into(), cwd: String::new() }
    }

    fn frame(ansi: Option<&str>, ansi_only: bool) -> serde_json::Value {
        let status = status();
        serde_json::from_str(&frame_json(&Frame {
            id: "sd_term_x",
            agent_type: "shell",
            status: &status,
            title: None,
            tag: 5,
            ansi,
            grid: Some(TerminalSize { columns: 80, rows: 24 }),
            ansi_only,
            updated_at: "2026-09-24T00:00:00.000Z",
        }))
        .unwrap()
    }

    #[test]
    fn ansi_only_omits_the_plain_tail() {
        let styled = "\u{1b}[31mred\u{1b}[0m text";
        // Old clients: both fields, unchanged.
        let both = frame(Some(styled), false);
        assert_eq!(both["tail"], "red text");
        assert_eq!(both["tailAnsi"], styled);
        assert_eq!(both["tailFormat"], "ansi-sgr");
        assert_eq!((both["columns"].as_u64(), both["rows"].as_u64()), (Some(80), Some(24)));
        // `?ansiOnly=1`: no `tail` key at all.
        let only = frame(Some(styled), true);
        assert!(only.get("tail").is_none());
        assert_eq!(only["tailAnsi"], styled);
        // A gone session keeps the explicit nulls either way.
        for ansi_only in [false, true] {
            let gone = frame(None, ansi_only);
            assert!(gone["tail"].is_null() && gone.get("tail").is_some());
            assert!(gone["tailAnsi"].is_null());
        }
        for key in ["id", "agentType", "status", "sessionTitle", "label", "tag", "tagColor", "updatedAt"] {
            assert!(only.get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn ansi_only_query_parsing() {
        assert!(ansi_only("ansiOnly=1"));
        assert!(ansi_only("x=2&ansiOnly=true"));
        assert!(!ansi_only(""));
        assert!(!ansi_only("ansiOnly=0"));
        assert!(!ansi_only("ansiOnlyX=1"));
    }

    #[test]
    fn input_wakes_only_its_own_session() {
        let mine = Arc::new(Activity::default());
        let other = Arc::new(Activity::default());
        let _a = Registration::new("sd_term_wake_mine", &mine);
        let _b = Registration::new("sd_term_wake_other", &other);
        wake("sd_term_wake_mine");
        assert_eq!(mine.snapshot().input, 1);
        assert_eq!(other.snapshot().input, 0);
    }

    fn read_text_frame(client: &mut TcpStream) -> Option<serde_json::Value> {
        let mut header = [0u8; 2];
        client.read_exact(&mut header).ok()?;
        let mut size = u64::from(header[1] & 0x7f);
        if size == 126 {
            let mut extended = [0u8; 2];
            client.read_exact(&mut extended).ok()?;
            size = u64::from(u16::from_be_bytes(extended));
        } else if size == 127 {
            let mut extended = [0u8; 8];
            client.read_exact(&mut extended).ok()?;
            size = u64::from_be_bytes(extended);
        }
        let mut body = vec![0u8; size as usize];
        client.read_exact(&mut body).ok()?;
        match header[0] & 0x0f {
            0x1 => serde_json::from_slice(&body).ok(),
            _ => None, // close
        }
    }

    /// Output reaches the phone through `%output` notifications, well before
    /// the one-second safety poll, and `ansiOnly` frames carry no plain tail.
    #[test]
    fn stream_pushes_output_promptly_and_ends_with_the_session() {
        let id = format!("sd_term_streamtest_{}", std::process::id());
        struct Session(String);
        impl Drop for Session {
            fn drop(&mut self) {
                let _ = Command::new("tmux").args(["kill-session", "-t", &self.0]).output();
            }
        }
        let made = Command::new("tmux").args(["new-session", "-d", "-s", &id, "cat"]).output().unwrap();
        assert!(made.status.success());
        let session = Session(id.clone());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let server_id = id.clone();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            stream(&mut Connection::plain(socket), &server_id, true);
        });
        let first = read_text_frame(&mut client).expect("attached frame");
        assert_eq!(first["id"], id.as_str());
        assert!(first.get("tail").is_none());
        assert!(first["tailAnsi"].is_string());
        // Let the idle safety poll settle, then type.
        std::thread::sleep(Duration::from_millis(100));
        let typed = Instant::now();
        let sent = Command::new("tmux").args(["send-keys", "-t", &id, "-l", "prompt-output-marker"]).output().unwrap();
        assert!(sent.status.success());
        let latency = loop {
            let frame = read_text_frame(&mut client).expect("frame after output");
            if frame["tailAnsi"].as_str().is_some_and(|t| t.contains("prompt-output-marker")) {
                break typed.elapsed();
            }
        };
        assert!(latency < Duration::from_millis(700), "output took {latency:?}");
        drop(session);
        let last = loop {
            match read_text_frame(&mut client) {
                Some(frame) if frame["status"] == "EXITED" => break frame,
                Some(_) => continue,
                None => panic!("stream closed without an EXITED frame"),
            }
        };
        assert!(last["tailAnsi"].is_null() && last["tail"].is_null());
        server.join().unwrap();
    }

    #[test]
    fn dirty_panes_are_coalesced_and_idle_panes_polled() {
        let activity = Activity::default();
        let seen = activity.snapshot();
        activity.poke();
        let captured = Instant::now();
        let far = captured + Duration::from_secs(60);
        wait_for_change(&activity, seen, captured, captured, far);
        assert!(captured.elapsed() >= MIN_FRAME, "captures are at most ~30 per second");
        let quiet = activity.snapshot();
        let started = Instant::now();
        wait_for_change(&activity, quiet, started, started, started + Duration::from_millis(50));
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(started.elapsed() < SAFETY_POLL);
    }

    /// An unchanged capture woken by phone input keeps the earlier throttle
    /// base, so the redraw that follows is captured at once, not a frame later.
    #[test]
    fn input_probe_does_not_delay_the_redraw() {
        let activity = Activity::default();
        let counted = Instant::now() - MIN_FRAME;
        let probe = Instant::now();
        let seen = activity.snapshot();
        activity.poke();
        let far = probe + Duration::from_secs(60);
        wait_for_change(&activity, seen, probe, counted, far);
        assert!(probe.elapsed() < MIN_FRAME / 2, "waited {:?}", probe.elapsed());
    }
}
