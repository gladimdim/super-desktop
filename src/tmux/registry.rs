use super::*;

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

/// A harness run from `commands` with `default_args`, with no npx fallback.
pub(in crate::tmux) const fn agent(
    name: &'static str,
    icon: &'static str,
    commands: &'static [&'static str],
    default_args: &'static [&'static str],
) -> AgentConfig {
    AgentConfig { name, icon, commands, default_args, npx_package: None }
}

pub fn get_agent_config(agent_type: &str) -> AgentConfig {
    match agent_type {
        "antigravity" | "agy" => agent("Antigravity", "🌌", &["agy", "antigravity"], &["--dangerously-skip-permissions"]),
        "claude" => agent("Claude Code", "⚡", &["claude"], &["--dangerously-skip-permissions"]),
        "codex" => agent("OpenAI Codex", "🤖", &["codex"], &["--no-alt-screen", "--dangerously-bypass-approvals-and-sandbox"]),
        "opencode" => agent("OpenCode", "🔮", &["opencode"], &["--mini", "--auto"]),
        // Native scrollback is required by the mobile snapshot viewer.
        // Fullscreen Grok keeps history inside its alternate-screen UI.
        "grok" => agent("Grok CLI", "🚀", &["grok"], &["--minimal", "--dangerously-skip-permissions"]),
        "aider" => agent("Aider", "🧠", &["aider"], &["--yes-always"]),
        "gemini" => agent("Gemini CLI", "✦", &["gemini"], &[]),
        "hermes" => agent("Hermes Agent", "🪽", &["hermes"], &["--cli"]),
        "pi" => agent("Pi", "🥧", &["pi"], &["--tui-mode", "regular"]),
        "openclaw" => agent("OpenClaw", "🦞", &["openclaw"], &["tui"]),
        "goose" => agent("Goose", "🪿", &["goose"], &["session"]),
        "qwen" => agent("Qwen Code", "🌟", &["qwen"], &[]),
        "crush" => agent("Crush", "💘", &["crush"], &[]),
        "kimi" => agent("Kimi Code", "🌙", &["kimi"], &[]),
        "kiro" => agent("Kiro CLI", "🧰", &["kiro-cli"], &[]),
        "cursor" => agent("Cursor Agent", "🎯", &["cursor-agent"], &[]),
        // Useful alongside terminal agents, but not an interactive
        // coding-agent chat. Keep its actual role visible.
        "herder" => agent("Herder worker", "🐑", &["herder"], &["worker"]),
        // `code` opens Reasonix' interactive coding session. Deliberately no
        // permission flag: Reasonix keeps its own `workspace-write` sandbox
        // (in-workspace writes approved, everything else asked in the card).
        "reasonix" => AgentConfig {
            npx_package: Some("reasonix"),
            ..agent("Reasonix", "🧭", &["reasonix"], &["code"])
        },
        // DeepSeek Harness (`dsh`) ships no terminal UI of its own; `dsh-tui`
        // (alias `dst`, both `dsh --profile dsh-tui`) is its community TUI.
        // No default arguments: permissions come from the dsh profile's
        // presets and there is no bypass flag. No npx fallback either: it
        // needs the global `@deepseek-ai/dsh` install and its dsh-tui profile.
        "dsh" => agent("DeepSeek Harness", "🐋", &["dsh-tui", "dst"], &[]),
        _ => agent("Terminal", "💻", &["bash"], &[]),
    }
}

/// `npx -y <package> <args…>`. `-y` keeps npx from stopping on its
/// "Ok to proceed?" install prompt the first time the package is fetched.
pub(in crate::tmux) fn npx_fallback_command<S: AsRef<str>>(package: &str, args: &[S]) -> String {
    let mut parts = vec!["npx".to_string(), "-y".to_string(), package.to_string()];
    parts.extend(args.iter().map(|a| crate::launch_args::quote(a.as_ref())));
    parts.join(" ")
}

