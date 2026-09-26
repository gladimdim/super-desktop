//! Batched status/title/preview refresh for local terminal cards.
//!
//! The overlay refreshes every card once per second. Asking tmux per card
//! (list-panes, show-options, display-message ×2, capture-pane) cost 4–6
//! processes per card per second; here one `tmux list-panes -a` describes every
//! session, and a card only captures its pane when the capture is used (a
//! visible preview, or a screen-based status for an agent without native
//! metadata), and only when the inventory shows the pane changed since its
//! last capture. Runs on a worker thread, never on the GTK main thread.
use crate::harness_metadata::Metadata;
use crate::tmux::{PaneLookup, PaneRow, PaneSnapshot, SessionStatus};

/// What one card needs refreshed.
pub struct CardRequest {
    pub session: String,
    pub agent: String,
    /// `Some(lines)` when the card shows a text preview (no live terminal).
    pub preview_lines: Option<usize>,
    /// The card's known opencode session, if any.
    pub oc_id: Option<String>,
    /// Re-resolve the opencode mapping this time (at most every 30 s).
    pub need_resolve: bool,
}

pub struct CardUpdate {
    pub status: SessionStatus,
    pub preview: Option<String>,
    /// Session title, else the last submitted prompt.
    pub prompt: Option<String>,
    pub oc_id: Option<String>,
    /// A setup hint for this card (see `setup_notice`); the card shows it once
    /// per session (`claim_setup_notice`), not on every refresh.
    pub notice: Option<crate::command_feedback::Notice>,
    /// The pane is scrolled back in tmux's copy mode (`PaneRow::in_mode`), so
    /// the card offers to jump back to the newest output. `None`: unknown.
    pub scrolled_back: Option<bool>,
}

/// An OpenClaw card whose gateway does not load the SUPER DESKTOP plugin never
/// reports status or a native title (the card falls back to the typed prompt
/// and the screen). Point at the one place that fixes it.
pub const OPENCLAW_SETUP_NOTICE: crate::command_feedback::Notice = crate::command_feedback::Notice {
    text: "OpenClaw status needs setup · Settings → Harness launchers",
    tone: crate::command_feedback::Tone::Warning,
    duration: std::time::Duration::from_secs(8),
};

/// The setup hint `metadata` calls for: an OpenClaw launch (built-in or a
/// custom launcher running `openclaw`) whose adapter has not reported, while
/// OpenClaw's config does not load our gateway plugin. An unreadable config
/// says nothing either way, so it gets no hint. `plugin` is only asked for an
/// OpenClaw card; the check is a cached stat of the config file.
pub(crate) fn setup_notice(
    metadata: Option<&Metadata>,
    plugin: impl FnOnce() -> crate::harness_metadata::OpenClawPlugin,
) -> Option<crate::command_feedback::Notice> {
    let metadata = metadata?;
    (metadata.agent == "openclaw"
        && !metadata.adapter_reported()
        && matches!(plugin(), crate::harness_metadata::OpenClawPlugin::Missing(_)))
    .then_some(OPENCLAW_SETUP_NOTICE)
}

thread_local! {
    /// Sessions whose card already showed its setup hint in this process.
    static SETUP_NOTICE_SHOWN: std::cell::RefCell<std::collections::HashSet<String>> =
        Default::default();
}

/// True the first time `session` asks, so a card shows its setup hint once
/// rather than on every one-second refresh. Main thread (the card applier).
pub fn claim_setup_notice(session: &str) -> bool {
    SETUP_NOTICE_SHOWN.with(|shown| {
        let mut shown = shown.borrow_mut();
        if shown.len() >= 1024 {
            shown.clear();
        }
        shown.insert(session.to_owned())
    })
}

/// Refresh every requested card from one tmux inventory.
pub fn refresh(requests: Vec<CardRequest>) -> Vec<CardUpdate> {
    if requests.is_empty() {
        return Vec::new();
    }
    let snapshot = crate::tmux::pane_snapshot();
    if let Some(snapshot) = &snapshot {
        crate::tmux::retain_captures(snapshot);
    }
    requests
        .iter()
        .map(|request| refresh_one(request, snapshot.as_ref()))
        .collect()
}

