use super::*;

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

#[cfg(target_os = "linux")]
pub(in crate::tmux) fn get_proc_comm(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
pub(crate) fn get_direct_children(pid: u32) -> Vec<u32> {
    let task_path = format!("/proc/{}/task/{}/children", pid, pid);
    if let Ok(content) = std::fs::read_to_string(&task_path) {
        return content
            .split_whitespace()
            .filter_map(|s| s.parse::<u32>().ok())
            .collect();
    }
    Vec::new()
}

#[cfg(target_os = "macos")]
pub(in crate::tmux) fn get_proc_comm(pid: u32) -> String { crate::platform::process::name(pid).unwrap_or_default() }

#[cfg(target_os = "macos")]
pub(crate) fn get_direct_children(pid: u32) -> Vec<u32> { crate::platform::process::children(pid) }

/// Compare the shell's process group with the terminal's foreground group.
/// Background jobs and persistent helpers are not evidence of a busy prompt.
pub(in crate::tmux) fn foreground_job_from_stat(stat: &str) -> bool {
    let Some((_, rest)) = stat.rsplit_once(')') else { return false; };
    let fields: Vec<_> = rest.split_whitespace().collect();
    let pgrp = fields.get(2).and_then(|v| v.parse::<i32>().ok());
    let foreground = fields.get(5).and_then(|v| v.parse::<i32>().ok());
    matches!((pgrp, foreground), (Some(group), Some(fg)) if fg > 0 && group != fg)
}

pub(in crate::tmux) fn explicit_turn_status(state: &str) -> Option<(&'static str, &'static str)> {
    match state {
        "working" => Some(("WORKING", "● WORKING")),
        "completed" => Some(("FINISHED", "✓ FINISHED")),
        // Same label as native adapters' `error` (harness_record::status).
        "error" => Some(("ERROR", "⚠ ERROR")),
        _ => None,
    }
}

pub(in crate::tmux) fn screen_indicates_work(screen: &str, height: usize) -> bool {
    let plain = strip_terminal_escapes(screen);
    let working = recent_status_lines(&plain, height).any(|line| {
        // Inspect status-shaped lines, never arbitrary prose, code or quoted prompts.
        let first = line.chars().next().unwrap_or(' ');
        let spinner = ('\u{2801}'..='\u{28ff}').contains(&first);
        let decorated = spinner || "•✻✽✶✳✢*".contains(first);
        let text = line.trim_start_matches(|c: char| c.is_whitespace() || "•✻✽✶✳✢*".contains(c)
            || ('\u{2801}'..='\u{28ff}').contains(&c)).to_lowercase();
        let interrupt = ["esc to cancel", "esc to interrupt", "ctrl+c to cancel", "ctrl+c to interrupt", "press esc to stop"];
        if interrupt.iter().any(|hint| text.starts_with(hint)) { return true; }
        if !decorated { return false; }
        if interrupt.iter().chain(&["esc cancels"]).any(|hint| text.contains(hint)) { return true; }
        if spinner && spinner_timer(&text) { return true; }
        ["thinking", "generating", "streaming", "working", "building", "compiling", "editing", "running", "analyzing"]
            .iter().any(|word| text.starts_with(&format!("{word}…")) || text.starts_with(&format!("{word}...")))
    });
    working
}

/// A braille spinner's activity label followed by its running elapsed timer:
/// Grok `⠋ Waiting for response… 0.8s` / `⠋ Responding… 0.1s`, Reasonix
/// `⣽  working · 0s · ↓2`. `text` is lowercased, without the spinner.
pub(in crate::tmux) fn spinner_timer(text: &str) -> bool {
    let label_end = text.find(|c: char| !(c.is_alphabetic() || c == ' ')).unwrap_or(text.len());
    let label = text[..label_end].trim();
    if label.is_empty() || label.len() > 40 {
        return false;
    }
    let rest = text[label_end..].trim_start_matches(['…', '.', ' ', '(', '·']);
    // The timer is set off from the label (`…`, `·` or `(`), never plain prose.
    let separator = &text[label_end..text.len() - rest.len()];
    if !separator.contains(['…', '·', '(']) && !separator.contains("...") {
        return false;
    }
    let digits = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    if digits == 0 || !rest[..digits].starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    let unit = &rest[digits..];
    // `12s`, or minutes first as in `1m 5s`.
    let unit = unit.strip_prefix('m').map(|after| after.trim_start()).map_or(unit, |after| {
        after.trim_start_matches(|c: char| c.is_ascii_digit())
    });
    unit.strip_prefix('s')
        .is_some_and(|after| after.is_empty() || after.starts_with([' ', ')', '·']))
}

/// Harness-specific in-progress indicators that are not spinner lines.
pub(in crate::tmux) fn agent_screen_indicates_work(agent_type: &str, screen: &str, height: usize) -> bool {
    if screen_indicates_work(screen, height) {
        return true;
    }
    match agent_type {
        // Hermes (classic `--cli`) replaces its empty composer's placeholder
        // while a turn runs, and marks a live turn's status-bar timer with ⏱.
        "hermes" => {
            let plain = strip_terminal_escapes(screen);
            let lines: Vec<&str> = recent_status_lines(&plain, height).collect();
            lines.iter().any(|line| {
                let composer = line.trim_start_matches(['⚕', ' ']);
                composer.strip_prefix('❯').is_some_and(|rest| {
                    rest.trim_start().starts_with("msg=interrupt · /queue")
                }) || (line.starts_with('⚕') && line.contains('│') && line.contains("│ ⏱ "))
            })
        }
        "dsh" => {
            let plain = strip_terminal_escapes(screen);
            let working = recent_status_lines(&plain, height).any(dsh_line_indicates_work);
            working
        }
        _ => false,
    }
}

/// Detect explicit confirmation prompts Aider shows when waiting for the user.
pub(in crate::tmux) fn aider_screen_waiting(screen: &str, height: usize) -> bool {
    let plain = strip_terminal_escapes(screen);
    let lines: Vec<&str> = recent_status_lines(&plain, height).collect();
    lines
        .into_iter()
        .rev()
        .take(4)
        .any(|raw| {
            let lower = raw.trim().to_ascii_lowercase();
            lower.contains("(y)es/(n)o")
                || lower.contains("[y/n]")
                || lower.contains("[y/n/a/d]")
                || (lower.contains("add ") && lower.contains(" to the chat?"))
                || lower.contains("run shell command?")
                || lower.contains("create new file?")
                || lower.contains("apply edit?")
        })
}

/// Spinner verbs dsh-TUI 0.11.0 picks one of per turn (`SPINNER_VERBS`), in
/// its English and Chinese UI (`spinner-verb-*`).
pub(in crate::tmux) const DSH_SPINNER_VERBS: &[&str] = &[
    "Analyzing", "Thinking", "Working", "Considering", "Reviewing", "Planning",
    "Checking", "Reading", "Searching", "Building", "Testing", "Connecting",
    "Preparing", "Exploring", "Reasoning", "Summarizing", "Resolving", "Responding",
    "分析中", "思考中", "工作中", "斟酌中", "审阅中", "规划中", "检查中", "读取中", "检索中",
    "构建中", "测试中", "连接中", "准备中", "探索中", "推理中", "总结中", "解析中", "回应中",
];

/// One trimmed screen line of dsh-TUI while a turn runs. Its status bar's hint
/// row reads `esc to interrupt` (already a generic signal) or, in its Chinese
/// UI, `esc 中断`. The working row above the composer is a breathing dot
/// (`·`/`•`/`●`), a spinner verb and `…`, then nothing (narrow card) or the
/// parenthesised elapsed time / token count / `thinking`:
/// `● Thinking… (3s · ↓ 12 tokens)`. Both rows vanish when the turn ends.
pub(in crate::tmux) fn dsh_line_indicates_work(line: &str) -> bool {
    if line.starts_with("esc 中断") {
        return true;
    }
    let Some(rest) = line.strip_prefix(['·', '•', '●']) else {
        return false;
    };
    let rest = rest.trim_start();
    DSH_SPINNER_VERBS.iter().any(|verb| {
        let Some(status) = rest.strip_prefix(verb).and_then(|after| after.strip_prefix('…')) else {
            return false;
        };
        let status = status.trim_start();
        status.is_empty()
            || status.strip_prefix('(').is_some_and(|inner| {
                inner.starts_with(|c: char| c.is_ascii_digit() || c == '↑' || c == '↓')
                    || inner.starts_with("thinking")
                    || inner.starts_with("thought for")
            })
    })
}

pub(in crate::tmux) fn resolve_effective_pid(pid_num: u32, is_shell_agent: bool) -> u32 {
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

pub(in crate::tmux) fn inspect_status_impl(
    session_name: &str,
    agent_type: &str,
    screen: Option<&str>,
) -> SessionStatus {
    let listing = tmux_stdout(&[
        "list-panes",
        "-t",
        session_name,
        "-F",
        "#{pane_pid}::#{pane_current_command}::#{pane_dead}::#{pane_height}::#{pane_current_path}",
    ]);
    if let Some(first_line) = listing.as_deref().and_then(|text| text.lines().next()) {
        let mut parts = first_line.splitn(5, "::");
        let row = PaneRow {
            pid: parts.next().unwrap_or("").trim().to_string(),
            cmd: parts.next().unwrap_or("").trim().to_string(),
            dead: parts.next().unwrap_or("0").trim() == "1",
            height: parts
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(24),
            cwd: parts.next().unwrap_or("").trim().to_string(),
            ..Default::default()
        };
        return status_for_pane(
            session_name,
            agent_type,
            &row,
            &|| crate::harness_metadata::inspect(session_name, agent_type),
            &mut || match screen {
                Some(screen) => Some(screen.to_string()),
                None => capture_visible_screen(session_name),
            },
        );
    }
    exited_status(agent_type)
}

/// Status of a session whose pane tmux no longer lists.
pub fn exited_status(agent_type: &str) -> SessionStatus {
    SessionStatus {
        status: "EXITED",
        label: "○ EXITED",
        pid: "-".to_string(),
        cmd: agent_type.to_string(),
        cwd: String::new(),
    }
}

/// Derive a card/bridge status from one already listed pane.
///
/// `metadata` and `screen` are only called when this agent's status depends
/// on them, so a batched caller can skip reading native metadata or capturing
/// the pane for everything else.
pub fn status_for_pane(
    session_name: &str,
    agent_type: &str,
    row: &PaneRow,
    metadata: &dyn Fn() -> Option<crate::harness_metadata::Metadata>,
    screen: &mut dyn FnMut() -> Option<String>,
) -> SessionStatus {
    let pid = row.pid.clone();
    let cmd = row.cmd.clone();
    let cwd = row.cwd.clone();
    let p_num = pid.parse::<u32>().unwrap_or(0);
    if row.dead || pid.is_empty() || (p_num != 0 && !crate::platform::process::exists(p_num)) {
        return SessionStatus { status: "EXITED", label: "○ EXITED", pid, cmd, cwd };
    }

    let is_shell_agent = crate::shell_title::is_regular(agent_type);
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

    // Codex's own rollout belongs to this exact pane process. Explicit
    // turn events override stale screen text and long-lived tool helpers.
    if agent_type == "codex" && p_num != 0 {
        let completion = crate::completion::inspect(session_name, p_num);
        if let Some((status, label)) = explicit_turn_status(&completion.state) {
            return SessionStatus { status, label, pid: display_pid, cmd: display_cmd, cwd };
        }
        if completion.supported {
            return SessionStatus { status: "UNKNOWN", label: "? UNKNOWN", pid: display_pid, cmd: display_cmd, cwd };
        }
    }

    if agent_type == "grok" && p_num != 0 {
        if let Some((raw, _)) = crate::grok_session::inspect_status(p_num, Some(&cwd)) {
            if let Some((status, label)) = crate::harness_record::status(raw) {
                return SessionStatus { status, label, pid: display_pid, cmd: display_cmd, cwd };
            }
        }
    }

    if crate::harness_metadata::has_native_metadata(agent_type) {
        // An adapter that never reported (an OpenClaw gateway without our
        // plugin, hooks that did not run) has no status to give: use the
        // screen like any other launcher rather than a permanent UNKNOWN.
        if let Some(metadata) = metadata().filter(|m| m.adapter_reported()) {
            if !(metadata.agent == "antigravity" && metadata.status == "unknown") {
                if let Some((status, label)) = crate::harness_metadata::status(&metadata.status) {
                    return SessionStatus { status, label, pid: display_pid, cmd: display_cmd, cwd };
                }
            }
        }
    }

    let is_shell_cmd = matches!(cmd.as_str(), "bash" | "zsh" | "fish" | "sh");
    if is_shell_agent {
        #[cfg(target_os = "linux")]
        let foreground = std::fs::read_to_string(format!("/proc/{p_num}/stat"))
            .map(|stat| foreground_job_from_stat(&stat)).unwrap_or(false);
        #[cfg(target_os = "macos")]
        let foreground = crate::platform::process::has_foreground_job(p_num);
        let busy = foreground || (!is_shell_cmd && !cmd.is_empty());
        return working_or_idle(busy, display_pid, display_cmd, cwd);
    }

    // Other agents use conservative visible status indicators. Child
    // process existence alone says nothing about a response in progress.
    let captured = screen();
    if agent_type == "aider"
        && captured
            .as_deref()
            .is_some_and(|text| aider_screen_waiting(text, row.height))
    {
        return SessionStatus {
            status: "WAITING",
            label: "◌ WAITING",
            pid: display_pid,
            cmd: display_cmd,
            cwd,
        };
    }
    let busy = captured
        .as_deref()
        .is_some_and(|text| agent_screen_indicates_work(agent_type, text, row.height));
    working_or_idle(busy, display_pid, display_cmd, cwd)
}

pub(in crate::tmux) fn working_or_idle(busy: bool, pid: String, cmd: String, cwd: String) -> SessionStatus {
    let (status, label) = if busy { ("WORKING", "● WORKING") } else { ("IDLE", "● IDLE") };
    SessionStatus { status, label, pid, cmd, cwd }
}

