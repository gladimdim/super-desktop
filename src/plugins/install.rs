//! Plugins from elsewhere: `plugin install <git url | folder>`, `update`,
//! `approve` and `remove`. An installed plugin keeps a record of where it
//! came from (`<state>/<id>.source.json`) and does not run until the user
//! approves it. Plugins made on this PC (`plugin new`, ✨ Create Plugin, a
//! folder an agent wrote) have no record and run as soon as they are valid.
use super::{manifest, paths};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The plugins folder and the state folder these commands work in.
#[derive(Clone, Debug)]
pub struct Places {
    pub plugins: PathBuf,
    pub state: PathBuf,
}

impl Places {
    /// The user's (or `$SD_PLUGINS_DIR` / `$SD_PLUGINS_STATE_DIR`).
    pub fn current() -> Self {
        Self { plugins: paths::plugins_dir(), state: paths::state_dir() }
    }

    fn source_path(&self, id: &str) -> PathBuf {
        self.state.join(format!("{id}.source.json"))
    }
}

/// Where an installed plugin came from, or `None` for one made here.
pub fn source(places: &Places, id: &str) -> Option<Value> {
    fs::read_to_string(places.source_path(id)).ok().and_then(|text| serde_json::from_str(&text).ok())
}

/// Whether the plugin may run: made here, or installed and approved.
pub fn approved(places: &Places, id: &str) -> bool {
    source(places, id).is_none_or(|record| record["approved"] == true)
}

fn write_source(places: &Places, id: &str, record: &Value) -> Result<(), String> {
    let path = places.source_path(id);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(&path, serde_json::to_string_pretty(record).unwrap_or_default()).map_err(|e| e.to_string())
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Makes the overlay reload a plugin (it watches the plugin's files).
fn nudge(dir: &Path) {
    if let Ok(file) = fs::OpenOptions::new().append(true).open(dir.join(manifest::MANIFEST_FILE)) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target)?;
        }
        // Symbolic links are not copied: a plugin cannot point outside itself.
    }
    Ok(())
}

#[derive(Debug)]
pub struct Installed {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: manifest::Manifest,
    pub commit: Option<String>,
}

