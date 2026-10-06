//! Settings → Updates: compare this build with its GitHub repository and
//! update the clone it was built from.
//!
//! A clone follows the releases: the `vX.Y.Z` tags on `origin` (see AGENTS.md).
//! A check fetches those tags, which changes no file, and reads the newest
//! one's Cargo.toml. Updating checks that tag out (a detached HEAD) and starts
//! the clone's `rebuild.sh` in a session of its own. That script builds first
//! and replaces this daemon only once the build succeeded, so a failed update
//! leaves the running app alone and this process reports it. A clone with
//! uncommitted changes or commits of its own is never updated.
//!
//! What a clone follows is kept in its git config under `superdesktop.channel`,
//! which install.sh writes too:
//! - `releases` (or nothing): the newest release, as above.
//! - `pinned`: the release tag HEAD is detached on (see the README's
//!   "Installing a specific version"). A clone detached on a release tag with
//!   nothing set was pinned by an older installer, and is pinned too. Its update
//!   is "switch to the newest release", which makes it follow the releases again.
//! - `branch`: its upstream branch, fast-forwarded when a newer version reaches
//!   it (the version in Cargo.toml is raised only for a release).
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
    if exe == Path::new("/usr/lib/super-desktop/super-desktop") {
        return Err("SUPER DESKTOP is managed by your package manager. Update it with Omarchy's system update (omarchy update), then restart SUPER DESKTOP or log out and back in.".into());
    }
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

/// Where a clone records what it follows; install.sh writes the same key.
const CHANNEL_KEY: &str = "superdesktop.channel";

/// What a check found.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// This build.
    pub current: Version,
    /// The newest release, or the version on the clone's upstream branch.
    pub latest: Version,
    /// What the update moves to: the newest release's tag
    /// (`refs/tags/vX.Y.Z`), or the upstream branch, for example `origin/master`.
    pub upstream: String,
    /// The newest release tag (`vX.Y.Z`), unless the clone follows a branch.
    pub release: Option<String>,
    /// Where that branch lives, for example the GitHub URL.
    pub url: String,
    pub dir: PathBuf,
    /// Subjects of the commits the update brings, newest first, at most
    /// `MAX_CHANGES`; `more` counts the rest.
    pub changes: Vec<String>,
    pub more: usize,
    /// Why this clone cannot be updated as it is, if it cannot.
    pub blocked: Option<String>,
    /// The release tag this clone is pinned to (a detached HEAD exactly on a
    /// `vX.Y.Z` tag), which it stays on until the user switches to the newest.
    pub pinned: Option<String>,
}

impl Status {
    pub fn available(&self) -> bool {
        self.latest > self.current
    }
}

/// Check the clone this daemon runs from.
pub fn check() -> Result<Status, String> {
    #[cfg(target_os = "macos")]
    { Err("Updates are unavailable for this macOS development build.".into()) }
    #[cfg(target_os = "linux")]
    check_clone(&this_clone()?, running())
}

/// The version a release tag names: `v1.2.3` is 1.2.3.
fn release_version(tag: &str) -> Option<Version> {
    tag.strip_prefix('v').and_then(Version::parse).filter(|version| tag == format!("v{version}"))
}

/// The newest of `tags` (one per line) that is a release tag `vX.Y.Z`.
fn newest_release(tags: &str) -> Option<String> {
    tags.lines()
        .map(str::trim)
        .filter_map(|tag| Some((release_version(tag)?, tag)))
        .max()
        .map(|(_, tag)| tag.to_string())
}

/// The release tag `dir` is pinned to: HEAD is detached exactly on a `vX.Y.Z` tag.
fn pinned_tag(dir: &Path) -> Option<String> {
    if run(dir, &["symbolic-ref", "--quiet", "HEAD"]).is_ok() {
        return None;
    }
    newest_release(&run(dir, &["tag", "--points-at", "HEAD"]).ok()?)
}

