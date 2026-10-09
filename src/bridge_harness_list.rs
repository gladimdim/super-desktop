//! The Android harness list (`GET /api/v1/harnesses` and its WebSocket
//! stream) from one shared collector.
//!
//! Before, every stream client rebuilt the whole document once a second and
//! every GET did the same work again: per session a capture, `list-panes`,
//! `show-options` and a second capture for the composer draft, plus two
//! `state.json` reads. Now one collector thread runs only while someone
//! consumes the list (a stream client, or a GET in the last few seconds),
//! produces at most one document per second from one `tmux list-panes -a` and
//! one capture per session whose pane changed since its last capture (see
//! `tmux::capture_pane_text_for`), and every consumer shares the serialized
//! result.
//! Stream clients are sent a document only when its content changed, plus a
//! heartbeat so Android's 45 s dead-socket timer never fires.
use super::*;
use crate::tmux::{PaneLookup, PaneSnapshot};
use std::sync::Arc;
use std::time::Instant;

type LauncherSessionMeta = (String, String, Option<String>, u8, Option<String>);

/// The Android snapshot and its live stream share one document contract.
/// One `state.json` read and one tmux inventory per document.
fn harness_document() -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "timestamp": utc_now_iso(),
        "harnesses": collect_harnesses(),
        "usage": crate::usage::launcher_usage(),
        "theme": theme_document(),
    })
}

/// Pick the directory the launcher should describe. Harness rows show the
/// workspace they were launched in; regular terminals track the pane's live
/// cwd so `cd` is reflected immediately.
fn launcher_directory(
    agent_type: &str,
    harness_home: &str,
    live_cwd: &str,
) -> (String, &'static str) {
    if crate::shell_title::is_regular(agent_type) && !live_cwd.trim().is_empty() {
        (live_cwd.trim().to_string(), "cwd")
    } else if crate::shell_title::is_regular(agent_type) {
        (harness_home.to_string(), "cwd")
    } else {
        (harness_home.to_string(), "home")
    }
}

/// Keep the directory as the final preview line because the current Android
/// launcher renders the final three non-empty lines of this field.
fn preview_with_directory(screen: &str, kind: &str, display_dir: &str) -> String {
    let lines: Vec<&str> = screen
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(11);
    let mut preview = lines[start..].join("\n");
    if !preview.is_empty() {
        preview.push('\n');
    }
    preview.push_str(kind);
    preview.push_str(" · ");
    preview.push_str(display_dir);
    preview
}

/// Read only recognized status-footer formats from the current pane bottom.
/// Never infer settings from global defaults or conversation history.
fn harness_model_effort(agent: &str, screen: &str) -> (Option<String>, Option<String>) {
    for line in screen.lines().rev().take(6).map(str::trim) {
        if let Some(rest) = line.strip_prefix("MODEL ") {
            if let Some((model, effort)) = rest.split_once("EFFORT ") {
                let model = model.trim();
                let effort = effort.split_whitespace().next().unwrap_or("");
                if !model.is_empty() && !effort.is_empty() {
                    return (Some(model.to_string()), Some(effort.to_string()));
                }
            }
        }
        if agent == "codex" {
            if let Some((settings, _)) = line.split_once(" · ") {
                let mut parts = settings.split_whitespace();
                if let (Some(model), Some(effort), None) = (parts.next(), parts.next(), parts.next()) {
                    if (model.starts_with("gpt-") || model.starts_with("o3") || model.starts_with("o4"))
                        && matches!(effort, "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra") {
                        return (Some(model.to_string()), Some(effort.to_string()));
                    }
                }
            }
        }
    }
    (None, None)
}