/// Installs from a git URL or a local folder, not approved.
pub fn install(places: &Places, source: &str) -> Result<Installed, String> {
    let root = &places.plugins;
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    // A hidden folder: the overlay ignores it while it is being filled.
    let staging = root.join(format!(".install-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let local = Path::new(source);
    let result = (|| {
        let commit = if local.is_dir() {
            copy_dir(local, &staging).map_err(|e| format!("cannot copy {source}: {e}"))?;
            None
        } else {
            let out = Command::new("git")
                .args(["clone", "--depth", "1", "--quiet", source])
                .arg(&staging)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .map_err(|e| format!("cannot run git: {e}"))?;
            if !out.status.success() {
                return Err(format!("git clone failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            git(&staging, &["rev-parse", "HEAD"]).ok()
        };
        // The id comes from the manifest; the folder takes its name.
        let text = fs::read_to_string(staging.join(manifest::MANIFEST_FILE))
            .map_err(|_| format!("{source} has no {} at its top level", manifest::MANIFEST_FILE))?;
        let value: Value = serde_json::from_str(&text).map_err(|e| format!("plugin.json is not valid JSON: {e}"))?;
        let id = value["id"].as_str().unwrap_or("").to_owned();
        if !manifest::valid_plugin_id(&id) {
            return Err(format!("plugin.json has an invalid id {id:?}"));
        }
        let dir = root.join(&id);
        if dir.exists() {
            return Err(format!("a plugin {id:?} is already installed ({}); use `super-desktop plugin update {id}` or remove it first", dir.display()));
        }
        // Recorded before the folder appears, so the overlay never runs it unapproved.
        write_source(places, &id, &json!({
            "source": source, "commit": commit, "installedAt": now(), "approved": false,
        }))?;
        fs::rename(&staging, &dir).map_err(|e| e.to_string())?;
        let manifest = manifest::load(&dir).map_err(|problems| {
            format!("installed, but plugin.json has problems: {}", problems.iter().map(|p| p.message.clone()).collect::<Vec<_>>().join("; "))
        })?;
        Ok(Installed { id, dir, manifest, commit })
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

pub fn approve(places: &Places, id: &str) -> Result<(), String> {
    let mut record = source(places, id).ok_or_else(|| format!("{id:?} was made on this PC; it needs no approval"))?;
    record["approved"] = json!(true);
    record["approvedAt"] = json!(now());
    write_source(places, id, &record)?;
    nudge(&places.plugins.join(id));
    Ok(())
}

pub struct Updated {
    pub from: String,
    pub to: String,
    pub changes: String,
}

/// Fast-forwards an installed plugin from its git source.
pub fn update(places: &Places, id: &str) -> Result<Updated, String> {
    let mut record = source(places, id).ok_or_else(|| format!("{id:?} was not installed from a source; edit it in place"))?;
    let dir = places.plugins.join(id);
    if !dir.join(".git").exists() {
        return Err(format!("{id:?} was installed from a folder, not git; install it again to update"));
    }
    let from = git(&dir, &["rev-parse", "HEAD"])?;
    git(&dir, &["fetch", "--depth", "50", "--quiet", "origin"])?;
    git(&dir, &["merge", "--ff-only", "--quiet", "FETCH_HEAD"])
        .map_err(|e| format!("cannot fast-forward (local changes or a rewritten history?): {e}"))?;
    let to = git(&dir, &["rev-parse", "HEAD"])?;
    let changes = if from == to { String::new() } else { git(&dir, &["diff", "--stat", &from, &to]).unwrap_or_default() };
    record["commit"] = json!(to);
    record["updatedAt"] = json!(now());
    write_source(places, id, &record)?;
    nudge(&dir);
    Ok(Updated { from, to, changes })
}

/// Moves a plugin's folder to the desktop trash (recoverable) and forgets
/// its state.
pub fn remove(places: &Places, id: &str, trash: &Path) -> Result<PathBuf, String> {
    if !manifest::valid_plugin_id(id) {
        return Err(format!("invalid plugin id {id:?}"));
    }
    let dir = places.plugins.join(id);
    if !dir.is_dir() {
        return Err(format!("no plugin {id:?} in {}", places.plugins.display()));
    }
    fs::create_dir_all(trash.join("files")).map_err(|e| e.to_string())?;
    fs::create_dir_all(trash.join("info")).map_err(|e| e.to_string())?;
    let name = (0..)
        .map(|n| if n == 0 { format!("sd-plugin-{id}") } else { format!("sd-plugin-{id}.{n}") })
        .find(|name| !trash.join("files").join(name).exists())
        .unwrap_or_default();
    let info = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        dir.display(),
        chrono_like_now()
    );
    fs::write(trash.join("info").join(format!("{name}.trashinfo")), info).map_err(|e| e.to_string())?;
    let target = trash.join("files").join(&name);
    if fs::rename(&dir, &target).is_err() {
        // Another file system: copy, then delete.
        copy_dir(&dir, &target).map_err(|e| e.to_string())?;
        fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    for suffix in ["source.json", "data.json", "cards.json", "status.json"] {
        let _ = fs::remove_file(places.state.join(format!("{id}.{suffix}")));
    }
    Ok(target)
}

/// The user's desktop trash (freedesktop.org).
pub fn user_trash() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
        .join("Trash")
}

/// `YYYY-MM-DDThh:mm:ss` in UTC, for the trash's info file.
fn chrono_like_now() -> String {
    let secs = now() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places(name: &str) -> (PathBuf, Places) {
        let root = std::env::temp_dir().join(format!("sd-plugins-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let places = Places { plugins: root.join("plugins"), state: root.join("state") };
        (root, places)
    }

    #[test]
    fn plugins_install_waits_for_approval_and_updates_from_git() {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipped: git is not installed");
            return;
        }
        let (root, places) = places("install");
        let upstream = root.join("upstream");
        fs::create_dir_all(&upstream).unwrap();
        let write_plugin = |color: &str| {
            fs::write(upstream.join("plugin.json"), json!({"id": "shared", "api": 1, "surfaces": [
                {"id": "main", "slot": "dock.left", "qml": "Dock.qml"}]}).to_string()).unwrap();
            fs::write(upstream.join("Dock.qml"), format!("import QtQuick\nRectangle {{ color: \"{color}\" }}\n")).unwrap();
        };
        let git_in = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(&upstream)
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        write_plugin("red");
        git_in(&["init", "-q"]);
        git_in(&["add", "."]);
        git_in(&["commit", "-qm", "first"]);

        // A local folder is copied.
        let copied = install(&places, upstream.to_str().unwrap()).unwrap();
        assert!(copied.commit.is_none());
        assert!(!approved(&places, "shared"), "installed plugins wait for approval");
        assert!(install(&places, upstream.to_str().unwrap()).is_err(), "a second install refuses to overwrite");
        let trash = root.join("trash");
        let trashed = remove(&places, "shared", &trash).unwrap();
        assert!(trashed.join("Dock.qml").is_file());
        assert!(trash.join("info/sd-plugin-shared.trashinfo").is_file());
        assert!(source(&places, "shared").is_none(), "removing forgets the record");

        // A git URL is cloned, approved, then updated by fast-forward.
        let url = format!("file://{}", upstream.display());
        let installed = install(&places, &url).unwrap();
        assert_eq!(installed.id, "shared");
        assert!(installed.commit.is_some());
        assert!(!approved(&places, "shared"));
        approve(&places, "shared").unwrap();
        assert!(approved(&places, "shared"));
        write_plugin("blue");
        git_in(&["commit", "-qam", "second"]);
        let updated = update(&places, "shared").unwrap();
        assert_ne!(updated.from, updated.to);
        assert!(updated.changes.contains("Dock.qml"));
        assert!(fs::read_to_string(places.plugins.join("shared/Dock.qml")).unwrap().contains("blue"));
        assert!(approved(&places, "shared"), "an update keeps the approval");

        // Made here: no record, runs without approval.
        assert!(approved(&places, "made-here"));
        assert!(approve(&places, "made-here").is_err());
        assert!(update(&places, "made-here").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn plugins_install_refuses_folders_without_a_manifest() {
        let (root, places) = places("install-bad");
        let empty = root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        let error = install(&places, empty.to_str().unwrap()).unwrap_err();
        assert!(error.contains("plugin.json"));
        assert!(manifest::discover(&places.plugins).is_empty(), "nothing is left behind");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn plugins_trash_dates_are_iso() {
        let date = chrono_like_now();
        assert_eq!(date.len(), 19);
        assert_eq!(&date[4..5], "-");
        assert_eq!(&date[10..11], "T");
    }
}
