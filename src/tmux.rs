use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct AgentConfig {
    pub name: &'static str,
    pub icon: &'static str,
    pub commands: &'static [&'static str],
    pub default_args: &'static [&'static str],
    /// npm package name for harnesses that are commonly run ad hoc through
    /// `npx <package>` instead of being installed on PATH. Only consulted
    /// when none of `commands` resolves (see `resolve_command`).
    pub npx_package: Option<&'static str>,
}

pub fn get_agent_config(agent_type: &str) -> AgentConfig {
    match agent_type {
        "antigravity" | "agy" => AgentConfig {
            name: "Antigravity",
            icon: "🌌",
            commands: &["agy", "antigravity"],
            default_args: &["--dangerously-skip-permissions"],
            npx_package: None,
        },
        "claude" => AgentConfig {
            name: "Claude Code",
            icon: "⚡",
            commands: &["claude"],
            default_args: &["--dangerously-skip-permissions"],
            npx_package: None,
        },
        "codex" => AgentConfig {
            name: "OpenAI Codex",
            icon: "🤖",
            commands: &["codex"],
            default_args: &["--dangerously-bypass-approvals-and-sandbox"],
            npx_package: None,
        },
        "opencode" => AgentConfig {
            name: "OpenCode",
            icon: "🔮",
            commands: &["opencode"],
            default_args: &["--auto"],
            npx_package: None,
        },
        "grok" => AgentConfig {
            name: "Grok CLI",
            icon: "🚀",
            commands: &["grok"],
            default_args: &["--dangerously-skip-permissions"],
            npx_package: None,
        },
        "aider" => AgentConfig {
            name: "Aider",
            icon: "🧠",
            commands: &["aider"],
            default_args: &["--yes-always"],
            npx_package: None,
        },
        // `code` opens Reasonix' interactive coding session. Deliberately no
        // permission flag: Reasonix keeps its own `workspace-write` sandbox
        // (in-workspace writes approved, everything else asked in the card).
        "reasonix" => AgentConfig {
            name: "Reasonix",
            icon: "🧭",
            commands: &["reasonix"],
            default_args: &["code"],
            npx_package: Some("reasonix"),
        },
        _ => AgentConfig {
            name: "Terminal",
            icon: "💻",
            commands: &["bash"],
            default_args: &[],
            npx_package: None,
        },
    }
}

/// `npx -y <package> <args…>`. `-y` keeps npx from stopping on its
/// "Ok to proceed?" install prompt the first time the package is fetched.
fn npx_fallback_command(package: &str, args: &[&str]) -> String {
    let mut parts = vec!["npx".to_string(), "-y".to_string(), package.to_string()];
    parts.extend(args.iter().map(|a| (*a).to_string()));
    parts.join(" ")
}

/// Harness types a card can be launched for, in top-bar order.
///
/// This is the single source of truth for "what super-desktop can run"; the
/// settings panel narrows it down to what is actually installed, and the HUD
/// builds one (possibly hidden) launch button per entry.
pub const HARNESS_KEYS: &[&str] = &[
    "antigravity",
    "claude",
    "codex",
    "opencode",
    "grok",
    "reasonix",
    "aider",
    "shell",
];

/// A harness type that resolves on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessInfo {
    /// Canonical agent key, i.e. `TerminalData::agent_type`.
    pub key: &'static str,
    /// Display name (`get_agent_config`), e.g. "Claude Code".
    pub name: &'static str,
    /// Emoji used when the brand SVG is missing.
    pub icon: &'static str,
    /// What a card launched for this harness would run, resolved here:
    /// `/usr/bin/claude --dangerously-skip-permissions`,
    /// `npx -y reasonix code`, …
    pub command: String,
}

/// `which <cmd>` → absolute path, or `None` when it is not on PATH.
///
/// Done in-process instead of shelling out: harness detection alone asks ~12
/// times per window build, and forking `which` costs tens of milliseconds on a
/// loaded machine — that dominated the time between the shortcut and the
/// overlay appearing.
fn which(cmd: &str) -> Option<String> {
    if cmd.is_empty() {
        return None;
    }
    if cmd.contains('/') {
        let p = Path::new(cmd);
        return is_executable(p).then(|| cmd.to_string());
    }
    which_in_path(cmd, &std::env::var_os("PATH")?)
}

/// PATH lookup split out of `which` so it can be tested without mutating the
/// process-wide environment (which other tests spawn processes from).
fn which_in_path(cmd: &str, path: &std::ffi::OsStr) -> Option<String> {
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(cmd);
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // `symlink_metadata` first is pointless: a broken symlink must report false.
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

fn with_default_args(path: &str, args: &[&str]) -> String {
    if args.is_empty() {
        path.to_string()
    } else {
        format!("{} {}", path, args.join(" "))
    }
}

/// Command to launch `key` here, or `None` when nothing resolves — i.e. the
/// harness is not available on this machine.
///
/// Mirrors `resolve_command`'s order (PATH → `npx` package → shell) but keeps
/// the "nothing found" case instead of falling back to `$SHELL`, so the
/// settings panel only ever offers harnesses that really exist.
pub fn detect_harness_command(key: &str) -> Option<String> {
    let cfg = get_agent_config(key);

    for cmd in harness_candidates(key) {
        if let Some(path) = which(cmd) {
            return Some(with_default_args(&path, cfg.default_args));
        }
    }
    // Harnesses that commonly run through `npx` (Reasonix) are available as
    // soon as npx is, exactly like `resolve_command` assumes.
    if let Some(package) = cfg.npx_package {
        if which("npx").is_some() {
            return Some(npx_fallback_command(package, cfg.default_args));
        }
    }
    if key == "shell" {
        if let Ok(shell) = std::env::var("SHELL") {
            if std::path::Path::new(&shell).is_file() {
                return Some(shell);
            }
        }
    }
    None
}

/// Binaries that can serve `key`, in preference order.
///
/// A terminal card is happy with any POSIX shell this box actually has; the
/// other harnesses only accept their own binary.
fn harness_candidates(key: &str) -> &'static [&'static str] {
    if key == "shell" {
        &["bash", "zsh", "fish", "sh"]
    } else {
        get_agent_config(key).commands
    }
}

/// Every harness installed on this machine, in `HARNESS_KEYS` order.
pub fn detect_harnesses() -> Vec<HarnessInfo> {
    HARNESS_KEYS
        .iter()
        .filter_map(|key| {
            let command = detect_harness_command(key)?;
            let cfg = get_agent_config(key);
            Some(HarnessInfo {
                key,
                name: cfg.name,
                icon: cfg.icon,
                command,
            })
        })
        .collect()
}

pub fn resolve_command(agent_type: &str, custom: Option<&str>) -> String {
    let cfg = get_agent_config(agent_type);
    if let Some(cmd) = custom {
        let trimmed = cmd.trim();
        if !cfg.default_args.is_empty() && !trimmed.is_empty() {
            let is_shell = trimmed.ends_with("/bash")
                || trimmed == "bash"
                || trimmed.ends_with("/zsh")
                || trimmed == "zsh"
                || trimmed.ends_with("/sh")
                || trimmed == "sh"
                || trimmed.ends_with("/fish")
                || trimmed == "fish";
            let already_has_arg = cfg.default_args.iter().any(|arg| trimmed.contains(arg));
            if !is_shell && !already_has_arg {
                return format!("{} {}", trimmed, cfg.default_args.join(" "));
            }
        }
        return trimmed.to_string();
    }
    for cmd in cfg.commands {
        if let Some(path) = which(cmd) {
            return with_default_args(&path, cfg.default_args);
        }
    }
    // Harness that ships on npm but has no binary on PATH (e.g. Reasonix is
    // usually run as `npx reasonix code`): launch it through npx rather than
    // silently degrading the card to a bare shell.
    if let Some(package) = cfg.npx_package {
        if which("npx").is_some() {
            return npx_fallback_command(package, cfg.default_args);
        }
    }
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
}

fn is_shell_command(cmd: &str) -> bool {
    let base = cmd
        .split_whitespace()
        .next()
        .and_then(|s| s.rsplit('/').next())
        .unwrap_or("");
    matches!(base, "bash" | "zsh" | "fish" | "sh" | "dash")
}

fn append_resume_flag(base: &str, flag: &str, markers: &[&str]) -> String {
    if markers.iter().any(|m| base.contains(m)) {
        base.to_string()
    } else {
        format!("{base} {flag}")
    }
}

/// Resolve the command used to (re)create a tmux session when the previous
/// one is gone, e.g. after a laptop reboot: the tmux server dies with it,
/// but every supported agent persists its conversations to disk continuously
/// (Claude: ~/.claude/projects, Codex: ~/.codex/sessions, OpenCode: its
/// session store, Reasonix: ~/.reasonix/projects, Aider:
/// .aider.chat.history.md), so relaunching with the agent's native resume
/// mechanism restores the conversation automatically.
///
/// OPENCODE ISOLATION: when `agent_session_id` (persisted per card, see
/// `TerminalData::agent_session_id`) is known, resume THAT exact session via
/// `opencode --session <id>` so N cards never share one conversation.
/// Without a stored id we fall back to `--continue --fork`: `--fork` clones
/// the latest session into an independent copy, so even legacy cards (created
/// before ids were persisted) diverge instead of live-sharing one session
/// where Ctrl+C / output in one card leaks into all others.
///
/// Agents without a non-interactive resume mechanism (shell, antigravity,
/// grok, unknown) relaunch fresh, exactly like before. An explicit shell
/// command is never decorated with agent flags.
#[allow(dead_code)]
pub fn resolve_resume_command(agent_type: &str, custom: Option<&str>) -> String {
    resolve_resume_command_with_session(agent_type, custom, None)
}

