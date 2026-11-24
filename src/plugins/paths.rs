//! Where plugins, their logs and status, and the QML host live.
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// Ensures `dir` exists as an owner-only (`0700`) non-symlink directory.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    if let Ok(meta) = fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "unsafe directory path"));
        }
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "directory owner mismatch"));
        }
        if meta.mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        if !parent.as_os_str().is_empty() && parent != Path::new("/") && !parent.exists() {
            ensure_private_dir(parent)?;
        }
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ensure_private_dir(dir),
        Err(e) => Err(e),
    }
}

/// Creates a fresh owner-only (`0700`) temporary directory with 128-bit randomness
/// under a verified private base directory (never directly using predictable `/tmp` names).
pub fn create_private_temp_dir(prefix: &str) -> std::io::Result<PathBuf> {
    let base = state_dir().join("tmp");
    ensure_private_dir(&base)?;
    for _ in 0..8 {
        let nonce = crate::control::random_hex(16)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let candidate = base.join(format!("{prefix}-{nonce}"));
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "could not allocate private temp dir"))
}

/// `$SD_PLUGINS_DIR`, else `~/.config/super-desktop/plugins`.
pub fn plugins_dir() -> PathBuf {
    match std::env::var_os("SD_PLUGINS_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home()
            .unwrap_or_else(|| PathBuf::from("/nonexistent-super-desktop-home"))
            .join(".config/super-desktop/plugins"),
    }
}

/// `$SD_PLUGINS_STATE_DIR`, else `~/.local/state/super-desktop/plugins`.
pub fn state_dir() -> PathBuf {
    match std::env::var_os("SD_PLUGINS_STATE_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => std::env::var_os("XDG_STATE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                home()
                    .unwrap_or_else(|| PathBuf::from("/nonexistent-super-desktop-home"))
                    .join(".local/state")
            })
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
    if !super::manifest::valid_plugin_id(id) {
        return;
    }
    let path = log_path(id);
    if let Some(dir) = path.parent() {
        if ensure_private_dir(dir).is_err() {
            return;
        }
    }
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink() || meta.len() > LOG_LIMIT) {
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return;
        }
        let _ = fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(file, "[{stamp}] {line}");
    }
}

/// Records what the overlay is doing with a plugin, for `plugin list`.
pub fn write_status(id: &str, state: &str, errors: &Value) {
    if !super::manifest::valid_plugin_id(id) {
        return;
    }
    let path = status_path(id);
    if let Some(dir) = path.parent() {
        if ensure_private_dir(dir).is_err() {
            return;
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = json!({ "state": state, "errors": errors, "updatedAt": stamp });
    if let Ok(nonce) = crate::control::random_hex(8) {
        let tmp = path.with_extension(format!("status.{nonce}.tmp"));
        if let Ok(mut file) = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
        {
            if file.write_all(body.to_string().as_bytes()).is_ok() {
                let _ = fs::rename(&tmp, &path);
                return;
            }
            let _ = fs::remove_file(&tmp);
        }
    }
}

pub fn read_status(id: &str) -> Option<Value> {
    fs::read_to_string(status_path(id))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}
