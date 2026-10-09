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

/// A private tmux server for one test: its own socket and no config, never
/// the user's server. It starts with its first session and is stopped, and
/// its directory removed, when dropped.
#[cfg(test)]
pub struct TmuxServer {
    directory: PathBuf,
    socket: String,
    env: Vec<(String, String)>,
}

#[cfg(test)]
impl TmuxServer {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        // Not under the isolation root: GTK and re-run children have their
        // own pid but keep the parent's root.
        let directory =
            std::env::temp_dir().join(format!("sd-tmux-{}-{serial}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        private_dir(&directory).unwrap();
        let socket = directory.join("socket").to_str().unwrap().to_string();
        Self { directory, socket, env: Vec::new() }
    }

    /// Also give every command, and so the server they start, `name=value`.
    pub fn env(mut self, name: &str, value: &str) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    /// A scratch directory that lives as long as the server.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn socket(&self) -> &str {
        &self.socket
    }

    /// `tmux` aimed at this server only.
    pub fn command(&self) -> Command {
        tmux_command(&self.socket, &self.env)
    }

    /// `command`, for code that keeps its own way to make tmux commands.
    pub fn commands(&self) -> impl Fn() -> Command + Send + Sync + 'static {
        let (socket, env) = (self.socket.clone(), self.env.clone());
        move || tmux_command(&socket, &env)
    }

    /// Runs a tmux command that must succeed; its trimmed output.
    pub fn run(&self, args: &[&str]) -> String {
        let out = self.command().args(args).output().expect("tmux must be installed");
        assert!(out.status.success(), "tmux {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

#[cfg(test)]
fn tmux_command(socket: &str, env: &[(String, String)]) -> Command {
    let mut command = Command::new(crate::tmux::tmux_bin());
    command
        .args(["-S", socket, "-f", "/dev/null"])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    for (name, value) in env {
        command.env(name, value);
    }
    command
}

#[cfg(test)]
impl Drop for TmuxServer {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-server").output();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// A session on this test process's own default tmux server, the one plain
/// `tmux` reaches here (see the module docs), for code under test that runs
/// plain `tmux`. Killed when dropped, together with a file it was given.
#[cfg(test)]
pub struct TmuxSession {
    name: String,
    file: Option<PathBuf>,
}

#[cfg(test)]
impl TmuxSession {
    /// `tmux new-session -d -s name`, then `options` (such as `-x 80`) and
    /// the `command` to run. Fails the test when tmux does.
    pub fn start(name: &str, options: &[&str], command: &[&str]) -> Self {
        let args: Vec<&str> = ["new-session", "-d", "-s", name].iter().chain(options).chain(command).copied().collect();
        tmux(&args);
        Self { name: name.to_string(), file: None }
    }

    /// Also remove `file` when the session is killed.
    pub fn removing(mut self, file: PathBuf) -> Self {
        self.file = Some(file);
        self
    }
}

#[cfg(test)]
impl Drop for TmuxSession {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.name])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output();
        if let Some(file) = &self.file {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// Polls `done` until it holds, failing the test after five seconds.
#[cfg(test)]
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Environment the test binary's own display and preloads must not reach a
/// re-run child through.
#[cfg(test)]
const DESKTOP_VARIABLES: [&str; 5] = [
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "WAYLAND_SOCKET",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "LD_PRELOAD",
];

/// Re-runs the test `test` (its full path) alone in a child of this binary,
/// in a fresh private root: `root_env` names the root, `HOME` is the root and
/// plain `tmux` reaches only the server in `root/tmux`. `configure` adds the
/// test's own environment. Fails when the child does.
#[cfg(test)]
pub fn rerun_in_private_root(
    test: &str,
    root_env: &str,
    configure: impl FnOnce(&Path, &mut Command),
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    // /tmp, not $TMPDIR: tmux socket paths must stay short.
    let root = PathBuf::from(format!("/tmp/sd-root-{}-{serial}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    private_dir(&root).unwrap();
    private_dir(&root.join("tmux")).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", test, "--nocapture"])
        .env(root_env, &root)
        .env("TMUX_TMPDIR", root.join("tmux"))
        .env("HOME", &root);
    for name in CARD_VARIABLES.iter().chain(&DESKTOP_VARIABLES) {
        child.env_remove(name);
    }
    configure(&root, &mut child);
    let output = child.output().unwrap();
    // Also when the child died before its guard ran. $TMUX outranks
    // TMUX_TMPDIR: left set, this would stop the user's own server.
    let _ = Command::new("tmux")
        .arg("kill-server")
        .env("TMUX_TMPDIR", root.join("tmux"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = std::fs::remove_dir_all(&root);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // A misspelt test path would run nothing and pass.
    assert!(stdout.contains("test result: ok. 1 passed"), "{test} did not run: {stdout}");
}

/// In the child `rerun_in_private_root` started: the private root, and a
/// guard that stops the root's tmux server. `None` in any other run.
#[cfg(test)]
pub fn private_root(root_env: &str) -> Option<(PathBuf, StopTmuxServer)> {
    let root = PathBuf::from(std::env::var_os(root_env)?);
    Some((root, StopTmuxServer))
}

/// Stops the private root's tmux server when dropped.
#[cfg(test)]
pub struct StopTmuxServer;

#[cfg(test)]
impl Drop for StopTmuxServer {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("kill-server")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Runs plain `tmux` (the private root's server) and asserts it succeeded;
/// its output.
#[cfg(test)]
pub fn tmux(args: &[&str]) -> String {
    let output = Command::new("tmux")
        .args(args)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
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
