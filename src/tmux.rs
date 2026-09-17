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
        // (--last is scoped to the cwd, which is always $HOME here).
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
fn session_state(session_name: &str) -> Option<bool> {
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
    text.lines().find_map(|line| {
        let (name, value) = line.split_once("::")?;
        (name.trim() == session_name).then(|| value.trim() == "on")
    })
}

pub fn create_session(agent_type: &str, custom_command: Option<&str>) -> (String, String) {
    let session_name = unique_session_name();
    let cmd = resolve_command(agent_type, custom_command);
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());

    let _ = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session_name,
            "-c",
            &home,
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
pub fn ensure_session(session_name: &str, agent_type: &str, custom_command: Option<&str>) {
    ensure_session_with_agent_id(session_name, agent_type, custom_command, None)
}

/// Same as `ensure_session` but resumes a persisted per-card agent session
/// (opencode `--session <id>`) so rebooted cards stay isolated 1:1.
pub fn ensure_session_with_agent_id(
    session_name: &str,
    agent_type: &str,
    custom_command: Option<&str>,
    agent_session_id: Option<&str>,
) {
    // One `tmux` call answers both questions this function needs: does the
    // session exist, and is `detach-on-destroy` already pinned? Every fork/exec
    // is tens of milliseconds on a loaded machine, and this runs once per card
    // while the overlay is being built.
    match session_state(session_name) {
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
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let _ = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                session_name,
                "-c",
                &home,
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

pub fn get_preview(session_name: &str, lines: usize) -> String {
    if let Ok(output) = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", session_name, "-S", &format!("-{}", lines * 3)])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut raw_lines: Vec<&str> = text.lines().collect();
            while let Some(last) = raw_lines.last() {
                if last.trim().is_empty() {
                    raw_lines.pop();
                } else {
                    break;
                }
            }
            if raw_lines.is_empty() {
                return "Ready. Waiting for input...".to_string();
            }
            let start = if raw_lines.len() > lines {
                raw_lines.len() - lines
            } else {
                0
            };
            return raw_lines[start..].join("\n");
        }
    }
    "Session offline or ended.".to_string()
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
}

pub fn inspect_status(session_name: &str, agent_type: &str) -> SessionStatus {
    if let Ok(output) = Command::new("tmux")
        .args([
            "list-panes",
            "-t",
            session_name,
            "-F",
            "#{pane_pid}::#{pane_current_command}::#{pane_dead}",
        ])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = text.lines().next() {
                let parts: Vec<&str> = first_line.split("::").collect();
                let pid = parts.get(0).unwrap_or(&"").trim().to_string();
                let cmd = parts.get(1).unwrap_or(&"").trim().to_string();
                let dead = parts.get(2).unwrap_or(&"0").trim();

                if dead == "1" || pid.is_empty() {
                    return SessionStatus {
                        status: "EXITED",
                        label: "○ EXITED",
                        pid,
                        cmd,
                    };
                }

                let p_num = pid.parse::<u32>().unwrap_or(0);
                if p_num != 0 && !std::path::Path::new(&format!("/proc/{}", p_num)).exists() {
                    return SessionStatus {
                        status: "EXITED",
                        label: "○ EXITED",
                        pid,
                        cmd,
                    };
                }

                let is_shell_agent = agent_type == "shell" || agent_type == "bash" || agent_type == "terminal";
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
                    };
                }

                // Check 3: Screen capture of bottom lines for spinners, cancel hints, or status words
                if let Ok(cap_out) = Command::new("tmux")
                    .args(["capture-pane", "-p", "-t", session_name, "-S", "-15"])
                    .output()
                {
                    if cap_out.status.success() {
                        let cap_text = String::from_utf8_lossy(&cap_out.stdout);
                        let non_empty_lines: Vec<&str> = cap_text
                            .lines()
                            .map(|l| l.trim())
                            .filter(|l| !l.is_empty())
                            .collect();

                        let tail_len = non_empty_lines.len().min(8);
                        let recent_lines = &non_empty_lines[non_empty_lines.len() - tail_len..];

                        for line in recent_lines {
                            // Check for Braille spinner characters (U+2801 to U+28FF)
                            let has_braille = line.chars().any(|c| c >= '\u{2801}' && c <= '\u{28FF}');
                            if has_braille {
                                return SessionStatus {
                                    status: "WORKING",
                                    label: "● WORKING",
                                    pid: display_pid,
                                    cmd: display_cmd,
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
                                };
                            }
                        }
                    }
                }

                return SessionStatus {
                    status: "IDLE",
                    label: "● IDLE",
                    pid: display_pid,
                    cmd: display_cmd,
                };
            }
        }
    }

    SessionStatus {
        status: "EXITED",
        label: "○ EXITED",
        pid: "-".to_string(),
        cmd: agent_type.to_string(),
    }
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
    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", session_name, "-S", "-300"])
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

/// Map a super-desktop tmux session to its opencode session id.
///
/// super-desktop launches the agent command at card creation, so the
/// opencode session is born shortly AFTER the tmux session (TUI + provider
/// init lag is typically 10-60s). Because several same-directory cards are
/// often created a minute apart, per-pane "nearest time" matching would
/// shift every card onto its neighbour's session — instead ALL live
/// `sd_term_*` panes and same-directory opencode sessions are matched
/// chronologically 1:1 (`assign_opencode_sessions`). Tolerance 10 min.
/// Returns None when opencode storage is unavailable or nothing matches.
///
/// NOTE: `/new` inside opencode (or restarting the agent in the same pane),
/// and sessions left behind by deleted cards, can skew the ranking; the
/// live composer draft still works in those cases.
pub fn get_opencode_session_id(session_name: &str) -> Option<String> {
    let cwd = tmux_display(session_name, "#{pane_current_path}")?;
    let created: i64 = tmux_display(session_name, "#{session_created}")?
        .parse()
        .ok()?;
    // All live super-desktop panes, oldest first.
    let list_out = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}|#{session_created}"])
        .output()
        .ok()?;
    if !list_out.status.success() {
        return None;
    }
    let mut panes: Vec<(String, i64)> = String::from_utf8_lossy(&list_out.stdout)
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
    panes.sort_by_key(|(_, t)| *t);
    let first = panes.iter().map(|(_, t)| *t).min().unwrap_or(created);

    let db = opencode_db_path()?;
    let sql = format!(
        "SELECT id, time_created FROM session WHERE directory = '{}' AND time_created/1000 >= {} ORDER BY time_created;",
        sql_escape(&cwd),
        first - 30
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

    assign_opencode_sessions(&panes, &sessions)
        .into_iter()
        .find(|(pane, _)| pane == session_name)
        .and_then(|(_, sess)| sess)
        .filter(|(id, t)| {
            !id.is_empty() && (*t - created).abs() <= 600 && *t >= created - 30
        })
        .map(|(id, _)| id)
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

        // Send a sleep command
        let _ = Command::new("tmux").args(["send-keys", "-t", sess, "sleep 1.5", "Enter"]).output();
        std::thread::sleep(std::time::Duration::from_millis(300));

        let status_busy = inspect_status(sess, "bash");
        assert_eq!(status_busy.status, "WORKING", "Shell executing sleep should be WORKING");

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

        let (sess, _cmd) = create_session("shell", Some("/usr/bin/bash"));
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
        ensure_session_with_agent_id(&sess, "shell", Some("/usr/bin/bash"), None);
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