/// Same as `resolve_resume_command` but honours a persisted per-card agent
/// session id (currently used for opencode).
pub fn resolve_resume_command_with_session(
    agent_type: &str,
    custom: Option<&str>,
    agent_session_id: Option<&str>,
) -> String {
    let base = resolve_command(agent_type, custom);
    if is_shell_command(&base) {
        return base;
    }
    // Exact per-card resume wins over "latest" heuristics.
    if agent_type == "opencode" {
        if let Some(id) = agent_session_id.map(str::trim).filter(|s| !s.is_empty()) {
            if base.contains("--session") {
                return base;
            }
            return format!("{base} --session {id}");
        }
    }
    match agent_type {
        // `claude --continue` resumes the most recent session for the cwd.
        "claude" => append_resume_flag(&base, "--continue", &["--continue", "--resume"]),
        // `opencode --continue --fork` clones the latest session into an
        // independent copy (no stored id to address directly). Plain
        // `--continue` without `--fork` would attach every restored card to
        // the SAME live session: output/Ctrl+C mixing across cards.
        "opencode" => {
            let with_continue =
                append_resume_flag(&base, "--continue", &["--continue", "--session"]);
            append_resume_flag(&with_continue, "--fork", &["--fork"])
        }
        // Codex resumes via subcommand: `codex resume --last`
        // (--last is scoped to the cwd — the card's workspace folder, see
        // TerminalData::workspace_dir).
        // Global flags before the subcommand parse fine under clap.
        "codex" => append_resume_flag(&base, "resume --last", &["resume"]),
        // Aider restores prior chat history for the repo on startup.
        "aider" => append_resume_flag(
            &base,
            "--restore-chat-history",
            &["--restore-chat-history"],
        ),
        // Reasonix resumes the most recent session with `--continue`; `--copy`
        // then works on a clone, so restored cards never attach to one shared
        // live session (reasonix's equivalent of opencode's `--fork`).
        "reasonix" => {
            let with_continue =
                append_resume_flag(&base, "--continue", &["--continue", "--resume"]);
            append_resume_flag(&with_continue, "--copy", &["--copy"])
        }
        _ => base,
    }
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
fn pin_client_exit(session_name: &str) {
    let _ = Command::new("tmux")
        .args(["set-option", "-t", session_name, "detach-on-destroy", "on"])
        .output();
}

/// Existence + the pinned-client flag of one session, in a single `tmux` call:
/// `Some(true)` = exists and already pins `detach-on-destroy on`, `Some(false)`
/// = exists but still inherits the global setting, `None` = no such session.
#[derive(Default)]
pub struct SessionInventory(std::sync::OnceLock<Option<std::collections::HashMap<String, bool>>>);

impl SessionInventory {
    fn state(&self, session_name: &str) -> Option<bool> {
        self.0.get_or_init(read_session_inventory).as_ref()?.get(session_name).copied()
    }
}

fn session_state(session_name: &str) -> Option<bool> {
    read_session_inventory()?.get(session_name).copied()
}

fn read_session_inventory() -> Option<std::collections::HashMap<String, bool>> {
    let out = Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_name}::#{detach-on-destroy}",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
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
    let session_name = unique_session_name();
    let cmd = resolve_command(agent_type, custom_command);
    let cwd = resolve_workspace_dir(workspace_dir);

    let _ = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session_name,
            "-c",
            &cwd,
            "-x",
            "120",
            "-y",
            "35",
            &cmd,
        ])
        .output();

    pin_client_exit(&session_name);

    (session_name, cmd)
}

/// True when a tmux session with this name already exists on the server.
pub fn session_exists(session_name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Generate a tmux session name that cannot collide with a live session.
///
/// The old scheme (`millis % 1_000_000`) wrapped every ~16 minutes AND
/// collided when two cards were created within the same millisecond: the
/// second `tmux new-session -d -s <dup>` silently failed and the new card
/// attached to the OLD session, so Ctrl+C / output in one card leaked into
/// the other. Full millis + pid + random suffix + existence check fixes it.
pub fn unique_session_name() -> String {
    for _ in 0..20 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        // Cheap randomness without new deps: nanos + pid mix.
        let nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let rand = (nano ^ (std::process::id() << 8)) % 46656;
        let candidate = format!("sd_term_{now}_{rand:04x}");
        if !session_exists(&candidate) {
            return candidate;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // Practically unreachable fallback.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("sd_term_{now}_{}", std::process::id())
}

pub fn kill_session(session_name: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session_name])
        .output();
}

#[allow(dead_code)]
pub fn ensure_session(
    session_name: &str,
    agent_type: &str,
    custom_command: Option<&str>,
    workspace_dir: Option<&str>,
) {
    ensure_session_with_agent_id(
        session_name,
        agent_type,
        custom_command,
        None,
        workspace_dir,
    )
}

/// Same as `ensure_session` but resumes a persisted per-card agent session
/// (opencode `--session <id>`) so rebooted cards stay isolated 1:1.
///
/// `workspace_dir` is the folder this card was created in (see
/// `TerminalData::workspace_dir`), not the top bar's current value: resume is
/// cwd-scoped for every harness, so a restored card must come back where it
/// was working.
pub fn ensure_session_with_agent_id(
    session_name: &str,
    agent_type: &str,
    custom_command: Option<&str>,
    agent_session_id: Option<&str>,
    workspace_dir: Option<&str>,
) {
    ensure_session_with_inventory(session_name, agent_type, custom_command, agent_session_id, workspace_dir, None);
}

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
        let cmd =
            resolve_resume_command_with_session(agent_type, custom_command, agent_session_id);
        let cwd = resolve_workspace_dir(workspace_dir);
        let _ = Command::new("tmux")
            .args([
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
                &cmd,
            ])
            .output();
    }

    pin_client_exit(session_name);
}

/// Type `text` into a live session (phone → harness), optionally followed by
/// Return.
///
/// `-l` sends the text literally, so quotes, pipes and globs reach the pane as
/// written instead of being interpreted by tmux, and `--` stops a message that
/// begins with a dash from being read as a flag.
pub fn send_keys(session_name: &str, text: &str, enter: bool) -> Result<(), String> {
    if !text.is_empty() {
        let out = Command::new("tmux")
            .args(["send-keys", "-t", session_name, "-l", "--", text])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
    }
    if enter {
        let out = Command::new("tmux")
            .args(["send-keys", "-t", session_name, "Enter"])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
    }
    Ok(())
}

/// True when tmux still knows this session.
pub fn session_alive(session_name: &str) -> bool {
    session_exists(session_name)
}

pub fn preview_from_screen(screen: &str, lines: usize) -> String {
    let mut tail: Vec<&str> = screen
        .lines()
        .rev()
        .skip_while(|line| line.trim().is_empty())
        .take(lines)
        .collect();
    if tail.is_empty() {
        return "Ready. Waiting for input...".to_string();
    }
    tail.reverse();
    tail.join("\n")
}

fn get_proc_comm(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn get_direct_children(pid: u32) -> Vec<u32> {
    let task_path = format!("/proc/{}/task/{}/children", pid, pid);
    if let Ok(content) = std::fs::read_to_string(&task_path) {
        return content
            .split_whitespace()
            .filter_map(|s| s.parse::<u32>().ok())
            .collect();
    }
    Vec::new()
}

fn is_transient_prompt_tool(comm: &str) -> bool {
    comm == "starship" || comm == "direnv" || comm == "oh-my-posh" || comm == "powerline"
}

fn has_active_subprocesses(pid_num: u32, is_shell_agent: bool) -> bool {
    let comm = get_proc_comm(pid_num);
    let is_shell_proc = comm == "bash" || comm == "zsh" || comm == "fish" || comm == "sh";
    let direct = get_direct_children(pid_num);

    if is_shell_agent {
        // Plain shell session: any non-transient child process means a foreground command is executing
        return direct
            .into_iter()
            .any(|p| !is_transient_prompt_tool(&get_proc_comm(p)));
    }

    // AI agent session
    if is_shell_proc {
        // The pane PID is tmux's shell wrapper launching the agent
        // The direct child is the agent process itself
        for child_pid in direct {
            let child_comm = get_proc_comm(child_pid);
            if is_transient_prompt_tool(&child_comm) {
                continue;
            }
            if child_comm != "bash" && child_comm != "zsh" && child_comm != "sh" {
                // This child is the agent. Check if the agent has spawned child tools
                if get_direct_children(child_pid)
                    .into_iter()
                    .any(|p| !is_transient_prompt_tool(&get_proc_comm(p)))
                {
                    return true;
                }
            } else if get_direct_children(child_pid)
                .into_iter()
                .any(|p| !is_transient_prompt_tool(&get_proc_comm(p)))
            {
                return true;
            }
        }
        false
    } else {
        // The pane PID is the agent itself directly
        direct
            .into_iter()
            .any(|p| !is_transient_prompt_tool(&get_proc_comm(p)))
    }
}

fn resolve_effective_pid(pid_num: u32, is_shell_agent: bool) -> u32 {
    if is_shell_agent {
        return pid_num;
    }
    let comm = get_proc_comm(pid_num);
    let is_shell_proc = comm == "bash" || comm == "zsh" || comm == "fish" || comm == "sh";
    if is_shell_proc {
        let direct = get_direct_children(pid_num);
        if let Some(&agent_pid) = direct.first() {
            return agent_pid;
        }
    }
    pid_num
}

#[allow(dead_code)]
pub struct SessionStatus {
    pub status: &'static str,
    pub label: &'static str,
    pub pid: String,
    pub cmd: String,
    /// Live working directory reported by tmux for the active pane.
    pub cwd: String,
}

pub fn inspect_status(session_name: &str, agent_type: &str) -> SessionStatus {
    inspect_status_impl(session_name, agent_type, None)
}

/// Reuse the capture already needed by the card's title and preview.
pub fn inspect_status_with_screen(
    session_name: &str,
    agent_type: &str,
    screen: &str,
) -> SessionStatus {
    inspect_status_impl(session_name, agent_type, Some(screen))
}

fn inspect_status_impl(
    session_name: &str,
    agent_type: &str,
    screen: Option<&str>,
) -> SessionStatus {
    if let Ok(output) = Command::new("tmux")
        .args([
            "list-panes",
            "-t",
            session_name,
            "-F",
            "#{pane_pid}::#{pane_current_command}::#{pane_dead}::#{pane_height}::#{pane_current_path}",
        ])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = text.lines().next() {
                let mut parts = first_line.splitn(5, "::");
                let pid = parts.next().unwrap_or("").trim().to_string();
                let cmd = parts.next().unwrap_or("").trim().to_string();
                let dead = parts.next().unwrap_or("0").trim();
                let height = parts
                    .next()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(24);
                let cwd = parts.next().unwrap_or("").trim().to_string();

                if dead == "1" || pid.is_empty() {
                    return SessionStatus {
                        status: "EXITED",
                        label: "○ EXITED",
                        pid,
                        cmd,
                        cwd,
                    };
                }

                let p_num = pid.parse::<u32>().unwrap_or(0);
                if p_num != 0 && !std::path::Path::new(&format!("/proc/{}", p_num)).exists() {
                    return SessionStatus {
                        status: "EXITED",
                        label: "○ EXITED",
                        pid,
                        cmd,
                        cwd,
                    };
                }

                let is_shell_agent =
                    agent_type == "shell" || agent_type == "bash" || agent_type == "terminal";
                let effective_pid = resolve_effective_pid(p_num, is_shell_agent);
                let display_pid = if effective_pid != 0 {
                    effective_pid.to_string()
                } else {
                    pid.clone()
                };

                let display_cmd = if cmd.is_empty() {
                    agent_type.to_string()
                } else {
                    cmd.clone()
                };

                // Check 1: Does the process have active child subprocesses running?
                // E.g. running a build, executing a tool, subshell command
                if p_num != 0 && has_active_subprocesses(p_num, is_shell_agent) {
                    return SessionStatus {
                        status: "WORKING",
                        label: "● WORKING",
                        pid: display_pid,
                        cmd: display_cmd,
                        cwd,
                    };
                }

                // Check 2: Shell session foreground command check
                let is_shell_cmd = cmd == "bash" || cmd == "zsh" || cmd == "fish" || cmd == "sh";
                if is_shell_agent && !is_shell_cmd && !cmd.is_empty() {
                    return SessionStatus {
                        status: "WORKING",
                        label: "● WORKING",
                        pid: display_pid,
                        cmd: display_cmd,
                        cwd,
                    };
                }

                // Check 3: Screen capture of bottom lines for spinners, cancel hints, or status words
                let captured = screen.map(std::borrow::Cow::Borrowed).or_else(|| {
                    let output = Command::new("tmux")
                        .args(["capture-pane", "-p", "-t", session_name, "-S", "-15"])
                        .output()
                        .ok()?;
                    output.status.success().then(|| {
                        std::borrow::Cow::Owned(
                            String::from_utf8_lossy(&output.stdout).into_owned(),
                        )
                    })
                });
                if let Some(cap_text) = captured {
                    for line in recent_status_lines(&cap_text, height) {
                        // Check for Braille spinner characters (U+2801 to U+28FF)
                        let has_braille =
                            line.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c));
                        if has_braille {
                            return SessionStatus {
                                status: "WORKING",
                                label: "● WORKING",
                                pid: display_pid,
                                cmd: display_cmd,
                                cwd,
                            };
                        }

                        let line_lower = line.to_lowercase();

                        // Check for interrupt hints
                        if line_lower.contains("esc to cancel")
                            || line_lower.contains("esc to interrupt")
                            || line_lower.contains("ctrl+c to cancel")
                            || line_lower.contains("ctrl+c to interrupt")
                            || line_lower.contains("press esc to stop")
                            || line_lower.contains("press ctrl-c to stop")
                            || line_lower.contains("to interrupt")
                            || line_lower.contains("to cancel")
                        {
                            return SessionStatus {
                                status: "WORKING",
                                label: "● WORKING",
                                pid: display_pid,
                                cmd: display_cmd,
                                cwd,
                            };
                        }

                        // Check for active progress words
                        if line_lower.contains("thinking...")
                            || line_lower.contains("thinking…")
                            || line_lower.contains("generating...")
                            || line_lower.contains("generating…")
                            || line_lower.contains("streaming...")
                            || line_lower.contains("streaming…")
                            || line_lower.contains("working...")
                            || line_lower.contains("working…")
                            || line_lower.contains("building...")
                            || line_lower.contains("building…")
                            || line_lower.contains("compiling...")
                            || line_lower.contains("compiling…")
                            || line_lower.contains("editing files...")
                            || line_lower.contains("editing files…")
                            || line_lower.contains("editing...")
                            || line_lower.contains("editing…")
                            || line_lower.contains("running...")
                            || line_lower.contains("running…")
                            || line_lower.contains("calling tool")
                            || line_lower.contains("running tool")
                            || line_lower.contains("analyzing...")
                            || line_lower.contains("analyzing…")
                        {
                            return SessionStatus {
                                status: "WORKING",
                                label: "● WORKING",
                                pid: display_pid,
                                cmd: display_cmd,
                                cwd,
                            };
                        }
                    }
                }

                return SessionStatus {
                    status: "IDLE",
                    label: "● IDLE",
                    pid: display_pid,
                    cmd: display_cmd,
                    cwd,
                };
            }
        }
    }

    SessionStatus {
        status: "EXITED",
        label: "○ EXITED",
        pid: "-".to_string(),
        cmd: agent_type.to_string(),
        cwd: String::new(),
    }
}