fn refresh_one(request: &CardRequest, snapshot: Option<&PaneSnapshot>) -> CardUpdate {
    match snapshot.map(|snapshot| snapshot.lookup(&request.session)) {
        Some(PaneLookup::Row(row)) => from_row(request, Some(row)),
        Some(PaneLookup::Missing) => from_row(request, None),
        // tmux did not answer, or this row was ambiguous: per-session queries.
        Some(PaneLookup::Unknown) | None => legacy(request),
    }
}

fn has_native_metadata(agent: &str) -> bool {
    crate::harness_metadata::native_agent(agent) || agent.starts_with("custom-")
}

/// `row` is `None` when tmux no longer lists the session.
fn from_row(request: &CardRequest, row: Option<&PaneRow>) -> CardUpdate {
    let (session, agent) = (request.session.as_str(), request.agent.as_str());
    let metadata = row
        .filter(|_| has_native_metadata(agent))
        .and_then(|row| crate::harness_metadata::inspect_option(agent, &row.metadata_option));
    // The preview capture doubles as the status screen; otherwise the status
    // captures only the visible rows, and only if it gets that far. Either is
    // reused while the pane has not changed since the last refresh.
    let screen = match (row, request.preview_lines) {
        (Some(row), Some(_)) => crate::tmux::capture_pane_text_for(session, row),
        _ => None,
    };
    let status = match row {
        Some(row) => crate::tmux::status_for_pane(
            session,
            agent,
            row,
            &|| metadata.clone(),
            &mut || {
                if request.preview_lines.is_some() {
                    screen.clone()
                } else {
                    crate::tmux::capture_visible_screen_for(session, row)
                }
            },
        ),
        None => crate::tmux::exited_status(agent),
    };
    let preview = request.preview_lines.map(|lines| preview_text(screen.as_deref(), &status, lines));
    let oc_id = resolve_oc_id(request);
    let prompt = card_prompt(agent, row, metadata.as_ref(), oc_id.as_deref());
    let prompt = card_title(agent, metadata.as_ref(), oc_id.as_deref(), &status.pid).or(prompt);
    let notice = setup_notice(metadata.as_ref(), crate::harness_metadata::openclaw_plugin);
    let scrolled_back = Some(row.is_some_and(|row| row.in_mode));
    CardUpdate { status, preview, prompt, oc_id, notice, scrolled_back }
}

fn preview_text(screen: Option<&str>, status: &SessionStatus, lines: usize) -> String {
    match screen {
        Some(screen) => crate::tmux::preview_from_screen(screen, lines),
        None if status.status == "EXITED" => "Session offline or ended.".into(),
        None => "Ready. Waiting for input...".into(),
    }
}

/// `resolve_own_opencode_id` only ever returns THIS pane's own session (own
/// `--session` flag, else a claims-aware match), so its text is the prompt
/// typed INTO this harness — never another terminal's input.
fn resolve_oc_id(request: &CardRequest) -> Option<String> {
    let mut oc_id = request.oc_id.clone();
    if request.need_resolve {
        let fresh = crate::tmux::resolve_own_opencode_id(&request.session, oc_id.as_deref());
        if fresh != oc_id {
            oc_id = fresh;
        }
    }
    oc_id
}

/// The prompt of a card whose native adapter reported, else the prompt typed
/// into its tmux session (`prompt_history`). `Some(answer)` is final; `None`
/// leaves the caller's agent-specific fallbacks (Codex rollout, OpenCode DB).
///
/// Native metadata is authoritative once its adapter has reported, even with
/// no prompt yet. A silent adapter (an OpenClaw gateway without our plugin,
/// hooks that never ran) falls back to the typed prompt like every other
/// launcher. Either way it must be something a person submitted
/// (`is_user_prompt`, see "Card titles" in AGENTS.md). Shared by the card
/// refresh and `bridge::last_user_text` (the phone's `lastPrompt`).
pub(crate) fn reported_or_typed_prompt(
    metadata: Option<&Metadata>,
    typed: impl FnOnce() -> Option<String>,
) -> Option<Option<String>> {
    if let Some(metadata) = metadata.filter(|m| m.adapter_reported()) {
        return Some(
            (!metadata.prompt.is_empty())
                .then(|| crate::tmux::truncate_prompt_title(&metadata.prompt)),
        );
    }
    typed().filter(|prompt| crate::harness_record::is_user_prompt(prompt)).map(Some)
}

