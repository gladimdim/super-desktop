//! Regular terminal titles come from shell submission or foreground argv,
//! never from task output. Shell hooks also cover history and completion.
use std::{fs, path::Path, process::Command};

pub fn is_regular(agent: &str) -> bool {
    matches!(agent, "shell" | "bash" | "terminal")
}

/// Instrument plain Bash, Zsh, and Fish launches cleanly; explicit commands
/// and wrapped invocations keep their original invocation and use the
/// process/input fallback.
pub fn launch_command(agent: &str, command: &str) -> String {
    if !is_regular(agent) {
        return command.to_string();
    }
    let Some(home) = std::env::var_os("HOME") else {
        return command.to_string();
    };
    let state_dir = Path::new(&home).join(".local/state/super-desktop");
    match command {
        "bash" | "/bin/bash" | "/usr/bin/bash" => {
            let script = state_dir.join("shell-title.bash");
            if install_file(&state_dir, &script, include_bytes!("../assets/shell-title.bash")).is_err() {
                return command.to_string();
            }
            format!(
                "{command} --rcfile {}",
                crate::harness_metadata::shell_quote(&script.to_string_lossy())
            )
        }
        "zsh" | "/bin/zsh" | "/usr/bin/zsh" => {
            let zdotdir = state_dir.join("zsh");
            let zshenv = concat!(
                "if [[ -n \"$SD_ORIG_ZDOTDIR\" && -f \"$SD_ORIG_ZDOTDIR/.zshenv\" ]]; then\n",
                "    source \"$SD_ORIG_ZDOTDIR/.zshenv\"\n",
                "elif [[ -z \"$SD_ORIG_ZDOTDIR\" && -f \"$HOME/.zshenv\" ]]; then\n",
                "    source \"$HOME/.zshenv\"\n",
                "fi\n",
                "export ZDOTDIR=\"$SD_ZSH_STATE_DIR\"\n",
            );
            let zprofile = concat!(
                "if [[ -n \"$SD_ORIG_ZDOTDIR\" && -f \"$SD_ORIG_ZDOTDIR/.zprofile\" ]]; then\n",
                "    source \"$SD_ORIG_ZDOTDIR/.zprofile\"\n",
                "elif [[ -z \"$SD_ORIG_ZDOTDIR\" && -f \"$HOME/.zprofile\" ]]; then\n",
                "    source \"$HOME/.zprofile\"\n",
                "fi\n",
            );
            let zshrc = format!(
                concat!(
                    "if [[ -n \"$SD_ORIG_ZDOTDIR\" ]]; then\n",
                    "    export ZDOTDIR=\"$SD_ORIG_ZDOTDIR\"\n",
                    "    [[ -f \"$ZDOTDIR/.zshrc\" ]] && source \"$ZDOTDIR/.zshrc\"\n",
                    "else\n",
                    "    unset ZDOTDIR\n",
                    "    [[ -f \"$HOME/.zshrc\" ]] && source \"$HOME/.zshrc\"\n",
                    "fi\n",
                    "unset SD_ORIG_ZDOTDIR SD_ZSH_STATE_DIR\n",
                    "{}"
                ),
                include_str!("../assets/shell-title.zsh")
            );
            let zlogin = concat!(
                "if [[ -n \"${ZDOTDIR:-}\" && -f \"$ZDOTDIR/.zlogin\" ]]; then\n",
                "    source \"$ZDOTDIR/.zlogin\"\n",
                "elif [[ -f \"$HOME/.zlogin\" ]]; then\n",
                "    source \"$HOME/.zlogin\"\n",
                "fi\n",
            );
            if install_file(&zdotdir, &zdotdir.join(".zshenv"), zshenv.as_bytes()).is_err()
                || install_file(&zdotdir, &zdotdir.join(".zprofile"), zprofile.as_bytes()).is_err()
                || install_file(&zdotdir, &zdotdir.join(".zshrc"), zshrc.as_bytes()).is_err()
                || install_file(&zdotdir, &zdotdir.join(".zlogin"), zlogin.as_bytes()).is_err()
            {
                return command.to_string();
            }
            let zdir_str = zdotdir.to_string_lossy();
            let mut envs = Vec::new();
            if let Ok(orig) = std::env::var("ZDOTDIR") {
                if !orig.is_empty() && orig != zdir_str {
                    envs.push(crate::harness_metadata::shell_quote(&format!("SD_ORIG_ZDOTDIR={orig}")));
                }
            }
            envs.push(crate::harness_metadata::shell_quote(&format!("SD_ZSH_STATE_DIR={zdir_str}")));
            envs.push(crate::harness_metadata::shell_quote(&format!("ZDOTDIR={zdir_str}")));
            format!("env {} {command}", envs.join(" "))
        }
        "fish" | "/bin/fish" | "/usr/bin/fish" => {
            let script = state_dir.join("shell-title.fish");
            if install_file(&state_dir, &script, include_bytes!("../assets/shell-title.fish")).is_err() {
                return command.to_string();
            }
            let source_cmd = format!(
                "source {}",
                crate::harness_metadata::shell_quote(&script.to_string_lossy())
            );
            format!(
                "{command} -C {}",
                crate::harness_metadata::shell_quote(&source_cmd)
            )
        }
        _ => command.to_string(),
    }
}