/// Match capture-pane -S -15: visible rows plus 15 rows of history. Limiting
/// before skipping blank rows prevents old scrollback spinners marking an
/// idle card busy when we reuse the deeper title capture.
fn recent_status_lines(screen: &str, height: usize) -> impl Iterator<Item = &str> {
    screen
        .lines()
        .rev()
        .take(height.saturating_add(15))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(8)
}

pub fn tmux_bin() -> String {
    // In-process lookup: this runs once per card at window build.
    which("tmux").unwrap_or_else(|| "tmux".to_string())
}

/// Max characters of the last prompt shown in the terminal card title.
/// Long enough to fill the whole header; the label ellipsizes the visual
/// overflow, the full text stays in the tooltip.
pub const PROMPT_TITLE_MAX_CHARS: usize = 120;

/// Capture the visible text of a tmux pane (TUI apps like opencode run
/// fullscreen, so this is the current screen, not scrollback history).
pub fn capture_pane_text(session_name: &str) -> Option<String> {
    capture_pane(session_name, false)
}

/// Capture tmux's real terminal styling as ANSI SGR sequences. The bridge
/// sends this beside its plain-text fallback so capable clients can reproduce
/// the agent's colours without changing the desktop card path.
#[cfg(test)]
pub fn capture_pane_ansi(session_name: &str) -> Option<String> {
    capture_pane(session_name, true)
}

fn capture_pane(session_name: &str, ansi: bool) -> Option<String> {
    let mut command = Command::new("tmux");
    command.arg("capture-pane");
    if ansi {
        command.arg("-e");
    }
    let output = command
        .args(["-p", "-t", session_name, "-S", "-300"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        return None;
    }
    Some(text)
}

/// Remove terminal control sequences while preserving the visible text.
/// Handles CSI/OSC/DCS strings as well as two-byte ESC commands; tmux `-e`
/// currently emits SGR CSI sequences, while the wider handling keeps the
/// plain fallback safe if tmux expands what it preserves later.
pub fn strip_terminal_escapes(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x1b if i + 1 < bytes.len() => {
                i += 1;
                match bytes[i] {
                    b'[' => {
                        i += 1;
                        while i < bytes.len() {
                            let byte = bytes[i];
                            i += 1;
                            if (0x40..=0x7e).contains(&byte) {
                                break;
                            }
                        }
                    }
                    b']' | b'P' | b'^' | b'_' => {
                        i += 1;
                        while i < bytes.len() {
                            if bytes[i] == 0x07 {
                                i += 1;
                                break;
                            }
                            if bytes[i] == 0x1b
                                && i + 1 < bytes.len()
                                && bytes[i + 1] == b'\\'
                            {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    _ => i += 1,
                }
            }
            0x1b => i += 1,
            byte if byte < 0x20 && !matches!(byte, b'\n' | b'\r' | b'\t') => i += 1,
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Capture the last user prompt from a tmux session and return a short
/// title-friendly snippet (first 10-30 chars of the prompt).
///
/// History heuristic for plain shells / simple TUIs: scans `capture-pane`
/// bottom-up looking for prompt markers (`>`, `❯`, `›`, `➜`, `$`, `#`,
/// `?`, `»`) on NON-bordered lines (e.g. `~ ❯ cmd`).
/// Bordered lines (`┃ …`, `│ …`) belong to fullscreen-TUI tool/output areas
/// (e.g. opencode renders agent tool calls as `┃  $ <cmd>`) and are NEVER
/// treated as user prompts — that showed agent output as the title.
/// Returns `None` when nothing prompt-like is found so callers keep the
/// default `icon + agent name` title.
#[allow(dead_code)]
pub fn get_last_prompt(session_name: &str) -> Option<String> {
    let text = capture_pane_text(session_name)?;
    extract_last_prompt(&text).map(|s| truncate_prompt_title(&s))
}

/// Capture the in-progress composer draft (text the user typed into the
/// prompt box but has not submitted yet) from an opencode-style TUI screen.
#[allow(dead_code)]
pub fn get_composer_draft(session_name: &str) -> Option<String> {
    let text = capture_pane_text(session_name)?;
    extract_composer_draft(&text)
}

/// Pure helper: extract last prompt from captured pane text (testable).
pub fn extract_last_prompt(captured: &str) -> Option<String> {
    for raw_line in captured.lines().rev() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        // Skip spinner / progress UI lines - those are agent output, not prompts.
        if line.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)) {
            continue;
        }
        let lower = line.to_lowercase();
        if lower.contains("thinking")
            || lower.contains("generating")
            || lower.contains("streaming")
            || lower.contains("working...")
            || lower.contains("working…")
            || lower.contains("esc to ")
            || lower.contains("ctrl+c to ")
            || lower.contains("ctrl-c to ")
            || lower.contains("to interrupt")
            || lower.contains("to cancel")
            || lower.contains("press esc")
        {
            continue;
        }
        // Skip pure box-drawing borders.
        let stripped_boxes: String = line
            .chars()
            .filter(|c| !"─│╭╮╰╯┌┐└┘┃━┃".contains(*c) && !c.is_whitespace())
            .collect();
        if stripped_boxes.is_empty() {
            continue;
        }

        if let Some(candidate) = prompt_text_from_line(line) {
            let cleaned: String = candidate
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if cleaned.chars().count() >= 2 {
                return Some(cleaned);
            }
        }
    }
    None
}

/// Extract user-typed text from a single pane line, if it looks like a prompt.
/// Returns None for plain agent output to avoid false-positive titles.
///
/// IMPORTANT: lines starting with TUI border glyphs (`┃`, `│`) are agent
/// tool/output areas in fullscreen TUIs (opencode shows tool calls as
/// `┃  $ <cmd>`) and are always rejected here — even when they contain
/// `$`/`#`/`>` markers. Composer drafts are handled separately by
/// `extract_composer_draft`, which is position-aware.
fn prompt_text_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    // Bordered line = TUI-managed area, never a shell prompt.
    if trimmed.starts_with(['┃', '│']) {
        return None;
    }
    let no_border = trimmed.trim_start_matches(['|', '╭', '╰', '├', '└', '─', ' ']);

    // Shell-style: text AFTER the last prompt glyph on the line ("~ ❯ cmd").
    for marker in ['❯', '➜', '$', '#', '»'] {
        if let Some(idx) = no_border.rfind(marker) {
            let after: String = no_border[idx + marker.len_utf8()..].trim().to_string();
            // Marker at end of line with nothing after = bare prompt, not a prompt WITH input.
            if !after.is_empty() {
                return Some(after);
            }
            return None;
        }
    }

    // TUI-style: line STARTS with prompt glyph ("> hello", "› hello", "? hello").
    let mut chars = no_border.chars();
    let first = chars.next()?;
    if matches!(first, '>' | '›' | '❯' | '?' | '»') {
        let rest: String = chars.collect::<String>().trim().to_string();
        if !rest.is_empty() {
            return Some(rest);
        }
    }
    None
}

/// Truncate a prompt to the title-friendly 10-30 char range.
/// Always caps at 30 chars (27 + "…"); shorter prompts are returned whole.
pub fn truncate_prompt_title(prompt: &str) -> String {
    let cleaned: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = cleaned.chars().count();
    if count <= PROMPT_TITLE_MAX_CHARS {
        return cleaned;
    }
    let truncated: String = cleaned.chars().take(PROMPT_TITLE_MAX_CHARS - 1).collect();
    format!("{truncated}…")
}

/// Pure helper: extract the in-progress composer draft from an opencode-style
/// TUI screen (testable).
///
/// The composer is the bordered box directly above the `╹` divider at the
/// bottom of the screen. Its text is literally what the user entered into
/// the prompt box. Skips the empty state, the `Ask anything…` placeholder
/// and the `Build … · …` model line. Returns the draft truncated to title
/// length, or None when the composer is empty.
pub fn extract_composer_draft(captured: &str) -> Option<String> {
    let lines: Vec<&str> = captured.lines().collect();
    let div_idx = lines.iter().rposition(|l| l.contains('╹'))?;
    let start = div_idx.saturating_sub(7);
    let mut draft_parts: Vec<String> = Vec::new();
    for line in &lines[start..div_idx] {
        let trimmed = line.trim();
        // Only bordered lines belong to the composer box interior. This
        // excludes the `▣ Build · …` activity status line sitting right
        // above the box, which would otherwise leak agent/model info
        // ("Build · Muse Spark …") into the title.
        if !trimmed.starts_with(['┃', '│']) {
            continue;
        }
        let t = trimmed.trim_start_matches(['┃', '│', '|', ' ']).trim();
        if t.is_empty() {
            continue;
        }
        // Placeholder shown when nothing is typed yet.
        if t.starts_with("Ask anything") {
            continue;
        }
        // Model selector line inside the composer box ("Build · …", "Build auto · …").
        // Requires the `·` so a real draft starting with "Build …" still matches.
        if t.starts_with("Build") && t.contains('·') {
            continue;
        }
        // Tool-call markers should never appear in the composer, but guard anyway.
        if t.starts_with('$') || t.starts_with('#') {
            continue;
        }
        draft_parts.push(t.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    if draft_parts.is_empty() {
        return None;
    }
    let draft = draft_parts.join(" ");
    if draft.chars().count() >= 2 {
        Some(truncate_prompt_title(&draft))
    } else {
        None
    }
}

fn tmux_display(session_name: &str, format: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "-t", session_name, format])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

fn opencode_db_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })?;
    let db = base.join("opencode/opencode.db");
    if db.exists() {
        Some(db)
    } else {
        None
    }
}

