//! Actual CLI clients against the production local transport in a child process.
#[allow(dead_code)]
#[path = "../src/control.rs"]
mod control;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

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
                control::Command::Launch { .. } | control::Command::InspectRequest { .. } | control::Command::Runtime { .. } | control::Command::Capture { .. } => panic!("client sent an unsupported operation"),
                _ => {}
            }
        }
        let data = match &request.command {
            control::Command::Runtime { id } => serde_json::json!({"id":id,"columns":120,"rows":35}),
            control::Command::Capture { id,history,lines } => serde_json::json!({"id":id,"history":history,"lines":lines,"text":"private\u{001b}text\u{009b}"}),
            control::Command::Launch { harness,cwd,allow_unsafe_harness,allow_download } => serde_json::json!({"harness":harness,"cwd":cwd,"allowUnsafeHarness":allow_unsafe_harness,"allowDownload":allow_download}),
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
    let root = std::env::temp_dir().join(format!("sd-cli-wire-{}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
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
        for (args, expected) in [
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
