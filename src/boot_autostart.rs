//! ⚡ Settings → System launch: start the daemon when this PC starts.
//!
//! The daemon runs headless — with no display it draws no overlay, while it
//! restores the cards, runs their harnesses and keeps the phone bridge up
//! (see `headless`) — so on
//! a headless PC (or a machine one only ever reaches from the phone) the daemon
//! alone is enough to keep it reachable. Hyprland's login autostart cannot
//! cover that: a headless PC starts no Hyprland session. This setting installs
//! one private systemd **user** service that runs `super-desktop daemon` at
//! every login of this user, a text console included.
//!
//! Ownership is exact: the setting owns exactly one file,
//! `super-desktop-boot.service` under `~/.config/systemd/user`, and turning it
//! off removes it. Hyprland's separate login autostart (`autostart.lua`) is
//! left alone; the singleton daemon guard makes the two coexist. Off by
//! default, and everything that touches the machine takes `*_at` parameters so
//! the tests drive the real code against a temp home and a stub `systemctl`
//! — the same shape as `shortcut::*_at`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// The one unit this app owns under `~/.config/systemd/user`.
const UNIT_NAME: &str = "super-desktop-boot.service";

/// The per-user launcher `install.sh` puts in `~/.local/bin`, and the Arch
/// package's own path as the fallback. An executable one of these is what the
/// unit starts.
const USER_LAUNCHER: &str = ".local/bin/super-desktop";
const PACKAGE_LAUNCHER: &str = "/usr/bin/super-desktop";

/// The last enable/disable failure, so `status` can say why the switch never
/// took rather than quietly showing "Off".
static LAST_CHANGE: Mutex<Option<String>> = Mutex::new(None);

/// The directory this user's units live in.
fn unit_dir(home: &Path) -> PathBuf {
    home.join(".config/systemd/user")
}

/// This app's unit file.
fn unit_file(home: &Path) -> PathBuf {
    unit_dir(home).join(UNIT_NAME)
}

/// Is this a runnable program?
fn runnable(path: &Path) -> bool {
    if let Ok(meta) = fs::metadata(path) {
        return meta.is_file() && meta.permissions().mode() & 0o111 != 0;
    }
    false
}

/// The launcher the unit will start: the per-user entry `install.sh` puts at
/// `~/.local/bin` (it delegates `daemon` to the built binary), then the Arch
/// package's path. An empty string means nothing is installed to start at login.
fn launcher(home: &Path, package: &str) -> String {
    let user = home.join(USER_LAUNCHER);
    if runnable(&user) {
        return user.to_string_lossy().to_string();
    }
    let packaged = PathBuf::from(package);
    if runnable(&packaged) {
        return packaged.to_string_lossy().to_string();
    }
    String::new()
}

/// The unit's contents: run the launcher's `daemon` action in the foreground,
/// which is exactly what `super-desktop daemon` does when nothing backgrounds
/// it. No display variables are needed: headless is fine, and where a desktop
/// session exists the service's daemon is the live one the Hyprland autostart
/// then defers to.
fn unit_text(executable: &str) -> String {
    format!(
        "\
# SUPER DESKTOP Settings → System launch. Turning the setting off
# removes this file. Without a display the daemon draws no overlay;
# its cards, harnesses and phone bridge still run and answer.
[Unit]
Description=SUPER DESKTOP daemon at login

[Service]
ExecStart={executable} daemon

[Install]
WantedBy=default.target
"
    )
}

/// Run `systemctl` (or a test stub), returning trimmed stdout. `Err` carries
/// whatever the tool said, verbatim.
fn systemctl_run(bin: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(if detail.is_empty() {
            format!("{bin} failed")
        } else {
            detail
        });
    }
    Ok(stdout)
}

/// Turn the setting on: write the unit, then `systemctl --user enable` it. A
/// failed enable leaves the file (an installed unit that no target wants never
/// starts) and says so; turning the setting off removes it.
fn enable_at(home: &Path, systemctl_bin: &str, package: &str) -> Result<(), String> {
    let executable = launcher(home, package);
    if executable.is_empty() {
        return Err("SUPER DESKTOP's installed launcher was not found — nothing to start at login.".into());
    }
    let directory = unit_dir(home);
    fs::create_dir_all(&directory).map_err(|e| format!("could not create {}: {e}", directory.display()))?;
    let path = unit_file(home);
    fs::write(&path, unit_text(&executable)).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    if let Err(reason) = systemctl_run(systemctl_bin, &["--user", "enable", UNIT_NAME]) {
        return Err(reason);
    }
    Ok(())
}