/// Same sources and order as `bridge::last_user_text`, from the batched row.
pub(crate) fn card_prompt(
    agent: &str,
    row: Option<&PaneRow>,
    metadata: Option<&Metadata>,
    oc_id: Option<&str>,
) -> Option<String> {
    let recorded = || {
        row.map(|row| row.last_prompt.trim())
            .filter(|prompt| !prompt.is_empty())
            .map(str::to_owned)
    };
    if crate::shell_title::is_regular(agent) {
        let row = row?;
        return crate::shell_title::last_from(
            row.pid.parse().ok()?,
            row.shell_tracking,
            &row.shell_command,
            recorded,
        );
    }
    if let Some(prompt) = reported_or_typed_prompt(metadata, recorded) {
        return prompt;
    }
    if agent == "codex" {
        return crate::completion::last_user_prompt_for_pid(row?.pid.parse().ok()?);
    }
    if agent == "opencode" {
        return crate::tmux::get_opencode_user_text_by_id(oc_id?);
    }
    None
}

/// Same sources as `bridge::session_title`, from already known data.
pub(crate) fn card_title(
    agent: &str,
    metadata: Option<&Metadata>,
    oc_id: Option<&str>,
    pid: &str,
) -> Option<String> {
    // An auto-generated placeholder (OpenCode's "New session - <timestamp>")
    // is no title: the caller falls back to the submitted prompt.
    if let Some(title) = metadata
        .filter(|m| !crate::harness_record::is_placeholder_title(&m.agent, &m.title))
        .map(|m| m.title.clone())
        .filter(|t| !t.is_empty())
    {
        return Some(title);
    }
    match agent {
        "opencode" => crate::tmux::get_opencode_title_by_id(oc_id?),
        "codex" => crate::completion::session_title(pid.parse().ok()?),
        _ => None,
    }
}

