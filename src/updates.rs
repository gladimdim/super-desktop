//! Settings → Updates: compare this build with its GitHub repository and
//! update the clone it was built from.
//!
//! Every commit raises the patch version in Cargo.toml (`.githooks/pre-commit`),
//! so a newer version on the clone's upstream branch means newer code. A check
//! fetches that branch, which changes no file, and reads its Cargo.toml.
//! Updating fast-forwards the clone and starts its `rebuild.sh` in a session of
//! its own. That script builds first and replaces this daemon only once the
//! build succeeded, so a failed update leaves the running app alone and this
//! process reports it. A clone with uncommitted changes or commits of its own
//! is never updated.
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long `git fetch` may take before the check gives up.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// New commits listed on the Updates page; the rest are counted.
pub const MAX_CHANGES: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// `major.minor.patch`; a pre-release or build suffix is ignored.
    pub fn parse(text: &str) -> Option<Self> {
        let core = text.trim().split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
        let version = Version(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The version this binary was built as.
pub fn running() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a major.minor.patch version")
}

/// The `version` of a Cargo.toml's `[package]` table.
pub fn manifest_version(toml: &str) -> Option<Version> {
    let mut in_package = false;
    for line in toml.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package {
            let value = line.strip_prefix("version").map(str::trim_start).and_then(|rest| rest.strip_prefix('='));
            if let Some(value) = value {
                return Version::parse(value.trim().trim_matches('"'));
            }
        }
    }
    None
}

/// A SUPER DESKTOP clone: a git work tree whose Cargo.toml names the package.
fn is_clone(dir: &Path) -> bool {
    dir.join(".git").exists()
        && std::fs::read_to_string(dir.join("Cargo.toml"))
            .is_ok_and(|toml| toml.lines().any(|line| line.trim() == r#"name = "super-desktop""#))
}

/// The clone a binary at `exe` was built in (`<clone>/target/release/<binary>`).
pub fn clone_of(exe: &Path) -> Option<PathBuf> {
    let release = exe.parent()?;
    let target = release.parent()?;
    if release.file_name()? != "release" || target.file_name()? != "target" {
        return None;
    }
    let dir = target.parent()?;
    is_clone(dir).then(|| dir.to_path_buf())
}

/// The clone this daemon runs from.
pub fn this_clone() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| format!("Cannot tell where SUPER DESKTOP runs from: {error}"))?;
    // A binary replaced on disk while running reads as "<path> (deleted)".
    let exe = exe.to_string_lossy().trim_end_matches(" (deleted)").to_string();
    let exe = Path::new(&exe).canonicalize().unwrap_or_else(|_| PathBuf::from(&exe));
    clone_of(&exe).ok_or_else(|| {
        format!(
            "SUPER DESKTOP runs from {}, not from a clone's target/release, so it cannot update itself. Run the installer again.",
            exe.display()
        )
    })
}

fn git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        // Never wait for a password nobody can type.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
        .env("GIT_HTTP_LOW_SPEED_TIME", "20")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null());
    command
}

/// `git <args>` in `dir`: its output, or its last error line.
fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(dir).args(args).output().map_err(|error| format!("Cannot run git: {error}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(last_line(&String::from_utf8_lossy(&out.stderr)).unwrap_or_else(|| format!("git {} failed", args.join(" "))))
    }
}

/// `run` for a command that talks to the network, stopped after `timeout`.
fn run_bounded(dir: &Path, args: &[&str], timeout: Duration) -> Result<(), String> {
    let mut child = git(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot run git: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
                }
                return if status.success() {
                    Ok(())
                } else {
                    Err(last_line(&stderr).unwrap_or_else(|| format!("git {} failed", args.join(" "))))
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("no answer within {} seconds", timeout.as_secs()));
            }
        }
    }
}

fn last_line(text: &str) -> Option<String> {
    text.lines().map(str::trim).rev().find(|line| !line.is_empty()).map(str::to_string)
}

/// What a check found.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// This build.
    pub current: Version,
    /// The version on the clone's upstream branch.
    pub latest: Version,
    /// The upstream branch, for example `origin/master`.
    pub upstream: String,
    /// Where that branch lives, for example the GitHub URL.
    pub url: String,
    pub dir: PathBuf,
    /// Subjects of the commits the update brings, newest first, at most
    /// `MAX_CHANGES`; `more` counts the rest.
    pub changes: Vec<String>,
    pub more: usize,
    /// Why this clone cannot be updated as it is, if it cannot.
    pub blocked: Option<String>,
}

impl Status {
    pub fn available(&self) -> bool {
        self.latest > self.current
    }
}

