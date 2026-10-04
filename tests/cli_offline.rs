//! Exercise both real entry points without a display, HOME state or daemon.
use std::os::unix::net::UnixListener;
use std::process::Command;

#[test]
fn cli_offline_entry_points_never_connect_or_start_the_application() {
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
    let listener = UnixListener::bind(root.join("super-desktop.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    for binary in [
        env!("CARGO_BIN_EXE_super-desktop-client"),
        env!("CARGO_BIN_EXE_super-desktop"),
    ] {
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
