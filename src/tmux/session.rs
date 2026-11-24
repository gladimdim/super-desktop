use super::*;

/// The grid one owned pane renders into, in cells.
///
/// This is the pane's own width and height — not a client's, which includes the
/// status line — so a phone rendering the pane's captured text can size that
/// text to the columns the pane actually has. Reads nothing and changes nothing.
/// The phone stream asks over its control connection (`tmux_control`).
#[cfg(test)]
pub fn pane_grid(session_name: &str) -> Option<crate::desktop_protocol::TerminalSize> {
    if !is_owned_session(session_name) {
        return None;
    }
    let output = command_stdout(
        Command::new(tmux_bin())
            .args([
                "display-message",
                "-p",
                "-t",
                &format!("={session_name}:"),
                "#{pane_width} #{pane_height}",
            ])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE"),
    )?;
    parse_pane_grid(&output)
}

/// `#{pane_width} #{pane_height}` as a validated grid.
pub fn parse_pane_grid(text: &str) -> Option<crate::desktop_protocol::TerminalSize> {
    let mut fields = text.split_whitespace();
    let columns = fields.next()?.parse::<u16>().ok()?;
    let rows = fields.next()?.parse::<u16>().ok()?;
    crate::desktop_protocol::TerminalSize { columns, rows }
        .validate()
        .ok()
}

/// Pin a harness session so the tmux client rendering it exits together with
/// the session instead of being re-homed to a different one.
///
/// Every card here is exactly one tmux client (`tmux attach-session` inside one
/// VTE). What a client does when its session is destroyed comes from the user's
/// tmux config, and omarchy ships `set -g detach-on-destroy off`: with it a
/// client whose session dies does NOT exit, it is switched to the next session.
/// A card that lost its session then starts rendering a DIFFERENT harness (the
/// same output as that other card), and the next Ctrl-C typed there destroys
/// that other harness — closing one harness looked like all of them closing.
///
/// `detach-on-destroy` is a session option (tmux >= 3.2), so it is pinned on
/// our own sessions only. While the user's global config keeps its value, older
/// tmux versions reject the per-session form; the call then fails and the
/// previous behaviour remains instead of us mutating the server options.
pub(in crate::tmux) fn pin_client_exit(session_name: &str) {
    if !is_owned_session(session_name) {
        return;
    }
    let exact = format!("={session_name}:");
    let _ = Command::new(tmux_bin())
        .args(["set-option", "-t", &exact, "detach-on-destroy", "on"])
        .output();
    // Same moments (create/attach): make mouse copies reach the clipboard.
    crate::tmux_clipboard::install_session(session_name);
}

/// Existence + the pinned-client flag of one session, in a single `tmux` call:
/// `Some(true)` = exists and already pins `detach-on-destroy on`, `Some(false)`
/// = exists but still inherits the global setting, `None` = no such session.
///
/// It can also carry a brand-new card's session (`fresh`): the card's first
/// attach creates it on its worker, so a launch never waits for tmux on GTK.
#[derive(Default)]
pub struct SessionInventory(
    pub(in crate::tmux) std::sync::OnceLock<Option<std::collections::HashMap<String, bool>>>,
    std::sync::Mutex<Option<(String, FreshSession)>>,
);

/// A session to start with the harness's fresh command, not its resume form.
pub struct FreshSession {
    agent_type: String,
    command: String,
    workspace_dir: String,
}

impl SessionInventory {
    // The CLI has already created and pinned this session. Do not execute a
    // second launcher if it exits before the first VTE attachment.
    pub(crate) fn cli_created(session: &str) -> Self {
        let entries = std::collections::HashMap::from([(session.to_string(), true)]);
        Self(std::sync::OnceLock::from(Some(entries)), Default::default())
    }

    /// `session` does not exist yet: the attach that receives this creates it
    /// with `command` (already resolved, see `resolve_command`).
    pub(crate) fn fresh(session: &str, agent_type: &str, command: &str, workspace_dir: &str) -> Self {
        let fresh = FreshSession {
            agent_type: agent_type.to_string(),
            command: command.to_string(),
            workspace_dir: workspace_dir.to_string(),
        };
        Self(Default::default(), std::sync::Mutex::new(Some((session.to_string(), fresh))))
    }