/// Minimum spacing of two collections.
pub(super) const PERIOD: Duration = Duration::from_secs(1);
/// A GET is answered from the shared document if it is younger than this.
pub(super) const FRESH: Duration = Duration::from_secs(1);
/// The collector keeps running this long after the last GET without streams,
/// so a phone polling every 2 s shares one collector instead of cold starts.
pub(super) const LINGER: Duration = Duration::from_secs(5);
/// Stream clients get an unchanged document at least this often.
pub(super) const HEARTBEAT: Duration = Duration::from_secs(20);
/// How long a GET waits for the collector before collecting on its own.
const GET_WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct HubState {
    document: Option<Arc<str>>,
    /// Digest of the document without its volatile timestamps.
    content: Option<String>,
    /// Bumped only when `content` changes.
    pub(super) generation: u64,
    /// Bumped by every collection.
    produced: u64,
    produced_at: Option<Instant>,
    streams: usize,
    last_get: Option<Instant>,
    running: bool,
}

impl HubState {
    /// Store one collection. Returns whether its content differs from the
    /// previous document (timestamps excluded).
    pub(super) fn publish(&mut self, document: Arc<str>, content: String, now: Instant) -> bool {
        self.produced = self.produced.wrapping_add(1);
        self.produced_at = Some(now);
        self.document = Some(document);
        let changed = self.content.as_deref() != Some(content.as_str());
        if changed {
            self.content = Some(content);
            self.generation = self.generation.wrapping_add(1);
        }
        changed
    }

    fn fresh(&self, now: Instant) -> Option<Arc<str>> {
        let at = self.produced_at?;
        (now.saturating_duration_since(at) < FRESH).then(|| self.document.clone()).flatten()
    }

    /// Whether the collector should keep running.
    pub(super) fn wanted(&self, now: Instant) -> bool {
        self.streams > 0 || self.last_get.is_some_and(|at| now.saturating_duration_since(at) < LINGER)
    }
}

/// Whether a stream client should be sent the current document.
///
/// `last_sent` is the generation it last received (`None` before the first
/// frame); the heartbeat resends an unchanged document.
pub(super) fn frame_due(last_sent: Option<u64>, current: u64, since_send: Duration) -> bool {
    last_sent != Some(current) || since_send >= HEARTBEAT
}

/// Digest of a harness document without `timestamp` and per-harness
/// `updatedAt`, which change on every collection.
pub(super) fn content_key(document: &serde_json::Value) -> String {
    let mut stable = document.clone();
    if let Some(object) = stable.as_object_mut() {
        object.remove("timestamp");
    }
    if let Some(items) = stable.get_mut("harnesses").and_then(|v| v.as_array_mut()) {
        for item in items {
            if let Some(object) = item.as_object_mut() {
                object.remove("updatedAt");
            }
        }
    }
    security::digest(stable.to_string().as_bytes())
}

struct Hub {
    state: Mutex<HubState>,
    changed: Condvar,
}

fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(|| Hub { state: Mutex::new(HubState::default()), changed: Condvar::new() })
}

fn ensure_running(state: &mut HubState) {
    if state.running {
        return;
    }
    state.running = true;
    if std::thread::Builder::new()
        .name("harness-list".into())
        .spawn(collector)
        .is_err()
    {
        state.running = false;
    }
}

fn collector() {
    // A panic in a collection must not leave waiters believing a collector
    // is still coming.
    struct Running;
    impl Drop for Running {
        fn drop(&mut self) {
            if let Ok(mut state) = hub().state.lock() {
                state.running = false;
            }
            hub().changed.notify_all();
        }
    }
    let _running = Running;
    let shared = hub();
    loop {
        let started = Instant::now();
        if !shared.state.lock().map(|s| s.wanted(started)).unwrap_or(false) {
            return;
        }
        let document = harness_document();
        let content = content_key(&document);
        let text: Arc<str> = Arc::from(document.to_string());
        if let Ok(mut state) = shared.state.lock() {
            state.publish(text, content, Instant::now());
        }
        shared.changed.notify_all();
        let next = started + PERIOD;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    }
}

