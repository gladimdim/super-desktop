//! Socket-to-worker acceptance with an inert harness and private tmux server.
use crate::control::{self, Command};
use serde_json::{json, Value};
use std::process::Command as Process;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
fn assert_output_contract<T: serde::de::DeserializeOwned>(name: &str, value: &Value) {
    serde_json::from_value::<T>(value.clone()).unwrap_or_else(|error| panic!("{name}: {error}"));
    if let Some(root)=std::env::var_os("SD_CLI_SCHEMA_FIXTURES") {
        let path=std::path::Path::new(&root).join(format!("{name}.json"));
        std::fs::write(path,serde_json::to_vec(value).unwrap()).unwrap();
    }
}

#[test]
fn actual_cli_workflow_uses_production_workers_without_a_desktop() {
    use std::os::unix::fs::PermissionsExt;
    let Some((root, _tmux)) = crate::test_isolation::private_root("SD_CLI_ACCEPTANCE_ROOT") else {
        crate::test_isolation::rerun_in_private_root(
            "control_acceptance::actual_cli_workflow_uses_production_workers_without_a_desktop",
            "SD_CLI_ACCEPTANCE_ROOT",
            |root, child| {
                child.env("XDG_STATE_HOME", root).env("XDG_RUNTIME_DIR", root);
            },
        );
        return;
    };
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
                Command::Capture { .. }
                | Command::Composer { .. }
                | Command::Runtime { .. }
                | Command::Lifecycle { .. } => crate::control_terminal::execute(
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
                ),
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
        assert_output_contract::<super_desktop::control_output::Envelope<super_desktop::control_output::Runtime>>("runtime", &runtime.1);
        let pane = runtime.1["data"]["paneIdentity"].as_str().unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let capture = call(&["terminal", "capture", id]);
            assert_output_contract::<super_desktop::control_output::Envelope<super_desktop::control_output::Capture>>("capture", &capture.1);
            if capture.1["data"]["text"]
                .as_str()
                .is_some_and(|s| s.contains("READY"))
            {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        let follow=Process::new(binaries.join(bin)).args(["terminal","follow",id,"--seconds","1","--format=jsonl"]).output().unwrap();
        assert!(follow.status.success());
        for line in String::from_utf8(follow.stdout).unwrap().lines() {
            let value:Value=serde_json::from_str(line).unwrap();
            assert_output_contract::<super_desktop::control_output::FollowLine>(&format!("follow-{}",value["type"].as_str().unwrap()),&value);
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
            assert_output_contract::<super_desktop::control_output::Envelope<super_desktop::control_output::Capture>>("capture", &capture.1);
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

#[test]
fn configured_launch_from_both_binaries_delivers_one_prompt_to_an_isolated_stub() {
    use std::os::unix::fs::PermissionsExt;
    let Some((root, _tmux)) = crate::test_isolation::private_root("SD_CONFIGURED_LAUNCH_ROOT") else {
        crate::test_isolation::rerun_in_private_root(
            "control_acceptance::configured_launch_from_both_binaries_delivers_one_prompt_to_an_isolated_stub",
            "SD_CONFIGURED_LAUNCH_ROOT",
            |root, child| {
                std::fs::create_dir(root.join("bin")).unwrap();
                let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap());
                child.env("XDG_STATE_HOME", root).env("XDG_RUNTIME_DIR", root).env("PATH", path);
            },
        );
        return;
    };
    let cat = root.join("claude");
    std::fs::copy("/usr/bin/cat", &cat).unwrap();
    let script = root.join("bin/claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nstty raw -echo\nsleep 0.2\nprintf '❯ '\nexec '{}' >> '{}/received'\n",
            cat.display(),
            root.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        crate::tmux::harness_binary("claude").as_deref(),
        script.to_str(),
        "Never invoke a real harness"
    );
    let args_file = root.join("arguments.json");
    std::fs::write(&args_file, "[]").unwrap();
    let mut initial = crate::state::AppState::default();
    initial.terminals.clear();
    let state = Arc::new(Mutex::new(initial));
    let saved = state.clone();
    let task = Arc::new(crate::session_task::SessionTask::default());
    let server = control::Server::bind(&root).unwrap();
    fn snapshot(state: &crate::state::AppState) -> crate::desktop_protocol::LocalWorkspaceSnapshot {
        serde_json::from_value(json!({"epoch":"acceptance","revision":1,"canvas":{"x":0,"y":0,"width":1024,"height":768,"scale":1.0,"topInset":56},"workspace":"/tmp","homeDirectory":"/tmp","visibleHarnesses":[],"harnessTypes":[],"cards":state.terminals.iter().map(|c|json!({"cardId":c.id,"sessionName":c.session_name,"agentType":c.agent_type,"title":"","status":"UNKNOWN","sessionAlive":null,"workspace":"/tmp","revision":1,"layout":{"x":c.x,"y":c.y,"width":c.width,"height":c.height,"restoredWidth":c.restored_width,"restoredHeight":c.restored_height,"iconified":false,"iconX":null,"iconY":null,"tag":0},"stackingOrder":0,"expanded":false,"terminalSize":null})).collect::<Vec<_>>()})).unwrap()
    }
    std::thread::spawn(move || {
        server.run(move |request, deadline| {
            let journal = crate::control_journal::root();
            match &request.command {
                Command::Launch { .. } => {
                    let snapshot = crate::control_service::Snapshot {
                        state: saved.lock().unwrap().clone(),
                        visible: false,
                        ready: true,
                    };
                    crate::control_launch::execute(
                        &journal,
                        &request,
                        snapshot,
                        deadline,
                        |mut data, _| {
                            data.x = 10;
                            data.y = 80;
                            saved.lock().unwrap().terminals.push(data);
                            Ok(())
                        },
                    )
                }
                Command::Geometry { .. } | Command::Resize { .. } | Command::Move { .. } => {
                    crate::control_geometry::dispatch(&journal, &request, deadline, |r| {
                        let mut state = saved.lock().unwrap();
                        let before = snapshot(&state);
                        let id = crate::control_geometry::card_id(&r.command);
                        let card = before.cards.iter().find(|c| c.card_id == id).unwrap();
                        if r.command.is_mutation() {
                            let prepared = match crate::control_geometry::prepare(r, &before, card)
                            {
                                Ok(p) => p,
                                Err(reply) => return Ok(reply),
                            };
                            let data = state.terminals.iter_mut().find(|c| c.id == id).unwrap();
                            data.x = prepared.rect.x as i32;
                            data.y = prepared.rect.y as i32;
                            data.width = prepared.rect.width;
                            data.height = prepared.rect.height;
                            data.restored_width = data.width;
                            data.restored_height = data.height;
                        }
                        let after = snapshot(&state);
                        Ok(control::Reply::success(
                            &r.request_id,
                            crate::control_geometry::describe(
                                &after,
                                after.cards.iter().find(|c| c.card_id == id).unwrap(),
                            ),
                        ))
                    })
                }
                Command::Composer { .. } => crate::control_terminal::execute(
                    &request,
                    &saved.lock().unwrap().clone(),
                    deadline,
                    |_| Ok(true),
                ),
                Command::Input {
                    id,
                    expect_epoch,
                    expect_revision,
                    ..
                } => crate::control_input::execute(&journal, &request, deadline, || {
                    let state = saved.lock().unwrap();
                    let current = snapshot(&state);
                    let card = current.cards.iter().find(|c| &c.card_id == id).unwrap();
                    assert_eq!(expect_epoch, &current.epoch);
                    assert_eq!(
                        expect_revision,
                        &crate::control_geometry::revision(&current, card)
                    );
                    Ok(Ok(Some(crate::control_close::Target {
                        data: state
                            .terminals
                            .iter()
                            .find(|c| &c.id == id)
                            .unwrap()
                            .clone(),
                        task: task.clone(),
                    })))
                }),
                Command::InspectRequest { id } => {
                    crate::control_journal::inspect(&journal, &request.request_id, id)
                }
                _ => crate::control_service::answer(
                    request,
                    crate::control_service::Snapshot {
                        state: saved.lock().unwrap().clone(),
                        visible: false,
                        ready: true,
                    },
                ),
            }
        })
    });
    // Visibility orchestration is tested against an inert legacy owner socket.
    let listener =
        std::os::unix::net::UnixListener::bind(crate::platform::runtime::socket_path()).unwrap();
    std::thread::spawn(move || {
        use std::io::{BufRead, Write};
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut line = String::new();
            std::io::BufReader::new(&stream)
                .read_line(&mut line)
                .unwrap();
            assert_eq!(line, "show\n");
            stream.write_all(b"{\"ok\":true,\"visible\":true}").unwrap();
        }
    });
    let current = std::env::current_exe().unwrap();
    let binaries = current.parent().unwrap().parent().unwrap();
    for (index, binary) in ["super-desktop-client", "super-desktop"].iter().enumerate() {
        let request_id = format!("hello-{index}");
        let arguments = [
            "harness",
            "launch",
            "claude",
            "--cwd",
            root.to_str().unwrap(),
            "--width",
            "600",
            "--height",
            "300",
            "--center",
            "--prompt",
            "Hello world",
            "--show",
            "--ready-timeout",
            "5s",
            "--args-file",
            args_file.to_str().unwrap(),
            "--allow-unsafe-harness",
            "--request-id",
            &request_id,
            "--format=json",
        ];
        let run = || {
            let output = Process::new(binaries.join(binary))
                .args(arguments)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice::<Value>(&output.stdout).unwrap()
        };
        let result = run();
        assert_eq!(
            result["data"]["geometry"]["rect"],
            json!({"x":212,"y":234,"width":600,"height":300})
        );
        assert_eq!(result["data"]["prompt"]["outcome"], "delivered");
        assert_eq!(
            run(),
            result,
            "Duplicate workflow must return historical result"
        );
        assert_eq!(
            saved_count(&state),
            index + 1,
            "Replay must not launch a second card"
        );
        let composer = Process::new(binaries.join(binary))
            .args([
                "terminal",
                "composer",
                result["data"]["id"].as_str().unwrap(),
                "--format=json",
            ])
            .output()
            .unwrap();
        assert!(composer.status.success());
        assert_output_contract::<super_desktop::control_output::Envelope<super_desktop::control_output::Composer>>("composer", &serde_json::from_slice(&composer.stdout).unwrap());
        assert_eq!(
            serde_json::from_slice::<Value>(&composer.stdout).unwrap()["data"]["ready"],
            true
        );
        let receipt = Process::new(binaries.join(binary))
            .args(["request", "inspect", &request_id, "--format=json"])
            .output()
            .unwrap();
        assert!(receipt.status.success());
    }
    let expected = b"\x1b[200~Hello world\x1b[201~\r".repeat(2);
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        let got = std::fs::read(root.join("received")).unwrap_or_default();
        if got == expected {
            break;
        }
        assert!(Instant::now() < until, "received {got:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    fn saved_count(state: &Arc<Mutex<crate::state::AppState>>) -> usize {
        state.lock().unwrap().terminals.len()
    }
}
