//! Curated per-harness slash commands and fuzzy matching for the Command Palette.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub command: &'static str,
    pub description: &'static str,
    pub submit: bool,
}

const CLAUDE_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/clear", description: "Clear conversation history", submit: true },
    SlashCommand { command: "/compact", description: "Compact conversation context", submit: true },
    SlashCommand { command: "/model", description: "Switch model", submit: true },
    SlashCommand { command: "/resume", description: "Resume a previous session", submit: true },
    SlashCommand { command: "/cost", description: "Show session token usage and cost", submit: true },
    SlashCommand { command: "/context", description: "Inspect context window usage", submit: true },
    SlashCommand { command: "/status", description: "Show Claude session status", submit: true },
    SlashCommand { command: "/diff", description: "Review pending changes", submit: true },
    SlashCommand { command: "/help", description: "Show Claude slash commands", submit: true },
];

const CODEX_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/model", description: "Switch model and reasoning effort", submit: true },
    SlashCommand { command: "/new", description: "Start a fresh thread", submit: true },
    SlashCommand { command: "/compact", description: "Compact conversation history", submit: true },
    SlashCommand { command: "/diff", description: "Show git diff for this thread", submit: true },
    SlashCommand { command: "/status", description: "Show thread status and limits", submit: true },
    SlashCommand { command: "/approvals", description: "Configure approval policy", submit: true },
    SlashCommand { command: "/help", description: "Show Codex commands", submit: true },
];

const ANTIGRAVITY_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/resume", description: "Pick a previous conversation", submit: true },
    SlashCommand { command: "/model", description: "Switch model", submit: true },
    SlashCommand { command: "/clear", description: "Start a clean conversation", submit: true },
    SlashCommand { command: "/compact", description: "Compact context", submit: true },
    SlashCommand { command: "/status", description: "Show session status", submit: true },
    SlashCommand { command: "/help", description: "Show help", submit: true },
];

const OPENCODE_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/models", description: "Switch active model", submit: true },
    SlashCommand { command: "/sessions", description: "Switch or browse sessions", submit: true },
    SlashCommand { command: "/new", description: "Start a new session", submit: true },
    SlashCommand { command: "/compact", description: "Summarize conversation", submit: true },
    SlashCommand { command: "/undo", description: "Undo last turn", submit: true },
    SlashCommand { command: "/redo", description: "Redo undone turn", submit: true },
    SlashCommand { command: "/share", description: "Share session transcript", submit: true },
    SlashCommand { command: "/help", description: "Show OpenCode commands", submit: true },
];

const GROK_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/minimal", description: "Toggle minimal scrollback mode", submit: true },
    SlashCommand { command: "/model", description: "Switch Grok model", submit: true },
    SlashCommand { command: "/clear", description: "Clear current chat", submit: true },
    SlashCommand { command: "/resume", description: "Resume a previous session", submit: true },
    SlashCommand { command: "/status", description: "Show session info", submit: true },
    SlashCommand { command: "/help", description: "Show Grok CLI commands", submit: true },
];

const HERMES_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/model", description: "Switch model", submit: true },
    SlashCommand { command: "/new", description: "Reset session", submit: true },
    SlashCommand { command: "/queue ", description: "Queue a follow-up message", submit: false },
    SlashCommand { command: "/status", description: "Show Hermes session status", submit: true },
    SlashCommand { command: "/skills", description: "List loaded skills", submit: true },
    SlashCommand { command: "/help", description: "Show Hermes commands", submit: true },
];

const PI_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/model", description: "Select model and provider", submit: true },
    SlashCommand { command: "/tree", description: "Browse conversation tree", submit: true },
    SlashCommand { command: "/compact", description: "Compact conversation", submit: true },
    SlashCommand { command: "/name ", description: "Set session display name", submit: false },
    SlashCommand { command: "/resume", description: "Switch session", submit: true },
    SlashCommand { command: "/settings", description: "Open Pi settings", submit: true },
    SlashCommand { command: "/help", description: "Show Pi commands", submit: true },
];

const OPENCLAW_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/status", description: "Show gateway & session status", submit: true },
    SlashCommand { command: "/model", description: "Switch model", submit: true },
    SlashCommand { command: "/new", description: "Start a fresh session", submit: true },
    SlashCommand { command: "/reset", description: "Reset current session", submit: true },
    SlashCommand { command: "/help", description: "Show OpenClaw commands", submit: true },
];

const AIDER_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/add ", description: "Add file(s) to chat", submit: false },
    SlashCommand { command: "/drop ", description: "Remove file(s) from chat", submit: false },
    SlashCommand { command: "/diff", description: "Show changes from last edit", submit: true },
    SlashCommand { command: "/undo", description: "Undo last git commit by Aider", submit: true },
    SlashCommand { command: "/run ", description: "Run a shell command", submit: false },
    SlashCommand { command: "/test ", description: "Run test command", submit: false },
    SlashCommand { command: "/model ", description: "Switch LLM model", submit: false },
    SlashCommand { command: "/clear", description: "Clear chat history", submit: true },
    SlashCommand { command: "/help", description: "Show Aider commands", submit: true },
];

const CRUSH_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/model", description: "Switch model", submit: true },
    SlashCommand { command: "/new", description: "Start new session", submit: true },
    SlashCommand { command: "/sessions", description: "Switch sessions", submit: true },
    SlashCommand { command: "/compact", description: "Compact conversation", submit: true },
    SlashCommand { command: "/help", description: "Show Crush commands", submit: true },
];

