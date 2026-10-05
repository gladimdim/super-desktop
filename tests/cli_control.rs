//! Actual CLI clients against the production local transport in a child process.
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use super_desktop::control;

#[test]
fn control_daemon_fixture() {
    let Some(runtime) = std::env::var_os("SD_CONTROL_TEST_RUNTIME") else {
        return;
    };
    let server = control::Server::bind(Path::new(&runtime)).unwrap();
    server.run(move |request, _| {
        if Path::new(&runtime).join("older-daemon").exists() {
            match &request.command {
                control::Command::Capabilities {} => return control::Reply::success(&request.request_id, serde_json::json!({"methods":["app.status"]})),
                control::Command::Attach {..} | control::Command::Viewport {..} | control::Command::Viewports {..} | control::Command::CardAction {..} | control::Command::Files {..} | control::Command::FilesEdit {..} | control::Command::Preferences { .. } | control::Command::PreferencesEdit { .. } | control::Command::Workspace { .. } | control::Command::WorkspaceEdit { .. } | control::Command::Input { .. } | control::Command::Mode { .. } | control::Command::Close { .. } | control::Command::Geometry { .. } | control::Command::Move { .. } | control::Command::Resize { .. } | control::Command::Launch { .. } | control::Command::InspectRequest { .. } | control::Command::Lifecycle { .. } | control::Command::Runtime { .. } | control::Command::Capture { .. } => panic!("client sent an unsupported operation"),
                _ => {}
            }
        }
        let data = match &request.command {
            control::Command::Audit {..}=>serde_json::json!({"entries":[{"id":"example","state":"recorded"}],"revision":"a".repeat(64),"nextCursor":null}),
            control::Command::Access {}=>serde_json::json!({"mode":"owner"}),
            control::Command::Attach {interactive,expect_pane_identity,..}=>{
                let nonce=control::new_request(control::Command::Status {}).unwrap().request_id;
                let name=format!("attach-{nonce}.sock");let path=Path::new(&runtime).join("super-desktop").join(&name);
                let listener=std::os::unix::net::UnixListener::bind(&path).unwrap();std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o600)).unwrap();
                let (token,identity,interactive)=(nonce.clone(),expect_pane_identity.clone(),*interactive);
                std::thread::spawn(move||{
                    use base64::Engine;
                    let (mut stream,_)=listener.accept().unwrap();let deadline=Instant::now()+Duration::from_secs(5);
                    let handshake=control::read_frame(&mut stream,256,deadline).unwrap();let value:serde_json::Value=serde_json::from_slice(&handshake).unwrap();assert_eq!(value["token"],token);
                    for event in [serde_json::json!({"type":"attached","ok":true,"sequence":0,"streamId":token,"paneIdentity":identity,"grid":{"columns":80,"rows":24}}),serde_json::json!({"type":"output","ok":true,"sequence":1,"streamId":token,"bytes":base64::engine::general_purpose::STANDARD.encode(b"fake\x1b[31moutput")})] {
                        control::write_frame(&mut stream,&serde_json::to_vec(&event).unwrap(),65536,deadline).unwrap();
                    }
                    if interactive {let _=control::read_frame(&mut stream,8192,deadline);}
                    let _=control::write_frame(&mut stream,&serde_json::to_vec(&serde_json::json!({"type":"end","ok":true,"sequence":2,"streamId":token,"reason":"detached"})).unwrap(),65536,deadline);
                    drop(stream);drop(listener);let _=std::fs::remove_file(path);
                });
                serde_json::json!({"socket":name,"token":nonce})
            },

            control::Command::Files {id,query:control::FilesQuery::Read {asset,offset}} => {
                use base64::Engine;use sha2::{Digest,Sha256};
                let bytes=vec![b'x';70000];let offset=*offset as usize;let end=(offset+65536).min(bytes.len());
                serde_json::json!({"id":id,"asset":{"id":asset},"offset":offset,"nextOffset":end,"eof":end==bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"bytes":base64::engine::general_purpose::STANDARD.encode(&bytes[offset..end])})
            },

            control::Command::Viewport {..} | control::Command::Viewports {..} | control::Command::CardAction {..} | control::Command::Files {..} | control::Command::FilesEdit {..} | control::Command::Preferences { .. } | control::Command::PreferencesEdit { .. } | control::Command::Workspace { .. } | control::Command::WorkspaceEdit { .. } | control::Command::Input { .. } | control::Command::Mode { .. } | control::Command::Close { .. } | control::Command::Geometry { .. } | control::Command::Move { .. } | control::Command::Resize { .. } => serde_json::to_value(&request.command).unwrap(),
            control::Command::Lifecycle { id } => serde_json::json!({"id":id,"paneIdentity":"a".repeat(64),"nativeMetadataObserved":id=="completed-card","lifecycle":if id=="completed-card" {"completed"} else {"unknown"},"completion":{"supported":id=="completed-card","state":if id=="completed-card" {"completed"} else {"unknown"},"completionId":if id=="completed-card" {Some("b".repeat(64))} else {None}}}),
            control::Command::Runtime { id } => serde_json::json!({"id":id,"columns":120,"rows":35}),
            control::Command::Capture { id,history,lines } => serde_json::json!({"id":id,"history":history,"lines":lines,"text":"private\u{001b}text\u{009b}","runtime":{"paneIdentity":"a".repeat(64),"columns":120,"rows":35},"truncated":false}),
            control::Command::Launch { harness,cwd,allow_unsafe_harness,allow_download,arguments } => serde_json::json!({"arguments":arguments,"harness":harness,"cwd":cwd,"allowUnsafeHarness":allow_unsafe_harness,"allowDownload":allow_download}),
            control::Command::InspectRequest { id } => serde_json::json!({"id":id}),
            control::Command::Capabilities {} => control::capabilities(),
            control::Command::Status {} => serde_json::json!({"visible":false,"ready":true}),
            control::Command::Terminals {} => serde_json::json!({"terminals":[]}),
            control::Command::Terminal { .. } => {
                return control::Reply::failure(&request.request_id, "not_found", "No terminal.")
            }
            control::Command::Harnesses { all } => serde_json::json!({"harnesses":[],"all":all}),
            control::Command::Harness { id } => {
                serde_json::json!({"id":id,"name":"untrusted\u{009b}31m\u{001b}]52;value"})
            }
        };
        control::Reply::success(&request.request_id, data)
    });
}