/// Turn the setting off: `systemctl --user disable` first (so systemd drops
/// the target's link while it can still see the file), then delete the file.
/// If the file is gone there is nothing left to start at login whatever
/// `disable` said, so a missing unit is not an error.
fn disable_at(home: &Path, systemctl_bin: &str) -> Result<(), String> {
    let path = unit_file(home);
    if path.is_file() {
        let _ = systemctl_run(systemctl_bin, &["--user", "disable", UNIT_NAME]);
        return fs::remove_file(&path).map_err(|e| format!("could not remove {}: {e}", path.display()));
    }
    Ok(())
}

/// What the Settings page shows: ask systemd (or the test stub) whether our
/// unit is wanted. Without `systemctl` the file's presence is the answer.
fn status_at(home: &Path, systemctl_bin: &str) -> String {
    let unit_exists = unit_file(home).is_file();
    let answer = systemctl_run(systemctl_bin, &["--user", "is-enabled", UNIT_NAME]);
    let (enabled, checked_by_systemd) = match answer {
        Ok(text) => (text.trim() == "enabled", true),
        Err(_) => (unit_exists, false),
    };
    let state = if enabled {
        if checked_by_systemd {
            "On — the daemon, its harnesses and its phone bridge start at every login on this PC, even with no desktop."
        } else {
            "On — the daemon, its harnesses and its phone bridge start at every login on this PC, even with no desktop. (checked by the file, not systemd)"
        }
    } else if unit_exists {
        "Not enabled — the unit is on disk but no login starts it; turning the switch on again will fix this."
    } else {
        "Off — the daemon starts when SUPER DESKTOP is opened on this PC."
    };
    if let Ok(record) = LAST_CHANGE.lock() {
        if let Some(reason) = record.clone() {
            return format!("{state} Last change failed: {reason}.");
        }
    }
    state.to_string()
}

#[cfg(target_os = "linux")]
pub fn status() -> String {
    status_at(&crate::state::home_dir(), "systemctl")
}

#[cfg(target_os = "macos")]
pub fn status() -> String {
    "System launch is unavailable in this macOS build.".to_string()
}

