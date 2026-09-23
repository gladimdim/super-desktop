//! Regular terminal titles come from shell submission or foreground argv,
//! never from task output. Shell hooks also cover history and completion.
use std::{fs, path::Path, process::Command};

pub fn is_regular(agent: &str) -> bool {
    matches!(agent, "shell" | "bash" | "terminal")
}

/// Instrument plain Bash launches only; explicit commands and other shells
/// keep their original invocation and use the process/input fallback.
pub fn launch_command(agent: &str, command: &str) -> String {
    if !is_regular(agent) || !matches!(command, "bash" | "/bin/bash" | "/usr/bin/bash") {
        return command.to_string();
    }
    let Some(home) = std::env::var_os("HOME") else {
        return command.to_string();
    };
    let directory = Path::new(&home).join(".local/state/super-desktop");
    let script = directory.join("shell-title.bash");
    if install_script(&directory, &script).is_err() {
        return command.to_string();
    }
    format!(
        "{command} --rcfile '{}'",
        script.to_string_lossy().replace('\'', "'\\''")
    )
}

fn install_script(directory: &Path, script: &Path) -> std::io::Result<()> {
    let content = include_bytes!("../assets/shell-title.bash");
    if fs::read(script).ok().as_deref() == Some(content.as_slice()) {
        return Ok(());
    }
    fs::create_dir_all(directory)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = directory.join(format!(".shell-title-{}-{stamp}", std::process::id()));
    let result = fs::write(&temporary, content).and_then(|_| fs::rename(&temporary, script));
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
                crate::prompt_history::last(session)
            }
        })
}

fn foreground_command(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    let foreground = fields.get(5)?.parse::<u32>().ok()?;
    let group = fields.get(2)?.parse::<u32>().ok()?;
    if foreground == 0 {
        return None;
    }
    let target = if foreground != group { foreground } else { pid };
    let raw = fs::read(format!("/proc/{target}/cmdline")).ok()?;
    let args: Vec<_> = raw
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
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
        struct Server(String, std::path::PathBuf);
        impl Server {
            fn run(&self, args: &[&str]) -> String {
                let out = Command::new(crate::tmux::tmux_bin())
                    .args(["-L", &self.0, "-f", "/dev/null"])
                    .args(args)
                    .env_remove("TMUX")
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                String::from_utf8_lossy(&out.stdout).trim().to_owned()
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
        }
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = Command::new(crate::tmux::tmux_bin())
                    .args(["-L", &self.0, "kill-server"])
                    .output();
                let _ = fs::remove_dir_all(&self.1);
            }
        }
        let name = format!("sd-shell-title-{}", std::process::id());
        let root = std::env::temp_dir().join(&name);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(".bashrc"),
            "PS1='READY> '; HISTCONTROL=; HISTIGNORE=; PROMPT_COMMAND='true'\n",
        )
        .unwrap();
        let rc = root.join("init.bash");
        fs::write(&rc, include_str!("../assets/shell-title.bash")).unwrap();
        let server = Server(name, root.clone());
        let launch = format!(
            "env HOME='{}' bash --noprofile --rcfile '{}'",
            root.display(),
            rc.display()
        );
        server.run(&["new-session", "-d", "-s", "test", &launch]);
        let deadline = Instant::now() + Duration::from_secs(4);
        while !server
            .run(&["capture-pane", "-p", "-t", "test"])
            .contains("READY>")
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
        let task = "printf 'output $ gibberish\\n'; cat";
        server.run(&["send-keys", "-t", "test", "-l", task]);
        server.run(&["send-keys", "-t", "test", "Enter"]);
        server.wait_title(task);
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
        server.run(&["send-keys", "-t", "test", "C-c"]);
        std::thread::sleep(Duration::from_millis(100));
        server.run(&["send-keys", "-t", "test", "printf 'next command'", "Enter"]);
        server.wait_title("printf 'next command'");
        std::thread::sleep(Duration::from_millis(100));
        server.run(&["send-keys", "-t", "test", "Up", "Up", "Enter"]);
        server.wait_title(task);
        server.run(&["send-keys", "-t", "test", "C-c"]);
        std::thread::sleep(Duration::from_millis(100));
        server.run(&["send-keys", "-t", "test", "slee", "Tab", "30", "Enter"]);
        server.wait_title("sleep 30");
        let pid = server
            .run(&["display-message", "-p", "-t", "test", "#{pane_pid}"])
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        while foreground_command(pid).as_deref() != Some("sleep 30") {
            assert!(
                Instant::now() < deadline,
                "foreground task command was not resolved"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
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
}
