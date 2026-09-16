use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct AgentConfig {
    pub name: &'static str,
    pub icon: &'static str,
    pub commands: &'static [&'static str],
}

pub fn get_agent_config(agent_type: &str) -> AgentConfig {
    match agent_type {
        "antigravity" => AgentConfig {
            name: "Antigravity",
            icon: "🌌",
            commands: &["agy", "antigravity"],
        },
        "claude" => AgentConfig {
            name: "Claude Code",
            icon: "⚡",
            commands: &["claude"],
        },
        "codex" => AgentConfig {
            name: "OpenAI Codex",
            icon: "🤖",
            commands: &["codex"],
        },
        "opencode" => AgentConfig {
            name: "OpenCode",
            icon: "🔮",
            commands: &["opencode"],
        },
        "grok" => AgentConfig {
            name: "Grok CLI",
            icon: "🚀",
            commands: &["grok"],
        },
        "aider" => AgentConfig {
            name: "Aider",
            icon: "🧠",
            commands: &["aider"],
        },
        _ => AgentConfig {
            name: "Terminal",
            icon: "💻",
            commands: &["bash"],
        },
    }
}

pub fn resolve_command(agent_type: &str, custom: Option<&str>) -> String {
    if let Some(cmd) = custom {
        return cmd.to_string();
    }
    let cfg = get_agent_config(agent_type);
    for cmd in cfg.commands {
        if let Ok(output) = Command::new("which").arg(cmd).output() {
            if output.status.success() {
                let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !s.is_empty() {
                    return s;
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
                let pid = parts.get(0).unwrap_or(&"").to_string();
                let cmd = parts.get(1).unwrap_or(&"").to_string();
                let dead = parts.get(2).unwrap_or(&"0");

                if *dead == "1" || pid.is_empty() {
                    return SessionStatus {
                        status: "EXITED",
                        label: "○ EXITED",
                        pid,
                        cmd,
                    };
                }

                // Check recent preview text for busy / thinking keywords
                let preview = get_preview(session_name, 4).to_lowercase();
                if preview.contains("thinking")
                    || preview.contains("generating")
                    || preview.contains("working")
                    || preview.contains("building")
                    || preview.contains("streaming")
                {
                    return SessionStatus {
                        status: "BUSY",
                        label: "● WORKING",
                        pid,
                        cmd,
                    };
                }

                return SessionStatus {
                    status: "ACTIVE",
                    label: "● ACTIVE",
                    pid,
                    cmd: if cmd.is_empty() { agent_type.to_string() } else { cmd },
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