#[cfg(target_os = "linux")]
pub fn apply(enabled: bool) {
    let result = if enabled {
        enable_at(&crate::state::home_dir(), "systemctl", PACKAGE_LAUNCHER)
    } else {
        disable_at(&crate::state::home_dir(), "systemctl")
    };
    if let Ok(mut record) = LAST_CHANGE.lock() {
        match result {
            Ok(_) => {
                *record = None;
            }
            Err(reason) => {
                *record = Some(reason);
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn apply(enabled: bool) {
    let _ = enabled;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A private temp home for one test tag.
    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sd-boot-autostart-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A stand-in for `systemctl` that logs its arguments and answers with
    /// `script`.
    fn stub_systemctl(dir: &Path, tag: &str, script: &str) -> String {
        let calls = dir.join("calls.log");
        let path = dir.join(format!("systemctl-{tag}"));
        fs::write(
            &path,
            &format!(
                "#!/bin/sh\necho \"$@\" >> {calls}\n{script}\n",
                calls = calls.display(),
                script = script,
            ),
        ).expect("write stub");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod stub");
        path.to_string_lossy().to_string()
    }

    /// An installed launcher the unit can start, made executable.
    fn make_launcher(dir: &Path, relative: &str) {
        let path = dir.join(relative);
        fs::create_dir_all(&path.parent().unwrap()).expect("mkdir launcher");
        fs::write(&path, "#!/bin/sh\n").expect("write launcher");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod launcher");
    }

    #[test]
    fn enable_writes_only_our_unit_and_the_stub_enables_it() {
        let home = temp_home("enable");
        make_launcher(&home, ".local/bin/super-desktop");
        let stub = stub_systemctl(&home, "ok", "exit 0");
        enable_at(&home, &stub, "/no/super-desktop").expect("enable must succeed");
        let calls = fs::read_to_string(home.join("calls.log")).expect("stub must have logged");
        assert_eq!(calls.trim(), "--user enable super-desktop-boot.service");
        let unit = fs::read_to_string(unit_file(&home)).expect("unit must exist");
        assert!(unit.contains("ExecStart="), "{unit}");
        assert!(unit.contains(" daemon"), "{unit}");
        assert!(unit.contains("WantedBy=default.target"), "{unit}");
        assert!(unit.contains(USER_LAUNCHER), "the unit starts the installed launcher");
    }

    #[test]
    fn enable_falls_back_to_the_package_launcher() {
        let home = temp_home("package");
        let package = home.join("pkg/super-desktop");
        make_launcher(&home, "pkg/super-desktop");
        let stub = stub_systemctl(&home, "ok", "exit 0");
        enable_at(&home, &stub, &package.to_string_lossy().to_string()).expect("the package launcher is used");
        let unit = fs::read_to_string(unit_file(&home)).expect("unit must exist");
        assert!(unit.contains("pkg/super-desktop"), "{unit}");
    }

    #[test]
    fn enable_without_an_installed_launcher_changes_nothing() {
        let home = temp_home("absent");
        let stub = stub_systemctl(&home, "ok", "exit 0");
        let failure = enable_at(&home, &stub, "/no/super-desktop");
        assert!(failure.is_err());
        assert!(failure.unwrap_err().contains("installed launcher was not found"), "the reason is kept");
        assert!(!unit_file(&home).is_file(), "no unit is written when there is nothing to start");
        assert!(!home.join("calls.log").is_file(), "the stub never ran");
    }

    #[test]
    fn failed_enable_keeps_the_unit_and_reports_the_tool_said() {
        let home = temp_home("fail");
        make_launcher(&home, ".local/bin/super-desktop");
        let stub = stub_systemctl(&home, "refused", "echo refused 1>&2; exit 1");
        let failure = enable_at(&home, &stub, "/no/super-desktop");
        assert!(failure.is_err(), "a failing enable must be reported");
        assert!(failure.unwrap_err().contains("refused"), "the tool's own words are kept");
        assert!(unit_file(&home).is_file(), "the written unit stays for a retry");
    }

    #[test]
    fn disable_disables_then_removes_the_unit() {
        let home = temp_home("disable");
        fs::create_dir_all(&unit_dir(&home)).expect("unit dir");
        fs::write(&unit_file(&home), "x\n").expect("seed unit");
        let stub = stub_systemctl(&home, "ok", "exit 0");
        disable_at(&home, &stub).expect("disable must succeed");
        assert!(!unit_file(&home).is_file(), "disable must delete the unit");
        disable_at(&home, &stub).expect("already-off disable is quiet");
    }

    #[test]
    fn status_reads_the_stub_and_falls_back_to_the_file() {
        let home = temp_home("status");
        fs::create_dir_all(&unit_dir(&home)).expect("unit dir");
        fs::write(&unit_file(&home), "x\n").expect("seed unit");
        let on = stub_systemctl(&home, "on", "echo enabled");
        assert!(status_at(&home, &on).starts_with("On"), "an enabled unit reads On");
        let missing = home.join("no-systemctl").to_string_lossy().to_string();
        assert!(status_at(&home, &missing).contains("checked by the file"), "no systemctl falls back to the file");
        let _ = fs::remove_file(&unit_file(&home));
        assert!(status_at(&home, &missing).starts_with("Off"), "nothing to start reads Off");
    }

    #[test]
    fn old_state_defaults_off_and_boot_autostart_round_trips() {
        let mut value = serde_json::to_value(crate::state::AppState::default()).unwrap();
        value.as_object_mut().unwrap().remove("boot_autostart");
        let mut state: crate::state::AppState = serde_json::from_value(value).unwrap();
        assert!(!state.boot_autostart);
        state.boot_autostart = true;
        let restored: crate::state::AppState =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(restored.boot_autostart);
    }
}
