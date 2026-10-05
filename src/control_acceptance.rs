//! Socket-to-worker acceptance with an inert harness and private tmux server.
use crate::control::{self, Command};
use serde_json::{json, Value};
use std::process::Command as Process;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
#[test]
fn actual_cli_workflow_uses_production_workers_without_a_desktop() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let Some(root) = std::env::var_os("SD_CLI_ACCEPTANCE_ROOT").map(std::path::PathBuf::from)
    else {
        let root = std::path::Path::new("/tmp").join(format!("sd-accept-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        std::fs::create_dir(root.join("tmux")).unwrap();
        let out = Process::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "control_acceptance::actual_cli_workflow_uses_production_workers_without_a_desktop",
                "--nocapture",
            ])
            .env("SD_CLI_ACCEPTANCE_ROOT", &root)
            .env("HOME", &root)
            .env("XDG_STATE_HOME", &root)
            .env("XDG_RUNTIME_DIR", &root)
            .env("TMUX_TMPDIR", root.join("tmux"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    };
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = Process::new("tmux").arg("kill-server").output();
        }
    }
    let _cleanup = Cleanup;
    let stub = root.join("stub");
    std::fs::write(
        &stub,
        "#!/bin/sh\nstty raw -echo\nprintf 'READY\\n'\nexec cat\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut initial = crate::state::AppState::default();
    initial.terminals.clear();
    initial.custom_harnesses = vec![crate::custom_harness::CustomHarness {
        id: "custom-acceptance".into(),
        name: "Acceptance stub".into(),
        icon: "⚡".into(),
        executable: stub.to_string_lossy().into(),
        arguments: vec![],
    }];
    let state = Arc::new(Mutex::new(initial));
    let task = Arc::new(crate::session_task::SessionTask::default());
    let journal = root.join("receipts");
    let server = control::Server::bind(&root).unwrap();
    let saved = state.clone();
    let receipts = journal.clone();
    std::thread::spawn(move || {
        server.run(move |request, deadline| {
            let snapshot = || crate::control_service::Snapshot {
                state: saved.lock().unwrap().clone(),
                visible: false,
                ready: true,
            };
            let target = || {
                let data = saved.lock().unwrap().terminals.first().cloned().ok_or(())?;
                Ok(Ok(Some(crate::control_close::Target {
                    data,
                    task: task.clone(),
                })))
            };
            match &request.command {
                Command::Launch { .. } => crate::control_launch::execute(
                    &receipts,
                    &request,
                    snapshot(),
                    deadline,
                    |data, _| {
                        saved.lock().unwrap().terminals.push(data);
                        Ok(())
                    },
                ),
                Command::Capture { .. } | Command::Runtime { .. } | Command::Lifecycle { .. } => {
                    crate::control_terminal::execute(
                        &request,
                        &snapshot().state,
                        deadline,
                        |card| {
                            Ok(saved
                                .lock()
                                .unwrap()
                                .terminals
                                .iter()
                                .any(|c| c.id == card.id))
                        },
                    )
                }
                Command::Input { .. } => {
                    crate::control_input::execute(&receipts, &request, deadline, target)
                }
                Command::Close { .. } => {
                    crate::control_close::execute(&receipts, &request, deadline, |action| {
                        match action {
                            crate::control_close::Action::Inspect => target(),
                            crate::control_close::Action::Remove(_) => {
                                task.close();
                                saved.lock().unwrap().terminals.clear();
                                Ok(Ok(None))
                            }
                        }
                    })
                }
                Command::InspectRequest { id } => {
                    crate::control_journal::inspect(&receipts, &request.request_id, id)
                }
                _ => crate::control_service::answer(request, snapshot()),
            }
        })
    });
    let current = std::env::current_exe().unwrap();
    let binaries = current.parent().unwrap().parent().unwrap();
    for (index, bin) in ["super-desktop-client", "super-desktop"].iter().enumerate() {
        // The first binary completes the mutation workflow; both read its receipt.
        let call = |args: &[&str]| -> (i32, Value) {
            let out = Process::new(binaries.join(bin))
                .args(args)
                .arg("--format=json")
                .output()
                .unwrap();
            let value = serde_json::from_slice(&out.stdout)
                .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
            (out.status.code().unwrap(), value)
        };
        assert_eq!(call(&["capabilities"]).0, 0);
        if index == 1 {
            assert_eq!(call(&["request", "inspect", "accept-close"]).0, 0);
            assert_eq!(
                call(&["terminal", "list"]).1["data"]["terminals"],
                json!([])
            );
            continue;
        }
        let launched = call(&[
            "harness",
            "launch",
            "custom-acceptance",
            "--cwd",
            root.to_str().unwrap(),
            "--allow-unsafe-harness",
            "--request-id",
            "accept-launch",
        ]);
        assert_eq!(launched.0, 0, "{}", launched.1);
        let id = launched.1["data"]["id"].as_str().unwrap();
        let runtime = call(&["terminal", "runtime", id]);
        assert_eq!(runtime.0, 0);
        let pane = runtime.1["data"]["paneIdentity"].as_str().unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let capture = call(&["terminal", "capture", id]);
            if capture.1["data"]["text"]
                .as_str()
                .is_some_and(|s| s.contains("READY"))
            {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        let input = root.join("input");
        std::fs::write(&input, "CLI acceptance literal ✓").unwrap();
        let revision = "a".repeat(64);
        let sent = call(&[
            "terminal",
            "send",
            id,
            "--file",
            input.to_str().unwrap(),
            "--expect-epoch",
            "fixture",
            "--expect-revision",
            &revision,
            "--expect-pane-identity",
            pane,
            "--request-id",
            "accept-send",
        ]);
        assert_eq!(sent.0, 0, "{}", sent.1);
        assert_eq!(sent.1["data"]["submissionObserved"], false);
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let capture = call(&["terminal", "capture", id]);
            if capture.1["data"]["text"]
                .as_str()
                .is_some_and(|s| s.contains("CLI acceptance literal"))
            {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(
            call(&[
                "terminal",
                "wait",
                id,
                "--until",
                "completed",
                "--after",
                "none",
                "--expect-pane-identity",
                pane,
                "--timeout",
                "1"
            ])
            .0,
            0,
            "stub has no native completion; never invent success"
        );
        let closed = call(&[
            "terminal",
            "close",
            id,
            "--expect-epoch",
            "fixture",
            "--expect-revision",
            &revision,
            "--expect-pane-identity",
            pane,
            "--request-id",
            "accept-close",
        ]);
        assert_eq!(closed.0, 0, "{}", closed.1);
        assert!(state.lock().unwrap().terminals.is_empty());
        let receipt = call(&["request", "inspect", "accept-close"]);
        assert_eq!(receipt.1["data"]["result"]["data"]["sessionClosed"], true);
    }
}