struct Fixture {
    child: Child,
    root: std::path::PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cli_local_commands_use_framed_owner_socket_and_report_errors() {
    let root = Path::new("/tmp").join(format!("sd-cli-wire-{}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let root = root.canonicalize().unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "control_daemon_fixture", "--nocapture"])
        .env("SD_CONTROL_TEST_RUNTIME", &root)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let fixture = Fixture { child, root };
    let path = fixture.root.join("super-desktop/control-v1.sock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "test control daemon did not bind"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for executable in [
        env!("CARGO_BIN_EXE_super-desktop-client"),
        env!("CARGO_BIN_EXE_super-desktop"),
    ] {
        let revision = "a".repeat(64);
        let note_file=fixture.root.join("note-input.txt");
        std::fs::write(&note_file,"line one\nПривіт\t$(literal)").unwrap();
        let layout_file=fixture.root.join("layout.json");
        std::fs::write(&layout_file,r#"{"version":1,"items":[]}"#).unwrap();
        for args in [
            vec!["workspace","layout","export"],
            vec!["workspace","layout","validate","--file",layout_file.to_str().unwrap()],
            vec!["workspace","layout","apply","--file",layout_file.to_str().unwrap(),"--request-id","layout","--expect-epoch","epoch","--expect-revision",&revision],
            vec!["workspace","arrange","--request-id","arrange","--expect-epoch","epoch","--expect-revision",&revision],
            vec!["workspace","inspect"],vec!["workspace","folders"],vec!["note","list"],vec!["note","inspect","note-1"],
            vec!["note","create","--file",note_file.to_str().unwrap(),"--request-id","note-create","--expect-epoch","epoch","--expect-revision",&revision],
            vec!["note","update","note-1","--file",note_file.to_str().unwrap(),"--request-id","note-update","--expect-epoch","epoch","--expect-revision",&revision],
            vec!["note","move","note-1","--x","80","--y","140","--request-id","note-move","--expect-epoch","epoch","--expect-revision",&revision],
            vec!["note","resize","note-1","--width","260","--height","200","--request-id","note-resize","--expect-epoch","epoch","--expect-revision",&revision],
            vec!["note","tag","set","note-1","3","--request-id","note-tag","--expect-epoch","epoch","--expect-revision",&revision],
            vec!["note","delete","note-1","--request-id","note-delete","--expect-epoch","epoch","--expect-revision",&revision],
            vec!["workspace","set","/tmp","--request-id","workspace-set","--expect-epoch","epoch","--expect-revision",&revision],
        ] {
            let output=Command::new(executable).args(&args).arg("--format=json").env("XDG_RUNTIME_DIR",&fixture.root).output().unwrap();
            assert!(output.status.success(),"{args:?}: {:?}",output);
            let value:serde_json::Value=serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["ok"],true);
            if args[1]=="create" || args[1]=="update" { assert_eq!(value["data"]["edit"]["text"],"line one\nПривіт\t$(literal)"); }
        }
        let args_file=fixture.root.join("args.json");std::fs::write(&args_file,r#"["--flag","literal value"]"#).unwrap();
        let custom_file=fixture.root.join("custom.json");std::fs::write(&custom_file,r#"{"id":"custom-cli","name":"test","icon":"🤖","executable":"/bin/true","arguments":[]}"#).unwrap();
        for (args,edit) in [
            (vec!["settings","list"],false),(vec!["settings","get","toolbarSize"],false),
            (vec!["settings","set","toolbarSize","--value",r#""small""#],true),(vec!["settings","reset","toolbarSize"],true),
            (vec!["harness","args","get","claude"],false),(vec!["harness","args","set","claude","--file",args_file.to_str().unwrap()],true),
            (vec!["harness","args","reset","claude"],true),(vec!["harness","custom","get","custom-cli"],false),
            (vec!["harness","custom","add","--file",custom_file.to_str().unwrap()],true),(vec!["harness","custom","update","--file",custom_file.to_str().unwrap()],true),
            (vec!["harness","custom","remove","custom-cli"],true),(vec!["harness","visibility","set","--file",args_file.to_str().unwrap()],true),
            (vec!["harness","visibility","reset"],true),(vec!["harness","rescan"],true),
            (vec!["theme","inspect"],false),(vec!["theme","reload"],true),(vec!["usage","inspect"],false),
        ] {
            let mut command=Command::new(executable);command.args(&args).arg("--format=json").env("XDG_RUNTIME_DIR",&fixture.root);
            if edit{command.args(["--expect-epoch","epoch","--expect-revision",&revision,"--request-id","settings-test"]);}
            let output=command.output().unwrap();assert!(output.status.success(),"{args:?}: {output:?}");
        }
        for (args,edit) in [
            (vec!["terminal","files","list","card-1"],false),
            (vec!["terminal","files","add","card-1","note.md"],true),
            (vec!["terminal","files","save","card-1",&revision,"--file",note_file.to_str().unwrap()],true),
            (vec!["terminal","files","remove","card-1",&revision],true),
            (vec!["terminal","files","read","card-1",&revision],false),
        ] {
            let mut command=Command::new(executable);command.args(&args).arg("--format=json").env("XDG_RUNTIME_DIR",&fixture.root);
            if edit{command.args(["--expect-epoch","epoch","--expect-revision",&revision,"--request-id","files-test"]);}
            let output=command.output().unwrap();assert!(output.status.success(),"{args:?}: {output:?}");
        }
        let export=fixture.root.join(format!("export-{}",Path::new(executable).file_name().unwrap().to_string_lossy()));
        let exported=Command::new(executable).args(["terminal","files","read","card-1",&revision,"--output",export.to_str().unwrap(),"--format=json"]).env("XDG_RUNTIME_DIR",&fixture.root).output().unwrap();
        assert!(exported.status.success(),"{exported:?}");assert_eq!(std::fs::read(&export).unwrap(),vec![b'x';70000]);
        let duplicate=Command::new(executable).args(["terminal","files","read","card-1",&revision,"--output",export.to_str().unwrap(),"--format=json"]).env("XDG_RUNTIME_DIR",&fixture.root).output().unwrap();
        assert_eq!(duplicate.status.code(),Some(8));assert_eq!(std::fs::read(&export).unwrap().len(),70000);
        for args in [vec!["terminal","focus","card-1"],vec!["terminal","raise","card-1"],vec!["terminal","tag","set","card-1","3"]] {
            let output=Command::new(executable).args(&args).args(["--expect-epoch","epoch","--expect-revision",&revision,"--request-id","card-action","--format=json"]).env("XDG_RUNTIME_DIR",&fixture.root).output().unwrap();assert!(output.status.success(),"{args:?}: {output:?}");
        }
        for (args,sizing,edit) in [
            (vec!["terminal","viewport","list","card-1"],false,false),
            (vec!["terminal","viewport","acquire","card-1","--columns","120","--rows","35","--ttl","2m"],true,true),
            (vec!["terminal","viewport","set","card-1","viewport-test","--columns","140","--rows","40"],true,true),
            (vec!["terminal","viewport","release","card-1","viewport-test"],false,true),
        ] {
            let mut command = Command::new(executable);
            command
                .args(&args)
                .arg("--format=json")
                .env("XDG_RUNTIME_DIR", &fixture.root);
            if sizing {
                command.args([
                    "--expect-epoch",
                    "epoch",
                    "--expect-revision",
                    &revision,
                    "--expect-pane-identity",
                    &revision,
                ]);
            }
            if edit {
                command.args(["--request-id", "viewport-test"]);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{args:?}: {output:?}");
        }
        let attach_args = [
            "terminal",
            "attach",
            "card-1",
            "--expect-epoch",
            "epoch",
            "--expect-revision",
            &revision,
            "--expect-pane-identity",
            &revision,
            "--request-id",
            "attach-test",
            "--seconds",
            "1",
        ];
        let attached = Command::new(executable)
            .args(attach_args)
            .arg("--format=jsonl")
            .env("XDG_RUNTIME_DIR", &fixture.root)
            .output()
            .unwrap();
        assert!(attached.status.success(), "{attached:?}");
        assert!(!attached.stdout.contains(&0x1b));
        let records = String::from_utf8(attached.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["type"], "attached");
        assert_eq!(records[2]["type"], "end");
        let refused = Command::new(executable)
            .args(attach_args)
            .args(["--interactive", "--raw"])
            .env("XDG_RUNTIME_DIR", &fixture.root)
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(2));
        // A private PTY, never the desktop/session terminal, verifies raw mode
        // is restored after the detach key and after a handled termination signal.
        for signal in [false, true] {
            use std::io::Write;
            use std::os::fd::AsRawFd;
            let (mut master, slave) = super_desktop::platform::pty::open(80, 24).unwrap();
            let mut before: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::tcgetattr(master.as_raw_fd(), &mut before) },
                0
            );
            let mut command = Command::new(executable);
            command
                .args(attach_args)
                .args(["--interactive", "--raw"])
                .env("XDG_RUNTIME_DIR", &fixture.root)
                .stdin(std::process::Stdio::from(slave.try_clone().unwrap()))
                .stdout(std::process::Stdio::from(slave))
                .stderr(std::process::Stdio::null());
            super_desktop::platform::pty::configure_child_session(&mut command);
            let mut child = command.spawn().unwrap();
            let until = Instant::now() + Duration::from_secs(3);
            loop {
                let mut current = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe { libc::tcgetattr(master.as_raw_fd(), &mut current) },
                    0
                );
                if current.c_lflag & libc::ICANON == 0 {
                    break;
                }
                assert!(Instant::now() < until, "client did not enter raw mode");
                std::thread::sleep(Duration::from_millis(10));
            }
            if signal {
                assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
            } else {
                master.write_all(&[0x1d]).unwrap();
            }
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < until, "client did not detach");
                std::thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(status.code(), Some(if signal { 143 } else { 0 }));
            let mut after = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::tcgetattr(master.as_raw_fd(), &mut after) },
                0
            );
            assert_eq!(after.c_lflag, before.c_lflag);
            assert_eq!(after.c_iflag, before.c_iflag);
            assert_eq!(after.c_oflag, before.c_oflag);
        }
        for action in ["minimize", "restore", "expand", "collapse"] {
            let args = [
                "terminal",
                action,
                "card-1",
                "--expect-epoch=epoch-1",
                "--expect-revision",
                &revision,
                "--request-id=mode-001",
            ];
            let output = Command::new(executable)
                .args(args)
                .arg("--format=json")
                .env("XDG_RUNTIME_DIR", &fixture.root)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(reply["data"]["method"], "terminal.mode");
            assert_eq!(reply["data"]["action"], action);
            assert_eq!(reply["data"]["id"], "card-1");
            assert_eq!(reply["data"]["expectRevision"], revision);
            assert_eq!(reply["requestId"], "mode-001");
            for extra in [
                "--clamp",
                "--width=640",
                "--expect-pane-identity=bad",
                "--all",
                "--allow-download",
                "--screen",
            ] {
                let output = Command::new(executable)
                    .args(args)
                    .arg(extra)
                    .arg("--format=json")
                    .env("XDG_RUNTIME_DIR", &fixture.root)
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), Some(2), "{action} {extra}");
            }
            for missing in [
                vec!["terminal", action, "card-1"],
                args[..4].to_vec(),
                args[..6].to_vec(),
            ] {
                let output = Command::new(executable)
                    .args(missing)
                    .arg("--format=json")
                    .env("XDG_RUNTIME_DIR", &fixture.root)
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), Some(2));
            }
        }
        for arguments in [vec!["audit","list"],vec!["access","list"],vec!["doctor"]] {
            let out=Command::new(executable).args(arguments).arg("--format=json").env("XDG_RUNTIME_DIR",&fixture.root).output().unwrap();assert!(out.status.success(),"{}",String::from_utf8_lossy(&out.stdout));
        }
        let export=fixture.root.join(format!("audit-{}.jsonl",Path::new(executable).file_name().unwrap().to_string_lossy()));
        let export_args=["audit","export","--output",export.to_str().unwrap(),"--format=json"];
        assert!(Command::new(executable).args(export_args).env("XDG_RUNTIME_DIR",&fixture.root).output().unwrap().status.success());
        assert!(std::fs::read_to_string(&export).unwrap().contains("example"));
        assert!(!Command::new(executable).args(export_args).env("XDG_RUNTIME_DIR",&fixture.root).output().unwrap().status.success());
        let events=Command::new(executable).args(["events","--resource","terminals","--seconds","1","--after",&revision,"--format=jsonl"]).env("XDG_RUNTIME_DIR",&fixture.root).output().unwrap();assert!(events.status.success(),"{}",String::from_utf8_lossy(&events.stdout));
        let frames:Vec<serde_json::Value>=String::from_utf8(events.stdout).unwrap().lines().map(|s|serde_json::from_str(s).unwrap()).collect();assert_eq!(frames.len(),2);assert_eq!(frames[0]["type"],"snapshot");assert_eq!(frames[0]["resyncRequired"],true);assert_eq!(frames[0]["sequence"],0);assert_eq!(frames[1]["type"],"end");assert_eq!(frames[1]["sequence"],1);
        let one_shot=fixture.root.join("one-shot.json");std::fs::write(&one_shot,r#"["literal ; $(touch NEVER)",""]"#).unwrap();
        let launch_args=["harness","launch","claude","--cwd","/tmp","--request-id","one-shot","--args-file",one_shot.to_str().unwrap(),"--format=json"];
        assert!(!Command::new(executable).args(launch_args).env("XDG_RUNTIME_DIR",&fixture.root).output().unwrap().status.success());
        let launched=Command::new(executable).args(launch_args).arg("--allow-unsafe-harness").env("XDG_RUNTIME_DIR",&fixture.root).output().unwrap();assert!(launched.status.success());let launched:serde_json::Value=serde_json::from_slice(&launched.stdout).unwrap();assert_eq!(launched["data"]["arguments"],serde_json::json!(["literal ; $(touch NEVER)",""]));
        let input_file = fixture.root.join("input.txt");
        std::fs::write(&input_file, "literal ✓\nsecond line").unwrap();
        for action in ["send", "prompt", "keys", "interrupt"] {
            let mut args = vec![
                "terminal",
                action,
                "card-1",
                "--expect-epoch=epoch-1",
                "--expect-revision",
                &revision,
                "--expect-pane-identity",
                &revision,
                "--request-id=input-001",
            ];
            if matches!(action, "send" | "prompt") {
                args.extend(["--file", input_file.to_str().unwrap()]);
            }
            if action == "prompt" { args.extend(["--attachment", &revision]); }
            if action == "keys" {
                args.extend(["Enter", "Ctrl-C"]);
            }
            let output = Command::new(executable)
                .args(&args)
                .arg("--format=json")
                .env("XDG_RUNTIME_DIR", &fixture.root)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(reply["data"]["method"], "terminal.input");
            assert_eq!(reply["requestId"], "input-001");
            if matches!(action, "send" | "prompt") {
                assert_eq!(reply["data"]["input"]["text"], "literal ✓\nsecond line");
            }
            if action == "prompt" { assert_eq!(reply["data"]["input"]["attachments"], serde_json::json!([revision])); }
            for extra in ["--clamp", "--all", "--width=500", "--raw"] {
                let output = Command::new(executable)
                    .args(&args)
                    .arg(extra)
                    .arg("--format=json")
                    .env("XDG_RUNTIME_DIR", &fixture.root)
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), Some(2), "{action} {extra}");
            }
        }
        let output = Command::new(executable)
            .args(["terminal", "status", "card-1", "--format=json"])
            .env("XDG_RUNTIME_DIR", &fixture.root)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        for (id, baseline, identity, expected) in [
            ("completed-card", "none", revision.as_str(), 0),
            ("card-1", "none", revision.as_str(), 6),
            (
                "completed-card",
                "none",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                5,
            ),
            (
                "completed-card",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                revision.as_str(),
                7,
            ),
        ] {
            let output = Command::new(executable)
                .args([
                    "terminal",
                    "wait",
                    id,
                    "--until=completed",
                    "--after",
                    baseline,
                    "--expect-pane-identity",
                    identity,
                    "--timeout=1s",
                    "--format=json",
                ])
                .env("XDG_RUNTIME_DIR", &fixture.root)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(expected),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        let output = Command::new(executable)
            .args([
                "terminal",
                "follow",
                "card-1",
                "--seconds=1",
                "--format=jsonl",
            ])
            .env("XDG_RUNTIME_DIR", &fixture.root)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            events.len(),
            2,
            "unchanged screen must not duplicate snapshots"
        );
        assert_eq!(events[0]["type"], "snapshot");
        assert_eq!(events[0]["mayHaveGaps"], true);
        assert_eq!(events[1]["type"], "end");
        assert_eq!(events[1]["sequence"], 2);
        let geometry = [
            "terminal",
            "move",
            "card-1",
            "--x",
            "-20",
            "--y=100",
            "--clamp",
            "--expect-epoch",
            "epoch-1",
            "--expect-revision",
            &revision,
            "--request-id",
            "move-001",
        ];
        let output = Command::new(executable)
            .args(geometry)
            .arg("--format=json")
            .env("XDG_RUNTIME_DIR", &fixture.root)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["data"]["x"], -20);
        assert_eq!(reply["data"]["clamp"], true);
        assert_eq!(reply["data"]["expectRevision"], revision);
        assert_eq!(reply["requestId"], "move-001");
        for bad in [
            vec!["--x=80"],
            vec!["--width=640"],
            vec!["--all"],
            vec!["--screen"],
            vec!["--clamp"],
            vec!["--cwd=/tmp"],
        ] {
            let output = Command::new(executable)
                .args(geometry)
                .args(bad)
                .arg("--format=json")
                .env("XDG_RUNTIME_DIR", &fixture.root)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2));
        }
        for (index, value) in [
            (4, "-32769"),
            (4, "1.5"),
            (4, "+1"),
            (10, "bad-revision"),
            (8, "bad epoch"),
            (12, "bad id"),
        ] {
            let mut bad = geometry.to_vec();
            bad[index] = value;
            let output = Command::new(executable)
                .args(bad)
                .arg("--format=json")
                .env("XDG_RUNTIME_DIR", &fixture.root)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2));
        }
        let output = Command::new(executable)
            .args([
                "terminal",
                "resize",
                "card-1",
                "--width",
                "640",
                "--height",
                "480",
                "--expect-epoch",
                "epoch-1",
                "--expect-revision",
                &revision,
                "--request-id",
                "resize-001",
                "--format=json",
            ])
            .env("XDG_RUNTIME_DIR", &fixture.root)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["data"]["width"], 640);
        assert_eq!(reply["data"]["height"], 480);
        assert_eq!(reply["data"]["clamp"], false);
        let close = [
            "terminal",
            "close",
            "card-1",
            "--expect-epoch",
            "epoch-1",
            "--expect-revision",
            &revision,
            "--expect-pane-identity",
            &revision,
            "--request-id",
            "close-001",
        ];
        let output = Command::new(executable)
            .args(close)
            .arg("--format=json")
            .env("XDG_RUNTIME_DIR", &fixture.root)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["data"]["method"], "terminal.close");
        assert_eq!(reply["data"]["expectPaneIdentity"], revision);
        assert_eq!(reply["requestId"], "close-001");
        for extra in [
            vec!["--clamp"],
            vec!["--x=80"],
            vec!["--all"],
            vec!["--screen"],
            vec!["--allow-download"],
            vec!["--expect-pane-identity=bad"],
        ] {
            let output = Command::new(executable)
                .args(close)
                .args(extra)
                .arg("--format=json")
                .env("XDG_RUNTIME_DIR", &fixture.root)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2));
        }
        for (index, value) in [(4, "bad epoch"), (6, "short"), (8, "g123"), (10, "bad/id")] {
            let mut bad = close.to_vec();
            bad[index] = value;
            let output = Command::new(executable)
                .args(bad)
                .arg("--format=json")
                .env("XDG_RUNTIME_DIR", &fixture.root)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2));
        }
        for (args, expected) in [
            (vec!["terminal", "close", "card-1"], 2),
            (
                vec![
                    "terminal",
                    "runtime",
                    "card-1",
                    "--expect-pane-identity=bad",
                ],
                2,
            ),
            (vec!["terminal", "geometry", "card-1"], 0),
            (vec!["terminal", "geometry", "card-1", "--clamp"], 2),
            (vec!["terminal", "move", "card-1", "--x=80", "--y=100"], 2),
            (
                vec![
                    "terminal",
                    "resize",
                    "card-1",
                    "--width=640",
                    "--height=480",
                ],
                2,
            ),
            (vec!["terminal", "runtime", "card-1"], 0),
            (vec!["terminal", "capture", "card-1"], 0),
            (
                vec!["terminal", "capture", "card-1", "--history", "--lines=37"],
                0,
            ),
            (vec!["terminal", "capture", "card-1", "--screen"], 0),
            (
                vec!["terminal", "capture", "card-1", "--screen", "--history"],
                2,
            ),
            (vec!["terminal", "capture", "card-1", "--lines", "37"], 2),
            (
                vec!["terminal", "capture", "card-1", "--history", "--lines=0"],
                2,
            ),
            (
                vec!["terminal", "capture", "card-1", "--history", "--lines=2001"],
                2,
            ),
            (vec!["terminal", "capture", "card-1", "--raw"], 2),
            (vec!["terminal", "capture", "card-1", "--request-id=x"], 2),
            (vec!["terminal", "runtime", "card-1", "--history"], 2),
            (vec!["terminal", "list", "--screen"], 2),
            (
                vec![
                    "harness",
                    "launch",
                    "shell",
                    "--cwd",
                    "/tmp/project with spaces",
                    "--request-id",
                    "launch-001",
                    "--allow-unsafe-harness",
                    "--allow-download",
                ],
                0,
            ),
            (
                vec![
                    "terminal",
                    "create",
                    "--cwd=/tmp/project with spaces",
                    "--request-id=shell-001",
                ],
                0,
            ),
            (vec!["request", "inspect", "launch-001"], 0),
            (vec!["terminal", "create", "--cwd", "/tmp"], 2),
            (
                vec![
                    "terminal",
                    "create",
                    "--cwd",
                    "relative",
                    "--request-id",
                    "bad-cwd",
                ],
                2,
            ),
            (vec!["terminal", "list", "--allow-unsafe-harness"], 2),
            (
                vec![
                    "terminal",
                    "create",
                    "--cwd",
                    "/tmp",
                    "--request-id",
                    "same",
                    "--request-id",
                    "same",
                ],
                2,
            ),
            (vec!["capabilities"], 0),
            (vec!["app", "status"], 0),
            (vec!["terminal", "list"], 0),
            (vec!["terminal", "inspect", "sd_term_missing"], 3),
            (vec!["harness", "list", "--all"], 0),
            (vec!["harness", "inspect", "shell"], 0),
            (vec!["terminal", "list", "--target", "peer:abc"], 6),
            (vec!["harness", "list", "--all", "--all"], 2),
            (vec!["terminal", "inspect"], 2),
            (vec!["app", "status", "--all"], 2),
        ] {
            let output = Command::new(executable)
                .args(&args)
                .args(["--format", "json"])
                .env("HOME", &fixture.root)
                .env("XDG_RUNTIME_DIR", &fixture.root)
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("LD_PRELOAD")
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(expected),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.stderr.is_empty(),
                "machine diagnostics must be structured"
            );
            let text = std::str::from_utf8(&output.stdout).unwrap();
            assert!(!text.contains('\u{009b}') && !text.contains('\u{001b}'));
            let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(data["ok"], expected == 0, "{data}");
            assert_eq!(data["target"], "local");
            if args == ["terminal", "capture", "card-1", "--history", "--lines=37"] {
                assert_eq!(data["data"]["history"], true);
                assert_eq!(data["data"]["lines"], 37);
            }
            if args.starts_with(&["harness", "launch"]) && expected == 0 {
                assert_eq!(data["requestId"], "launch-001");
                assert_eq!(data["data"]["cwd"], "/tmp/project with spaces");
                assert_eq!(data["data"]["allowUnsafeHarness"], true);
                assert_eq!(data["data"]["allowDownload"], true);
            }
            if args.starts_with(&["terminal", "create"]) && expected == 0 {
                assert_eq!(data["requestId"], "shell-001");
                assert_eq!(data["data"]["harness"], "shell");
            }
            if args == ["harness", "list", "--all"] {
                assert_eq!(data["data"]["all"], true);
            }
        }
    }
    std::fs::write(fixture.root.join("older-daemon"), "").unwrap();
    for executable in [
        env!("CARGO_BIN_EXE_super-desktop-client"),
        env!("CARGO_BIN_EXE_super-desktop"),
    ] {
        for args in [
            vec![
                "terminal",
                "create",
                "--cwd",
                "/tmp",
                "--request-id",
                "older-001",
            ],
            vec!["request", "inspect", "older-001"],
            vec!["terminal", "status", "card-1"],
            vec!["terminal", "interrupt", "card-1", "--expect-epoch=epoch-1", "--expect-revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "--expect-pane-identity=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "--request-id=older-input"],
            vec!["terminal", "minimize", "card-1", "--expect-epoch=epoch-1", "--expect-revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "--request-id=older-mode"],
            vec!["terminal", "restore", "card-1", "--expect-epoch=epoch-1", "--expect-revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "--request-id=older-mode"],
            vec!["terminal", "expand", "card-1", "--expect-epoch=epoch-1", "--expect-revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "--request-id=older-mode"],
            vec!["terminal", "collapse", "card-1", "--expect-epoch=epoch-1", "--expect-revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "--request-id=older-mode"],

            vec!["terminal", "close", "card-1", "--expect-epoch=epoch-1", "--expect-revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "--expect-pane-identity=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "--request-id=older-close"],
            vec!["terminal", "geometry", "card-1"],
            vec!["terminal", "move", "card-1", "--x=80", "--y=100", "--expect-epoch=epoch-1", "--expect-revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "--request-id=older-move"],
            vec!["terminal", "runtime", "card-1"],
            vec!["terminal", "capture", "card-1"],
        ] {
            let output = Command::new(executable)
                .args(args)
                .arg("--format=json")
                .env("XDG_RUNTIME_DIR", &fixture.root)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(6));
            let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(reply["error"]["code"], "unsupported_command");
        }
    }
    // Idle clients occupy only the bounded worker pool; excess connections are
    // closed immediately rather than spawning unbounded threads or blocking accept.
    let stalled: Vec<_> = (0..control::MAX_CONNECTIONS)
        .map(|_| std::os::unix::net::UnixStream::connect(&path).unwrap())
        .collect();
    let mut excess = std::os::unix::net::UnixStream::connect(&path).unwrap();
    excess
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut byte = [0];
    use std::io::Read;
    assert_eq!(excess.read(&mut byte).unwrap(), 0);
    drop(stalled);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_super-desktop-client"))
        .args(["app", "status", "--format=json"])
        .env("XDG_RUNTIME_DIR", &fixture.root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(
        path.exists(),
        "client must not unlink an unsafe server socket"
    );
}