const REASONIX_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/model", description: "Select Reasonix model", submit: true },
    SlashCommand { command: "/clear", description: "Clear conversation", submit: true },
    SlashCommand { command: "/compact", description: "Compact history", submit: true },
    SlashCommand { command: "/status", description: "Show session status", submit: true },
    SlashCommand { command: "/help", description: "Show Reasonix commands", submit: true },
];

const DSH_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/settings", description: "Open DeepSeek Harness settings", submit: true },
    SlashCommand { command: "/model", description: "Switch model", submit: true },
    SlashCommand { command: "/clear", description: "Clear conversation", submit: true },
    SlashCommand { command: "/help", description: "Show commands", submit: true },
];

const GOOSE_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/clear", description: "Reset conversation history", submit: true },
    SlashCommand { command: "/mode", description: "Change Goose mode", submit: true },
    SlashCommand { command: "/plan", description: "Create or view execution plan", submit: true },
    SlashCommand { command: "/exit", description: "Exit Goose session", submit: true },
    SlashCommand { command: "/help", description: "Show Goose commands", submit: true },
];

const AMP_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/new", description: "Start a new Amp thread", submit: true },
    SlashCommand { command: "/model", description: "Switch model mode", submit: true },
    SlashCommand { command: "/compact", description: "Compact thread context", submit: true },
    SlashCommand { command: "/help", description: "Show Amp commands", submit: true },
];

const COPILOT_COMMANDS: &[SlashCommand] = &[
    SlashCommand { command: "/model", description: "Select model", submit: true },
    SlashCommand { command: "/clear", description: "Clear current session", submit: true },
    SlashCommand { command: "/compact", description: "Compact context", submit: true },
    SlashCommand { command: "/diff", description: "Review pending changes", submit: true },
    SlashCommand { command: "/help", description: "Show Copilot CLI commands", submit: true },
];

pub fn commands_for(agent: &str) -> &'static [SlashCommand] {
    match agent {
        "claude" => CLAUDE_COMMANDS,
        "codex" => CODEX_COMMANDS,
        "antigravity" | "agy" => ANTIGRAVITY_COMMANDS,
        "opencode" => OPENCODE_COMMANDS,
        "grok" => GROK_COMMANDS,
        "hermes" => HERMES_COMMANDS,
        "pi" => PI_COMMANDS,
        "openclaw" => OPENCLAW_COMMANDS,
        "aider" => AIDER_COMMANDS,
        "crush" => CRUSH_COMMANDS,
        "reasonix" => REASONIX_COMMANDS,
        "dsh" => DSH_COMMANDS,
        "goose" => GOOSE_COMMANDS,
        "amp" => AMP_COMMANDS,
        "copilot" => COPILOT_COMMANDS,
        _ => &[],
    }
}

/// Case-insensitive subsequence matcher prioritizing exact prefixes, word
/// boundaries, and contiguous runs. Empty queries score 0 (everything matches).
pub fn fuzzy_score(query: &str, haystack: &str) -> Option<i32> {
    let q_trimmed = query.trim();
    if q_trimmed.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = q_trimmed.to_ascii_lowercase().chars().collect();
    let h: Vec<char> = haystack.to_ascii_lowercase().chars().collect();
    if q.len() > h.len() {
        return None;
    }

    let mut score: i32 = 0;
    let mut hi = 0usize;
    let mut consecutive = 0i32;
    for (qi, &qc) in q.iter().enumerate() {
        let mut found = None;
        while hi < h.len() {
            if h[hi] == qc {
                found = Some(hi);
                hi += 1;
                break;
            }
            hi += 1;
        }
        let idx = found?;
        score += 10;
        if idx == qi {
            score += 20;
        }
        let boundary = idx == 0
            || matches!(h[idx - 1], ' ' | '/' | '-' | '_' | '·' | '.');
        if boundary {
            score += 15;
        }
        consecutive = if qi > 0 && idx > 0 && h[idx - 1] == q[qi - 1] {
            consecutive + 1
        } else {
            0
        };
        score += consecutive * 12;
    }
    if h.starts_with(&q) {
        score += 50;
    }
    Some(score - (h.len() as i32).min(60) / 4)
}

pub fn send(session: &str, agent: &str, cmd: &SlashCommand) {
    let session = session.to_string();
    let agent = agent.to_string();
    let text = cmd.command;
    let submit = cmd.submit;
    std::thread::spawn(move || {
        let _ = crate::tmux::send_keys(&session, &agent, text, submit);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_score_ranks_prefix_and_word_boundaries_and_rejects_misses() {
        let exact = fuzzy_score("/comp", "/compact").unwrap();
        let word = fuzzy_score("comp", "Run /compact command").unwrap();
        let loose = fuzzy_score("comp", "c o m p x").unwrap();
        assert!(exact > word, "exact={exact} word={word}");
        assert!(word > loose, "word={word} loose={loose}");
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_score("zzz", "/compact"), None);
    }

    #[test]
    fn all_supported_ai_harnesses_have_slash_commands_and_shell_is_empty() {
        for agent in [
            "claude", "codex", "antigravity", "agy", "opencode", "grok", "hermes",
            "pi", "openclaw", "aider", "crush", "reasonix", "dsh", "goose", "amp", "copilot",
        ] {
            let cmds = commands_for(agent);
            assert!(!cmds.is_empty(), "expected commands for {agent}");
            assert!(cmds.iter().all(|c| c.command.starts_with('/')), "{agent}");
        }
        assert!(commands_for("shell").is_empty());
    }
}