/// The document for `GET /api/v1/harnesses`: the shared one if fresh, else
/// the collector's next one.
pub(super) fn current_document() -> Arc<str> {
    let hub = hub();
    let now = Instant::now();
    let Ok(mut state) = hub.state.lock() else {
        return Arc::from(harness_document().to_string());
    };
    state.last_get = Some(now);
    if let Some(document) = state.fresh(now) {
        return document;
    }
    ensure_running(&mut state);
    let seen = state.produced;
    let waited = hub
        .changed
        .wait_timeout_while(state, GET_WAIT, |s| s.produced == seen && s.running);
    if let Ok((state, _)) = waited {
        if state.produced != seen {
            if let Some(document) = state.document.clone() {
                return document;
            }
        }
    }
    Arc::from(harness_document().to_string())
}

/// Registered stream client; keeps the collector alive while it exists.
struct Subscription;
impl Subscription {
    fn new() -> Self {
        if let Ok(mut state) = hub().state.lock() {
            state.streams += 1;
            ensure_running(&mut state);
        }
        Subscription
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        if let Ok(mut state) = hub().state.lock() {
            state.streams = state.streams.saturating_sub(1);
        }
    }
}

/// Push the `/harnesses` document on connect, whenever its content changes
/// (checked at most once a second), and at least every `HEARTBEAT`.
///
/// Liveness comes from writes and from draining client frames at least once a
/// second: a vanished client fails a write (5 s timeout) or its read.
pub(super) fn stream(stream: &mut Connection) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let deadline = Instant::now() + Duration::from_secs(STREAM_MAX_SECS);
    let _subscription = Subscription::new();
    let hub = hub();
    let mut last_sent: Option<u64> = None;
    let mut sent_at = Instant::now();
    while Instant::now() < deadline {
        if !ws_client_alive(stream) {
            return;
        }
        let frame = {
            let Ok(state) = hub.state.lock() else { return };
            let waited = hub.changed.wait_timeout_while(state, Duration::from_secs(1), |s| {
                // The first frame waits for a fresh document; afterwards only
                // a new generation (or the heartbeat below) matters.
                let ready = match last_sent {
                    None => s.fresh(Instant::now()).is_some(),
                    Some(sent) => s.generation != sent,
                };
                !ready && s.running
            });
            let Ok((mut state, _)) = waited else { return };
            if !state.running {
                // The collector exits only without consumers; this client is
                // one, so this is a panic or spawn failure: try again.
                ensure_running(&mut state);
            }
            let ready = last_sent.is_some() || state.fresh(Instant::now()).is_some();
            match state.document.clone() {
                Some(document) if ready && frame_due(last_sent, state.generation, sent_at.elapsed()) => {
                    Some((state.generation, document))
                }
                _ => None,
            }
        };
        if let Some((generation, document)) = frame {
            if crate::ws::write_text(stream, &document).is_err() {
                return;
            }
            last_sent = Some(generation);
            sent_at = Instant::now();
        }
    }
    let _ = crate::ws::write_close(stream, 1000, "reconnect");
}