/// Fetch what `dir` follows and compare its version with `current`: the newest
/// release tag on `origin`, or the upstream branch of a clone set to follow one.
pub fn check_clone(dir: &Path, current: Version) -> Result<Status, String> {
    let channel = run(dir, &["config", "--get", CHANNEL_KEY]).ok();
    let pinned = match channel.as_deref() {
        Some("pinned") | None => pinned_tag(dir),
        Some(_) => None,
    };
    let (upstream, release, url) = if channel.as_deref() == Some("branch") {
        let upstream = run(dir, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"])
            .map_err(|_| format!("The branch checked out in {} follows no branch on GitHub, so there is nothing to compare with.", dir.display()))?;
        let (remote, branch) = upstream
            .split_once('/')
            .ok_or_else(|| format!("{upstream} is not a remote branch."))?;
        let url = run(dir, &["remote", "get-url", remote]).unwrap_or_else(|_| remote.to_string());
        run_bounded(dir, &["fetch", "--quiet", "--no-tags", remote, branch], FETCH_TIMEOUT)
            .map_err(|error| format!("Could not reach {url}: {error}"))?;
        (upstream, None, url)
    } else {
        let url = run(dir, &["remote", "get-url", "origin"]).unwrap_or_else(|_| "origin".to_string());
        // Only the release tags: a tag moved on GitHub replaces the local one.
        run_bounded(dir, &["fetch", "--quiet", "--no-tags", "origin", "+refs/tags/v*:refs/tags/v*"], FETCH_TIMEOUT)
            .map_err(|error| format!("Could not reach {url}: {error}"))?;
        let tag = newest_release(&run(dir, &["tag", "--list", "v*"])?)
            .ok_or_else(|| format!("{url} has no releases yet."))?;
        (format!("refs/tags/{tag}"), Some(tag), url)
    };
    let manifest = run(dir, &["show", &format!("{upstream}:Cargo.toml")])?;
    let shown = release.as_deref().unwrap_or(&upstream);
    let latest = manifest_version(&manifest)
        .ok_or_else(|| format!("Cargo.toml on {shown} names no version."))?;
    let subjects = run(dir, &["log", "--format=%s", &format!("HEAD..{upstream}")]).unwrap_or_default();
    let subjects: Vec<String> = subjects.lines().map(str::to_string).collect();
    let more = subjects.len().saturating_sub(MAX_CHANGES);
    Ok(Status {
        current,
        latest,
        blocked: blocker(dir, &upstream, shown),
        pinned,
        upstream,
        release,
        url,
        dir: dir.to_path_buf(),
        changes: subjects.into_iter().take(MAX_CHANGES).collect(),
        more,
    })
}

/// Why `dir` cannot be moved forward to `upstream` (named `shown`) as it is.
fn blocker(dir: &Path, upstream: &str, shown: &str) -> Option<String> {
    match run(dir, &["status", "--porcelain", "--untracked-files=no"]) {
        Ok(changes) if !changes.trim().is_empty() => {
            return Some(format!("{} has uncommitted changes. Commit or stash them, then update.", dir.display()));
        }
        Err(error) => return Some(error),
        Ok(_) => {}
    }
    run(dir, &["merge-base", "--is-ancestor", "HEAD", upstream]).err().map(|_| {
        format!("{} has commits that are not on {shown}. Merge or rebase them, then update.", dir.display())
    })
}

impl Status {
    /// What `blocker` calls the place the update moves to.
    fn shown(&self) -> &str {
        self.release.as_deref().unwrap_or(&self.upstream)
    }
}

/// Move `dir` to `target`: check out a release (and follow the releases from
/// then on, leaving any pin), or fast-forward the branch.
fn move_to(status: &Status, target: &str) -> Result<(), String> {
    if status.release.is_some() {
        run(&status.dir, &["-c", "advice.detachedHead=false", "checkout", "--quiet", "--detach", target])?;
        run(&status.dir, &["config", CHANNEL_KEY, "releases"])?;
    } else {
        run(&status.dir, &["merge", "--ff-only", "--quiet", target])?;
    }
    Ok(())
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

/// Move the clone to the newest release (or its upstream branch) and start its
/// `rebuild.sh`, which replaces this daemon once the new build is ready. A
/// pinned clone leaves its pin for the newest release.
pub fn start_update(status: &Status, paths: &Paths) -> Result<Child, String> {
    if let Some(reason) = blocker(&status.dir, &status.upstream, status.shown()) {
        return Err(reason);
    }
    move_to(status, &status.upstream)?;
    start_rebuild(&status.dir, status.latest, paths)
}

/// Local CLI installation pins the reviewed commit instead of a moving ref.
/// Pinned installs remain pinned; switching branches belongs to the existing UI.
pub fn cli_commit(status:&Status)->Result<String,String>{
    let commit=run(&status.dir,&["rev-parse","--verify",&format!("{}^{{commit}}",status.upstream)])?;
    if commit.len()!=40||!commit.bytes().all(|b|b.is_ascii_hexdigit()){return Err("Invalid upstream commit".into());}Ok(commit)
}
pub fn start_cli_update(status:&Status,commit:&str,paths:&Paths)->Result<Child,String>{
    if status.pinned.is_some(){return Err("This install is pinned; switch to latest explicitly in Settings first.".into());}
    if !status.available(){return Err("No newer released version is available.".into());}
    if status.release.is_none()&&(run(&status.dir,&["symbolic-ref","--quiet","HEAD"]).is_err()||run(&status.dir,&["rev-parse","--abbrev-ref","--symbolic-full-name","@{upstream}"])?!=status.upstream){return Err("The installed branch changed; check again.".into());}
    if cli_commit(status)?!=commit{return Err("The upstream changed; check again.".into());}
    if let Some(reason)=blocker(&status.dir,&status.upstream,status.shown()){return Err(reason);}
    let version=run(&status.dir,&["show",&format!("{commit}:Cargo.toml")])?;
    if manifest_version(&version)!=Some(status.latest){return Err("Checked version changed.".into());}
    move_to(status,commit)?;
    start_rebuild(&status.dir,status.latest,paths)
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
    fn only_a_release_is_offered_and_the_update_checks_its_tag_out() {
        let scratch = Scratch::new("update");
        let (author, installed) = repositories(&scratch);
        let current = Version(1, 1, 0);
        test_git(&author, &["tag", "-a", "v1.1.0", "-m", "SUPER DESKTOP 1.1.0"]);
        test_git(&author, &["push", "--quiet", "origin", "v1.1.0"]);

        let status = check_clone(&installed, current).unwrap();
        assert!(!status.available(), "nothing new yet: {status:?}");
        assert_eq!(status.release.as_deref(), Some("v1.1.0"));
        assert_eq!(status.upstream, "refs/tags/v1.1.0");
        assert_eq!(status.pinned, None, "a clone on master is not pinned");

        // A version raised on master without a release is not offered.
        publish(&author, "1.1.1", "Add the first thing");
        publish(&author, "1.1.2", "Fix the second thing");
        assert!(!check_clone(&installed, current).unwrap().available());
        test_git(&author, &["tag", "-a", "v1.1.2", "-m", "SUPER DESKTOP 1.1.2"]);
        test_git(&author, &["tag", "v1.1.10-rc"]);
        test_git(&author, &["tag", "not-a-release"]);
        test_git(&author, &["push", "--quiet", "origin", "--tags"]);
        publish(&author, "1.1.3", "Unreleased work");
        let status = check_clone(&installed, current).unwrap();
        assert!(status.available());
        assert_eq!(status.latest, Version(1, 1, 2));
        assert_eq!(status.release.as_deref(), Some("v1.1.2"));
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
        assert!(diverged.blocked.as_deref().is_some_and(|reason| reason.contains("not on v1.1.2")), "{diverged:?}");
        assert!(start_update(&diverged, &paths).is_err());
        test_git(&installed, &["reset", "--quiet", "--hard", "HEAD~1"]);

        // The update checks the release out, runs the clone's rebuild.sh in
        // its own session with its output in the log, and marks the pending
        // version. The clone then follows the releases, not master.
        let status = check_clone(&installed, current).unwrap();
        let child = start_update(&status, &paths).unwrap();
        assert_eq!(std::fs::read_to_string(&paths.pending).unwrap().trim(), "1.1.2");
        assert_eq!(wait_rebuild(child, &paths), Ok(()));
        assert!(!paths.pending.exists());
        assert_eq!(test_git(&installed, &["rev-parse", "HEAD"]), test_git(&author, &["rev-parse", "v1.1.2^{commit}"]));
        assert_eq!(test_git(&installed, &["config", "--get", CHANNEL_KEY]), "releases");
        assert_eq!(std::fs::read_to_string(installed.join("built")).unwrap().trim(), "built");
        assert!(std::fs::read_to_string(&paths.log).unwrap().contains("building"));
        let after = check_clone(&installed, Version(1, 1, 2)).unwrap();
        assert!(!after.available());
        assert_eq!(after.pinned, None, "following the releases is not a pin");
    }

    #[test]
    fn a_clone_set_to_follow_master_is_fast_forwarded_to_a_raised_version() {
        let scratch = Scratch::new("branch");
        let (author, installed) = repositories(&scratch);
        test_git(&installed, &["config", CHANNEL_KEY, "branch"]);
        let current = Version(1, 1, 0);
        let status = check_clone(&installed, current).unwrap();
        assert_eq!((status.upstream.as_str(), status.release.as_deref()), ("origin/master", None));
        assert!(!status.available());
        publish(&author, "1.1.1", "Add the first thing");
        let status = check_clone(&installed, current).unwrap();
        assert!(status.available());
        assert_eq!(status.changes, ["Add the first thing"]);
        let paths = Paths { log: scratch.0.join("state/update.log"), pending: scratch.0.join("state/update-pending") };
        assert_eq!(wait_rebuild(start_update(&status, &paths).unwrap(), &paths), Ok(()));
        assert_eq!(test_git(&installed, &["symbolic-ref", "--short", "HEAD"]), "master");
        assert_eq!(test_git(&installed, &["rev-parse", "HEAD"]), test_git(&author, &["rev-parse", "HEAD"]));
        assert_eq!(test_git(&installed, &["config", "--get", CHANNEL_KEY]), "branch");
    }

    #[test]
    fn cli_update_installs_only_the_reviewed_commit_and_refuses_moved_refs() {
        let scratch=Scratch::new("cli-pinned-commit");let (author,installed)=repositories(&scratch);let current=Version(1,1,0);
        let paths=Paths {log:scratch.0.join("state/update.log"),pending:scratch.0.join("state/update-pending")};
        // A release tag moved after the check is refused.
        publish(&author,"1.1.1","Reviewed release");test_git(&author,&["tag","v1.1.1"]);test_git(&author,&["push","--quiet","origin","v1.1.1"]);
        let checked=check_clone(&installed,current).unwrap();let commit=cli_commit(&checked).unwrap();
        test_git(&author,&["commit","--quiet","--allow-empty","-m","Retagged"]);test_git(&author,&["tag","-f","v1.1.1"]);test_git(&author,&["push","--quiet","--force","origin","v1.1.1"]);
        let moved=check_clone(&installed,current).unwrap();assert!(start_cli_update(&moved,&commit,&paths).is_err());assert!(!installed.join("built").exists());
        let commit=cli_commit(&moved).unwrap();let mut child=start_cli_update(&moved,&commit,&paths).unwrap();assert!(child.wait().unwrap().success());
        assert_eq!(test_git(&installed,&["rev-parse","HEAD"]),commit);assert_eq!(test_git(&installed,&["config","--get",CHANNEL_KEY]),"releases");
        // A clone following master binds the branch commit, as before.
        let scratch=Scratch::new("cli-branch");let (author,installed)=repositories(&scratch);test_git(&installed,&["config",CHANNEL_KEY,"branch"]);
        publish(&author,"1.1.1","Reviewed release");let checked=check_clone(&installed,current).unwrap();let commit=cli_commit(&checked).unwrap();
        publish(&author,"1.1.2","Later release");let latest=check_clone(&installed,current).unwrap();assert!(start_cli_update(&checked,&commit,&paths).is_err());assert!(!installed.join("built").exists());
        let commit=cli_commit(&latest).unwrap();let mut child=start_cli_update(&latest,&commit,&paths).unwrap();assert!(child.wait().unwrap().success());assert_eq!(test_git(&installed,&["rev-parse","HEAD"]),commit);
        test_git(&installed,&["checkout","--quiet","--detach","HEAD"]);assert!(start_cli_update(&latest,&commit,&paths).is_err());
    }

    #[test]
    fn a_clone_pinned_to_a_release_tag_is_offered_a_switch_to_the_newest_release() {
        let scratch = Scratch::new("pinned");
        let (author, installed) = repositories(&scratch);
        // Release v1.1.0 with an annotated tag, as real releases are, then release more.
        test_git(&author, &["tag", "-a", "v1.1.0", "-m", "SUPER DESKTOP 1.1.0"]);
        test_git(&author, &["tag", "not-a-release"]);
        publish(&author, "1.1.1", "Add the first thing");
        test_git(&author, &["tag", "-a", "v1.1.1", "-m", "SUPER DESKTOP 1.1.1"]);
        publish(&author, "1.1.2", "Unreleased work");
        test_git(&author, &["push", "--quiet", "origin", "--tags"]);
        test_git(&installed, &["fetch", "--quiet", "--tags"]);
        test_git(&installed, &["checkout", "--quiet", "--detach", "v1.1.0"]);
        let current = Version(1, 1, 0);

        // Detached on a release with nothing recorded: an older installer's pin.
        assert_eq!(pinned_tag(&installed).as_deref(), Some("v1.1.0"));
        let status = check_clone(&installed, current).unwrap();
        assert_eq!(status.pinned.as_deref(), Some("v1.1.0"));
        assert_eq!(status.release.as_deref(), Some("v1.1.1"));
        assert!(status.available());
        assert_eq!(status.latest, Version(1, 1, 1));
        assert_eq!(status.changes, ["Add the first thing"]);
        assert_eq!(status.blocked, None);
        // A check leaves the pin alone, and so does a recorded pin.
        assert_eq!(pinned_tag(&installed).as_deref(), Some("v1.1.0"));
        test_git(&installed, &["config", CHANNEL_KEY, "pinned"]);
        assert_eq!(check_clone(&installed, current).unwrap().pinned.as_deref(), Some("v1.1.0"));
        // A clone that follows the releases is not pinned on a release tag.
        test_git(&installed, &["config", CHANNEL_KEY, "releases"]);
        assert_eq!(check_clone(&installed, current).unwrap().pinned, None);
        // A plain branch is never taken for a pin.
        test_git(&installed, &["checkout", "--quiet", "-B", "master", "origin/master"]);
        assert_eq!(pinned_tag(&installed), None);
    }

    #[test]
    fn switching_a_pinned_clone_moves_it_to_the_newest_release() {
        let scratch = Scratch::new("unpin");
        let (author, installed) = repositories(&scratch);
        test_git(&author, &["tag", "v1.1.0"]);
        publish(&author, "1.1.1", "Add the first thing");
        test_git(&author, &["tag", "v1.1.1"]);
        publish(&author, "1.1.2", "Unreleased work");
        test_git(&author, &["push", "--quiet", "origin", "--tags"]);
        test_git(&installed, &["fetch", "--quiet", "--tags"]);
        test_git(&installed, &["checkout", "--quiet", "--detach", "v1.1.0"]);
        test_git(&installed, &["config", CHANNEL_KEY, "pinned"]);

        let status = check_clone(&installed, Version(1, 1, 0)).unwrap();
        let paths = Paths { log: scratch.0.join("state/update.log"), pending: scratch.0.join("state/update-pending") };
        let child = start_update(&status, &paths).unwrap();
        assert_eq!(std::fs::read_to_string(&paths.pending).unwrap().trim(), "1.1.1");
        assert_eq!(wait_rebuild(child, &paths), Ok(()));
        // On the newest release, following the releases, not master.
        assert_eq!(test_git(&installed, &["rev-parse", "HEAD"]), test_git(&author, &["rev-parse", "v1.1.1^{commit}"]));
        assert_eq!(test_git(&installed, &["config", "--get", CHANNEL_KEY]), "releases");
        let after = check_clone(&installed, Version(1, 1, 1)).unwrap();
        assert_eq!(after.pinned, None);
        assert!(!after.available());
    }

    #[test]
    fn release_tags_are_exact_versions() {
        assert_eq!(release_version("v1.1.21"), Some(Version(1, 1, 21)));
        for tag in ["1.1.21", "v1.1", "v1.1.21-rc", "v01.1.2", "not-a-release"] {
            assert_eq!(release_version(tag), None, "{tag}");
        }
        assert_eq!(newest_release("v1.1.9\nv1.1.10\nv1.2.0-rc\nother\n").as_deref(), Some("v1.1.10"));
        assert_eq!(newest_release("other\n"), None);
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

    /// The repository's pre-commit hook, in a scratch repository: it never
    /// changes a version, and refuses a commit where Cargo.toml and Cargo.lock
    /// disagree.
    #[test]
    fn a_commit_leaves_the_version_alone_and_keeps_the_lock_in_step() {
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
        assert_eq!(versions(), ("1.1.0".into(), "1.1.0".into()));

        // Ordinary commits do not raise it, and leave the work tree clean.
        std::fs::write(repo.join("README.md"), "two\n").unwrap();
        test_git(&repo, &["commit", "--quiet", "-am", "Second"]);
        assert_eq!(versions(), ("1.1.0".into(), "1.1.0".into()));
        assert_eq!(test_git(&repo, &["status", "--porcelain"]), "");

        // A manifest raised without its lock is refused.
        std::fs::write(repo.join("Cargo.toml"), manifest("1.2.0")).unwrap();
        let refused = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
            .args(["commit", "--quiet", "-am", "Half a release"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(!refused.status.success(), "a manifest and a lock that disagree must not be committed");
        assert!(String::from_utf8_lossy(&refused.stderr).contains("Cargo.lock says 1.1.0"));
        assert_eq!(versions(), ("1.1.0".into(), "1.1.0".into()));

        // A release raises both together.
        std::fs::write(repo.join("Cargo.lock"), lock("1.2.0")).unwrap();
        test_git(&repo, &["commit", "--quiet", "-am", "Release 1.2.0"]);
        assert_eq!(versions(), ("1.2.0".into(), "1.2.0".into()));
    }
}