/// Check the clone this daemon runs from.
pub fn check() -> Result<Status, String> {
    check_clone(&this_clone()?, running())
}

/// Fetch `dir`'s upstream branch and compare its version with `current`.
pub fn check_clone(dir: &Path, current: Version) -> Result<Status, String> {
    let upstream = run(dir, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"])
        .map_err(|_| format!("The branch checked out in {} follows no branch on GitHub, so there is nothing to compare with.", dir.display()))?;
    let (remote, branch) = upstream
        .split_once('/')
        .ok_or_else(|| format!("{upstream} is not a remote branch."))?;
    let url = run(dir, &["remote", "get-url", remote]).unwrap_or_else(|_| remote.to_string());
    run_bounded(dir, &["fetch", "--quiet", "--no-tags", remote, branch], FETCH_TIMEOUT)
        .map_err(|error| format!("Could not reach {url}: {error}"))?;
    let manifest = run(dir, &["show", &format!("{upstream}:Cargo.toml")])?;
    let latest = manifest_version(&manifest)
        .ok_or_else(|| format!("Cargo.toml on {upstream} names no version."))?;
    let subjects = run(dir, &["log", "--format=%s", &format!("HEAD..{upstream}")]).unwrap_or_default();
    let subjects: Vec<String> = subjects.lines().map(str::to_string).collect();
    let more = subjects.len().saturating_sub(MAX_CHANGES);
    Ok(Status {
        current,
        latest,
        blocked: blocker(dir, &upstream),
        upstream,
        url,
        dir: dir.to_path_buf(),
        changes: subjects.into_iter().take(MAX_CHANGES).collect(),
        more,
    })
}

/// Why `dir` cannot be fast-forwarded to `upstream` as it is.
fn blocker(dir: &Path, upstream: &str) -> Option<String> {
    match run(dir, &["status", "--porcelain", "--untracked-files=no"]) {
        Ok(changes) if !changes.trim().is_empty() => {
            return Some(format!("{} has uncommitted changes. Commit or stash them, then update.", dir.display()));
        }
        Err(error) => return Some(error),
        Ok(_) => {}
    }
    run(dir, &["merge-base", "--is-ancestor", "HEAD", upstream]).err().map(|_| {
        format!("{} has commits that are not on {upstream}. Merge or rebase them, then update.", dir.display())
    })
}

/// Where an update writes its log, and what tells the next daemon that it was
/// started by an update.
#[derive(Clone, Debug)]
pub struct Paths {
    pub log: PathBuf,
    pub pending: PathBuf,
}

impl Paths {
    pub fn user() -> Result<Self, String> {
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        let dir = Path::new(&home).join(".local/state/super-desktop");
        Ok(Self { log: dir.join("update.log"), pending: dir.join("update-pending") })
    }
}

/// Fast-forward the clone to its upstream and start its `rebuild.sh`, which
/// replaces this daemon once the new build is ready.
pub fn start_update(status: &Status, paths: &Paths) -> Result<Child, String> {
    if let Some(reason) = blocker(&status.dir, &status.upstream) {
        return Err(reason);
    }
    run(&status.dir, &["merge", "--ff-only", "--quiet", &status.upstream])?;
    start_rebuild(&status.dir, status.latest, paths)
}

/// Start `<dir>/rebuild.sh` in a session of its own, so replacing this daemon
/// does not take the rebuild down with it. Its output goes to `paths.log`.
pub fn start_rebuild(dir: &Path, target: Version, paths: &Paths) -> Result<Child, String> {
    if let Some(parent) = paths.log.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("Cannot write {}: {error}", parent.display()))?;
    }
    let log = std::fs::File::create(&paths.log).map_err(|error| format!("Cannot write {}: {error}", paths.log.display()))?;
    let log_err = log.try_clone().map_err(|error| error.to_string())?;
    std::fs::write(&paths.pending, format!("{target}\n"))
        .map_err(|error| format!("Cannot write {}: {error}", paths.pending.display()))?;
    let mut command = Command::new(dir.join("rebuild.sh"));
    command
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    // SAFETY: setsid is async-signal-safe and touches no memory of this process.
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().map_err(|error| {
        let _ = std::fs::remove_file(&paths.pending);
        format!("Cannot start {}: {error}", dir.join("rebuild.sh").display())
    })
}

