//! Launch options for the Antigravity CLI.

use std::{
    path::Path,
    process::{Command, Stdio},
};

const INLINE_SETTINGS: &str = r#"{"altScreenMode":"never","clearScrollbackOnResize":false}"#;

pub fn is_launcher(agent: &str) -> bool {
    matches!(agent, "agy" | "antigravity")
}

/// Recognize a direct interactive CLI invocation without changing shell wrappers.
pub fn interactive_args(command: &str) -> Option<Vec<String>> {
    if command.contains(['$', '`', '\n', ';', '|', '&', '<', '>', '#', '\\']) {
        return None;
    }
    let args = shlex::split(command)?;
    let executable = Path::new(args.first()?).file_name()?.to_str()?;
    if !is_launcher(executable)
        || args.get(1).is_some_and(|arg| !arg.starts_with('-'))
        || args.iter().skip(1).any(|arg| {
            matches!(
                arg.split('=').next().unwrap_or_default(),
                "-p" | "--print" | "--prompt" | "--version" | "-v" | "--help" | "-h" | "--"
            )
        })
    {
        return None;
    }
    Some(args)
}

/// Apply inline rendering to this launch, when the CLI supports session
/// overrides. A user's explicit settings and older CLIs keep their own mode.
pub fn launch_command(agent: &str, command: &str) -> String {
    with_inline_rendering(agent, command, supports_settings)
}

fn with_inline_rendering(
    agent: &str,
    command: &str,
    supports_settings: impl FnOnce(&str) -> bool,
) -> String {
    if !is_launcher(agent) && !agent.starts_with("custom-") {
        return command.into();
    }
    let Some(args) = interactive_args(command) else {
        return command.into();
    };
    if args
        .iter()
        .any(|arg| arg == "--settings" || arg.starts_with("--settings="))
        || !supports_settings(&args[0])
    {
        return command.into();
    }
    format!("{command} --settings '{INLINE_SETTINGS}'")
}

fn supports_settings(executable: &str) -> bool {
    // agy writes --help to stderr. Merging it this way lets the bounded
    // subprocess reader keep older or misidentified executables from blocking.
    let mut probe = Command::new("sh");
    probe
        .args([
            "-c",
            "exec \"$1\" --help 2>&1",
            "super-desktop-agy",
            executable,
        ])
        .stdin(Stdio::null());
    let Some(output) = crate::workspace_model::bounded_output(probe) else {
        return false;
    };
    let help = String::from_utf8_lossy(&output);
    let has_flag = |flag| {
        help.lines()
            .any(|line| line.split_whitespace().next() == Some(flag))
    };
    has_flag("--settings") && has_flag("--conversation")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_inline_rendering_is_launch_scoped_and_capability_gated() {
        let command = "'/path with spaces/agy' --conversation=123";
        let result = with_inline_rendering("antigravity", command, |exe| {
            assert_eq!(exe, "/path with spaces/agy");
            true
        });
        assert_eq!(
            shlex::split(&result).unwrap(),
            [
                "/path with spaces/agy",
                "--conversation=123",
                "--settings",
                r#"{"altScreenMode":"never","clearScrollbackOnResize":false}"#
            ]
        );
        assert_eq!(
            with_inline_rendering("antigravity", command, |_| false),
            command
        );
        assert!(
            with_inline_rendering("custom-test", "antigravity", |_| true).contains("--settings")
        );
    }

    #[test]
    fn antigravity_leaves_custom_settings_and_noninteractive_commands_alone() {
        for command in [
            "agy --settings custom.json",
            "agy --settings=custom.json",
            "agy --print='one shot'",
            "agy --prompt='alias'",
            "agy acp",
            "agy --help",
            "agy; echo done",
            "wrapper agy",
        ] {
            assert_eq!(
                with_inline_rendering("antigravity", command, |_| panic!("unexpected probe")),
                command
            );
        }
    }
}
