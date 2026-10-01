//! What is installed and active (`~/.config/super-desktop/plugins.json`), and
//! each plugin's settings, secrets and storage.
//!
//! Kept out of `state.json` on purpose: `AppState` drops unknown fields on
//! save, and an older build reading `state.json` must not lose anything
//! because plugins exist. Unknown fields here are preserved for the same
//! reason. Writes are atomic (temp file + rename) and serialized with a lock
//! file, because the CLI and the daemon can both write.
use super::manifest::{self, Manifest};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const MAX_STORAGE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Store {
    #[serde(default)]
    pub plugins: Vec<Installed>,
    /// `auto` or a provider id (`claude`, `codex`); see `api::llm`.
    #[serde(default = "auto")]
    pub llm_provider: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn auto() -> String {
    "auto".into()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub id: String,
    /// Where the code is: the linked folder, or the installed copy.
    pub dir: PathBuf,
    pub source: Source,
    pub version: String,
    #[serde(default)]
    pub active: bool,
    /// Permissions the user agreed to; activation needs every manifest
    /// permission to be in here.
    #[serde(default)]
    pub granted: Vec<String>,
    /// The user's own key for a shortcut id, or `null` to turn it off.
    #[serde(default)]
    pub shortcuts: BTreeMap<String, Option<String>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Source {
    /// `plugin link <dir>`: used in place, for development.
    Linked { path: PathBuf },
    /// Installed from a git repository at a pinned commit.
    Git { repo: String, tag: Option<String>, commit: String },
}

impl Store {
    pub fn load() -> Self {
        Self::load_from(&super::config_file())
    }

    pub fn load_from(path: &Path) -> Self {
        fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_else(|| Store {
            llm_provider: auto(),
            ..Store::default()
        })
    }

    pub fn get(&self, id: &str) -> Option<&Installed> {
        self.plugins.iter().find(|p| p.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Installed> {
        self.plugins.iter_mut().find(|p| p.id == id)
    }
}

/// Read-modify-write `plugins.json` under a lock.
pub fn update<T>(change: impl FnOnce(&mut Store) -> T) -> std::io::Result<T> {
    update_at(&super::config_file(), change)
}

pub fn update_at<T>(path: &Path, change: impl FnOnce(&mut Store) -> T) -> std::io::Result<T> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(path.with_extension("lock"))?;
    // SAFETY: flock on a descriptor we own; released when `lock` is dropped.
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut store = Store::load_from(path);
    let result = change(&mut store);
    write_json(path, &serde_json::to_value(&store).map_err(std::io::Error::other)?, 0o600)?;
    Ok(result)
}

/// Atomic write: a temp file in the same directory, then rename.
pub fn write_json(path: &Path, value: &Value, mode: u32) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = fs::OpenOptions::new().create(true).truncate(true).write(true).mode(mode).open(&tmp)?;
        file.write_all(serde_json::to_string_pretty(value).map_err(std::io::Error::other)?.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

fn read_object(path: &Path) -> Map<String, Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn settings_file(id: &str) -> PathBuf {
    super::settings_dir(id).join("settings.json")
}

fn secrets_file(id: &str) -> PathBuf {
    super::settings_dir(id).join("secrets.json")
}

fn storage_file(id: &str) -> PathBuf {
    super::data_dir(id).join("storage.json")
}

/// Settings as the plugin sees them: declared defaults, overridden by saved
/// values that still fit their declaration. Secrets are never included.
pub fn settings(manifest: &Manifest) -> Map<String, Value> {
    let saved = read_object(&settings_file(&manifest.id));
    let mut values = Map::new();
    for setting in &manifest.contributes.settings {
        if setting.kind == "secret" {
            continue;
        }
        let value = saved
            .get(&setting.key)
            .filter(|v| manifest::check_value(setting, v).is_ok())
            .cloned()
            .or_else(|| setting.default.clone())
            .unwrap_or(match setting.kind.as_str() {
                "bool" => Value::Bool(false),
                "paths" => Value::Array(Vec::new()),
                "number" => setting.min.and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::from(0)),
                "enum" => setting.values.as_ref().and_then(|v| v.first()).map(|v| Value::String(v.clone())).unwrap_or(Value::Null),
                _ => Value::String(String::new()),
            });
        values.insert(setting.key.clone(), expand_home(setting, value));
    }
    values
}

/// `~/` in path settings becomes the home directory before plugins see it.
fn expand_home(setting: &manifest::Setting, value: Value) -> Value {
    let home = std::env::var("HOME").unwrap_or_default();
    let expand = |text: &str| text.strip_prefix("~/").map(|rest| format!("{home}/{rest}")).unwrap_or_else(|| text.to_string());
    match (setting.kind.as_str(), value) {
        ("path", Value::String(text)) => Value::String(expand(&text)),
        ("paths", Value::Array(items)) => Value::Array(items.iter().map(|i| Value::String(expand(i.as_str().unwrap_or("")))).collect()),
        (_, value) => value,
    }
}

pub fn secret(manifest: &Manifest, key: &str) -> Option<String> {
    manifest.setting(key).filter(|s| s.kind == "secret")?;
    read_object(&secrets_file(&manifest.id)).get(key).and_then(Value::as_str).map(str::to_string)
}

/// Save one setting after checking it against its declaration.
pub fn set_setting(manifest: &Manifest, key: &str, value: Value) -> Result<(), String> {
    let setting = manifest.setting(key).ok_or_else(|| format!("no setting `{key}` in the manifest"))?;
    manifest::check_value(setting, &value)?;
    let (path, mode) = if setting.kind == "secret" { (secrets_file(&manifest.id), 0o600) } else { (settings_file(&manifest.id), 0o644) };
    let mut values = read_object(&path);
    values.insert(key.to_string(), value);
    write_json(&path, &Value::Object(values), mode).map_err(|e| e.to_string())
}

pub fn storage_get(id: &str, key: &str) -> Value {
    read_object(&storage_file(id)).get(key).cloned().unwrap_or(Value::Null)
}

pub fn storage_set(id: &str, key: &str, value: Value) -> Result<(), String> {
    let path = storage_file(id);
    let mut values = read_object(&path);
    if value.is_null() {
        values.remove(key);
    } else {
        values.insert(key.to_string(), value);
    }
    let value = Value::Object(values);
    if value.to_string().len() > MAX_STORAGE_BYTES {
        return Err("storage is limited to 1 MiB per plugin".into());
    }
    write_json(&path, &value, 0o600).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sd-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plugin_store_round_trip_keeps_unknown_fields() {
        let dir = temp("roundtrip");
        let path = dir.join("plugins.json");
        fs::write(&path, r#"{"plugins":[{"id":"a","dir":"/x","source":{"kind":"linked","path":"/x"},"version":"0.1.0","active":true,"future":1}],"newer":true}"#).unwrap();
        update_at(&path, |store| {
            assert_eq!(store.llm_provider, "auto");
            store.get_mut("a").unwrap().active = false;
        })
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["newer"], true);
        assert_eq!(value["plugins"][0]["future"], 1);
        assert_eq!(value["plugins"][0]["active"], false);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_store_missing_file_is_empty() {
        let store = Store::load_from(Path::new("/nonexistent/plugins.json"));
        assert!(store.plugins.is_empty() && store.llm_provider == "auto");
    }
}
