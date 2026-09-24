//! Batched status/title/preview refresh for local terminal cards.
//!
//! The overlay refreshes every card once per second. Asking tmux per card
//! (list-panes, show-options, display-message ×2, capture-pane) cost 4–6
//! processes per card per second; here one `tmux list-panes -a` describes every
//! session, and a card only captures its pane when the capture is used (a
//! visible preview, or a screen-based status for an agent without native
//! metadata). Runs on a worker thread, never on the GTK main thread.
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
}

/// Refresh every requested card from one tmux inventory.
pub fn refresh(requests: Vec<CardRequest>) -> Vec<CardUpdate> {
    if requests.is_empty() {
        return Vec::new();
    }
    let snapshot = crate::tmux::pane_snapshot();
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
    // captures only the visible rows, and only if it gets that far.
    let screen = match (row, request.preview_lines) {
        (Some(_), Some(_)) => crate::tmux::capture_pane_text(session),
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
                    crate::tmux::capture_visible_screen(session)
                }
            },
        ),
        None => crate::tmux::exited_status(agent),
    };
    let preview = request.preview_lines.map(|lines| preview_text(screen.as_deref(), &status, lines));
    let oc_id = resolve_oc_id(request);
    let prompt = card_prompt(agent, row, metadata.as_ref(), oc_id.as_deref());
    let prompt = card_title(agent, metadata.as_ref(), oc_id.as_deref(), &status.pid).or(prompt);
    CardUpdate { status, preview, prompt, oc_id }
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
    if let Some(metadata) = metadata {
        return (!metadata.prompt.is_empty())
            .then(|| crate::tmux::truncate_prompt_title(&metadata.prompt));
    }
    if let Some(prompt) = recorded() {
        return Some(prompt);
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
    CardUpdate { status, preview, prompt, oc_id }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(fields: &[&str]) -> String {
        format!("\u{1e}{}\n", fields.join("\u{1f}"))
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
        assert!(matches!(snapshot.lookup("sd_term_gone"), PaneLookup::Missing));
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
        // Native metadata is authoritative, even when it has no prompt yet.
        let metadata = Metadata { prompt: String::new(), ..Default::default() };
        assert_eq!(card_prompt("claude", Some(&recorded), Some(&metadata), None), None);
        let metadata = Metadata { title: "Named".into(), ..Default::default() };
        assert_eq!(card_title("claude", Some(&metadata), None, "1").as_deref(), Some("Named"));
        assert_eq!(card_title("grok", None, None, "1"), None);
        assert_eq!(card_prompt("grok", None, None, None), None);
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
