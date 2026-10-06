//! Keeps the test binary away from the user's running desktop.
//!
//! `cargo test` is often run from inside a SUPER DESKTOP card. The test process
//! then inherits the card's `$TMUX` (the user's tmux server, which plain `tmux`
//! commands follow even when `TMUX_TMPDIR` is set), the runtime directory with
//! the running desktop's control socket, and the card's harness variables. A
//! test running `tmux kill-server`, closing a session, talking to the control
//! socket or recording a harness event would act on the user's live terminals.
//!
//! Before any test runs, the test process gets its own tmux directory and
//! runtime directory and loses the card's variables. Every process a test
//! starts, including this binary re-run as a child, inherits that environment.
//! On exit the private tmux servers are stopped and the directory removed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Set to the private root in the test process and everything it starts.
pub const ROOT_ENV: &str = "SD_TEST_ISOLATION_ROOT";

/// What a card puts in its own environment.
const CARD_VARIABLES: [&str; 6] = [
    "TMUX",
    "TMUX_PANE",
    "SD_HARNESS_PID",
    "SD_HARNESS_FILE",
    "SD_HARNESS_EXE",
    "SD_HARNESS_AGENT",
];

#[used]
#[cfg_attr(target_os = "linux", link_section = ".init_array")]
#[cfg_attr(target_os = "macos", link_section = "__DATA,__mod_init_func")]
static ISOLATE: extern "C" fn() = isolate;

/// Runs before `main`, while the process has a single thread.
extern "C" fn isolate() {
    // A child of the test binary keeps what its parent gave it: tests that
    // re-run themselves set their own private tmux and runtime directories.
    if std::env::var_os(ROOT_ENV).is_some() {
        return;
    }
    for name in CARD_VARIABLES {
        std::env::remove_var(name);
    }
    // /tmp, not $TMPDIR: tmux socket paths must stay short.
    let root = owner_root();
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = private_dir(&root)
        .and_then(|_| private_dir(&root.join("tmux")))
        .and_then(|_| private_dir(&root.join("runtime")))
    {
        eprintln!(
            "test isolation: cannot create {}: {error}; refusing to run tests against the live desktop",
            root.display()
        );
        std::process::abort();
    }
    std::env::set_var("TMUX_TMPDIR", root.join("tmux"));
    std::env::set_var("XDG_RUNTIME_DIR", root.join("runtime"));
    std::env::set_var(ROOT_ENV, &root);
    unsafe { libc::atexit(cleanup) };
}

fn owner_root() -> PathBuf {
    PathBuf::from(format!("/tmp/sd-test-{}", std::process::id()))
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

extern "C" fn cleanup() {
    let root = owner_root();
    let sockets = root
        .join("tmux")
        .join(format!("tmux-{}", unsafe { libc::getuid() }));
    for socket in std::fs::read_dir(sockets).into_iter().flatten().flatten() {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(socket.path())
            .arg("kill-server")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fails when the isolation is removed: run from a card, plain `tmux`
    /// would then reach the user's server again.
    #[test]
    fn test_isolation_keeps_tests_off_the_users_tmux_server_and_desktop() {
        let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("test isolation did not run"));
        assert_eq!(root, owner_root());
        for name in CARD_VARIABLES {
            assert!(std::env::var_os(name).is_none(), "{name} reached the tests");
        }
        assert!(PathBuf::from(std::env::var_os("TMUX_TMPDIR").unwrap()).starts_with(&root));
        assert!(crate::platform::runtime::socket_path().starts_with(&root));

        let tmux = |args: &[&str]| Command::new("tmux").args(args).stderr(Stdio::null()).output();
        let Ok(version) = tmux(&["-V"]) else {
            eprintln!("skipping the tmux half: tmux is not installed");
            return;
        };
        assert!(version.status.success());
        let session = format!("sd_isolation_probe_{}", std::process::id());
        let made = tmux(&["-f", "/dev/null", "new-session", "-d", "-s", &session, "sleep 30"]).unwrap();
        assert!(made.status.success(), "{made:?}");
        let socket = tmux(&["display-message", "-p", "-t", &format!("={session}:"), "#{socket_path}"]).unwrap();
        let _ = tmux(&["kill-session", "-t", &format!("={session}")]);
        let socket = String::from_utf8_lossy(&socket.stdout).trim().to_string();
        assert!(Path::new(&socket).starts_with(&root), "plain tmux reached {socket}");
    }
}