fn sqlite_query(db: &std::path::Path, sql: &str) -> Option<String> {
    let output = Command::new("sqlite3")
        .args([
            "-readonly",
            "-noheader",
            "-list",
            db.to_str()?,
            sql,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Read `/proc/<pid>/cmdline` as a space-joined string (`None` when the
/// process is gone or unreadable).
fn read_cmdline(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.iter()
            .map(|b| if *b == 0 { ' ' } else { *b as char })
            .collect(),
    )
}

/// Pure helper: pull `--session <id>` / `--session=<id>` out of a cmdline
/// string (testable).
pub fn extract_session_flag(cmdline: &str) -> Option<String> {
    let mut words = cmdline.split_whitespace();
    while let Some(w) = words.next() {
        if w == "--session" {
            return words.next().filter(|s| !s.is_empty()).map(|s| s.to_string());
        }
        if let Some(id) = w.strip_prefix("--session=") {
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Deterministic per-pane opencode session id: walk the pane's process tree
/// looking for an opencode process launched with `--session <id>`.
///
/// Reboot-resumed cards are relaunched exactly that way (see
/// `resolve_resume_command_with_session`), so this is ground truth for the
/// prompt typed INTO this pane's harness — it can never shift onto a
/// neighbour's session when other consoles open or close, unlike
/// chronological matching.
pub fn pane_opencode_session_flag(pane_pid: u32) -> Option<String> {
    if pane_pid == 0 {
        return None;
    }
    let mut stack = vec![pane_pid];
    let mut seen = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(cmd) = read_cmdline(pid) {
            let base = cmd
                .split_whitespace()
                .next()
                .and_then(|s| s.rsplit('/').next())
                .unwrap_or("");
            if base == "opencode" {
                if let Some(id) = extract_session_flag(&cmd) {
                    return Some(id);
                }
            }
        }
        stack.extend(get_direct_children(pid));
    }
    None
}

/// `session_name -> (agent_type, persisted agent_session_id)` for every card
/// in `state.json`. Lets the opencode matcher tell harness panes apart
/// without depending on process state.
fn state_terminal_info() -> std::collections::HashMap<String, (String, Option<String>)> {
    let mut out = std::collections::HashMap::new();
    let path = crate::state::get_state_path();
    let content = std::fs::read_to_string(path).unwrap_or_default();
    if content.is_empty() {
        return out;
    }
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return out,
    };
    if let Some(terms) = v.get("terminals").and_then(|t| t.as_array()) {
        for t in terms {
            let name = t
                .get("session_name")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let agent = t
                .get("agent_type")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let sid = t
                .get("agent_session_id")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            out.insert(name, (agent, sid));
        }
    }
    out
}

/// Live `sd_term_*` panes that may own an opencode session: only panes whose
/// card is an opencode harness participate. Shell/reasonix/… cards must never
/// consume an opencode session in the chronological assignment — that stole a
/// neighbour's session and surfaced its prompt in the wrong card's title.
/// Panes unknown to `state.json` are kept (nothing proves they are not
/// opencode).
fn opencode_panes_live() -> Option<Vec<(String, i64)>> {
    let list_out = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}|#{session_created}"])
        .output()
        .ok()?;
    if !list_out.status.success() {
        return None;
    }
    let panes: Vec<(String, i64)> = String::from_utf8_lossy(&list_out.stdout)
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(2, '|');
            let name = p.next()?.to_string();
            let t: i64 = p.next()?.parse().ok()?;
            if name.starts_with("sd_term_") {
                Some((name, t))
            } else {
                None
            }
        })
        .collect();
    let mut panes = keep_opencode_panes(&panes, &state_terminal_info());
    panes.sort_by_key(|(_, t)| *t);
    Some(panes)
}

/// All same-directory opencode sessions born at/after `not_before` (seconds),
/// oldest first.
fn opencode_sessions_since(cwd: &str, not_before: i64) -> Option<Vec<(String, i64)>> {
    let db = opencode_db_path()?;
    let sql = format!(
        "SELECT id, time_created FROM session WHERE directory = '{}' AND time_created/1000 >= {} ORDER BY time_created;",
        sql_escape(cwd),
        not_before
    );
    let rows = sqlite_query(&db, &sql)?;
    let mut sessions: Vec<(String, i64)> = rows
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(2, '|');
            let id = p.next()?.to_string();
            let t_ms: i64 = p.next()?.parse().ok()?;
            Some((id, t_ms / 1000))
        })
        .collect();
    sessions.sort_by_key(|(_, t)| *t);
    Some(sessions)
}

/// `session_name -> pane_pid` for every live pane, in a single `tmux` call.
fn live_pane_pids() -> Option<std::collections::HashMap<String, u32>> {
    let out = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{session_name} #{pane_pid}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut map = std::collections::HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut p = line.split_whitespace();
        if let (Some(name), Some(pid)) = (p.next(), p.next()) {
            if name.starts_with("sd_term_") {
                if let Ok(pid) = pid.parse::<u32>() {
                    map.insert(name.to_string(), pid);
                }
            }
        }
    }
    Some(map)
}

/// Deterministic ownership map: `tmux pane -> opencode session id` for panes
/// whose own process tree carries `--session <id>` (reboot-resumed cards are
/// launched exactly that way). Ground truth that never shifts when other
/// consoles open or close.
fn flag_claims(pids: &std::collections::HashMap<String, u32>) -> std::collections::HashMap<String, String> {
    pids.iter()
        .filter_map(|(name, pid)| {
            pane_opencode_session_flag(*pid).map(|id| (name.clone(), id))
        })
        .collect()
}

/// Pure helper: drop panes whose session is deterministically known (they are
/// satisfied and must not shift the chronological ranking of the rest), plus
/// their owned sessions from the candidates.
pub fn drop_determined_panes(
    panes: &[(String, i64)],
    sessions: &[(String, i64)],
    flags: &std::collections::HashMap<String, String>,
) -> (Vec<(String, i64)>, Vec<(String, i64)>) {
    let owned: std::collections::HashSet<&str> =
        flags.values().map(|s| s.as_str()).collect();
    let free_panes = panes
        .iter()
        .filter(|(name, _)| !flags.contains_key(name))
        .cloned()
        .collect();
    let free_sessions = sessions
        .iter()
        .filter(|(id, _)| !owned.contains(id.as_str()))
        .cloned()
        .collect();
    (free_panes, free_sessions)
}