/// The per-session path, used when the batched inventory cannot describe a
/// card.
fn legacy(request: &CardRequest) -> CardUpdate {
    let (session, agent) = (request.session.as_str(), request.agent.as_str());
    let screen = crate::tmux::capture_pane_text(session);
    let status =
        crate::tmux::inspect_status_with_screen(session, agent, screen.as_deref().unwrap_or(""));
    let preview = request.preview_lines.map(|lines| preview_text(screen.as_deref(), &status, lines));
    let oc_id = resolve_oc_id(request);
    let prompt = crate::bridge::last_user_text(
        session,
        agent,
        oc_id.as_deref(),
        screen.as_deref().unwrap_or(""),
    );
    let prompt = crate::bridge::session_title(session, agent, &status.pid).or(prompt);
    let notice = setup_notice(
        crate::harness_metadata::inspect(session, agent).as_ref(),
        crate::harness_metadata::openclaw_plugin,
    );
    // Without the inventory the card keeps its button as it is; the next
    // refresh or scroll corrects it.
    CardUpdate { status, preview, prompt, oc_id, notice, scrolled_back: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(fields: &[&str]) -> String {
        format!("\u{1e}{}\n", fields.join("\u{1f}"))
    }

    /// The stamp fields of an active pane: id, active, width, history size,
    /// cursor x/y, alternate screen, window activity.
    const STAMP: [&str; 8] = ["%7", "1", "132", "30", "4", "39", "0", "1790000000"];

    /// A listing record: the card's fields, then `STAMP`, then not in a mode.
    fn row(fields: &[&str]) -> String {
        record(&[fields, &STAMP[..], &["0"]].concat())
    }

    #[test]
    fn snapshot_parses_every_session_in_one_listing() {
        let text = [
            row(&["sd_term_a", "1", "10", "bash", "0", "40", "/tmp/a", "", "ls -la", "1", "make test"]),
            // A second pane of the same active window: the first one wins.
            row(&["sd_term_a", "1", "11", "vim", "0", "40", "/tmp/a", "", "", "", ""]),
            // Inactive windows are ignored, as with `list-panes -t`.
            row(&["sd_term_b", "0", "20", "zsh", "0", "40", "/tmp/b", "", "", "", ""]),
            row(&["sd_term_b", "1", "21", "claude", "1", "30", "/tmp/b", "/x/meta.json", "", "", ""]),
            // Values may contain newlines and tabs.
            row(&["sd_term_c", "1", "30", "codex", "0", "24", "/tmp/with\ttab", "", "fix\nthis", "", ""]),
            // A value containing the field separator is never guessed at.
            row(&["sd_term_d", "1", "40", "pi", "0", "24", "/tmp/d", "", "bad\u{1f}value", "", ""]),
            // Neither is a record of another length (an older format).
            record(&["sd_term_e", "1", "50", "pi", "0", "24", "/tmp/e", "", "", "", ""]),
            // A split window whose first pane is not the active one, and a
            // tmux that left a stamp field empty: listed, but not stamped.
            record(&[
                &["sd_term_f", "1", "60", "grok", "0", "24", "/tmp/f", "", "", "", ""][..],
                &["%8", "0", "80", "0", "0", "0", "0", "1790000000", "0"],
            ]
            .concat()),
            record(&[
                &["sd_term_g", "1", "70", "grok", "0", "24", "/tmp/g", "", "", "", ""][..],
                &["%9", "1", "80", "0", "0", "0", "0", "", "0"],
            ]
            .concat()),
            // Scrolled back through the history: the pane is in copy mode.
            record(&[
                &["sd_term_h", "1", "80", "claude", "0", "24", "/tmp/h", "", "", "", ""][..],
                &STAMP[..],
                &["1"],
            ]
            .concat()),
        ]
        .concat();
        let snapshot = crate::tmux::parse_pane_snapshot(&text);
        let PaneLookup::Row(a) = snapshot.lookup("sd_term_a") else { panic!("a") };
        assert_eq!((a.pid.as_str(), a.cmd.as_str(), a.height), ("10", "bash", 40));
        assert_eq!(a.last_prompt, "ls -la");
        assert!(a.shell_tracking);
        assert_eq!(a.shell_command, "make test");
        let PaneLookup::Row(b) = snapshot.lookup("sd_term_b") else { panic!("b") };
        assert_eq!(b.pid, "21");
        assert!(b.dead);
        assert_eq!(b.metadata_option, "/x/meta.json");
        let PaneLookup::Row(c) = snapshot.lookup("sd_term_c") else { panic!("c") };
        assert_eq!(c.cwd, "/tmp/with\ttab");
        assert_eq!(c.last_prompt, "fix\nthis");
        assert!(matches!(snapshot.lookup("sd_term_d"), PaneLookup::Unknown));
        assert!(matches!(snapshot.lookup("sd_term_e"), PaneLookup::Unknown));
        assert!(matches!(snapshot.lookup("sd_term_gone"), PaneLookup::Missing));
        let PaneLookup::Row(h) = snapshot.lookup("sd_term_h") else { panic!("h") };
        assert!(h.in_mode && !a.in_mode && !b.in_mode, "copy mode is read per pane");
        // What an unchanged pane's capture is reused by.
        assert_eq!(
            a.stamp,
            Some(crate::tmux::PaneStamp {
                pane_id: "%7".into(),
                pid: "10".into(),
                width: 132,
                height: 40,
                history_size: 30,
                cursor: (4, 39),
                alternate: false,
                activity: 1_790_000_000,
            })
        );
        let PaneLookup::Row(f) = snapshot.lookup("sd_term_f") else { panic!("f") };
        assert_eq!((f.pid.as_str(), f.stamp.as_ref()), ("60", None));
        let PaneLookup::Row(g) = snapshot.lookup("sd_term_g") else { panic!("g") };
        assert_eq!((g.pid.as_str(), g.stamp.as_ref()), ("70", None));
    }

    #[test]
    fn batched_prompt_sources_follow_the_bridge_order() {
        let shell = PaneRow {
            pid: std::process::id().to_string(),
            last_prompt: "recorded".into(),
            shell_tracking: true,
            shell_command: "cargo test".into(),
            ..Default::default()
        };
        assert_eq!(card_prompt("shell", Some(&shell), None, None).as_deref(), Some("cargo test"));
        let recorded = PaneRow { last_prompt: " typed prompt ".into(), ..Default::default() };
        assert_eq!(card_prompt("grok", Some(&recorded), None, None).as_deref(), Some("typed prompt"));
        // Native metadata is authoritative once its adapter reported, even
        // when it has no prompt yet.
        let metadata = Metadata { native_session: "own".into(), ..Default::default() };
        assert_eq!(card_prompt("claude", Some(&recorded), Some(&metadata), None), None);
        let metadata = Metadata { title: "Named".into(), ..Default::default() };
        assert_eq!(card_title("claude", Some(&metadata), None, "1").as_deref(), Some("Named"));
        assert_eq!(card_title("grok", None, None, "1"), None);
        assert_eq!(card_prompt("grok", None, None, None), None);
    }

    // Card title regression guards (see AGENTS.md "Card titles"). Run with
    // `cargo test card_title_`.

    /// What `harness-event init` leaves before the adapter's first event.
    fn silent(agent: &str) -> Metadata {
        Metadata {
            version: 1,
            agent: agent.into(),
            status: "unknown".into(),
            pid: 42,
            process_start: "launch".into(),
            observed_at_ms: 1,
            ..Default::default()
        }
    }

    #[test]
    fn card_title_silent_native_adapter_falls_back_to_the_typed_prompt() {
        let typed = PaneRow { last_prompt: "Fix the login bug".into(), ..Default::default() };
        for agent in ["openclaw", "claude", "opencode", "pi"] {
            let metadata = silent(agent);
            let prompt = card_prompt(agent, Some(&typed), Some(&metadata), None);
            assert_eq!(prompt.as_deref(), Some("Fix the login bug"), "{agent}");
            // The card title is the native title, else this prompt.
            let title = card_title(agent, Some(&metadata), None, "0").or(prompt);
            assert_eq!(title.as_deref(), Some("Fix the login bug"), "{agent}");
        }
        // The phone's lastPrompt (`bridge::last_user_text`) makes the same call.
        assert_eq!(
            reported_or_typed_prompt(Some(&silent("openclaw")), || Some("Deploy it".into())),
            Some(Some("Deploy it".into()))
        );
        assert_eq!(reported_or_typed_prompt(Some(&silent("openclaw")), || None), None);
    }

    #[test]
    fn card_title_typed_fallback_still_rejects_injected_turns() {
        for injected in [
            "<task-notification> <task-id>b1</task-id> <status>completed</status>",
            "<system-reminder>Background task finished</system-reminder>",
            "<bash-stdout>1 2 3</bash-stdout>",
        ] {
            let row = PaneRow { last_prompt: injected.into(), ..Default::default() };
            assert_eq!(card_prompt("openclaw", Some(&row), Some(&silent("openclaw")), None), None, "{injected}");
            assert_eq!(card_prompt("grok", Some(&row), None, None), None, "{injected}");
            assert_eq!(
                reported_or_typed_prompt(Some(&silent("claude")), || Some(injected.into())),
                None,
                "{injected}"
            );
        }
    }

    #[test]
    fn card_title_reporting_adapter_wins_over_the_typed_prompt() {
        let typed = PaneRow { last_prompt: "typed text".into(), ..Default::default() };
        // Any adapter event: a native session, the JS reporter's emitter
        // (OpenCode's load-time idle), a title or a prompt.
        for (field, reported) in [
            ("session", Metadata { native_session: "agent:main:sd_term_x/abc".into(), ..silent("openclaw") }),
            ("emitter", Metadata { emitter: 4242, status: "idle".into(), ..silent("opencode") }),
            ("title", Metadata { title: "Named".into(), ..silent("pi") }),
        ] {
            assert!(reported.adapter_reported(), "{field}");
            assert_eq!(card_prompt(&reported.agent, Some(&typed), Some(&reported), None), None, "{field}");
        }
        let prompted = Metadata { native_session: "own".into(), prompt: "Native prompt".into(), ..silent("claude") };
        assert_eq!(card_prompt("claude", Some(&typed), Some(&prompted), None).as_deref(), Some("Native prompt"));
        assert_eq!(
            reported_or_typed_prompt(Some(&prompted), || Some("typed text".into())),
            Some(Some("Native prompt".into()))
        );
        assert!(!silent("openclaw").adapter_reported());
    }

    #[test]
    fn silent_native_adapter_status_uses_the_screen() {
        let row = PaneRow {
            pid: std::process::id().to_string(),
            cmd: "openclaw".into(),
            height: 24,
            ..Default::default()
        };
        let status = |metadata: Metadata, screen: &str| {
            let screen = screen.to_string();
            crate::tmux::status_for_pane("sd_term_status_test", "openclaw", &row,
                &|| Some(metadata.clone()), &mut || Some(screen.clone())).status
        };
        let busy = "✻ Thinking… (esc to interrupt)\n";
        assert_eq!(status(silent("openclaw"), busy), "WORKING");
        assert_eq!(status(silent("openclaw"), "> \n"), "IDLE");
        // Once the adapter reports, its status wins, including UNKNOWN.
        let reported = Metadata { native_session: "agent:main:sd_term_x".into(), ..silent("openclaw") };
        assert_eq!(status(reported.clone(), busy), "UNKNOWN");
        assert_eq!(status(Metadata { status: "idle".into(), ..reported }, busy), "IDLE");
    }

    #[test]
    fn openclaw_setup_notice_needs_a_silent_openclaw_launch_without_the_plugin() {
        use crate::harness_metadata::OpenClawPlugin;
        let missing = || OpenClawPlugin::Missing("the plugin is not registered");
        assert_eq!(setup_notice(Some(&silent("openclaw")), missing), Some(OPENCLAW_SETUP_NOTICE));
        // A custom launcher running `openclaw` records agent "openclaw" too.
        let custom = Metadata { launcher: "custom-abc".into(), ..silent("openclaw") };
        assert_eq!(setup_notice(Some(&custom), missing), Some(OPENCLAW_SETUP_NOTICE));
        assert_eq!(setup_notice(Some(&silent("openclaw")), || OpenClawPlugin::Connected), None);
        assert_eq!(setup_notice(Some(&silent("openclaw")), || OpenClawPlugin::Unreadable), None);
        assert_eq!(setup_notice(None, missing), None);
        let reported = Metadata { native_session: "agent:main:sd_term_x".into(), ..silent("openclaw") };
        assert_eq!(setup_notice(Some(&reported), missing), None);
        // Other agents never ask about OpenClaw's config.
        assert_eq!(setup_notice(Some(&silent("claude")), || panic!("not asked")), None);
        // Shown once per session, not on every refresh.
        let session = format!("sd_term_notice_{}", std::process::id());
        assert!(claim_setup_notice(&session));
        assert!(!claim_setup_notice(&session));
        assert!(claim_setup_notice(&format!("{session}_other")));
    }

    /// The batched path must describe live sessions exactly like the
    /// per-session path it replaces.
    #[test]
    fn batched_refresh_matches_per_session_queries() {
        use std::process::Command;
        let sessions: Vec<String> = (0..3)
            .map(|_| format!("test_sd_{}", crate::tmux::unique_session_name()))
            .collect();
        struct Cleanup(Vec<String>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for session in &self.0 {
                    let _ = Command::new("tmux").args(["kill-session", "-t", session]).output();
                }
            }
        }
        let _cleanup = Cleanup(sessions.clone());
        for session in &sessions {
            assert!(Command::new("tmux")
                .args(["new-session", "-d", "-s", session, "bash", "--norc", "--noprofile"])
                .status()
                .unwrap()
                .success());
        }
        let _ = Command::new("tmux")
            .args(["set-option", "-t", &sessions[1], "@super_desktop_last_prompt", "typed\tprompt"])
            .output();
        let requests = || {
            sessions
                .iter()
                .enumerate()
                .map(|(index, session)| CardRequest {
                    session: session.clone(),
                    agent: if index == 1 { "grok" } else { "shell" }.into(),
                    preview_lines: (index == 2).then_some(6),
                    oc_id: None,
                    need_resolve: false,
                })
                .collect::<Vec<_>>()
        };
        // Let the shells reach their prompt.
        let mut batched = Vec::new();
        for _ in 0..30 {
            batched = refresh(requests());
            if batched.iter().all(|u| u.status.status == "IDLE") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let legacy: Vec<CardUpdate> = requests().iter().map(legacy).collect();
        for (batched, legacy) in batched.iter().zip(&legacy) {
            assert_eq!(batched.status.status, legacy.status.status);
            assert_eq!(batched.status.pid, legacy.status.pid);
            assert_eq!(batched.status.cwd, legacy.status.cwd);
            assert_eq!(batched.prompt, legacy.prompt);
        }
        assert_eq!(batched[1].prompt.as_deref(), Some("typed\tprompt"));
        assert!(batched[0].preview.is_none() && batched[2].preview.is_some());
    }

    /// A gone session costs no subprocess and says so.
    #[test]
    fn missing_session_is_exited_without_probing() {
        let update = from_row(
            &CardRequest {
                session: "sd_term_missing_card".into(),
                agent: "grok".into(),
                preview_lines: Some(6),
                oc_id: None,
                need_resolve: false,
            },
            None,
        );
        assert_eq!(update.status.status, "EXITED");
        assert_eq!(update.preview.as_deref(), Some("Session offline or ended."));
        assert_eq!(update.prompt, None);
    }
}