/// Wait for a rebuild this daemon started. A successful one replaces this
/// process before it ends, so returning at all usually means it failed: the
/// reason is the end of its log.
pub fn wait_rebuild(mut child: Child, paths: &Paths) -> Result<(), String> {
    let status = child.wait().map_err(|error| error.to_string())?;
    let _ = std::fs::remove_file(&paths.pending);
    if status.success() {
        return Ok(());
    }
    let log = std::fs::read_to_string(&paths.log).unwrap_or_default();
    let reason = log
        .lines()
        .rev()
        .find(|line| line.contains("ERROR") || line.starts_with("error"))
        .or_else(|| log.lines().rev().find(|line| !line.trim().is_empty()))
        .unwrap_or("the rebuild stopped")
        .trim()
        .to_string();
    Err(format!("{reason} (full log: {})", paths.log.display()))
}

/// What the daemon started after an update says about it: `None` when this
/// start did not follow an update. `pending` is the version the update was for.
pub fn update_outcome(pending: &str, running: Version, log: &Path) -> Option<(String, String)> {
    let target = Version::parse(pending)?;
    Some(if running >= target {
        ("SUPER DESKTOP updated".to_string(), format!("Now running {running}."))
    } else {
        (
            "SUPER DESKTOP update did not finish".to_string(),
            format!("Still running {running}, not {target}. See {}.", log.display()),
        )
    })
}

/// At daemon start: tell the user how the update that restarted it went.
pub fn announce_finished_update() {
    let Ok(paths) = Paths::user() else { return };
    let Ok(pending) = std::fs::read_to_string(&paths.pending) else { return };
    let _ = std::fs::remove_file(&paths.pending);
    if let Some((summary, body)) = update_outcome(&pending, running(), &paths.log) {
        notify(&summary, &body);
    }
}

