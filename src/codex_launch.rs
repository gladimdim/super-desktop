//! Launch options for Codex CLI.

use std::{
    collections::HashMap,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};

const OPTIONS_WITH_VALUES: &[&str] = &[
    "-c",
    "--config",
    "--enable",
    "--disable",
    "--remote",
    "--remote-auth-token-env",
    "-i",
    "--image",
    "-m",
    "--model",
    "--local-provider",
    "-p",
    "--profile",
    "-s",
    "--sandbox",
    "-C",
    "--cd",
    "--add-dir",
    "-a",
    "--ask-for-approval",
];

const NON_INTERACTIVE_SUBCOMMANDS: &[&str] = &[
    "exec",
    "e",
    "review",
    "login",
    "logout",
    "mcp",
    "plugin",
    "app-server",
    "remote-control",
    "completion",
    "update",
    "doctor",
    "sandbox",
    "debug",
    "apply",
    "a",
    "archive",
    "delete",
    "migrate-rollouts",
    "migrate-nfs-db",
    "unarchive",
    "cloud",
    "exec-server",
    "features",
    "agents",
    "queue",
    "help",
];

fn is_launcher(agent: &str) -> bool {
    agent == "codex"
}

/// Recognize a direct interactive CLI invocation without changing shell wrappers.
fn interactive_args(command: &str) -> Option<Vec<String>> {
    if command.contains(['$', '`', '\n', ';', '|', '&', '<', '>', '#', '\\']) {
        return None;
    }
    let args = shlex::split(command)?;
    let executable = Path::new(args.first()?).file_name()?.to_str()?;
    if !is_launcher(executable) {
        return None;
    }
    let mut i = 1;
    let mut checked_positional = false;
    while i < args.len() {
        let arg = args[i].as_str();
        let flag = arg.split('=').next().unwrap_or_default();
        if matches!(flag, "-h" | "--help" | "-V" | "--version" | "--") {
            return None;
        }
        if arg.starts_with('-') {
            if !arg.contains('=') && OPTIONS_WITH_VALUES.contains(&arg) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if !checked_positional {
            checked_positional = true;
            if NON_INTERACTIVE_SUBCOMMANDS.contains(&arg) {
                return None;
            }
        }
        i += 1;
    }
    Some(args)
}

fn user_configured_daemon(args: &[String]) -> bool {
    let tail = args.get(1..).unwrap_or(&[]);
    if tail.iter().any(|arg| {
        arg == "--no-daemon"
            || arg == "--remote"
            || arg.starts_with("--remote=")
            || arg == "--enable=daemon_auto_start"
            || ((arg.starts_with("-c=") || arg.starts_with("--config="))
                && arg.contains("daemon_auto_start"))
    }) {
        return true;
    }
    tail.windows(2).any(|pair| {
        (pair[0] == "--enable" && pair[1] == "daemon_auto_start")
            || (matches!(pair[0].as_str(), "-c" | "--config")
                && pair[1].contains("daemon_auto_start"))
    })
}

/// Apply `--no-daemon` to interactive Codex launches when the CLI supports it,
/// keeping per-card rollout files directly attached to the card's process.
pub fn launch_command(agent: &str, command: &str) -> String {
    with_no_daemon(agent, command, supports_no_daemon)
}

fn with_no_daemon(
    agent: &str,
    command: &str,
    supports: impl FnOnce(&str) -> bool,
) -> String {
    if !is_launcher(agent) && !agent.starts_with("custom-") {
        return command.into();
    }
    let Some(mut args) = interactive_args(command) else {
        return command.into();
    };
    if user_configured_daemon(&args) || !supports(&args[0]) {
        return command.into();
    }
    args.insert(1, "--no-daemon".into());
    args.iter()
        .map(|word| crate::launch_args::quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

type ProbeStamp = Option<(u64, i64, i64)>;

fn supports_no_daemon(executable: &str) -> bool {
    static CACHE: Mutex<Option<HashMap<PathBuf, (ProbeStamp, bool)>>> = Mutex::new(None);
    let resolved = crate::tmux::which(executable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(executable));
    let stamp = std::fs::metadata(&resolved)
        .ok()
        .map(|meta| (meta.len(), meta.mtime(), meta.mtime_nsec()));
    if let Ok(guard) = CACHE.lock() {
        if let Some(map) = guard.as_ref() {
            if let Some((cached_stamp, supported)) = map.get(&resolved) {
                if *cached_stamp == stamp {
                    return *supported;
                }
            }
        }
    }

    let mut probe = Command::new("sh");
    probe
        .args([
            "-c",
            "exec \"$1\" --help 2>&1",
            "super-desktop-codex",
            executable,
        ])
        .stdin(Stdio::null());
    let supported = crate::workspace_model::bounded_output(probe)
        .map(|output| {
            let help = String::from_utf8_lossy(&output);
            help.lines()
                .any(|line| line.split_whitespace().next() == Some("--no-daemon"))
        })
        .unwrap_or(false);

    if let Ok(mut guard) = CACHE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        if map.len() >= 32 {
            map.clear();
        }
        map.insert(resolved, (stamp, supported));
    }
    supported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_no_daemon_injects_on_interactive_and_resume_commands() {
        for (agent, input, expected) in [
            ("codex", "codex", vec!["codex", "--no-daemon"]),
            (
                "codex",
                "codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox",
                vec![
                    "codex",
                    "--no-daemon",
                    "--no-alt-screen",
                    "--dangerously-bypass-approvals-and-sandbox",
                ],
            ),
            (
                "codex",
                "codex resume 12345678-1234-1234-1234-123456789abc",
                vec![
                    "codex",
                    "--no-daemon",
                    "resume",
                    "12345678-1234-1234-1234-123456789abc",
                ],
            ),
            (
                "codex",
                "codex resume --last",
                vec!["codex", "--no-daemon", "resume", "--last"],
            ),
            (
                "codex",
                "codex fork",
                vec!["codex", "--no-daemon", "fork"],
            ),
            (
                "custom-codex",
                "'/path with spaces/codex' -m 'gpt test'",
                vec!["/path with spaces/codex", "--no-daemon", "-m", "gpt test"],
            ),
        ] {
            let got = with_no_daemon(agent, input, |_| true);
            assert_eq!(shlex::split(&got).unwrap(), expected, "{input}");
        }
        assert_eq!(with_no_daemon("codex", "codex", |_| false), "codex");
    }

    #[test]
    fn codex_no_daemon_respects_user_daemon_flags_and_non_interactive_modes() {
        for command in [
            "codex --no-daemon",
            "codex --no-alt-screen --no-daemon",
            "codex --remote ws://127.0.0.1:8080",
            "codex --remote=unix:///tmp/codex.sock",
            "codex --enable daemon_auto_start",
            "codex --enable=daemon_auto_start",
            "codex -c features.daemon_auto_start=true",
            "codex --config=features.daemon_auto_start=true",
            "codex exec 'hi'",
            "codex -m o3 app-server",
            "codex mcp list",
            "codex doctor",
            "codex --help",
            "sh -c codex",
            "codex && echo done",
        ] {
            assert_eq!(
                with_no_daemon("codex", command, |_| panic!("unexpected probe for {command}")),
                command,
                "{command}"
            );
        }
        assert_eq!(
            with_no_daemon("claude", "codex", |_| panic!("wrong agent probed")),
            "codex"
        );
    }
}
