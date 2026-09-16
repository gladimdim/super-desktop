use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct AgentConfig {
    pub name: &'static str,
    pub icon: &'static str,
    pub commands: &'static [&'static str],
    pub default_args: &'static [&'static str],
}

pub fn get_agent_config(agent_type: &str) -> AgentConfig {
    match agent_type {
        "antigravity" | "agy" => AgentConfig {
            name: "Antigravity",
            icon: "🌌",
            commands: &["agy", "antigravity"],
            default_args: &["--dangerously-skip-permissions"],
        },
        "claude" => AgentConfig {
            name: "Claude Code",
            icon: "⚡",
            commands: &["claude"],
            default_args: &["--dangerously-skip-permissions"],
        },
        "codex" => AgentConfig {
            name: "OpenAI Codex",
            icon: "🤖",
            commands: &["codex"],
            default_args: &["--dangerously-bypass-approvals-and-sandbox"],
        },
        "opencode" => AgentConfig {
            name: "OpenCode",
            icon: "🔮",
            commands: &["opencode"],
            default_args: &["--auto"],
        },
        "grok" => AgentConfig {
            name: "Grok CLI",
            icon: "🚀",
            commands: &["grok"],
            default_args: &["--dangerously-skip-permissions"],
        },
        "aider" => AgentConfig {
            name: "Aider",
            icon: "🧠",
            commands: &["aider"],
            default_args: &["--yes-always"],
        },
        _ => AgentConfig {
            name: "Terminal",
            icon: "💻",
            commands: &["bash"],
            default_args: &[],
        },
    }
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
        if let Ok(output) = Command::new("which").arg(cmd).output() {
            if output.status.success() {
                let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !s.is_empty() {
                    if cfg.default_args.is_empty() {
                        return s;
                    } else {
                        return format!("{} {}", s, cfg.default_args.join(" "));
                    }
                }
            }
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
/// session store, Aider: .aider.chat.history.md), so relaunching with the
/// agent's native resume mechanism restores the conversation automatically.
///
/// This is intentionally "continue most recent": exact per-terminal session
/// IDs can't be tracked reliably (all cards share $HOME as cwd), so with
/// several cards of the same agent each restored card continues that agent's
/// latest session. Use /clear (or rename/fork) inside the agent if a card
/// should start over instead.
///
/// Agents without a non-interactive resume mechanism (shell, antigravity,
/// grok, unknown) relaunch fresh, exactly like before. An explicit shell
/// command is never decorated with agent flags.
pub fn resolve_resume_command(agent_type: &str, custom: Option<&str>) -> String {
    let base = resolve_command(agent_type, custom);
    if is_shell_command(&base) {
        return base;
    }
    match agent_type {
        // `claude --continue` resumes the most recent session for the cwd.
        "claude" => append_resume_flag(&base, "--continue", &["--continue", "--resume"]),
        // `opencode --continue` continues the last session in the TUI.
        "opencode" => append_resume_flag(&base, "--continue", &["--continue", "--session"]),
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
        _ => base,
    }
}

pub fn create_session(agent_type: &str, custom_command: Option<&str>) -> (String, String) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    let session_name = format!("sd_term_{}", now % 1000000);
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

    (session_name, cmd)
}

pub fn kill_session(session_name: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session_name])
        .output();
}

pub fn ensure_session(session_name: &str, agent_type: &str, custom_command: Option<&str>) {
    let exists = Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !exists {
        // Reboot survival: the tmux server is gone, but agent CLIs persist
        // conversations to disk continuously, so recreate with the agent's
        // native resume mechanism (see resolve_resume_command). Brand-new
        // terminals in create_session() still launch fresh.
        let cmd = resolve_resume_command(agent_type, custom_command);
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
    if let Ok(output) = Command::new("which").arg("tmux").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "tmux".to_string()
}

/// Max characters of the last prompt shown in the terminal card title.
pub const PROMPT_TITLE_MAX_CHARS: usize = 30;

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
        if Command::new("which").arg("agy").output().map(|o| o.status.success()).unwrap_or(false) {
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
    fn test_truncate_prompt_title_caps_at_30() {
        let short = "fix bug";
        assert_eq!(truncate_prompt_title(short), "fix bug");
        let long = "this is a very long prompt that definitely exceeds thirty chars";
        let out = truncate_prompt_title(long);
        assert_eq!(out.chars().count(), 30);
        assert!(out.ends_with('…'));
        assert!(out.starts_with("this is a very long prompt"));
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
        assert!(opencode.ends_with("--continue"), "got: {opencode}");
        assert!(opencode.contains("--auto"));

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
        let has_py = Command::new("which")
            .arg("python3")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
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
}