/// A session name of our own `sd_term_*` shape, safe to use in a tmux target
/// and as a file name.
pub fn is_owned_session(session: &str) -> bool {
    session.starts_with("sd_term_")
        && session.len() <= 128
        && session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
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
    "dsh",
    "aider",
    "hermes",
    "pi",
    "openclaw",
    "goose",
    "qwen",
    "crush",
    "kimi",
    "kiro",
    "cursor",
    "herder",
    // Retired (see RETIRED_HARNESSES): kept launchable, listed last.
    "gemini",
    "shell",
];

/// Harnesses whose upstream is no longer maintained, with their successor.
/// They still launch when installed, but fresh installs never pick them for
/// the top bar and `state::retire_harnesses` swaps them for the successor once.
pub const RETIRED_HARNESSES: &[(&str, &str)] = &[
    // Google moved individual developers from Gemini CLI to Antigravity CLI.
    ("gemini", "antigravity"),
];

/// The successor of a retired harness, e.g. `"antigravity"` for `"gemini"`.
pub fn retired_successor(key: &str) -> Option<&'static str> {
    RETIRED_HARNESSES.iter().find(|(old, _)| *old == key).map(|(_, new)| *new)
}

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

/// `which <cmd>` → absolute path, or `None` when it is not installed.
///
/// Done in-process instead of shelling out: harness detection alone asks ~12
/// times per window build, and forking `which` costs tens of milliseconds on a
/// loaded machine — that dominated the time between the shortcut and the
/// overlay appearing.
pub(crate) fn which(cmd: &str) -> Option<String> {
    if cmd.is_empty() {
        return None;
    }
    if cmd.contains('/') {
        let p = Path::new(cmd);
        return is_executable(p).then(|| cmd.to_string());
    }
    std::env::var_os("PATH")
        .and_then(|path| which_in_path(cmd, &path))
        .or_else(|| std::env::var_os("HOME")
            .and_then(|home| which_in_common_user_bins(cmd, Path::new(&home))))
}

/// Desktop launchers inherit a PATH captured when the user session started.
/// Check common per-user installer locations too, so a fresh CLI can be found
/// by Rescan without logging out or restarting the daemon.
pub(in crate::tmux) fn which_in_common_user_bins(cmd: &str, home: &Path) -> Option<String> {
    const USER_BINS: &[&str] = &[
        ".local/bin", ".cargo/bin", ".bun/bin", ".npm-global/bin",
        ".volta/bin", ".local/share/mise/shims",
    ];
    USER_BINS.iter().find_map(|dir| {
        let candidate = home.join(dir).join(cmd);
        is_executable(&candidate).then(|| candidate.to_string_lossy().into_owned())
    })
}