/// Sessions to describe, oldest card first. Uses the batched inventory when
/// tmux answered it, else `list-sessions`.
fn live_sessions(state: &crate::state::AppState, snapshot: Option<&PaneSnapshot>) -> Vec<String> {
    let mut sessions: Vec<String> = match snapshot {
        Some(snapshot) => snapshot.sessions().filter(|s| s.starts_with("sd_term_")).cloned().collect(),
        None => Command::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|name| name.starts_with("sd_term_"))
                    .collect()
            })
            .unwrap_or_default(),
    };
    // Names first (tmux lists sessions by name), then creation time from
    // state.json; unknown sessions go last.
    sessions.sort();
    sessions.dedup();
    let order: HashMap<&str, f64> =
        state.terminals.iter().map(|t| (t.session_name.as_str(), t.created_at)).collect();
    sessions.sort_by(|a, b| {
        order
            .get(a.as_str())
            .unwrap_or(&f64::MAX)
            .partial_cmp(order.get(b.as_str()).unwrap_or(&f64::MAX))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sessions
}

/// Every launcher-visible harness from one state read, one inventory and one
/// capture per session whose pane changed.
pub(super) fn collect(state: &crate::state::AppState, snapshot: Option<&PaneSnapshot>) -> Vec<serde_json::Value> {
    let meta: HashMap<String, LauncherSessionMeta> = state
        .terminals
        .iter()
        .map(|t| {
            (
                t.session_name.clone(),
                (t.agent_type.clone(), t.command.clone(), t.agent_session_id.clone(), t.tag, t.workspace_dir.clone()),
            )
        })
        .collect();
    if let Some(snapshot) = snapshot {
        crate::tmux::retain_captures(snapshot);
    }
    let mut out = vec![];
    for session in live_sessions(state, snapshot) {
        let (agent_type, cmd_fallback, persisted_sid, tag, workspace_dir) = meta
            .get(&session)
            .cloned()
            .unwrap_or_else(|| ("shell".to_string(), String::new(), None, 0, None));
        let cfg = get_agent_config(&agent_type);
        let custom = state.custom_harnesses.iter().find(|item| item.id == agent_type);
        let agent_name = custom.map_or(cfg.name, |item| item.name.as_str());
        let agent_icon = custom.map_or(cfg.icon, |item| item.icon.as_str());
        let lookup = snapshot.map(|s| s.lookup(&session));
        // One capture serves preview, footer settings, status and the draft,
        // and is reused while the pane's inventory row shows no change.
        let screen = match &lookup {
            Some(PaneLookup::Row(row)) => crate::tmux::capture_pane_text_for(&session, row),
            _ => capture_pane_text(&session),
        }
        .unwrap_or_default();
        let (footer_model, effort) = harness_model_effort(&agent_type, &screen);
        let (status, metadata, title, prompt) = match lookup {
            Some(PaneLookup::Row(row)) => {
                let metadata = crate::harness_metadata::inspect_option(&agent_type, &row.metadata_option);
                let status = crate::tmux::status_for_pane(
                    &session,
                    &agent_type,
                    row,
                    &|| metadata.clone(),
                    &mut || Some(screen.clone()),
                );
                let oc_id = (agent_type == "opencode")
                    .then(|| resolve_own_opencode_id(&session, persisted_sid.as_deref()))
                    .flatten();
                let prompt = crate::card_status::card_prompt(&agent_type, Some(row), metadata.as_ref(), oc_id.as_deref());
                let title = crate::card_status::card_title(&agent_type, metadata.as_ref(), oc_id.as_deref(), &status.pid);
                (status, metadata, title, prompt)
            }
            Some(PaneLookup::Missing) => {
                let status = crate::tmux::exited_status(&agent_type);
                let title = session_title(&session, &agent_type, &status.pid);
                let prompt = last_user_text(&session, &agent_type, persisted_sid.as_deref());
                (status, None, title, prompt)
            }
            // tmux did not answer, or this row was ambiguous: per-session queries.
            Some(PaneLookup::Unknown) | None => {
                let metadata = crate::harness_metadata::inspect(&session, &agent_type);
                let status = inspect_status_with_screen(&session, &agent_type, &screen);
                let title = session_title(&session, &agent_type, &status.pid);
                let prompt = last_user_text(&session, &agent_type, persisted_sid.as_deref());
                (status, metadata, title, prompt)
            }
        };
        let model = metadata.map(|value| value.model).filter(|model| !model.is_empty()).or(footer_model);
        let harness_home = resolve_workspace_dir(workspace_dir.as_deref());
        let (directory, directory_kind) = launcher_directory(&agent_type, &harness_home, &status.cwd);
        let directory_display = crate::state::display_dir(&directory);
        let cmd = if status.cmd.is_empty() {
            if cmd_fallback.is_empty() { agent_type.clone() } else { cmd_fallback.clone() }
        } else {
            status.cmd.clone()
        };
        out.push(serde_json::json!({
            "id": session,
            "agentType": agent_type,
            "agentName": agent_name,
            "model": model,
            "effort": effort,
            "icon": agent_icon,
            "status": status.status,
            "label": status.label,
            "pid": status.pid,
            "cmd": cmd,
            "sessionTitle": title,
            "lastPrompt": prompt,
            // Same capture as the preview (was a second capture-pane).
            "composerDraft": extract_composer_draft(&screen),
            "preview": preview_with_directory(&screen, directory_kind, &directory_display),
            "directory": &directory,
            "directoryDisplay": &directory_display,
            "directoryKind": directory_kind,
            "homeDirectory": (directory_kind == "home").then_some(directory.as_str()),
            "cwd": (directory_kind == "cwd").then_some(directory.as_str()),
            // Group colour: the same 8-swatch tag the desktop card shows, so a
            // phone row can be coloured identically (`tagColor` is null when
            // the card has no tag).
            "tag": tag,
            "tagColor": tag_color(tag),
            "updatedAt": utc_now_iso(),
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_come_from_live_footer_only() {
        assert_eq!(super::harness_model_effort("codex", "prompt\n  gpt-6-astra medium · ~/project · title"),
            (Some("gpt-6-astra".into()), Some("medium".into())));
        assert_eq!(super::harness_model_effort("reasonix", "MODEL deepseek-v4-flash   EFFORT auto\nstatus"),
            (Some("deepseek-v4-flash".into()), Some("auto".into())));
        assert_eq!(super::harness_model_effort("codex", &format!("gpt-6-astra high · old\n{}", "blank\n".repeat(8))), (None, None));
        assert_eq!(super::harness_model_effort("shell", "gpt-6-astra high · text"), (None, None));
    }

    #[test]
    fn test_launcher_directory_tracks_shell_cwd_and_harness_home() {
        assert_eq!(
            launcher_directory("shell", "/home/me", "/tmp/project"),
            ("/tmp/project".to_string(), "cwd")
        );
        assert_eq!(
            launcher_directory("codex", "/home/me/Github/app", "/tmp/other"),
            ("/home/me/Github/app".to_string(), "home")
        );
        assert_eq!(
            launcher_directory("terminal", "/home/me", ""),
            ("/home/me".to_string(), "cwd")
        );
    }

    #[test]
    fn test_launcher_preview_keeps_directory_visible_in_its_tail() {
        let screen = (0..20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let preview = preview_with_directory(&screen, "home", "~/Github/app");
        let lines: Vec<_> = preview.lines().collect();
        assert_eq!(lines.len(), 12);
        assert_eq!(lines.first(), Some(&"line 9"));
        assert_eq!(lines.last(), Some(&"home · ~/Github/app"));
        assert!(lines.iter().rev().take(3).any(|line| line.starts_with("home · ")));
    }

    fn document(stamp: &str, status: &str) -> serde_json::Value {
        serde_json::json!({
            "protocolVersion": 3,
            "timestamp": stamp,
            "harnesses": [{"id":"sd_term_a","status":status,"updatedAt":stamp}],
            "usage": [],
            "theme": {"name":"x"},
        })
    }

    #[test]
    fn content_changes_ignore_timestamps() {
        let first = content_key(&document("2026-09-24T10:00:00.000Z", "IDLE"));
        assert_eq!(first, content_key(&document("2026-09-24T10:00:01.000Z", "IDLE")));
        assert_ne!(first, content_key(&document("2026-09-24T10:00:01.000Z", "WORKING")));

        let mut state = HubState::default();
        let now = Instant::now();
        assert!(state.publish(Arc::from("a"), first.clone(), now));
        let generation = state.generation;
        // Same content, new timestamp: the stored text is refreshed for GET
        // and heartbeats, but stream clients are not woken for it.
        assert!(!state.publish(Arc::from("b"), first.clone(), now));
        assert_eq!(state.generation, generation);
        assert_eq!(state.fresh(now).as_deref(), Some("b"));
        assert!(state.publish(Arc::from("c"), "other".into(), now));
        assert_eq!(state.generation, generation + 1);
        assert!(state.fresh(now + FRESH).is_none(), "a GET after FRESH triggers a collection");
    }

    #[test]
    fn stream_frames_follow_changes_and_heartbeat() {
        // First frame always goes out.
        assert!(frame_due(None, 1, Duration::ZERO));
        // Unchanged content: nothing until the heartbeat.
        assert!(!frame_due(Some(1), 1, Duration::from_secs(1)));
        assert!(!frame_due(Some(1), 1, HEARTBEAT - Duration::from_millis(1)));
        assert!(frame_due(Some(1), 1, HEARTBEAT));
        // A new generation is sent immediately.
        assert!(frame_due(Some(1), 2, Duration::ZERO));
        // Android drops a socket after 45 s of silence.
        assert!(HEARTBEAT < Duration::from_secs(45));
    }

    /// A reused capture must give the phone exactly the document a fresh
    /// capture of the same pane gives.
    #[test]
    fn reused_captures_give_identical_documents() {
        let session = crate::tmux::unique_session_name();
        // A screen-status agent's footer and working line at the bottom of a
        // short pane, then silence.
        let script = "printf 'MODEL gpt-test EFFORT high\\n⠋ Responding… 3s\\n'; exec sleep 600";
        let _session = crate::test_isolation::TmuxSession::start(&session, &["-x", "80", "-y", "4"], &[script]);
        let mut state = crate::state::AppState::default();
        state.terminals.push(
            serde_json::from_value(serde_json::json!({
                "id": "card", "session_name": session, "agent_type": "grok", "command": "grok",
                "x": 0, "y": 0, "created_at": 1.0,
            }))
            .unwrap(),
        );
        // This session's row only, so other sessions on the server stay out.
        let snapshot = || {
            let format = crate::tmux::pane_snapshot_format();
            let target = format!("={session}:");
            let out = Command::new("tmux").args(["list-panes", "-t", &target, "-F", &format]).output().unwrap();
            crate::tmux::parse_pane_snapshot(&String::from_utf8_lossy(&out.stdout))
        };
        let document = || {
            let mut items = collect(&state, Some(&snapshot()));
            for item in &mut items {
                item.as_object_mut().unwrap().remove("updatedAt");
            }
            serde_json::to_string(&items).unwrap()
        };
        // Wait for the output, then for a later second so the next capture
        // can be reused.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = snapshot();
            let PaneLookup::Row(row) = snapshot.lookup(&session) else { panic!("session row missing") };
            let activity = row.stamp.as_ref().expect("stamped").activity;
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            let screen = crate::tmux::capture_pane_text(&session).unwrap_or_default();
            if screen.contains("Responding") && now > activity {
                break;
            }
            assert!(Instant::now() < deadline, "the pane never settled: {screen:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
        let fresh = document();
        let reused = document();
        assert_eq!(fresh, reused);
        let items: serde_json::Value = serde_json::from_str(&fresh).unwrap();
        assert_eq!(items[0]["status"], "WORKING", "{items}");
        assert_eq!((&items[0]["model"], &items[0]["effort"]), (&"gpt-test".into(), &"high".into()));
        assert!(items[0]["preview"].as_str().unwrap().contains("Responding… 3s"));
        // Forget every capture: a fresh one gives the same document again.
        crate::tmux::retain_captures(&PaneSnapshot::default());
        assert_eq!(document(), fresh);
    }

    #[test]
    fn collector_runs_only_with_consumers() {
        let now = Instant::now();
        let mut state = HubState::default();
        assert!(!state.wanted(now));
        state.last_get = Some(now);
        assert!(state.wanted(now + LINGER - Duration::from_millis(1)));
        assert!(!state.wanted(now + LINGER));
        state.streams = 1;
        assert!(state.wanted(now + LINGER * 10));
    }
}
