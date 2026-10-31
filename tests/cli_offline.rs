//! Exercise both real entry points without a display, HOME state or daemon.
use std::os::unix::net::UnixListener;
use std::process::Command;

/// Copying an executable while another test forks can leave its write
/// descriptor open in that child, so running the copy fails with ETXTBSY.
static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn cli_offline_entry_points_never_connect_or_start_the_application() {
    let _spawning = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("sd-cli-offline-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let listener = UnixListener::bind(root.join("super-desktop.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    // The installed executable can answer offline commands even if the GTK
    // sibling binary is absent. This also catches unintended exec fallbacks.
    let standalone = root.join("super-desktop-client");
    std::fs::copy(env!("CARGO_BIN_EXE_super-desktop-client"), &standalone).unwrap();
    for executable in [
        standalone.as_path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_super-desktop")),
    ] {
        for (args, code) in [
            (vec!["--help"], 0),
            (vec!["help", "agents"], 0),
            (vec!["status", "--help"], 0),
            (vec!["harness", "list", "--help"], 0),
            (vec!["harness", "launch", "--help"], 0),
            (vec!["request", "inspect", "--help"], 0),
            (vec!["terminal", "--help"], 0),
            (vec!["terminal", "runtime", "--help"], 0),
            (vec!["terminal", "capture", "--help"], 0),
            (vec!["help", "terminal", "inspect"], 0),
            (vec!["schema", "harness", "list"], 0),
            (vec!["schema", "terminal", "follow"], 0),
            (vec!["--version"], 0),
            (vec!["schema", "--format", "json"], 0),
            (vec!["schema", "missing"], 2),
            (vec!["completion", "bash"], 0),
            (vec!["missing-command"], 2),
        ] {
            let output = Command::new(executable)
                .args(&args)
                .env("HOME", &root)
                .env("XDG_RUNTIME_DIR", &root)
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("WAYLAND_SOCKET")
                .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
                .env_remove("LD_PRELOAD")
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(code),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            if args[0] == "schema" {
                let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(value["schemaVersion"], 1);
                assert_eq!(value["ok"], code == 0);
                assert!(output.stderr.is_empty());
                if args==["schema","terminal","follow"] {
                    assert_eq!(value["data"]["automation"]["terminal follow"]["textPointer"],"/data/text");
                    assert!(value["data"]["responseSchemas"]["terminal follow"]["$defs"]["Capture"]["properties"]["text"].is_object());
                    assert_eq!(value["data"]["responseSchemaCoverage"]["complete"],false);
                }
            }
            if args[0] == "completion" {
                let mut bash = Command::new("bash")
                    .args(["-n"])
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                use std::io::Write;
                bash.stdin
                    .take()
                    .unwrap()
                    .write_all(&output.stdout)
                    .unwrap();
                assert!(bash.wait().unwrap().success());
            }
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        2,
        "offline commands wrote state"
    );
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cli_missing_control_daemon_is_an_error_without_legacy_fallback() {
    let root = std::env::temp_dir().join(format!("sd-cli-missing-{}", std::process::id()));
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let root = root.canonicalize().unwrap();
    let listener = UnixListener::bind(root.join("super-desktop.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    for binary in [
        env!("CARGO_BIN_EXE_super-desktop-client"),
        env!("CARGO_BIN_EXE_super-desktop"),
    ] {
        for extra in [
            vec!["--width","600"],
            vec!["--width","0","--height","300"],
            vec!["--ready-timeout","30s"],
            vec!["--prompt","Hello","--ready-timeout","301s"],
            vec!["--prompt","Hello","--prompt-stdin"],
        ] {
            let output=Command::new(binary).args(["harness","launch","claude","--cwd","/tmp","--request-id","invalid","--format=json"])
                .args(extra).env("HOME",&root).env("XDG_STATE_HOME",&root).env("XDG_RUNTIME_DIR",&root).output().unwrap();
            assert_eq!(output.status.code(),Some(2),"{}",String::from_utf8_lossy(&output.stdout));
        }
        let output = Command::new(binary)
            .args(["app", "status", "--format", "json"])
            .env("HOME", &root)
            .env("XDG_RUNTIME_DIR", &root)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("LD_PRELOAD")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(6));
        let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(data["error"]["code"], "unavailable");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(!root.join("super-desktop").exists());
    }
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// Argument errors of the structured local commands are public output: pin
/// their exit codes, error codes and messages, including the forms that place
/// options between command words. None of them may reach a daemon.
#[test]
fn cli_structured_argument_errors_keep_their_codes_and_messages() {
    let _spawning = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
    use std::os::unix::fs::DirBuilderExt;
    let root = std::env::temp_dir().join(format!("sd-cli-arguments-{}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(root.join("super-desktop"))
        .unwrap();
    let root = root.canonicalize().unwrap();
    let listener = UnixListener::bind(root.join("super-desktop/control-v1.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let rev = "a".repeat(64);
    let long_id = "a".repeat(65);
    let guards = ["--expect-epoch", "e", "--expect-revision", &rev, "--request-id", "r"];
    let mutation = "Mutation requires --request-id with 1-64 ASCII letters, digits, '_' or '-'.";
    let guard_misuse = "Guard options are for move/resize/close/mode changes; --clamp is only for move/resize.";
    let unknown = "Unknown option. Use this command's --help.";
    let invalid = "Invalid command or arguments. Use --help for accepted syntax.";
    let ranges = "Coordinates must be within -32768..32768; dimensions within 1..32768.";
    let capture = "Screen/history options are only for terminal capture; --lines requires --history.";
    let launch_only = "Launch options are only accepted by launch/create commands.";
    let lines = "--lines requires an integer from 1 to 2000.";
    let format = "Use --format text or --format json once.";
    let cases: Vec<(Vec<&str>, i32, &str, &str)> = vec![
        (vec!["terminal", "move", "card-1", "--x", "1"], 2, "invalid_arguments", mutation),
        (vec!["terminal", "move", "card-1", "--x"], 2, "invalid_arguments", "Missing geometry option value."),
        (vec!["terminal", "move", "card-1", "--x="], 2, "invalid_arguments", "Missing geometry option value."),
        (vec!["terminal", "move", "card-1", "--x", "1", "--x", "2"], 2, "invalid_arguments", "Do not repeat geometry options."),
        ([&["terminal", "move", "card-1", "--x", "1", "--y", "2", "--clamp", "--clamp"][..], &guards].concat(), 2, "invalid_arguments", unknown),
        ([&["terminal", "move", "card-1", "--x", "1", "--y", "2", "--clamp=1"][..], &guards].concat(), 2, "invalid_arguments", unknown),
        (vec!["terminal", "capture", "card-1", "--screen", "--history"], 2, "invalid_arguments", "Choose --screen or --history once."),
        (vec!["terminal", "capture", "card-1", "--screen", "--screen"], 2, "invalid_arguments", "Choose --screen or --history once."),
        (vec!["terminal", "capture", "card-1", "--lines", "0"], 2, "invalid_arguments", lines),
        (vec!["terminal", "capture", "card-1", "--lines=+5"], 2, "invalid_arguments", lines),
        (vec!["terminal", "capture", "card-1", "--history", "--lines", "5", "--lines", "6"], 2, "invalid_arguments", "Use --lines once."),
        (vec!["terminal", "capture", "card-1", "--lines", "5"], 2, "invalid_arguments", capture),
        (vec!["terminal", "runtime", "card-1", "--screen"], 2, "invalid_arguments", capture),
        (vec!["terminal", "list", "--request-id", "r"], 2, "invalid_arguments", "--request-id is only for mutation commands."),
        (vec!["terminal", "list", "--request-id"], 2, "invalid_arguments", "Missing option value."),
        (vec!["terminal", "list", "--cwd", "/tmp"], 2, "invalid_arguments", launch_only),
        (vec!["terminal", "list", "--allow-download"], 2, "invalid_arguments", launch_only),
        (vec!["terminal", "list", "--cwd", "/tmp", "--cwd", "/tmp"], 2, "invalid_arguments", "Do not repeat --cwd or --request-id."),
        (vec!["terminal", "list", "--format", "xml"], 2, "invalid_arguments", format),
        (vec!["terminal", "list", "--format=text", "--format=text"], 2, "invalid_arguments", format),
        (vec!["terminal", "list", "--format"], 2, "invalid_arguments", "Missing option value."),
        (vec!["terminal", "list", "--target", "remote"], 6, "unsupported_target", "This command supports --target local only; no local fallback was attempted."),
        (vec!["terminal", "list", "--target", "local", "--target", "local"], 2, "invalid_arguments", "Use --target once."),
        (vec!["terminal", "list", "--target"], 2, "invalid_arguments", "Missing option value."),
        (vec!["terminal", "list", "--bogus"], 2, "invalid_arguments", unknown),
        (vec!["terminal", "list", "--all"], 2, "invalid_arguments", invalid),
        (vec!["terminal", "list", "--clamp"], 2, "invalid_arguments", guard_misuse),
        (vec!["terminal", "list", "--x", "1"], 2, "invalid_arguments", guard_misuse),
        (vec!["terminal", "move", "card-1", "--x", "1", "--y", "2", "--expect-epoch", "e", "--expect-revision", &rev], 2, "invalid_arguments", mutation),
        (vec!["terminal", "move", "card-1", "--x", "1", "--expect-epoch", "e", "--expect-revision", &rev, "--request-id", "r"], 2, "invalid_arguments", "Move/resize require both coordinates/dimensions. All guarded operations require epoch/revision from terminal geometry; close also requires pane identity from terminal runtime."),
        (vec!["terminal", "move", "card-1", "--x", "1", "--y", "2", "--expect-epoch", "bad e", "--expect-revision", &rev, "--request-id", "r"], 2, "invalid_arguments", "Use the epoch and opaque revision returned by terminal geometry."),
        (vec!["terminal", "close", "card-1", "--expect-epoch", "e", "--expect-revision", &rev, "--expect-pane-identity", "bad", "--request-id", "r"], 2, "invalid_arguments", "Copy paneIdentity from terminal runtime."),
        ([&["terminal", "move", "card-1", "--x", "40000", "--y", "2"][..], &guards].concat(), 2, "invalid_arguments", ranges),
        ([&["terminal", "resize", "card-1", "--width", "0", "--height", "2"][..], &guards].concat(), 2, "invalid_arguments", ranges),
        ([&["terminal", "move", "card-1", "--x", "01", "--y", "2"][..], &guards].concat(), 2, "invalid_arguments", "Use canonical decimal integers for geometry."),
        ([&["terminal", "minimize", "card-1", "--clamp"][..], &guards].concat(), 2, "invalid_arguments", guard_misuse),
        (vec!["harness", "--format", "json", "launch", "claude", "--request-id", "r", "--cwd", "rel"], 2, "invalid_arguments", "Launch requires --cwd with an absolute directory path of at most 4096 bytes."),
        (vec!["harness", "--format", "json", "launch", "claude", "--cwd", "/tmp"], 2, "invalid_arguments", mutation),
        (vec!["harness", "--format", "json", "launch", "claude", "--cwd", "/tmp", "--request-id", "r", "--width", "600"], 2, "invalid_arguments", guard_misuse),
        (vec!["harness", "--format", "json", "launch", "claude", "--cwd", "/tmp", "--request-id", "r", "--args-file", "/x"], 2, "invalid_arguments", unknown),
        (vec!["terminal", "--format", "json", "create", "--cwd", "/tmp", "--request-id", "r", "--allow-download"], 2, "invalid_arguments", invalid),
        (vec!["terminal", "bogus", "x"], 2, "invalid_arguments", invalid),
        (vec!["terminal", "--format", "json", "status", "card-1"], 2, "invalid_arguments", invalid),
        (vec!["harness", "list", "extra"], 2, "invalid_arguments", invalid),
        (vec!["harness", "list", "--all", "--all"], 2, "invalid_arguments", unknown),
        (vec!["theme", "--format", "json", "inspect"], 2, "invalid_arguments", invalid),
        (vec!["request", "inspect", &long_id], 2, "invalid_arguments", invalid),
        (vec!["capabilities", "extra"], 2, "invalid_arguments", invalid),
    ];
    for executable in [
        env!("CARGO_BIN_EXE_super-desktop-client"),
        env!("CARGO_BIN_EXE_super-desktop"),
    ] {
        for (args, code, error, message) in &cases {
            let output = Command::new(executable)
                .args(args)
                .env("HOME", &root)
                .env("XDG_RUNTIME_DIR", &root)
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("LD_PRELOAD")
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(*code), "{args:?}");
            if args.contains(&"json") {
                assert!(output.stderr.is_empty(), "{args:?}");
                let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(reply["ok"], false, "{args:?}");
                assert_eq!(reply["requestId"], "", "{args:?}");
                assert_eq!(reply["error"]["code"], *error, "{args:?}");
                assert_eq!(reply["error"]["message"], *message, "{args:?}");
            } else {
                assert!(output.stdout.is_empty(), "{args:?}");
                assert_eq!(
                    String::from_utf8_lossy(&output.stderr),
                    format!("{error}: {message}\n"),
                    "{args:?}"
                );
            }
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "{args:?} reached the daemon"
            );
        }
    }
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}
