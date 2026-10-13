//! Where plugins, their logs and status, and the QML host live.
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `$SD_PLUGINS_DIR`, else `~/.config/super-desktop/plugins`.
pub fn plugins_dir() -> PathBuf {
    match std::env::var_os("SD_PLUGINS_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home().join(".config/super-desktop/plugins"),
    }
}

/// `$SD_PLUGINS_STATE_DIR`, else `~/.local/state/super-desktop/plugins`.
pub fn state_dir() -> PathBuf {
    match std::env::var_os("SD_PLUGINS_STATE_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => std::env::var_os("XDG_STATE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/state"))
            .join("super-desktop/plugins"),
    }
}

pub fn log_path(id: &str) -> PathBuf {
    state_dir().join(format!("{id}.log"))
}

pub fn status_path(id: &str) -> PathBuf {
    state_dir().join(format!("{id}.status.json"))
}

/// The plugin's saved values, shared by its surfaces (`sd.values`).
pub fn data_path(id: &str) -> PathBuf {
    state_dir().join(format!("{id}.data.json"))
}

/// The theme the overlay last applied, for `plugin snapshot`.
pub fn theme_path() -> PathBuf {
    state_dir().join("theme.json")
}

/// A plugin folder from an id or a path given on the command line.
pub fn resolve_plugin_dir(arg: &str) -> PathBuf {
    let path = Path::new(arg);
    if arg.contains('/') || arg == "." || path.join("plugin.json").is_file() {
        return path.to_path_buf();
    }
    plugins_dir().join(arg)
}

/// `$SD_QML_HOST`, else `sd-qml-host` next to this executable, else `PATH`.
pub fn qml_host_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SD_QML_HOST").filter(|p| !p.is_empty()) {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("sd-qml-host");
            if candidate.is_file() {
                return Some(candidate);
            }
            // `cargo test` binaries live one level down, in target/<profile>/deps.
            if let Some(parent) = dir.parent() {
                let candidate = parent.join("sd-qml-host");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("sd-qml-host"))
            .find(|candidate| candidate.is_file())
    })
}

const LOG_LIMIT: u64 = 256 * 1024;

/// Appends one line to the plugin's log, rotating it at 256 KiB.
pub fn append_log(id: &str, line: &str) {
    let path = log_path(id);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_LIMIT) {
        let _ = fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(file, "[{stamp}] {line}");
    }
}

/// Records what the overlay is doing with a plugin, for `plugin list`.
pub fn write_status(id: &str, state: &str, errors: &Value) {
    let path = status_path(id);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = json!({ "state": state, "errors": errors, "updatedAt": stamp });
    let _ = fs::write(path, body.to_string());
}

pub fn read_status(id: &str) -> Option<Value> {
    fs::read_to_string(status_path(id))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}