/// A desktop notification (Omarchy's shell shows it). Blocks until sent.
pub fn notify(summary: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["--app-name=SUPER DESKTOP", "--icon=system-software-update", summary, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(Version::parse("1.1.0"), Some(Version(1, 1, 0)));
        assert_eq!(Version::parse(" 2.10.3-beta.1 "), Some(Version(2, 10, 3)));
        assert!(Version(1, 1, 10) > Version(1, 1, 9));
        assert!(Version(1, 2, 0) > Version(1, 1, 99));
        for bad in ["1.1", "1.1.x", "", "1.1.0.4", "v1.1.0"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert_eq!(Version(1, 1, 9).to_string(), "1.1.9");
        assert!(running() >= Version(1, 1, 0));
    }

    #[test]
    fn the_package_version_is_read_from_its_own_table() {
        let toml = "[package]\nname = \"super-desktop\"\nversion = \"1.4.2\"\n\n[dependencies]\nfoo = { version = \"9.9.9\" }\n";
        assert_eq!(manifest_version(toml), Some(Version(1, 4, 2)));
        let later = "[dependencies]\nversion = \"9.9.9\"\n[package]\nversion=\"0.3.1\"\n";
        assert_eq!(manifest_version(later), Some(Version(0, 3, 1)));
        assert_eq!(manifest_version("[package]\nname = \"x\"\n"), None);
        // This repository's own manifest is what `running` reports.
        let own = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
        assert_eq!(manifest_version(&own), Some(running()));
    }

    /// A scratch folder per test, removed afterwards.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "sd-updates-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `git` for test repositories: none of the user's configuration, hooks
    /// or signing.
    fn test_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("git must be installed");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn manifest(version: &str) -> String {
        format!("[package]\nname = \"super-desktop\"\nversion = \"{version}\"\nedition = \"2021\"\n\n[dependencies]\nlibc = {{ version = \"0.2.0\" }}\n")
    }

    fn lock(version: &str) -> String {
        format!("version = 4\n\n[[package]]\nname = \"libc\"\nversion = \"0.2.0\"\n\n[[package]]\nname = \"super-desktop\"\nversion = \"{version}\"\ndependencies = [\n \"libc\",\n]\n")
    }

    fn lock_version(lock: &str) -> Option<String> {
        let mut lines = lock.lines();
        lines.find(|line| *line == r#"name = "super-desktop""#)?;
        lines.next()?.strip_prefix("version = \"")?.strip_suffix('"').map(str::to_string)
    }

    /// GitHub, the installed clone, and a second clone that publishes to it.
    fn repositories(scratch: &Scratch) -> (PathBuf, PathBuf) {
        let github = scratch.0.join("github.git");
        let author = scratch.0.join("author");
        test_git(&scratch.0, &["init", "--quiet", "--bare", "--initial-branch=master", github.to_str().unwrap()]);
        test_git(&scratch.0, &["clone", "--quiet", github.to_str().unwrap(), author.to_str().unwrap()]);
        std::fs::write(author.join("Cargo.toml"), manifest("1.1.0")).unwrap();
        std::fs::write(author.join("Cargo.lock"), lock("1.1.0")).unwrap();
        std::fs::write(author.join("rebuild.sh"), "#!/bin/sh\necho building\necho built > built\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(author.join("rebuild.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        test_git(&author, &["add", "."]);
        test_git(&author, &["commit", "--quiet", "-m", "First"]);
        test_git(&author, &["push", "--quiet", "origin", "master"]);
        let installed = scratch.0.join("installed");
        test_git(&scratch.0, &["clone", "--quiet", github.to_str().unwrap(), installed.to_str().unwrap()]);
        (author, installed)
    }

    fn publish(author: &Path, version: &str, subject: &str) {
        std::fs::write(author.join("Cargo.toml"), manifest(version)).unwrap();
        std::fs::write(author.join("Cargo.lock"), lock(version)).unwrap();
        test_git(author, &["commit", "--quiet", "-am", subject]);
        test_git(author, &["push", "--quiet", "origin", "master"]);
    }

    #[test]
    fn a_clone_is_found_from_its_own_release_binary() {
        let scratch = Scratch::new("clone");
        let (_, installed) = repositories(&scratch);
        let release = installed.join("target/release");
        std::fs::create_dir_all(&release).unwrap();
        assert_eq!(clone_of(&release.join("super-desktop")), Some(installed.clone()));
        assert_eq!(clone_of(&installed.join("target/debug/super-desktop")), None);
        assert_eq!(clone_of(Path::new("/usr/bin/super-desktop")), None);
        // A folder that is not SUPER DESKTOP's is not taken for it.
        std::fs::write(installed.join("Cargo.toml"), "[package]\nname = \"other\"\n").unwrap();
        assert_eq!(clone_of(&release.join("super-desktop")), None);
    }

    #[test]
    fn a_newer_github_version_is_offered_and_installed_by_fast_forward() {
        let scratch = Scratch::new("update");
        let (author, installed) = repositories(&scratch);
        let current = Version(1, 1, 0);

        let status = check_clone(&installed, current).unwrap();
        assert!(!status.available(), "nothing new yet: {status:?}");
        assert_eq!(status.upstream, "origin/master");
        assert!(status.changes.is_empty());

        publish(&author, "1.1.1", "Add the first thing");
        publish(&author, "1.1.2", "Fix the second thing");
        let status = check_clone(&installed, current).unwrap();
        assert!(status.available());
        assert_eq!(status.latest, Version(1, 1, 2));
        assert_eq!(status.changes, ["Fix the second thing", "Add the first thing"]);
        assert_eq!(status.blocked, None);
        // A check changes no file.
        assert_eq!(manifest_version(&std::fs::read_to_string(installed.join("Cargo.toml")).unwrap()), Some(current));

        // Local work is never updated over.
        std::fs::write(installed.join("Cargo.lock"), lock("1.1.0-local")).unwrap();
        let dirty = check_clone(&installed, current).unwrap();
        assert!(dirty.blocked.as_deref().is_some_and(|reason| reason.contains("uncommitted changes")), "{dirty:?}");
        let paths = Paths { log: scratch.0.join("state/update.log"), pending: scratch.0.join("state/update-pending") };
        assert!(start_update(&dirty, &paths).is_err());
        test_git(&installed, &["checkout", "--quiet", "--", "Cargo.lock"]);
        test_git(&installed, &["commit", "--quiet", "--allow-empty", "-m", "Local commit"]);
        let diverged = check_clone(&installed, current).unwrap();
        assert!(diverged.blocked.as_deref().is_some_and(|reason| reason.contains("not on origin/master")), "{diverged:?}");
        test_git(&installed, &["reset", "--quiet", "--hard", "HEAD~1"]);

        // The update fast-forwards, runs the clone's rebuild.sh in its own
        // session with its output in the log, and marks the pending version.
        let status = check_clone(&installed, current).unwrap();
        let child = start_update(&status, &paths).unwrap();
        assert_eq!(std::fs::read_to_string(&paths.pending).unwrap().trim(), "1.1.2");
        assert_eq!(wait_rebuild(child, &paths), Ok(()));
        assert!(!paths.pending.exists());
        assert_eq!(test_git(&installed, &["rev-parse", "HEAD"]), test_git(&author, &["rev-parse", "HEAD"]));
        assert_eq!(std::fs::read_to_string(installed.join("built")).unwrap().trim(), "built");
        assert!(std::fs::read_to_string(&paths.log).unwrap().contains("building"));
        assert!(!check_clone(&installed, Version(1, 1, 2)).unwrap().available());
    }

    #[test]
    fn a_failed_rebuild_is_reported_from_its_log() {
        let scratch = Scratch::new("failed");
        let (_, installed) = repositories(&scratch);
        std::fs::write(installed.join("rebuild.sh"), "#!/bin/sh\necho compiling\necho 'ERROR: build produced no binary' >&2\nexit 1\n").unwrap();
        let paths = Paths { log: scratch.0.join("update.log"), pending: scratch.0.join("update-pending") };
        let child = start_rebuild(&installed, Version(1, 1, 3), &paths).unwrap();
        let error = wait_rebuild(child, &paths).unwrap_err();
        assert!(error.starts_with("ERROR: build produced no binary"), "{error}");
        assert!(error.contains("update.log"));
        assert!(!paths.pending.exists(), "a failure is reported here, not by the next start");
    }

    #[test]
    fn the_next_start_says_how_the_update_went() {
        let log = Path::new("/tmp/update.log");
        let (summary, body) = update_outcome("1.1.4\n", Version(1, 1, 4), log).unwrap();
        assert_eq!((summary.as_str(), body.as_str()), ("SUPER DESKTOP updated", "Now running 1.1.4."));
        let (summary, body) = update_outcome("1.1.4", Version(1, 1, 3), log).unwrap();
        assert_eq!(summary, "SUPER DESKTOP update did not finish");
        assert!(body.contains("1.1.3") && body.contains("/tmp/update.log"));
        assert_eq!(update_outcome("garbage", Version(1, 1, 3), log), None);
    }

    /// The repository's pre-commit hook, in a scratch repository.
    #[test]
    fn every_commit_raises_the_patch_version() {
        let scratch = Scratch::new("hook");
        let repo = scratch.0.join("repo");
        test_git(&scratch.0, &["init", "--quiet", "--initial-branch=master", repo.to_str().unwrap()]);
        let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks");
        test_git(&repo, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
        std::fs::write(repo.join("Cargo.toml"), manifest("1.1.0")).unwrap();
        std::fs::write(repo.join("Cargo.lock"), lock("1.1.0")).unwrap();
        std::fs::write(repo.join("README.md"), "one\n").unwrap();
        test_git(&repo, &["add", "."]);
        test_git(&repo, &["commit", "--quiet", "-m", "First"]);
        let committed = |file: &str| test_git(&repo, &["show", &format!("HEAD:{file}")]);
        let versions = || {
            (
                manifest_version(&committed("Cargo.toml")).unwrap().to_string(),
                lock_version(&committed("Cargo.lock")).unwrap(),
            )
        };
        assert_eq!(versions(), ("1.1.1".into(), "1.1.1".into()));

        std::fs::write(repo.join("README.md"), "two\n").unwrap();
        test_git(&repo, &["commit", "--quiet", "-am", "Second"]);
        assert_eq!(versions(), ("1.1.2".into(), "1.1.2".into()));
        // The work tree follows, so nothing is left to commit.
        assert_eq!(test_git(&repo, &["status", "--porcelain"]), "");

        // An unstaged edit to Cargo.toml stays out of the commit.
        let edited = manifest("1.1.2").replace("[dependencies]\n", "[dependencies]\nserde = \"1\"\n");
        std::fs::write(repo.join("Cargo.toml"), &edited).unwrap();
        std::fs::write(repo.join("README.md"), "three\n").unwrap();
        test_git(&repo, &["add", "README.md"]);
        test_git(&repo, &["commit", "--quiet", "-m", "Third"]);
        assert_eq!(versions(), ("1.1.3".into(), "1.1.3".into()));
        assert!(!committed("Cargo.toml").contains("serde"));
        let work_tree = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
        assert!(work_tree.contains("serde") && manifest_version(&work_tree) == Some(Version(1, 1, 3)), "{work_tree}");

        // A release raised by hand keeps its version, and the lock follows it.
        std::fs::write(repo.join("Cargo.toml"), manifest("1.2.0")).unwrap();
        test_git(&repo, &["commit", "--quiet", "-am", "Release 1.2"]);
        assert_eq!(versions(), ("1.2.0".into(), "1.2.0".into()));
        // A partial commit (`git commit <paths>`) still raises it.
        std::fs::write(repo.join("README.md"), "four\n").unwrap();
        test_git(&repo, &["commit", "--quiet", "-m", "Fourth", "README.md"]);
        assert_eq!(versions(), ("1.2.1".into(), "1.2.1".into()));
        std::fs::write(repo.join("README.md"), "five\n").unwrap();
        test_git(&repo, &["commit", "--quiet", "-am", "Fifth"]);
        assert_eq!(versions(), ("1.2.2".into(), "1.2.2".into()), "never lowered after a partial commit");
    }
}
