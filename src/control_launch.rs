//! Local CLI launches use frozen launcher settings and never fall back to HOME.
use crate::{
    control::{Command, Reply, Request},
    state::{AppState, TerminalData},
};
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::time::{Duration, Instant};

pub struct Prepared {
    pub harness: String,
    pub directory: String,
    pub command: String,
}

type Refusal = (&'static str, &'static str);

pub fn prepare(state: &AppState, request: &Command) -> Result<Prepared, Refusal> {
    let Command::Launch {
        harness,
        cwd,
        allow_unsafe_harness,
        allow_download,
    } = request
    else {
        return Err(("invalid_arguments", "Expected a launch request."));
    };
    if !Path::new(cwd).is_absolute() || cwd.len() > 4096 || cwd.contains('\0') {
        return Err((
            "invalid_arguments",
            "--cwd must name an absolute directory of at most 4096 bytes.",
        ));
    }
    let directory = std::fs::canonicalize(cwd)
        .ok()
        .filter(|p| p.is_dir())
        .and_then(|p| p.to_str().map(str::to_owned))
        .ok_or((
            "invalid_arguments",
            "The working directory is missing, inaccessible or not UTF-8.",
        ))?;
    let command = if let Some(custom) = state.custom_harnesses.iter().find(|c| c.id == *harness) {
        custom
            .validate()
            .map_err(|_| ("unavailable", "The custom launcher is unavailable."))?;
        if !allow_unsafe_harness {
            return Err(("unsafe_harness","Custom launchers have unverified execution policies; use --allow-unsafe-harness to accept the configured command."));
        }
        // A custom executable can download or execute arbitrary programs. It
        // is owner-configured code, not a verified package-runner policy.
        custom.command()
    } else {
        if !crate::tmux::HARNESS_KEYS.contains(&harness.as_str()) {
            return Err(("not_found", "No configured harness has that ID."));
        }
        let defaults = crate::launch_args::builtin(harness);
        let arguments = state.harness_args.get(harness).unwrap_or(&defaults);
        crate::launch_args::validate(arguments)
            .map_err(|_| ("invalid_arguments", "Saved launcher arguments are invalid."))?;
        if !allow_unsafe_harness
            && (crate::control_service::permission_bypass(arguments) || *arguments != defaults)
        {
            return Err(("unsafe_harness","Launcher arguments bypass permission checks or have an unverified override; use --allow-unsafe-harness to accept them."));
        }
        let mut parts = if let Some(binary) = crate::tmux::harness_binary(harness) {
            vec![binary]
        } else if let Some(package) = crate::tmux::get_agent_config(harness).npx_package {
            let runner = crate::tmux::which("npx")
                .ok_or(("unavailable", "The package runner is unavailable."))?;
            if !allow_download {
                return Err((
                    "download_requires_opt_in",
                    "This launcher uses a package runner; use --allow-download to permit it.",
                ));
            }
            vec![runner, "-y".into(), package.into()]
        } else if harness == "shell" {
            vec![std::env::var("SHELL")
                .ok()
                .filter(|shell| Path::new(shell).is_absolute() && Path::new(shell).is_file())
                .ok_or(("unavailable", "The shell executable is unavailable."))?]
        } else {
            return Err(("unavailable", "The harness executable is unavailable."));
        };
        parts.extend(arguments.iter().cloned());
        parts
            .iter()
            .map(|part| crate::launch_args::quote(part))
            .collect::<Vec<_>>()
            .join(" ")
    };
    Ok(Prepared {
        harness: harness.clone(),
        directory,
        command,
    })
}

/// A bounded tmux invocation, never a shell interpolated from user arguments.
fn tmux(args: &[&str], deadline: Instant) -> Result<(), ()> {
    if Instant::now() >= deadline {
        return Err(());
    }
    let mut child = Process::new(crate::tmux::tmux_bin())
        .args(args)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() { Ok(()) } else { Err(()) };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
        }
    }
}

pub fn start(prepared: &Prepared, session: &str, deadline: Instant) -> Result<TerminalData, ()> {
    if !Path::new(&prepared.directory).is_dir() {
        return Err(());
    }
    let command = crate::shell_title::launch_command(&prepared.harness, &prepared.command);
    let launch = crate::harness_metadata::prepare(session, &prepared.harness, &command);
    // tmux may fall back when -c disappears. Guard the actual launcher too so
    // a directory removed after validation cannot execute it elsewhere.
    let guarded = format!(
        "cd -- {} && {}",
        crate::launch_args::quote(&prepared.directory),
        launch.command
    );
    tmux(
        &[
            "new-session",
            "-d",
            "-s",
            session,
            "-c",
            &prepared.directory,
            "-x",
            "120",
            "-y",
            "35",
            &guarded,
        ],
        deadline,
    )?;
    // Pin the attach behavior before allowing a card to attach to the session.
    tmux(
        &["set-option", "-t", session, "detach-on-destroy", "on"],
        deadline,
    )?;
    launch.register(session);
    crate::tmux_clipboard::install_session(session);
    Ok(TerminalData {
        id: session.into(),
        session_name: session.into(),
        agent_type: prepared.harness.clone(),
        command: prepared.command.clone(),
        x: 0,
        y: 0,
        width: 640,
        height: 480,
        restored_width: 640,
        restored_height: 480,
        iconified: false,
        icon_x: None,
        icon_y: None,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64(),
        tag: 0,
        agent_session_id: None,
        workspace_dir: Some(prepared.directory.clone()),
    })
}