fn install_file(directory: &Path, target: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if fs::read(target).ok().as_deref() == Some(content) {
        return Ok(());
    }
    crate::control::ensure_private_dir(directory)?;
    let nonce = crate::control::random_hex(8)
        .unwrap_or_else(|_| format!("{}", std::process::id()));
    let temporary = directory.join(format!(".shell-title-{nonce}.tmp"));
    let result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary)
        .and_then(|mut f| {
            f.write_all(content)?;
            f.flush()?;
            fs::rename(&temporary, target)
        });
    let _ = fs::remove_file(temporary);
    result
}

pub fn last(session: &str) -> Option<String> {
    let output = Command::new(crate::tmux::tmux_bin())
        .args([
            "display-message",
            "-p",
            "-t",
            &format!("={session}:"),
            "#{pane_pid}\n#{@super_desktop_shell_tracking}\n#{@super_desktop_shell_command}",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let metadata = String::from_utf8_lossy(&output.stdout);
    let mut fields = metadata.splitn(3, '\n');
    let pid = fields.next()?.trim().parse().ok()?;
    let tracked = fields.next()?.trim() == "1";
    let submitted = fields.next()?.trim();
    last_from(pid, tracked, submitted, || crate::prompt_history::last(session))
}

/// `last` for pane fields already read by the caller (one batched
/// `list-panes -a`); `history` supplies the session's recorded prompt.
pub fn last_from(
    pid: u32,
    tracked: bool,
    submitted: &str,
    history: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let submitted = submitted.trim();
    if tracked && !submitted.is_empty() {
        return Some(crate::tmux::truncate_prompt_title(submitted));
    }
    // Existing sessions cannot have a hook safely injected into a running task.
    // Read only that pane's foreground process, not any background children.
    foreground_command(pid)
        .map(|command| crate::tmux::truncate_prompt_title(&command))
        .or_else(|| {
            if tracked {
                None
            } else {
                history()
            }
        })
}

fn foreground_command(pid: u32) -> Option<String> {
    let (group, foreground) = crate::platform::process::terminal_groups(pid)?;
    if foreground == 0 {
        return None;
    }
    let target = if foreground != group { foreground } else { pid };
    let args = crate::platform::process::cmdline(target)?;
    let executable = Path::new(args.first()?)
        .file_name()?
        .to_str()?
        .trim_start_matches('-');
    if matches!(executable, "bash" | "zsh" | "fish" | "sh" | "dash") {
        return None;
    }
    Some(args.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn bash_records_submitted_commands_not_output_or_task_input() {
        use crate::test_isolation::wait_until;
        struct Server(crate::test_isolation::TmuxServer);
        impl Server {
            fn run(&self, args: &[&str]) -> String {
                self.0.run(args)
            }
            fn wait_title(&self, expected: &str) {
                let deadline = Instant::now() + Duration::from_secs(4);
                loop {
                    let title = self.run(&[
                        "show-options",
                        "-pqv",
                        "-t",
                        "test",
                        "@super_desktop_shell_command",
                    ]);
                    if title == expected {
                        return;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "expected {expected:?}, got {title:?}"
                    );
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            fn wait_foreground(&self, expected: &str) {
                let pid = self.run(&["display-message", "-p", "-t", "test", "#{pane_pid}"])
                    .parse().unwrap();
                wait_until(&format!("foreground task {expected:?}"), || {
                    foreground_command(pid).as_deref() == Some(expected)
                });
            }
        }
        let server = Server(crate::test_isolation::TmuxServer::new());
        let root = server.0.directory().to_path_buf();
        fs::write(
            root.join(".bashrc"),
            "PS1='READY> '; HISTCONTROL=; HISTIGNORE=; PROMPT_COMMAND='true'\n",
        )
        .unwrap();
        let rc = root.join("init.bash");
        fs::write(&rc, include_str!("../assets/shell-title.bash")).unwrap();
        let launch = format!(
            "env HOME='{}' bash --noprofile --rcfile '{}'",
            root.display(),
            rc.display()
        );
        server.run(&["new-session", "-d", "-s", "test", &launch]);
        wait_until("the prompt", || {
            server.run(&["capture-pane", "-p", "-t", "test"]).contains("READY>")
        });
        let task = "printf 'output $ gibberish\\n'; cat";
        server.run(&["send-keys", "-t", "test", "-l", task]);
        server.run(&["send-keys", "-t", "test", "Enter"]);
        server.wait_title(task);
        server.wait_foreground("cat");
        server.run(&[
            "send-keys",
            "-t",
            "test",
            "task input is not a command",
            "Enter",
        ]);
        server.wait_title(task);
        server.run(&["send-keys", "-t", "test", "C-c"]);
        std::thread::sleep(Duration::from_millis(100));
        server.run(&["send-keys", "-t", "test", "Up", "Enter"]);
        server.wait_title(task);
        // This title is identical to the previous turn's. It cannot prove
        // that history replay has started; wait before interrupting the task.
        server.wait_foreground("cat");
        server.run(&["send-keys", "-t", "test", "C-c"]);
        std::thread::sleep(Duration::from_millis(100));
        server.run(&["send-keys", "-t", "test", "printf 'next command'", "Enter"]);
        server.wait_title("printf 'next command'");
        std::thread::sleep(Duration::from_millis(100));
        server.run(&["send-keys", "-t", "test", "Up", "Up", "Enter"]);
        server.wait_title(task);
        server.wait_foreground("cat");
        server.run(&["send-keys", "-t", "test", "C-c"]);
        std::thread::sleep(Duration::from_millis(100));
        server.run(&["send-keys", "-t", "test", "slee", "Tab", "30", "Enter"]);
        server.wait_title("sleep 30");
        server.wait_foreground("sleep 30");
        server.run(&["send-keys", "-t", "test", "C-c"]);
        std::thread::sleep(Duration::from_millis(100));
        server.run(&[
            "send-keys",
            "-t",
            "test",
            "HISTCONTROL=ignorespace",
            "Enter",
        ]);
        server.wait_title("HISTCONTROL=ignorespace");
        std::thread::sleep(Duration::from_millis(100));
        server.run(&["send-keys", "-t", "test", " sleep 30", "Enter"]);
        server.wait_title("");
    }

    #[test]
    fn no_foreground_without_a_controlling_terminal() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // A process without a controlling terminal must not invent a title.
        assert_eq!(foreground_command(child.id()), None);
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn launch_command_instruments_plain_bash_zsh_and_fish_only() {
        let bash = launch_command("shell", "bash");
        assert!(bash.starts_with("bash --rcfile "));
        assert!(bash.contains("shell-title.bash"));

        let zsh = launch_command("shell", "/bin/zsh");
        assert!(zsh.starts_with("env "));
        assert!(zsh.contains("ZDOTDIR="));
        assert!(zsh.ends_with(" /bin/zsh"));

        let fish = launch_command("terminal", "fish");
        assert!(fish.starts_with("fish -C "));
        assert!(fish.contains("shell-title.fish"));

        for untouched in ["zsh -c 'echo hi'", "fish script.fish", "tmux attach"] {
            assert_eq!(launch_command("shell", untouched), untouched);
        }
        assert_eq!(launch_command("claude", "bash"), "bash");
    }
}