    pub(in crate::tmux) fn state(&self, session_name: &str) -> Option<bool> {
        self.0.get_or_init(read_session_inventory).as_ref()?.get(session_name).copied()
    }

    /// The fresh session for `session_name`, once: a later attach of the same
    /// card must find (or resume) the session, never launch a second one.
    pub(in crate::tmux) fn take_fresh(&self, session_name: &str) -> Option<FreshSession> {
        let mut fresh = self.1.lock().unwrap_or_else(|e| e.into_inner());
        if fresh.as_ref().is_some_and(|(name, _)| name == session_name) {
            fresh.take().map(|(_, session)| session)
        } else {
            None
        }
    }
}

pub(in crate::tmux) fn session_state(session_name: &str) -> Option<bool> {
    read_session_inventory()?.get(session_name).copied()
}

pub(in crate::tmux) fn read_session_inventory() -> Option<std::collections::HashMap<String, bool>> {
    let text = tmux_stdout(&["list-sessions", "-F", "#{session_name}::#{detach-on-destroy}"])?;
    Some(text.lines().filter_map(|line| {
        let (name, value) = line.split_once("::")?;
        Some((name.trim().to_string(), value.trim() == "on"))
    }).collect())
}

/// The directory a harness session runs in: the requested folder when it is a
/// real directory, otherwise the home directory.
///
/// Silently falling back matters because this value comes from a config file
/// and from the top bar: a deleted checkout or an unmounted drive must still
/// produce a working terminal rather than a session that starts nowhere.
pub fn resolve_workspace_dir(requested: Option<&str>) -> String {
    requested
        .map(str::trim)
        .filter(|d| !d.is_empty() && Path::new(d).is_dir())
        .map(str::to_string)
        .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

/// Start a harness session in `workspace_dir` (`None` = the home directory).
///
/// The directory is passed to `tmux new-session -c`, so the harness — and
/// everything it derives from the process cwd, including Reasonix's workspace
/// write lease — is scoped to that folder instead of `$HOME`.
pub fn create_session(
    agent_type: &str,
    custom_command: Option<&str>,
    workspace_dir: Option<&str>,
) -> (String, String) {
    let cmd = resolve_command(agent_type, custom_command);
    // Names are unique within this process (see `session_id`). Only a session
    // left by an older process can collide, and tmux refuses that name.
    for _ in 0..20 {
        let session_name = super_desktop::session_id::candidate();
        if start_session(&session_name, agent_type, &cmd, workspace_dir) != Started::Duplicate {
            return (session_name, cmd);
        }
    }
    (super_desktop::session_id::candidate(), cmd)
}

#[derive(Debug, PartialEq)]
pub(in crate::tmux) enum Started {
    Yes,
    Duplicate,
    Failed,
}

/// Start `session_name` running `command` (wrapped like every card launch) and
/// set it up for a card: `detach-on-destroy on` and the clipboard hook.
///
/// One tmux process instead of five (`has-session`, `new-session`,
/// `set-option`, `list-keys`, `set-hook`), each ~13 ms on a handheld APU. A
/// failed command skips the rest of tmux's chain, so nothing is changed on a
/// session that already existed.
pub(in crate::tmux) fn start_session(session_name: &str, agent_type: &str, command: &str, workspace_dir: Option<&str>) -> Started {
    let launch = crate::shell_title::launch_command(agent_type, command);
    let launch = crate::harness_metadata::prepare(session_name, agent_type, &launch);
    let cwd = resolve_workspace_dir(workspace_dir);

    let exact = format!("={session_name}:");
    let mut tmux = Command::new(tmux_bin());
    tmux.args([
        "new-session",
        "-d",
        "-s",
        session_name,
        "-c",
        &cwd,
        "-x",
        "120",
        "-y",
        "35",
        "--",
        &launch.command,
    ]);
    tmux.args([";", "set-option", "-t", &exact, "detach-on-destroy", "on"]);
    if let Some(hook) = crate::tmux_clipboard::session_hook_args(session_name) {
        tmux.arg(";").args(hook);
    }
    tmux.args([";", "display-message", "-p", "#{pid}"]);
    let Ok(out) = tmux.output() else {
        return Started::Failed;
    };
    if !out.status.success() {
        if String::from_utf8_lossy(&out.stderr).contains("duplicate session") {
            return Started::Duplicate;
        }
        // The harness may have exited at once, taking the session (and the
        // rest of the chain) with it: still register it, as before.
        launch.register(session_name);
        return Started::Failed;
    }
    launch.register(session_name);
    crate::tmux_clipboard::ensure_bindings(String::from_utf8_lossy(&out.stdout).trim());
    Started::Yes
}

/// True when a tmux session with this name already exists on the server.
pub fn session_exists(session_name: &str) -> bool {
    if !is_owned_session(session_name) {
        return false;
    }
    let exact = format!("={session_name}");
    Command::new(tmux_bin())
        .args(["has-session", "-t", &exact])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Generate a session name distinct from concurrent calls, even when the
/// clock does not advance. Keep checking tmux for names left by older processes.
#[cfg(test)]
pub fn unique_session_name() -> String {
    for _ in 0..20 {
        let candidate = super_desktop::session_id::candidate();
        if !session_exists(&candidate) {
            return candidate;
        }
    }
    super_desktop::session_id::candidate()
}

pub fn kill_session(session_name: &str) {
    if !is_owned_session(session_name) {
        return;
    }
    let exact = format!("={session_name}");
    let _ = Command::new(tmux_bin())
        .args(["kill-session", "-t", &exact])
        .output();
}

/// Kill `session_name` and start a new session under the same name running `command`.
pub fn restart_with_command(
    session_name: &str,
    agent_type: &str,
    command: &str,
    workspace_dir: Option<&str>,
) -> bool {
    kill_session(session_name);
    start_session(session_name, agent_type, command, workspace_dir) == Started::Yes
}

/// Kill `session_name` and start a clean session under the same name using its
/// original launch command, bypassing `--continue` / `resume` flags.
pub fn restart_fresh(
    session_name: &str,
    agent_type: &str,
    custom_command: Option<&str>,
    workspace_dir: Option<&str>,
) {
    let cmd = resolve_command(agent_type, custom_command);
    let _ = restart_with_command(session_name, agent_type, &cmd, workspace_dir);
}

/// (Re)create `session_name` unless it is alive, resuming a persisted per-card
/// agent session (opencode `--session <id>`) so rebooted cards stay isolated 1:1.
///
/// `workspace_dir` is the folder this card was created in (see
/// `TerminalData::workspace_dir`), not the top bar's current value: resume is
/// cwd-scoped for every harness, so a restored card must come back where it
/// was working.
///
/// Share an inventory only during the initial restoration batch, never across
/// later attachments (which need to discover sessions closed in the meantime).
pub fn ensure_session_with_inventory(
    session_name: &str,
    agent_type: &str,
    custom_command: Option<&str>,
    agent_session_id: Option<&str>,
    workspace_dir: Option<&str>,
    inventory: Option<&SessionInventory>,
) {
    if let Some(fresh) = inventory.and_then(|inventory| inventory.take_fresh(session_name)) {
        let started = start_session(
            session_name,
            &fresh.agent_type,
            &fresh.command,
            Some(&fresh.workspace_dir),
        );
        if started == Started::Duplicate {
            eprintln!("SUPER DESKTOP: session {session_name} already existed; attaching to it");
        }
        return;
    }
    // One `tmux` call answers both questions this function needs: does the
    // session exist, and is `detach-on-destroy` already pinned? Every fork/exec
    // is tens of milliseconds on a loaded machine, and this runs once per card
    // while the overlay is being built.
    match inventory.map_or_else(|| session_state(session_name), |snapshot| snapshot.state(session_name)) {
        // Already there and already pinned: nothing to do.
        Some(true) => return,
        // Exists but still inheriting the user's `detach-on-destroy off`
        // (sessions made by an older build, or a plain `tmux new-session`).
        Some(false) => {
            pin_client_exit(session_name);
            return;
        }
        None => {}
    }

    {
        // Reboot survival: the tmux server is gone, but agent CLIs persist
        // conversations to disk continuously, so recreate with the agent's
        // native resume mechanism (see resolve_resume_command). Brand-new
        // terminals in create_session() still launch fresh.
        let cwd = resolve_workspace_dir(workspace_dir);
        let cmd = resolve_resume_command_in(agent_type, custom_command, agent_session_id, Some(&cwd));
        // Another attach may have created it since the inventory was read.
        if start_session(session_name, agent_type, &cmd, workspace_dir) == Started::Duplicate {
            pin_client_exit(session_name);
        }
    }
}

/// Harnesses read a burst of typed keys as a paste, and a Return that arrives
/// inside it becomes a newline in the draft instead of submitting it. Shells
/// take keys one at a time and submit at once.
pub fn settles_before_enter(agent: &str) -> bool {
    !crate::shell_title::is_regular(agent)
}

/// Shortest wait between typed text and Return: longer than the harnesses'
/// paste-detection windows (Codex needs it, Claude Code collects ~100 ms).
pub(in crate::tmux) const ENTER_SETTLE_MIN: std::time::Duration = std::time::Duration::from_millis(200);
/// Longest wait for a harness to show the typed text before Return goes anyway.
pub(in crate::tmux) const ENTER_SETTLE_MAX: std::time::Duration = std::time::Duration::from_millis(1500);

/// Wait until typed text can be submitted: [ENTER_SETTLE_MIN], then until the
/// visible screen differs from `before` (the harness has read the keys), for
/// at most [ENTER_SETTLE_MAX] in all. A busy harness that lags behind its
/// input would otherwise read the text and Return in one burst. Without a
/// `before` screen, or when `screen` fails, only the minimum applies.
pub fn settle_before_enter(before: Option<&str>, screen: impl FnMut() -> Option<String>) {
    settle_before_enter_within(before, screen, ENTER_SETTLE_MIN, ENTER_SETTLE_MAX)
}

pub(in crate::tmux) fn settle_before_enter_within(
    before: Option<&str>,
    mut screen: impl FnMut() -> Option<String>,
    min: std::time::Duration,
    max: std::time::Duration,
) {
    let started = std::time::Instant::now();
    std::thread::sleep(min);
    let Some(before) = before else { return };
    while started.elapsed() < max {
        match screen() {
            Some(now) if now == before => std::thread::sleep(std::time::Duration::from_millis(25)),
            _ => return,
        }
    }
}

/// Type `text` into a live session (phone → harness), optionally followed by
/// Return.
///
/// `-l` sends the text literally, so quotes, pipes and globs reach the pane as
/// written instead of being interpreted by tmux, and `--` stops a message that
/// begins with a dash from being read as a flag. For a harness `agent`,
/// Return waits until the typed text has settled ([settle_before_enter]).
pub fn send_keys(session_name: &str, agent: &str, text: &str, enter: bool) -> Result<(), String> {
    send_keys_inner(session_name, agent, text, enter, true)
}

/// Sends keystrokes to a tmux session without recording prompt history (used by plugins).
pub fn send_keys_unrecorded(session_name: &str, agent: &str, text: &str, enter: bool) -> Result<(), String> {
    send_keys_inner(session_name, agent, text, enter, false)
}

fn send_keys_inner(session_name: &str, agent: &str, text: &str, enter: bool, record_prompt: bool) -> Result<(), String> {
    if !super::is_owned_session(session_name) && !session_name.starts_with("sd_plugin_api_") {
        return Err("invalid_session".to_string());
    }
    let target = format!("={session_name}:");
    let settle = enter && !text.is_empty() && settles_before_enter(agent);
    let before = if settle { capture_visible_screen(session_name) } else { None };
    if !text.is_empty() {
        let out = Command::new(tmux_bin())
            .args(["send-keys", "-t", &target, "-l", "--", text])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
    }
    if settle {
        settle_before_enter(before.as_deref(), || capture_visible_screen(session_name));
    }
    if enter {
        let out = Command::new(tmux_bin())
            .args(["send-keys", "-t", &target, "Enter"])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
    }
    if record_prompt && enter && !text.trim().is_empty() {
        crate::prompt_history::record(session_name, text);
    }
    Ok(())
}

