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
        let cmd = resolve_command(agent_type, custom_command);
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
}