pub fn execute(
    root: &Path,
    request: &Request,
    snapshot: crate::control_service::Snapshot,
    deadline: Instant,
    adopt: impl FnOnce(TerminalData, Instant) -> Result<(), ()>,
) -> Reply {
    crate::control_journal::execute(root, request, |session| {
        if !snapshot.ready {
            return Reply::failure(
                &request.request_id,
                "desktop_not_ready",
                "The local desktop is not ready; no session was launched.",
            );
        }
        if snapshot.state.terminals.len() >= crate::workspace_model::MAX_DESKTOP_CARDS {
            return Reply::failure(
                &request.request_id,
                "limit_reached",
                "The desktop card limit has been reached.",
            );
        }
        let prepared = match prepare(&snapshot.state, &request.command) {
            Ok(prepared) => prepared,
            Err((code, message)) => return Reply::failure(&request.request_id, code, message),
        };
        if Instant::now() >= deadline {
            return Reply::failure(
                &request.request_id,
                "timeout",
                "The request expired before launching.",
            );
        }
        let data = match start(&prepared, session, deadline) {
            Ok(data) => data,
            Err(()) => return Reply::unknown(&request.request_id),
        };
        if adopt(data, deadline).is_err() {
            return Reply::unknown(&request.request_id);
        }
        if crate::state::flush_state_saves_checked().is_err() {
            return Reply::unknown(&request.request_id);
        }
        Reply::success(
            &request.request_id,
            serde_json::json!({"id":session,"sessionName":session,
            "harnessId":prepared.harness,"launchDirectory":prepared.directory,
            "state":"created","readiness":"not_observed"}),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn launch(harness: &str, cwd: &str, unsafe_allowed: bool) -> Command {
        Command::Launch {
            harness: harness.into(),
            cwd: cwd.into(),
            allow_unsafe_harness: unsafe_allowed,
            allow_download: false,
        }
    }

    #[test]
    fn cli_launch_rejects_unknown_harness_relative_missing_cwd_and_unsafe_defaults() {
        let state = AppState::default();
        assert!(prepare(&state, &launch("unknown", "/", true)).is_err());
        assert!(prepare(&state, &launch("shell", "relative", true)).is_err());
        assert!(prepare(&state, &launch("shell", "/does-not-exist-cli-test", true)).is_err());
        assert_eq!(
            prepare(&state, &launch("claude", "/", false))
                .err()
                .unwrap()
                .0,
            "unsafe_harness"
        );
    }

    #[test]
    fn cli_launch_tmux_integration() {
        let root = std::env::temp_dir().join(format!("sd-launch-native-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("tmux")).unwrap();
        let output = Process::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "control_launch::tests::cli_launch_tmux_inner",
                "--nocapture",
            ])
            .env("SD_CLI_LAUNCH_TEST_ROOT", &root)
            .env("HOME", &root)
            .env("XDG_STATE_HOME", root.join("state"))
            .env("TMUX_TMPDIR", root.join("tmux"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .env_remove("LD_PRELOAD")
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn cli_launch_tmux_inner() {
        let Some(root) = std::env::var_os("SD_CLI_LAUNCH_TEST_ROOT").map(std::path::PathBuf::from)
        else {
            return;
        };
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Process::new("tmux")
                    .arg("kill-server")
                    .env_remove("TMUX")
                    .env_remove("TMUX_PANE")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _cleanup = Cleanup;
        let workspace = root.join("project with spaces");
        std::fs::create_dir(&workspace).unwrap();
        let script = root.join("stub harness.sh");
        std::fs::write(
            &script,
            b"#!/bin/sh\npwd > \"$1/cwd\"\nprintf '%s' \"$2\" > \"$1/argument\"\nexec sleep 30\n",
        )
        .unwrap();
        let evil = format!(
            "$(touch {}) ; ' quoted",
            root.join("must-not-exist").display()
        );
        let mut state = AppState::default();
        state
            .custom_harnesses
            .push(crate::custom_harness::CustomHarness {
                id: "custom-cli-test".into(),
                name: "CLI test".into(),
                icon: "⚡".into(),
                executable: "/bin/sh".into(),
                arguments: vec![
                    script.to_str().unwrap().into(),
                    root.to_str().unwrap().into(),
                    evil.clone(),
                ],
            });
        assert_eq!(
            prepare(
                &state,
                &launch("custom-cli-test", workspace.to_str().unwrap(), false)
            )
            .err()
            .unwrap()
            .0,
            "unsafe_harness"
        );
        let request = Request {
            control_version: 1,
            request_id: "actual-launch".into(),
            command: launch("custom-cli-test", workspace.to_str().unwrap(), true),
        };
        let journal = root.join("receipts");
        let snapshot = crate::control_service::Snapshot {
            state: state.clone(),
            visible: false,
            ready: true,
        };
        let reply = execute(
            &journal,
            &request,
            snapshot,
            Instant::now() + Duration::from_secs(3),
            |data, _| {
                assert_eq!(data.workspace_dir.as_deref(), workspace.to_str());
                Ok(())
            },
        );
        assert!(reply.ok, "{reply:?}");
        let id = reply.data.as_ref().unwrap()["id"].as_str().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !root.join("argument").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(root.join("argument")).unwrap(),
            evil
        );
        assert_eq!(
            std::fs::read_to_string(root.join("cwd"))
                .unwrap()
                .trim_end(),
            workspace.to_str().unwrap()
        );
        assert!(!root.join("must-not-exist").exists());
        let pid = Process::new("tmux")
            .args([
                "display-message",
                "-p",
                "-t",
                &format!("={id}:"),
                "#{pane_pid}",
            ])
            .output()
            .unwrap()
            .stdout;
        let replay = execute(
            &journal,
            &request,
            crate::control_service::Snapshot {
                state,
                visible: false,
                ready: true,
            },
            Instant::now() + Duration::from_secs(3),
            |_, _| panic!("replay must not adopt again"),
        );
        assert_eq!(replay.data.unwrap()["id"], id);
        let after = Process::new("tmux")
            .args([
                "display-message",
                "-p",
                "-t",
                &format!("={id}:"),
                "#{pane_pid}",
            ])
            .output()
            .unwrap()
            .stdout;
        assert_eq!(pid, after);
        let count = Process::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&count.stdout).lines().count(), 1);
        let prepared = Prepared {
            harness: "custom-cli-test".into(),
            directory: root.join("deleted").to_str().unwrap().into(),
            command: "/bin/sh".into(),
        };
        assert!(start(
            &prepared,
            "sd_term_cli_missingcwd",
            Instant::now() + Duration::from_secs(1)
        )
        .is_err());

        let exited = "sd_term_cli_exited_before_attach";
        let inventory = crate::tmux::SessionInventory::cli_created(exited);
        crate::tmux::ensure_session_with_inventory(
            exited,
            "shell",
            Some("/bin/sh"),
            None,
            Some(workspace.to_str().unwrap()),
            Some(&inventory),
        );
        assert!(
            !crate::tmux::session_exists(exited),
            "initial attachment must not relaunch an exited process"
        );

        // This child has its own HOME and server. Restrict PATH to fixture
        // executables to test package and argument policy without downloads.
        let bin = root.join("tools ; quoted");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink("/bin/true", bin.join("npx")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", bin.join("bash")).unwrap();
        let old_path = std::env::var_os("PATH").unwrap();
        std::env::set_var("PATH", &bin);
        let defaults = AppState::default();
        let denied = prepare(
            &defaults,
            &launch("reasonix", workspace.to_str().unwrap(), false),
        );
        let allowed = prepare(
            &defaults,
            &Command::Launch {
                harness: "reasonix".into(),
                cwd: workspace.to_str().unwrap().into(),
                allow_download: true,
                allow_unsafe_harness: false,
            },
        );
        let shell = prepare(
            &defaults,
            &launch("shell", workspace.to_str().unwrap(), false),
        );
        let mut overridden = defaults;
        overridden
            .harness_args
            .insert("shell".into(), vec!["-c".into(), evil.clone()]);
        let override_denied = prepare(
            &overridden,
            &launch("shell", workspace.to_str().unwrap(), false),
        );
        let override_allowed = prepare(
            &overridden,
            &launch("shell", workspace.to_str().unwrap(), true),
        );
        std::env::set_var("PATH", old_path);
        assert_eq!(denied.err().unwrap().0, "download_requires_opt_in");
        assert_eq!(
            shlex::split(&allowed.unwrap().command).unwrap()[..3],
            [bin.join("npx").to_str().unwrap(), "-y", "reasonix"]
        );
        assert_eq!(
            shlex::split(&shell.unwrap().command).unwrap(),
            vec![bin.join("bash").to_str().unwrap()]
        );
        assert_eq!(override_denied.err().unwrap().0, "unsafe_harness");
        assert_eq!(
            shlex::split(&override_allowed.unwrap().command).unwrap()[2],
            evil
        );
    }
}