/// Pure helper: keep only panes that may own an opencode session — panes
/// whose card is an opencode harness, plus panes unknown to `state.json`
/// (nothing proves they are not opencode). Shell/reasonix/… cards must never
/// consume an opencode session in the chronological assignment.
pub fn keep_opencode_panes(
    panes: &[(String, i64)],
    info: &std::collections::HashMap<String, (String, Option<String>)>,
) -> Vec<(String, i64)> {
    panes
        .iter()
        .filter(|(name, _)| {
            info.get(name)
                .map(|(agent, _)| agent == "opencode")
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// Pure heal policy for `resolve_own_opencode_id`: adopt heuristic match `q`
/// (born `q_birth`) over persisted `p` (born `p_birth`, if known) for a pane
/// born at `created` only when `q` was born strictly nearer to the pane —
/// the signature of a stale guess made before a neighbouring console closed.
/// A persisted id whose birth is unknown (pre-reboot ground truth outside
/// the live window, or garbage) is always kept: its DB lookup either hits
/// the card's own old session or yields nothing (and the title falls back to
/// the pane's own text — never another console's prompt).
pub fn should_adopt_heuristic_match(
    p_birth: Option<i64>,
    q_birth: i64,
    created: i64,
) -> bool {
    match p_birth {
        Some(pb) => (q_birth - created).abs() < (pb - created).abs(),
        None => false,
    }
}

/// Heuristic match for a pane with no deterministic id: chronological 1:1
/// assignment over live OPENCODE panes, minus panes/sessions deterministically
/// owned via `--session` flags. Returns the matched id plus its birth
/// (seconds) for the heal policy in `resolve_own_opencode_id`.
fn heuristic_opencode_match(
    session_name: &str,
    flags: &std::collections::HashMap<String, String>,
) -> Option<(String, i64)> {
    let cwd = tmux_display(session_name, "#{pane_current_path}")?;
    let created: i64 = tmux_display(session_name, "#{session_created}")?
        .parse()
        .ok()?;
    let panes = opencode_panes_live()?;
    if !panes.iter().any(|(n, _)| n == session_name) {
        return None;
    }
    let first = panes.iter().map(|(_, t)| *t).min().unwrap_or(created);
    let sessions = opencode_sessions_since(&cwd, first - 30)?;
    let (free_panes, free_sessions) = drop_determined_panes(&panes, &sessions, flags);

    assign_opencode_sessions(&free_panes, &free_sessions)
        .into_iter()
        .find(|(pane, _)| pane == session_name)
        .and_then(|(_, sess)| sess)
        .filter(|(id, t)| {
            !id.is_empty() && (*t - created).abs() <= 600 && *t >= created - 30
        })
}

/// Birth (seconds) of one opencode session id, or `None` when it is not in
/// the local store (pre-reboot ground truth pruned from the query window, or
/// garbage that can safely never match).
fn opencode_session_birth(session_id: &str) -> Option<i64> {
    let db = opencode_db_path()?;
    let sql = format!(
        "SELECT time_created FROM session WHERE id = '{}' LIMIT 1;",
        sql_escape(session_id)
    );
    let row = sqlite_query(&db, &sql)?;
    row.parse::<i64>().ok().map(|ms| ms / 1000)
}

/// Resolve the opencode session id OWNED by this tmux pane — i.e. the session
/// behind the prompt typed INTO this card's harness, never a neighbour's.
///
/// Layers, most-trusted first:
/// 1. `--session <id>` in the pane's own process tree (reboot-resumed cards
///    are launched exactly that way) — deterministic ground truth.
/// 2. Claims-aware chronological match over live OPENCODE panes (flag-owned
///    pane/session pairs leave the ranking entirely; other harness types
///    never participate).
/// 3. `persisted` fallback (pre-reboot ground truth older than the live
///    window).
///
/// Heal policy when the heuristic `q` disagrees with `persisted` `p`: adopt
/// `q` only when `q` was born strictly nearer to this pane than `p` was —
/// the signature of a stale guess made before a neighbouring console closed
/// and shifted the ranking. Otherwise keep `p`: it is either ground truth
/// older than any live session (reboot resume — a newer `q` there belongs to
/// somebody else) or an id whose DB lookup yields nothing, in which case the
/// title safely falls back to this pane's own text.
pub fn resolve_own_opencode_id(
    session_name: &str,
    persisted: Option<&str>,
) -> Option<String> {
    let flags = flag_claims(&live_pane_pids().unwrap_or_default());
    if let Some(id) = flags.get(session_name) {
        return Some(id.clone());
    }
    let clean_persisted = persisted.map(str::trim).filter(|s| !s.is_empty());
    let q = heuristic_opencode_match(session_name, &flags);
    match (clean_persisted, q) {
        (_, None) => clean_persisted.map(str::to_string),
        (None, Some((id, _))) => Some(id),
        (Some(p), Some((qid, _))) if qid == p => Some(p.to_string()),
        (Some(p), Some((qid, qb))) => {
            let created: i64 = tmux_display(session_name, "#{session_created}")?
                .parse()
                .ok()?;
            let pb = opencode_session_birth(p)?;
            if should_adopt_heuristic_match(Some(pb), qb, created) {
                Some(qid)
            } else {
                Some(p.to_string())
            }
        }
    }
}

/// Map a super-desktop tmux session to its opencode session id.
///
/// super-desktop launches the agent command at card creation, so the
/// opencode session is born shortly AFTER the tmux session (TUI + provider
/// init lag is typically 10-60s). Same-directory opencode cards are matched
/// chronologically 1:1 (`assign_opencode_sessions`). Tolerance 10 min.
/// Returns None when opencode storage is unavailable or nothing matches.
///
/// Only live OPENCODE panes participate (other harness types never consume a
/// session), and pane/session pairs deterministically owned via `--session`
/// flags leave the ranking entirely — otherwise closing one console shifted
/// every surviving card onto its neighbour's session and its prompt showed
/// in the wrong title.
/// Prefer `resolve_own_opencode_id` when the card's persisted id is known: it
/// adds the deterministic `--session` fast path plus a heal policy for stale
/// guesses.
///
/// NOTE: `/new` inside opencode (or restarting the agent in the same pane),
/// and sessions left behind by deleted cards, can skew the ranking; the
/// live composer draft still works in those cases.
pub fn get_opencode_session_id(session_name: &str) -> Option<String> {
    resolve_own_opencode_id(session_name, None)
}

/// Pure helper: chronological 1:1 assignment of tmux panes to opencode
/// sessions (both sorted oldest-first). Each pane claims the earliest
/// still-unclaimed session born at/after the pane (30s grace for clock
/// skew). Panes with no session left get None.
pub fn assign_opencode_sessions(
    panes: &[(String, i64)],
    sessions: &[(String, i64)],
) -> Vec<(String, Option<(String, i64)>)> {
    let mut used = vec![false; sessions.len()];
    panes
        .iter()
        .map(|(pane, created)| {
            let mut match_idx: Option<usize> = None;
            for (i, (_, s_created)) in sessions.iter().enumerate() {
                if !used[i] && *s_created >= *created - 30 {
                    match_idx = Some(i);
                    break;
                }
            }
            if let Some(i) = match_idx {
                used[i] = true;
                (pane.clone(), Some(sessions[i].clone()))
            } else {
                (pane.clone(), None)
            }
        })
        .collect()
}

/// Pure helper: pull the `text` field out of an opencode `part` JSON blob.
pub fn extract_text_from_part_json(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.get("text")?.as_str().map(|s| s.to_string())
}

/// Return the last message the USER submitted to an opencode session as a
/// title-friendly snippet — i.e. exactly what was entered, even after it
/// scrolled off the TUI screen. Reads opencode's local session database;
/// falls back to None when unavailable.
#[allow(dead_code)]
pub fn get_opencode_last_user_text(session_name: &str) -> Option<String> {
    let sess_id = get_opencode_session_id(session_name)?;
    get_opencode_user_text_by_id(&sess_id)
}

/// Same as above but for an already-resolved opencode session id (lets
/// callers cache the tmux-pane → opencode-session mapping).
pub fn get_opencode_user_text_by_id(opencode_session_id: &str) -> Option<String> {
    let db = opencode_db_path()?;
    let sql = format!(
        "SELECT p.data FROM part p JOIN message m ON m.id = p.message_id WHERE p.session_id = '{}' AND m.data LIKE '%\"role\":\"user\"%' AND p.data LIKE '%\"type\":\"text\"%' ORDER BY p.time_created DESC LIMIT 1;",
        sql_escape(opencode_session_id)
    );
    let json = sqlite_query(&db, &sql)?;
    let text = extract_text_from_part_json(&json)?;
    let cleaned: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() >= 2 {
        Some(truncate_prompt_title(&cleaned))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_capture_keeps_status_within_the_original_history_window() {
        let screen = format!("old thinking...\n{}ready\n", "\n".repeat(40));
        assert_eq!(recent_status_lines(&screen, 24).collect::<Vec<_>>(), ["ready"]);
        let screen = (0..40).map(|n| format!("line {n}\n")).collect::<String>();
        assert_eq!(recent_status_lines(&screen, 24).collect::<Vec<_>>(),
            ["line 39", "line 38", "line 37", "line 36", "line 35", "line 34", "line 33", "line 32"]);
    }

    #[test]
    fn shared_capture_preview_keeps_internal_blank_lines_and_unicode() {
        assert_eq!(preview_from_screen("old\nfirst\n\n🦀 last\n \n\n", 3), "first\n\n🦀 last");
        assert_eq!(preview_from_screen("\n \n", 8), "Ready. Waiting for input...");
        assert_eq!(preview_from_screen("one\ntwo", 1), "two");
    }

    #[test]
    fn terminal_escape_stripping_preserves_only_visible_text() {
        let styled = concat!(
            "\x1b[1mBold\x1b[0m ",
            "\x1b[38;2;137;180;250mblue\x1b[39m\n",
            "\x1b]0;hidden title\x07next\tline\x1bPignored\x1b\\!"
        );
        assert_eq!(strip_terminal_escapes(styled), "Bold blue\nnext\tline!");
    }

    #[test]
    fn test_nonexistent_session_exited() {
        let status = inspect_status("nonexistent_session_xyz_999", "agy");
        assert_eq!(status.status, "EXITED");
        assert_eq!(status.label, "○ EXITED");
    }

    #[test]
    fn test_braille_spinner_range() {
        let spinners = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];
        for s in spinners {
            assert!(s >= '\u{2801}' && s <= '\u{28FF}', "Character {} should be in braille range", s);
        }
    }

    #[test]
    fn test_shell_idle_and_working() {
        let sess = "test_sd_tmux_idle_test_session";
        let _ = Command::new("tmux").args(["kill-session", "-t", sess]).output();
        let _ = Command::new("tmux").args(["new-session", "-d", "-s", sess, "bash", "--norc", "--noprofile"]).output();

        let mut status_idle = inspect_status(sess, "bash");
        for _ in 0..20 {
            if status_idle.status == "IDLE" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            status_idle = inspect_status(sess, "bash");
        }
        assert_eq!(status_idle.status, "IDLE", "Shell at prompt should be IDLE");
        assert!(
            std::path::Path::new(&status_idle.cwd).is_dir(),
            "tmux must expose its live pane cwd, got {:?}",
            status_idle.cwd
        );
        let screen = capture_pane_text(sess).unwrap_or_default();
        assert_eq!(inspect_status_with_screen(sess, "bash", &screen).status, status_idle.status);

        // Send a sleep command
        let _ = Command::new("tmux").args(["send-keys", "-t", sess, "sleep 1.5", "Enter"]).output();
        std::thread::sleep(std::time::Duration::from_millis(300));

        let status_busy = inspect_status(sess, "bash");
        assert_eq!(status_busy.status, "WORKING", "Shell executing sleep should be WORKING");
        let screen = capture_pane_text(sess).unwrap_or_default();
        assert_eq!(inspect_status_with_screen(sess, "bash", &screen).status, status_busy.status);

        // Wait for sleep to complete
        let mut status_done = inspect_status(sess, "bash");
        for _ in 0..30 {
            if status_done.status == "IDLE" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            status_done = inspect_status(sess, "bash");
        }
        assert_eq!(status_done.status, "IDLE", "Shell after command completes should return to IDLE");

        let _ = Command::new("tmux").args(["kill-session", "-t", sess]).output();
    }

    #[test]
    fn test_agent_idle_detection() {
        if which("agy").is_some() {
            let sess = "test_sd_tmux_agy_idle_test";
            let _ = Command::new("tmux").args(["kill-session", "-t", sess]).output();
            let _ = Command::new("tmux").args(["new-session", "-d", "-s", sess, "agy"]).output();

            // Wait for agent to finish initialization and arrive at the prompt
            let mut idle = false;
            for _ in 0..15 {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let status = inspect_status(sess, "agy");
                if status.status == "IDLE" {
                    assert_eq!(status.label, "● IDLE");
                    idle = true;
                    break;
                }
            }

            assert!(idle, "AI agent waiting at prompt must be detected as IDLE");
            let _ = Command::new("tmux").args(["kill-session", "-t", sess]).output();
        }
    }

    #[test]
    fn test_resolve_command_ai_agent_arguments() {
        // Antigravity
        let agy_cmd = resolve_command("antigravity", None);
        assert!(agy_cmd.ends_with("--dangerously-skip-permissions"), "Antigravity must include --dangerously-skip-permissions, got: {}", agy_cmd);

        // Claude
        let claude_cmd = resolve_command("claude", None);
        assert!(claude_cmd.ends_with("--dangerously-skip-permissions"), "Claude must include --dangerously-skip-permissions, got: {}", claude_cmd);

        // Codex
        let codex_cmd = resolve_command("codex", None);
        assert!(codex_cmd.ends_with("--dangerously-bypass-approvals-and-sandbox"), "Codex must include --dangerously-bypass-approvals-and-sandbox, got: {}", codex_cmd);

        // OpenCode
        let opencode_cmd = resolve_command("opencode", None);
        assert!(opencode_cmd.ends_with("--auto"), "OpenCode must include --auto, got: {}", opencode_cmd);

        // Grok
        let grok_cmd = resolve_command("grok", None);
        assert!(grok_cmd.ends_with("--dangerously-skip-permissions"), "Grok must include --dangerously-skip-permissions, got: {}", grok_cmd);

        // Shell (must not have AI permission flags)
        let shell_cmd = resolve_command("shell", None);
        assert!(!shell_cmd.contains("--dangerously-skip-permissions"));
        assert!(!shell_cmd.contains("--auto"));

        // Custom command for AI agent without flags gets flags appended
        let custom_agy = resolve_command("antigravity", Some("/custom/bin/agy"));
        assert_eq!(custom_agy, "/custom/bin/agy --dangerously-skip-permissions");

        // Custom command that already has flag does not duplicate it
        let custom_already = resolve_command("antigravity", Some("/custom/bin/agy --dangerously-skip-permissions"));
        assert_eq!(custom_already, "/custom/bin/agy --dangerously-skip-permissions");

        // Custom shell does not get AI flags
        let custom_bash = resolve_command("antigravity", Some("/bin/bash"));
        assert_eq!(custom_bash, "/bin/bash");
    }

    /// The one `tmux` call that replaced `session_exists` + a separate
    /// `set-option` probe on the window-build path.
    #[test]
    fn test_session_state_reports_existence_and_pin_in_one_call() {
        let has_tmux = Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !has_tmux {
            return;
        }
        let sess = "test_sd_session_state_probe";
        let _ = Command::new("tmux").args(["kill-session", "-t", sess]).output();
        let _ = Command::new("tmux")
            .args(["new-session", "-d", "-s", sess, "-c", "/tmp", "/usr/bin/bash"])
            .output();

        assert_eq!(session_state("test_sd_session_state_missing"), None);
        // Inherits the server config until pinned.
        let _ = Command::new("tmux")
            .args(["set-option", "-t", sess, "detach-on-destroy", "off"])
            .output();
        assert_eq!(session_state(sess), Some(false));
        pin_client_exit(sess);
        assert_eq!(session_state(sess), Some(true));

        let _ = Command::new("tmux").args(["kill-session", "-t", sess]).output();
        assert_eq!(session_state(sess), None);
    }

    #[test]
    fn restoration_inventory_is_shared_and_not_a_global_cache() {
        let inventory = SessionInventory::default();
        inventory.0.set(Some(std::collections::HashMap::from([
            ("test_a".to_string(), true), ("test_b".to_string(), false),
        ]))).unwrap();
        assert_eq!(inventory.state("test_a"), Some(true));
        assert_eq!(inventory.state("test_b"), Some(false));
        assert_eq!(inventory.state("missing"), None);
        assert!(SessionInventory::default().0.get().is_none());
    }

    #[test]
    fn test_which_lookup_is_in_process_and_needs_an_executable_bit() {
        let dir = std::env::temp_dir().join(format!("sd-which-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let file = dir.join("sd_fake_harness");
        std::fs::write(&file, "#!/bin/sh\n").expect("write fake harness");

        let path_var = dir.as_os_str();
        assert_eq!(
            which_in_path("sd_fake_harness", path_var),
            None,
            "a non-executable file must not resolve"
        );

        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
                .expect("chmod +x");
        }
        assert_eq!(
            which_in_path("sd_fake_harness", path_var).as_deref(),
            file.to_str(),
            "an executable on PATH must resolve to its absolute path"
        );
        assert_eq!(which_in_path("sd_not_installed_xyz", path_var), None);
        assert_eq!(which("/definitely/not/here"), None);
        assert!(which(file.to_str().unwrap()).is_some(), "explicit paths are checked as-is");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_reasonix_command_uses_code_and_no_permission_flag() {
        // Reasonix keeps its own `workspace-write` sandbox, so — unlike every
        // other AI harness here — no permission flag may be appended.
        let custom = resolve_command("reasonix", Some("/custom/bin/reasonix"));
        assert_eq!(custom, "/custom/bin/reasonix code");

        // A caller-supplied command that already names `code` is left alone.
        assert_eq!(
            resolve_command("reasonix", Some("npx -y reasonix code")),
            "npx -y reasonix code"
        );
    }

    #[test]
    fn test_reasonix_resume_mirrors_opencode_fork() {
        assert_eq!(
            resolve_resume_command("reasonix", Some("/custom/bin/reasonix")),
            "/custom/bin/reasonix code --continue --copy"
        );
        // Existing resume flags are never duplicated; the missing half is added.
        assert_eq!(
            resolve_resume_command("reasonix", Some("/custom/bin/reasonix code --continue")),
            "/custom/bin/reasonix code --continue --copy"
        );
        // `--copy` alone gets completed with the missing `--continue`
        // (order follows the caller's command line, so assert on the flags).
        let only_copy =
            resolve_resume_command("reasonix", Some("/custom/bin/reasonix code --copy"));
        assert_eq!(only_copy.matches("--continue").count(), 1, "got: {only_copy}");
        assert_eq!(only_copy.matches("--copy").count(), 1, "got: {only_copy}");
    }

    #[test]
    fn test_npx_fallback_avoids_install_prompt() {
        // `-y` keeps a card from blocking on npx's "Ok to proceed?" prompt.
        assert_eq!(
            npx_fallback_command("reasonix", &["code"]),
            "npx -y reasonix code"
        );
        assert_eq!(
            npx_fallback_command("reasonix", &["code", "--continue"]),
            "npx -y reasonix code --continue"
        );
    }

    #[test]
    fn test_only_reasonix_declares_an_npx_fallback() {
        for agent in ["claude", "codex", "opencode", "grok", "aider", "shell"] {
            assert!(
                get_agent_config(agent).npx_package.is_none(),
                "{agent} must not fall back to npx"
            );
        }
        assert_eq!(get_agent_config("reasonix").npx_package, Some("reasonix"));
    }

    #[test]
    fn test_reasonix_resolution_prefers_path_then_npx() {
        // Whichever way Reasonix is installed, a card must never degrade to a
        // bare shell while `npx` is available.
        let resolved = resolve_command("reasonix", None);
        let has = |cmd: &str| {
            which(cmd).is_some()
        };
        if has("reasonix") {
            assert!(resolved.ends_with("code"), "got: {resolved}");
        } else if has("npx") {
            assert_eq!(resolved, "npx -y reasonix code");
        } else {
            assert_eq!(resolved, std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()));
        }
    }

    #[test]
    fn test_extract_last_prompt_tui_markers() {
        // Plain (non-bordered) TUI prompt and shell prompt are user input.
        let pane = "Welcome to Claude\n> fix the login bug please\n";
        assert_eq!(
            extract_last_prompt(pane).as_deref(),
            Some("fix the login bug please")
        );

        let shell = "~/project ❯ cargo test --all\n";
        assert_eq!(
            extract_last_prompt(shell).as_deref(),
            Some("cargo test --all")
        );
    }

    #[test]
    fn test_extract_last_prompt_ignores_bordered_tool_calls() {
        // opencode renders AGENT tool calls as bordered `$`/`#` lines —
        // these are agent output, never user input.
        let pane = "Done — color tags are live.\n┃\n┃  $ grep -n \"tag\" src/state.rs\n┃\n┃  src/state.rs:17:    pub tag: u8,\n┃\n";
        assert_eq!(extract_last_prompt(pane), None);

        // Bordered `>` lines (TUI output area) are ignored too.
        let pane2 = "some output\n│ > quoted markdown, not a prompt\n";
        assert_eq!(extract_last_prompt(pane2), None);
    }

    #[test]
    fn test_extract_last_prompt_skips_noise() {
        // Bare prompt glyph with no input + spinner + cancel hint => None.
        let pane = "❯\n⠋ Working...\nEsc to cancel\n";
        assert_eq!(extract_last_prompt(pane), None);

        // Plain agent output without prompt markers must not become a title.
        let pane2 = "All tests passed in 3.2s\nDone.\n";
        assert_eq!(extract_last_prompt(pane2), None);
    }

    #[test]
    fn test_truncate_prompt_title_caps_length() {
        let short = "fix bug";
        assert_eq!(truncate_prompt_title(short), "fix bug");
        let long = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua enim";
        let out = truncate_prompt_title(long);
        assert_eq!(out.chars().count(), PROMPT_TITLE_MAX_CHARS);
        assert!(out.ends_with('…'));
        assert!(out.starts_with("lorem ipsum dolor sit amet"));
    }

    #[test]
    fn test_extract_composer_draft_typing() {
        let screen = "┃\n┃  fix the login bug please\n┃\n┃  Build · Muse Spark 1.3 Free OpenCode Zen · high\n╹▀▀▀▀▀▀▀▀▀▀\n tab agents  ctrl+p commands\n";
        assert_eq!(
            extract_composer_draft(screen).as_deref(),
            Some("fix the login bug please")
        );
    }

    #[test]
    fn test_extract_composer_draft_empty_and_busy() {
        // Empty composer: placeholder + model line only.
        let empty = "┃\n┃  Ask anything… \"Fix broken tests\"\n┃\n┃  Build · Muse Spark 1.3 Free OpenCode Zen · high\n╹▀▀▀▀▀▀▀▀▀▀\n tab agents  ctrl+p commands\n";
        assert_eq!(extract_composer_draft(empty), None);

        // Busy agent: empty box, interrupt hint lives below the divider.
        let busy = "┃\n┃\n┃\n┃  Build auto · Muse Spark 1.3 Free OpenCode Zen · high\n╹▀▀▀▀▀▀▀▀▀▀\n ⬝⬝■■  esc interrupt   131.5K (13%)  ctrl+p commands\n";
        assert_eq!(extract_composer_draft(busy), None);

        // Activity status line above the box must not leak into the title.
        let with_status = " ▣  Build · Muse Spark 1.3 Free · 28m 26s\n┃\n┃\n┃\n┃  Build auto · Muse Spark 1.3 Free OpenCode Zen · high\n╹▀▀▀▀▀▀▀▀▀▀\n";
        assert_eq!(extract_composer_draft(with_status), None);

        // No divider at all (plain shell screen).
        assert_eq!(extract_composer_draft("~/project ❯ cargo test\n"), None);
    }

    #[test]
    fn test_extract_composer_draft_status_plus_real_draft() {
        // Status line above the box is skipped; the real bordered draft wins.
        let screen = " ▣  Build · Muse Spark 1.3 Free · 28m 26s\n┃\n┃  can you restore the setup\n┃\n┃  Build auto · Muse Spark 1.3 Free OpenCode Zen · high\n╹▀▀▀▀▀▀▀▀▀▀\n";
        assert_eq!(
            extract_composer_draft(screen).as_deref(),
            Some("can you restore the setup")
        );
    }

    #[test]
    fn test_extract_composer_draft_keeps_build_draft() {
        // A real draft starting with "Build" has no `·`, so it must match.
        let screen = "┃\n┃  Build me a dashboard\n┃\n┃  Build · Provider · high\n╹▀▀▀▀▀\n";
        assert_eq!(
            extract_composer_draft(screen).as_deref(),
            Some("Build me a dashboard")
        );
    }

    #[test]
    fn test_extract_text_from_part_json() {
        let json = r#"{"type":"text","text":"go to Github/super-desktop. I want more"}"#;
        assert_eq!(
            extract_text_from_part_json(json).as_deref(),
            Some("go to Github/super-desktop. I want more")
        );
        assert_eq!(extract_text_from_part_json(r#"{"type":"tool","x":1}"#), None);
        assert_eq!(extract_text_from_part_json("not json"), None);
    }

    #[test]
    fn test_sql_escape_doubles_quotes() {
        assert_eq!(sql_escape("a'b"), "a''b");
        assert_eq!(sql_escape("/home/gladimdim"), "/home/gladimdim");
    }

    #[test]
    fn test_assign_opencode_sessions_chronological() {
        // Real-world shape: 4 same-directory cards created ~35s apart, each
        // opencode session born 20-55s after its pane. Nearest-matching would
        // shift cards onto neighbours; chronological 1:1 must hold.
        let panes = vec![
            ("sd_term_848323".to_string(), 1789580848),
            ("sd_term_883125".to_string(), 1789580883),
            ("sd_term_931822".to_string(), 1789580931),
            ("sd_term_278233".to_string(), 1789581278),
        ];
        let sessions = vec![
            ("ses_hover".to_string(), 1789580880),
            ("ses_title".to_string(), 1789580927),
            ("ses_120fps".to_string(), 1789580985),
            ("ses_dedup".to_string(), 1789581297),
        ];
        let got = assign_opencode_sessions(&panes, &sessions);
        let ids: Vec<Option<String>> =
            got.into_iter().map(|(_, s)| s.map(|(id, _)| id)).collect();
        assert_eq!(
            ids,
            vec![
                Some("ses_hover".to_string()),
                Some("ses_title".to_string()),
                Some("ses_120fps".to_string()),
                Some("ses_dedup".to_string()),
            ]
        );
    }

    #[test]
    fn test_extract_session_flag() {
        assert_eq!(
            extract_session_flag("/usr/bin/opencode --auto --session ses_abc123"),
            Some("ses_abc123".to_string())
        );
        assert_eq!(
            extract_session_flag("/usr/bin/opencode --session=ses_xyz --auto"),
            Some("ses_xyz".to_string())
        );
        assert_eq!(extract_session_flag("/usr/bin/opencode --auto"), None);
        assert_eq!(extract_session_flag("/usr/bin/opencode --session"), None);
        assert_eq!(extract_session_flag("/usr/bin/opencode --session="), None);
        assert_eq!(extract_session_flag("/usr/bin/bash"), None);
    }

    #[test]
    fn test_keep_opencode_panes_drops_other_harnesses() {
        // Regression: a reasonix card consumed an opencode session in the
        // chronological assignment, shifting an opencode card onto its
        // neighbour's session — the neighbour's prompt then showed in the
        // wrong card's title.
        let panes = vec![
            ("sd_term_a".to_string(), 1000),
            ("sd_term_r".to_string(), 1100),
            ("sd_term_b".to_string(), 1200),
            ("sd_term_unknown".to_string(), 1300),
        ];
        let mut info = std::collections::HashMap::new();
        info.insert("sd_term_a".to_string(), ("opencode".to_string(), None));
        info.insert(
            "sd_term_r".to_string(),
            ("reasonix".to_string(), None),
        );
        info.insert("sd_term_b".to_string(), ("opencode".to_string(), None));
        info.insert("sd_term_shell".to_string(), ("shell".to_string(), None));
        let kept = keep_opencode_panes(&panes, &info);
        let names: Vec<&str> = kept.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["sd_term_a", "sd_term_b", "sd_term_unknown"]);
    }

    #[test]
    fn test_non_opencode_pane_cannot_steal_opencode_session() {
        // The reported bug, end to end at the pure level: panes A (opencode),
        // R (reasonix) and B (opencode) created in that order; opencode
        // sessions S1/S2/S3 born right after each *opencode* pane (TUI lag).
        // With R participating, R steals S2 and B shifts onto S3 (another
        // console's session); filtered to opencode panes, A→S1 and B→S2.
        let panes = vec![
            ("sd_term_a".to_string(), 1000),
            ("sd_term_r".to_string(), 1100),
            ("sd_term_b".to_string(), 1200),
        ];
        let sessions = vec![
            ("s1".to_string(), 1020),
            ("s2".to_string(), 1237),
            ("s3".to_string(), 1300),
        ];
        // Old behaviour (all panes participate): R steals S2, B lands on S3.
        let all = assign_opencode_sessions(&panes, &sessions);
        let id_of_all = |pane: &str| {
            all.iter()
                .find(|(p, _)| p == pane)
                .and_then(|(_, s)| s.as_ref().map(|(id, _)| id.as_str()))
        };
        assert_eq!(id_of_all("sd_term_r"), Some("s2"));
        assert_eq!(id_of_all("sd_term_b"), Some("s3"));
        // Fixed behaviour: only opencode panes participate.
        let mut info = std::collections::HashMap::new();
        info.insert("sd_term_a".to_string(), ("opencode".to_string(), None));
        info.insert("sd_term_r".to_string(), ("reasonix".to_string(), None));
        info.insert("sd_term_b".to_string(), ("opencode".to_string(), None));
        let filtered = keep_opencode_panes(&panes, &info);
        let got = assign_opencode_sessions(&filtered, &sessions);
        let id_of = |pane: &str| {
            got.iter()
                .find(|(p, _)| p == pane)
                .and_then(|(_, s)| s.as_ref().map(|(id, _)| id.clone()))
        };
        assert_eq!(id_of("sd_term_a").as_deref(), Some("s1"));
        assert_eq!(id_of("sd_term_b").as_deref(), Some("s2"));
    }

    #[test]
    fn test_drop_determined_panes() {
        // Flag-owned pairs leave the ranking entirely: the satisfied pane no
        // longer shifts its neighbours, and its session cannot be stolen.
        let panes = vec![
            ("a".to_string(), 1000),
            ("b".to_string(), 1200),
        ];
        let sessions = vec![
            ("s1".to_string(), 1020),
            ("s2".to_string(), 1237),
        ];
        let mut flags = std::collections::HashMap::new();
        flags.insert("a".to_string(), "s1".to_string());
        let (free_panes, free_sessions) = drop_determined_panes(&panes, &sessions, &flags);
        assert_eq!(free_panes, vec![("b".to_string(), 1200)]);
        assert_eq!(free_sessions, vec![("s2".to_string(), 1237)]);
        let got = assign_opencode_sessions(&free_panes, &free_sessions);
        assert_eq!(
            got[0].1.as_ref().map(|(id, _)| id.as_str()),
            Some("s2")
        );
    }

    #[test]
    fn test_should_adopt_heuristic_match() {
        // Stale guess S3 (+305s) vs nearer live match S2 (+37s): adopt.
        assert!(should_adopt_heuristic_match(Some(7305), 7037, 7000));
        // Persisted is nearer: keep.
        assert!(!should_adopt_heuristic_match(Some(7037), 7305, 7000));
        // Tie is not "strictly nearer": keep (no flapping).
        assert!(!should_adopt_heuristic_match(Some(7100), 6900, 7000));
        // Unknown birth (pre-reboot ground truth or garbage): always keep.
        // A newer heuristic hit there belongs to somebody else, and garbage
        // safely yields no DB text (own-pane text wins instead).
        assert!(!should_adopt_heuristic_match(None, 7037, 7000));
    }

    #[test]
    fn test_assign_opencode_sessions_shortage_and_grace() {
        // More panes than sessions: last pane gets None.
        let panes = vec![
            ("a".to_string(), 1000),
            ("b".to_string(), 1100),
            ("c".to_string(), 1200),
        ];
        let sessions = vec![("s1".to_string(), 1020)];
        let got = assign_opencode_sessions(&panes, &sessions);
        assert_eq!(got[0].1.as_ref().map(|(id, _)| id.as_str()), Some("s1"));
        assert_eq!(got[1].1, None);
        assert_eq!(got[2].1, None);

        // Session born up to 30s before the pane still matches (clock skew);
        // older ones do not.
        let panes = vec![("a".to_string(), 1000)];
        let sessions = vec![("s1".to_string(), 969)];
        assert_eq!(assign_opencode_sessions(&panes, &sessions)[0].1, None);
        let sessions = vec![("s1".to_string(), 970)];
        assert!(assign_opencode_sessions(&panes, &sessions)[0].1.is_some());
    }

    #[test]
    fn test_resolve_resume_command_per_agent() {
        // Deterministic: explicit binary paths, no `which` dependence.
        let claude = resolve_resume_command("claude", Some("/usr/bin/claude"));
        assert!(claude.ends_with("--continue"), "got: {claude}");
        assert!(claude.contains("--dangerously-skip-permissions"));

        let codex = resolve_resume_command("codex", Some("/usr/bin/codex"));
        assert!(codex.ends_with("resume --last"), "got: {codex}");
        assert!(codex.contains("--dangerously-bypass-approvals-and-sandbox"));

        let opencode = resolve_resume_command("opencode", Some("/usr/bin/opencode"));
        // No stored id -> forked continue (isolated copy, never shared live).
        assert!(opencode.contains("--continue"), "got: {opencode}");
        assert!(opencode.ends_with("--fork"), "got: {opencode}");
        assert!(opencode.contains("--auto"));

        // Stored per-card id -> exact session resume.
        let opencode_exact = resolve_resume_command_with_session(
            "opencode",
            Some("/usr/bin/opencode"),
            Some("ses_abc123"),
        );
        assert!(
            opencode_exact.ends_with("--session ses_abc123"),
            "got: {opencode_exact}"
        );

        let aider = resolve_resume_command("aider", Some("/usr/bin/aider"));
        assert!(aider.ends_with("--restore-chat-history"), "got: {aider}");
        assert!(aider.contains("--yes-always"));

        // No resume mechanism -> fresh launch unchanged.
        assert_eq!(resolve_resume_command("shell", Some("/bin/bash")), "/bin/bash");
        let grok = resolve_resume_command("grok", Some("/usr/bin/grok"));
        assert!(
            !grok.contains("resume") && !grok.contains("continue"),
            "got: {grok}"
        );
        let agy = resolve_resume_command("antigravity", Some("/usr/bin/agy"));
        assert!(
            !agy.contains("resume") && !agy.contains("continue"),
            "got: {agy}"
        );
    }

    #[test]
    fn test_resolve_resume_command_idempotent_and_shell_safe() {
        // Already-resuming commands are returned untouched (no flag pile-up).
        assert_eq!(
            resolve_resume_command(
                "claude",
                Some("/usr/bin/claude --dangerously-skip-permissions --continue")
            ),
            "/usr/bin/claude --dangerously-skip-permissions --continue"
        );
        let codex_resumed =
            resolve_resume_command("codex", Some("/usr/bin/codex resume --last"));
        assert!(codex_resumed.contains("resume --last"));
        assert_eq!(codex_resumed.matches("resume").count(), 1);
        // Opencode fallback is idempotent: no flag pile-up on second reboot.
        let oc_once = resolve_resume_command("opencode", Some("/usr/bin/opencode"));
        let oc_twice = resolve_resume_command("opencode", Some(&oc_once));
        assert_eq!(oc_once, oc_twice, "got: {oc_twice}");
        assert_eq!(oc_twice.matches("--fork").count(), 1);
        assert_eq!(oc_twice.matches("--continue").count(), 1);
        // Explicit shells are never decorated, even for resumable agents.
        assert_eq!(
            resolve_resume_command("claude", Some("/bin/bash")),
            "/bin/bash"
        );
        assert_eq!(
            resolve_resume_command("opencode", Some("/bin/zsh")),
            "/bin/zsh"
        );
    }

    fn pane_cmdline(sess: &str) -> Option<String> {
        let out = Command::new("tmux")
            .args(["display-message", "-p", "-t", sess, "#{pane_pid}"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let pid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(
            raw.iter()
                .map(|b| if *b == 0 { ' ' } else { *b as char })
                .collect(),
        )
    }

    #[test]
    fn test_ensure_session_recreates_with_resume_command() {
        // End-to-end reboot simulation: kill the tmux session, then verify
        // ensure_session() recreates it with the agent's resume command.
        // python3 tolerates trailing CLI flags (they land in sys.argv) and
        // sleeps, so the pane stays alive long enough to inspect.
        let has_tmux = Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let has_py = which("python3").is_some();
        if !has_tmux || !has_py {
            return;
        }
        for (sess, agent, marker) in [
            ("test_sd_resume_claude", "claude", "--continue"),
            ("test_sd_resume_codex", "codex", "resume --last"),
        ] {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", sess])
                .output();
            ensure_session(
                sess,
                agent,
                Some("python3 -c 'import time; time.sleep(30)'"),
                None,
            );
            let mut ok = false;
            for _ in 0..40 {
                if let Some(cmdline) = pane_cmdline(sess) {
                    if cmdline.contains(marker) && cmdline.contains("sleep") {
                        ok = true;
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", sess])
                .output();
            assert!(ok, "recreated {sess} should run the resume command");
        }
    }

    #[test]
    fn test_unique_session_names_never_collide() {
        // Rapid creation (same-millisecond) must still yield distinct names:
        // the old `millis % 1_000_000` scheme attached both cards to one tmux
        // session, mixing output/Ctrl+C across terminals.
        let mut names = std::collections::HashSet::new();
        for _ in 0..50 {
            let n = unique_session_name();
            assert!(n.starts_with("sd_term_"), "got: {n}");
            assert!(names.insert(n.clone()), "duplicate session name: {n}");
        }
    }

    /// Read tmux option output: `tmux show-options <args>`.
    fn show_option(args: &[&str]) -> String {
        let out = Command::new("tmux")
            .args(["show-options"])
            .args(args)
            .output()
            .expect("tmux show-options");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Session-local value of `detach-on-destroy`; empty when the session has
    /// no override of its own (i.e. it inherits the user's global setting).
    fn session_detach_on_destroy(sess: &str) -> String {
        show_option(&["-t", sess, "-v", "detach-on-destroy"])
    }

    /// Kills the listed sessions on drop so a failing assertion cannot leak
    /// harness sessions into the user's running tmux server.
    struct SessionCleanup(Vec<String>);

    impl Drop for SessionCleanup {
        fn drop(&mut self) {
            for name in &self.0 {
                let _ = Command::new("tmux")
                    .args(["kill-session", "-t", name])
                    .output();
            }
        }
    }

    #[test]
    fn test_resolve_workspace_dir_prefers_a_real_folder() {
        // Pure: no tmux needed.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        assert_eq!(resolve_workspace_dir(None), home);
        assert_eq!(resolve_workspace_dir(Some("")), home);
        assert_eq!(resolve_workspace_dir(Some("   ")), home);
        // A folder that no longer exists (deleted checkout, unmounted drive)
        // must not produce a session that starts nowhere.
        assert_eq!(resolve_workspace_dir(Some("/definitely/gone/xyz")), home);
        assert_eq!(resolve_workspace_dir(Some("/tmp")), "/tmp");
    }

    #[test]
    fn test_harness_sessions_start_in_the_workspace_folder() {
        // The whole point of the top bar's workspace field: a harness card must
        // actually run in that folder, because every agent scopes its own
        // history/resume to the cwd — and Reasonix keys its workspace write
        // lease on it, which is what makes parallel cards collide at `$HOME`.
        let has_tmux = Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !has_tmux {
            return;
        }

        let dir = std::env::temp_dir().join(format!("sd-workspace-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let dir_s = std::fs::canonicalize(&dir)
            .unwrap_or(dir.clone())
            .to_string_lossy()
            .into_owned();

        let pane_dir = |sess: &str| -> Option<String> {
            for _ in 0..40 {
                if let Some(p) = tmux_display(sess, "#{pane_current_path}") {
                    // The pane reports the path it physically started in.
                    let resolved = std::fs::canonicalize(p.trim())
                        .map(|c| c.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| p.trim().to_string());
                    if resolved == dir_s {
                        return Some(resolved);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            None
        };

        // 1. A new card runs in the folder from the top bar.
        let (sess, _cmd) = create_session("shell", Some("/usr/bin/bash"), Some(&dir_s));
        let mut cleanup = SessionCleanup(vec![sess.clone()]);
        assert_eq!(
            pane_dir(&sess).as_deref(),
            Some(dir_s.as_str()),
            "a new harness must start in the workspace folder, not $HOME"
        );

        // 2. The same card after a tmux server restart (reboot) resumes in that
        //    same folder — resuming in another cwd would attach the card to a
        //    different history.
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &sess])
            .output();
        ensure_session_with_agent_id(&sess, "shell", Some("/usr/bin/bash"), None, Some(&dir_s));
        assert_eq!(
            pane_dir(&sess).as_deref(),
            Some(dir_s.as_str()),
            "a restored harness must resume in its own folder"
        );

        // 3. With nothing configured the old behaviour stands: $HOME.
        let (sess_home, _cmd) = create_session("shell", Some("/usr/bin/bash"), None);
        cleanup.0.push(sess_home.clone());
        let home_s = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let home_s = std::fs::canonicalize(&home_s)
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or(home_s);
        let pane_home = {
            let mut found = None;
            for _ in 0..40 {
                if let Some(p) = tmux_display(&sess_home, "#{pane_current_path}") {
                    found = Some(
                        std::fs::canonicalize(p.trim())
                            .map(|c| c.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| p.trim().to_string()),
                    );
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            found
        };
        assert_eq!(
            pane_home.as_deref(),
            Some(home_s.as_str()),
            "an unset folder must still start in $HOME"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_client_exits_with_its_own_session() {
        // Regression: with `set -g detach-on-destroy off` (omarchy's tmux
        // default) a client whose session is destroyed is switched to another
        // session instead of exiting. Each card here owns one tmux client, so a
        // card that lost its session rendered a DIFFERENT harness and the next
        // Ctrl-C typed into it killed that harness: closing one harness looked
        // like all of them closing. Sessions we create must pin the option
        // per session, without touching the user's global value.
        //
        // Start the server first: it (and its global options, i.e. the user's
        // config) only exist once a session has been created.
        let probe = "test_sd_detach_on_destroy_probe";
        let mut cleanup = SessionCleanup(vec![probe.to_string()]);
        let _ = Command::new("tmux")
            .args(["new-session", "-d", "-s", probe, "-c", "/tmp", "/usr/bin/bash"])
            .output();
        let global_before = show_option(&["-gv", "detach-on-destroy"]);
        assert!(
            !global_before.is_empty(),
            "tmux server must be reachable for this test"
        );

        // tmux < 3.2 kept detach-on-destroy as a server option, where the
        // per-session form is rejected; the fix is then a no-op (and so is
        // this test).
        let _ = Command::new("tmux")
            .args(["set-option", "-t", probe, "detach-on-destroy", "off"])
            .output();
        if session_detach_on_destroy(probe).is_empty() {
            eprintln!("skipping: this tmux has no session-scoped detach-on-destroy");
            return;
        }

        let (sess, _cmd) = create_session("shell", Some("/usr/bin/bash"), None);
        cleanup.0.push(sess.clone());
        assert_eq!(
            session_detach_on_destroy(&sess),
            "on",
            "new harness session must pin detach-on-destroy"
        );
        assert_eq!(
            show_option(&["-gv", "detach-on-destroy"]),
            global_before,
            "the user's global tmux config must stay untouched"
        );

        // Restored/legacy sessions (or ones created before this fix) inherit
        // the config value; startup must re-pin them too.
        let _ = Command::new("tmux")
            .args(["set-option", "-t", &sess, "detach-on-destroy", "off"])
            .output();
        assert_eq!(session_detach_on_destroy(&sess), "off");
        ensure_session_with_agent_id(&sess, "shell", Some("/usr/bin/bash"), None, None);
        assert_eq!(
            session_detach_on_destroy(&sess),
            "on",
            "restored harness session must be re-pinned"
        );
        assert_eq!(show_option(&["-gv", "detach-on-destroy"]), global_before);
    }

    #[test]
    fn test_harness_candidates_prefer_own_binary() {
        // A harness is only detected through its own binary…
        assert_eq!(harness_candidates("claude"), ["claude"]);
        assert_eq!(harness_candidates("antigravity"), ["agy", "antigravity"]);
        // …while a terminal card accepts any POSIX shell on the box.
        let shell = harness_candidates("shell");
        assert!(shell.contains(&"bash") && shell.contains(&"zsh") && shell.contains(&"fish"));
    }

    #[test]
    fn test_detected_harnesses_are_really_launchable() {
        // Whatever the settings panel offers must resolve to something that
        // exists here, and must agree with what a card would run.
        for info in detect_harnesses() {
            let bin = info.command.split_whitespace().next().unwrap_or_default();
            assert!(
                std::path::Path::new(bin).is_file() || bin == "npx",
                "{} resolved to a command that is not here: {}",
                info.key,
                info.command
            );
            assert_eq!(detect_harness_command(info.key).as_deref(), Some(info.command.as_str()));
            assert!(HARNESS_KEYS.contains(&info.key), "{} is not a supported harness", info.key);
        }
        // Detection is a filter of the declared order, never a reordering.
        let keys: Vec<&str> = detect_harnesses().iter().map(|h| h.key).collect();
        let expected: Vec<&str> = HARNESS_KEYS
            .iter()
            .copied()
            .filter(|k| keys.contains(k))
            .collect();
        assert_eq!(keys, expected);
        assert!(keys.len() <= HARNESS_KEYS.len());
    }

    #[test]
    fn test_supported_harnesses_all_have_their_own_config() {
        // Every key shown in the top bar / settings panel needs its own agent
        // config; falling through to the `_` arm would label it "Terminal".
        for key in HARNESS_KEYS {
            let cfg = get_agent_config(key);
            assert!(!cfg.name.is_empty() && !cfg.icon.is_empty(), "{key} has no label");
            if *key != "shell" {
                assert_ne!(cfg.name, "Terminal", "{key} needs its own AgentConfig arm");
            }
        }
        // A terminal card can always be launched, even on a bash-less box.
        assert!(detect_harness_command("shell").is_some());
    }

    #[test]
    fn test_resume_with_stored_opencode_session() {
        let cmd = resolve_resume_command_with_session(
            "opencode",
            Some("/usr/bin/opencode --auto"),
            Some("ses_f51a6ff90ffe11x52WFauyf57C"),
        );
        assert!(cmd.contains("--session ses_f51a6ff90ffe11x52WFauyf57C"));
        assert!(!cmd.contains("--continue"), "got: {cmd}");
        assert!(!cmd.contains("--fork"), "got: {cmd}");
        // Empty/blank stored ids fall back to forked continue.
        let fallback =
            resolve_resume_command_with_session("opencode", Some("/usr/bin/opencode"), Some("  "));
        assert!(fallback.contains("--continue") && fallback.contains("--fork"));
    }
}