/// PATH lookup split out of `which` so it can be tested without mutating the
/// process-wide environment (which other tests spawn processes from).
pub(in crate::tmux) fn which_in_path(cmd: &str, path: &std::ffi::OsStr) -> Option<String> {
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(cmd);
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

pub(in crate::tmux) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // `symlink_metadata` first is pointless: a broken symlink must report false.
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

pub(in crate::tmux) fn with_default_args<S: AsRef<str>>(path: &str, args: &[S]) -> String {
    std::iter::once(path.to_string())
        .chain(args.iter().map(|arg| crate::launch_args::quote(arg.as_ref())))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Command to launch `key` here, or `None` when nothing resolves — i.e. the
/// harness is not available on this machine.
///
/// Mirrors `resolve_command`'s order (PATH → `npx` package → shell) but keeps
/// the "nothing found" case instead of falling back to `$SHELL`, so the
/// settings panel only ever offers harnesses that really exist.
pub fn detect_harness_command(key: &str) -> Option<String> {
    harness_command_with(key, &crate::launch_args::effective(key))
}

/// `detect_harness_command` with `args` as the starting parameters, for the
/// settings preview of parameters that are not saved yet.
pub fn harness_command_with(key: &str, args: &[String]) -> Option<String> {
    let cfg = get_agent_config(key);

    if let Some(path) = first_installed(harness_candidates(key), which) {
        return Some(with_default_args(&path, args));
    }
    // Harnesses that commonly run through `npx` (Reasonix) are available as
    // soon as npx is, exactly like `resolve_command` assumes.
    if let Some(package) = cfg.npx_package {
        if which("npx").is_some() {
            return Some(npx_fallback_command(package, args));
        }
    }
    if key == "shell" {
        if let Ok(shell) = std::env::var("SHELL") {
            if std::path::Path::new(&shell).is_file() {
                return Some(with_default_args(&shell, args));
            }
        }
    }
    None
}

/// Absolute path of the installed binary for `key` (PATH, then common user
/// bin folders), without its starting parameters. For helper subcommands such
/// as `openclaw plugins install`, which must run the same CLI a card would.
pub fn harness_binary(key: &str) -> Option<String> {
    first_installed(harness_candidates(key), which)
}

/// The first of `candidates` that `lookup` resolves: preference order wins
/// over whatever else is installed (DeepSeek Harness: `dsh-tui`, then `dst`).
pub(in crate::tmux) fn first_installed(candidates: &[&str], lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    candidates.iter().find_map(|cmd| lookup(cmd))
}

/// Binaries that can serve `key`, in preference order.
///
/// A terminal card is happy with any POSIX shell this box actually has; the
/// other harnesses only accept their own binary.
pub(in crate::tmux) fn harness_candidates(key: &str) -> &'static [&'static str] {
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

/// The command a card for `agent_type` runs: `custom` (a saved card command
/// or a custom launcher) with any missing starting parameters added, or the
/// installed binary with them. Starting parameters are the user's saved ones
/// from Settings, else the built-in defaults (see `launch_args`).
pub fn resolve_command(agent_type: &str, custom: Option<&str>) -> String {
    resolve_command_with(agent_type, custom, &crate::launch_args::effective(agent_type))
}

pub(in crate::tmux) fn resolve_command_with(agent_type: &str, custom: Option<&str>, args: &[String]) -> String {
    let cfg = get_agent_config(agent_type);
    if let Some(cmd) = custom {
        let trimmed = cmd.trim();
        if !trimmed.is_empty() && !is_shell_command(trimmed) {
            return with_missing_default_args(agent_type, trimmed, args);
        }
        return trimmed.to_string();
    }
    if let Some(path) = first_installed(cfg.commands, which) {
        return with_default_args(&path, args);
    }
    // Harness that ships on npm but has no binary on PATH (e.g. Reasonix is
    // usually run as `npx reasonix code`): launch it through npx rather than
    // silently degrading the card to a bare shell.
    if let Some(package) = cfg.npx_package {
        if which("npx").is_some() {
            return npx_fallback_command(package, args);
        }
    }
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
}

// Permission flags already present must not suppress a missing display-mode
// default. Preserve explicit user mode overrides and never append to shell syntax.
pub(in crate::tmux) fn with_missing_default_args(agent: &str, command: &str, defaults: &[String]) -> String {
    let Some(words) = shlex::split(command) else { return command.to_string() };
    if words.iter().any(|word| matches!(word.as_str(), "|" | "||" | "&&" | ";")) {
        return command.to_string();
    }
    let has = |flag: &str| words.iter().any(|word| word == flag || word.starts_with(&format!("{flag}=")));
    let mut result = command.to_string();
    let mut i = 0;
    while i < defaults.len() {
        let flag = defaults[i].as_str();
        let value = defaults.get(i + 1).filter(|value| !value.starts_with('-'));
        let overridden = (agent == "grok" && flag == "--minimal" && has("--fullscreen"))
            || (agent == "hermes" && flag == "--cli" && has("--tui"));
        if !has(flag) && !overridden {
            result.push(' ');
            result.push_str(&crate::launch_args::quote(flag));
            if let Some(value) = value {
                result.push(' ');
                result.push_str(&crate::launch_args::quote(value));
            }
        }
        i += if value.is_some() { 2 } else { 1 };
    }
    result
}

pub(in crate::tmux) fn is_shell_command(cmd: &str) -> bool {
    let base = cmd
        .split_whitespace()
        .next()
        .and_then(|s| s.rsplit('/').next())
        .unwrap_or("");
    matches!(base, "bash" | "zsh" | "fish" | "sh" | "dash")
}

