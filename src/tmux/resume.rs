use super::*;

/// Whether the CLI will find conversation `id` when resumed in `cwd`.
///
/// Claude looks in the project directory named after the folder (every
/// character but ASCII letters and digits becomes `-`), under
/// `$CLAUDE_CONFIG_DIR` or `~/.claude`. Codex keeps
/// `sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl` under `$CODEX_HOME` or
/// `~/.codex`, whatever the folder.
pub(in crate::tmux) fn conversation_exists(agent: &str, id: &str, cwd: Option<&str>) -> bool {
    let home = || std::env::var_os("HOME").map(std::path::PathBuf::from);
    let dir = |variable: &str, default: &str| {
        std::env::var_os(variable)
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| home().map(|home| home.join(default)))
    };
    match agent {
        "claude" => {
            let (Some(root), Some(cwd)) = (crate::harness_record::claude_config_dir(), cwd) else {
                return false;
            };
            claude_transcript(&root, cwd, id).is_file()
        }
        "codex" => dir("CODEX_HOME", ".codex")
            .is_some_and(|root| codex_rollout_exists(&root.join("sessions"), id)),
        "pi" => dir("PI_CODING_AGENT_SESSION_DIR", ".pi/agent/sessions")
            .is_some_and(|root| pi_session_exists(&root, id)),
        "grok" => crate::grok_session::conversation_exists(id),
        "antigravity" | "agy" => crate::antigravity::conversation_exists(id),
        "hermes" => crate::hermes::conversation_exists(id),
        _ => false,
    }
}

fn pi_session_exists(sessions: &std::path::Path, id: &str) -> bool {
    let exact_name = format!("{id}.jsonl");
    let suffix = format!("_{id}.jsonl");
    std::fs::read_dir(sessions)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|dir| std::fs::read_dir(dir.path()).into_iter().flatten().flatten())
        .any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name == exact_name || name.ends_with(&suffix))
        })
}

pub(in crate::tmux) fn claude_transcript(root: &std::path::Path, cwd: &str, id: &str) -> std::path::PathBuf {
    let project: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    root.join("projects").join(project).join(format!("{id}.jsonl"))
}

pub(in crate::tmux) fn codex_rollout_exists(sessions: &std::path::Path, id: &str) -> bool {
    let suffix = format!("-{id}.jsonl");
    let children = |dir: &std::path::Path| {
        std::fs::read_dir(dir).into_iter().flatten().flatten().map(|entry| entry.path())
    };
    children(sessions)
        .flat_map(|year| children(&year).collect::<Vec<_>>())
        .flat_map(|month| children(&month).collect::<Vec<_>>())
        .flat_map(|day| children(&day).collect::<Vec<_>>())
        .any(|file| {
            file.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(&suffix))
        })
}

/// A harness conversation id that can be put on a command line as is:
/// Claude and Codex use UUIDs, OpenCode `ses_…`.
pub fn is_resumable_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && !id.starts_with('-')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(in crate::tmux) fn append_resume_flag(base: &str, flag: &str, markers: &[&str]) -> String {
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
/// Shell, Grok and unknown agents relaunch fresh. Antigravity does the same
/// because the card has no attributable conversation ID yet. Its explicit
/// resume command can select a supplied conversation without guessing.
/// An explicit shell command is never decorated with agent flags.
///
/// DeepSeek Harness (`dsh-tui`) relaunches fresh too. Its bare `--resume`
/// reads one global `~/.dsh-tui/resume.txt` (the last session any dsh-tui
/// process exited from, in any folder), not a session of the card's
/// workspace, so it would attach every restored card to that one session.
///
/// CLAUDE / CODEX ISOLATION: a card's own conversation id is stored like
/// OpenCode's (see `card_status::own_session`), and resumed exactly with
/// `claude --resume <id>` / `codex resume <id>`. Without that,
/// `--continue` / `resume --last` gave every card of a harness in one folder
/// the same, most recent conversation. A stored id is used only while its
/// conversation file is where the CLI looks for it (`conversation_exists`);
/// otherwise the card falls back to the latest conversation, as before,
/// rather than a harness that exits at once on an unknown id.
pub fn resolve_resume_command_in(
    agent_type: &str,
    custom: Option<&str>,
    agent_session_id: Option<&str>,
    cwd: Option<&str>,
) -> String {
    resume_command_with(agent_type, custom, agent_session_id, &|agent, id| {
        conversation_exists(agent, id, cwd)
    })
}

pub(in crate::tmux) fn resume_command_with(
    agent_type: &str,
    custom: Option<&str>,
    agent_session_id: Option<&str>,
    exists: &dyn Fn(&str, &str) -> bool,
) -> String {
    let base = resolve_command(agent_type, custom);
    if is_shell_command(&base) {
        return base;
    }
    // Exact per-card resume wins over "latest" heuristics.
    if agent_type == "opencode" {
        if let Some(id) = agent_session_id
            .map(str::trim)
            .filter(|s| opencode::is_valid_opencode_session_id(s))
        {
            if base.contains("--session") {
                return base;
            }
            let qid = crate::launch_args::quote(id);
            return format!("{base} --session {qid}");
        }
    }
    if let Some(id) = agent_session_id.map(str::trim).filter(|id| is_resumable_id(id)) {
        let qid = crate::launch_args::quote(id);
        match agent_type {
            "claude" if exists(agent_type, id) => {
                return append_resume_flag(&base, &format!("--resume {qid}"), &["--continue", "--resume"]);
            }
            // Global flags before the subcommand parse fine under clap.
            "codex" if exists(agent_type, id) => {
                return append_resume_flag(&base, &format!("resume {qid}"), &["resume"]);
            }
            "grok" if exists(agent_type, id) => {
                return append_resume_flag(&base, &format!("--resume {qid}"), &["--resume", "--continue", "-r", "-c"]);
            }
            "pi" if exists(agent_type, id) => {
                return append_resume_flag(&base, &format!("--session {qid}"), &["--session", "--session-id", "--continue", "--resume", "-c", "-r"]);
            }
            "antigravity" | "agy" if exists(agent_type, id) => {
                return append_resume_flag(&base, &format!("--conversation {qid}"), &["--conversation", "--continue", "--resume", "-c", "-r"]);
            }
            "hermes" if exists(agent_type, id) => {
                return append_resume_flag(&base, &format!("--resume {qid}"), &["--resume", "--continue", "-r", "-c"]);
            }
            "goose" => {
                return append_resume_flag(&base, &format!("--name {qid} --resume"), &["--name", "-n", "--resume", "-r"]);
            }
            _ => {}
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

