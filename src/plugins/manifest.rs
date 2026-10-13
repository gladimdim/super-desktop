//! `plugin.json`: what a plugin folder adds, and the checks that tell an
//! agent exactly what to fix.
use serde::Serialize;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

pub const API_VERSION: u64 = 1;
pub const MANIFEST_FILE: &str = "plugin.json";
pub const MAX_SURFACES: usize = 8;
pub const DOCK_SIZE_DEFAULT: i32 = 64;
pub const DOCK_SIZE_MIN: i32 = 32;
pub const DOCK_SIZE_MAX: i32 = 480;

/// Where a surface is placed in the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Slot {
    #[serde(rename = "dock.left")]
    DockLeft,
    #[serde(rename = "dock.right")]
    DockRight,
}

impl Slot {
    pub const SUPPORTED: &'static [&'static str] = &["dock.left", "dock.right"];

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dock.left" => Some(Self::DockLeft),
            "dock.right" => Some(Self::DockRight),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::DockLeft => "dock.left",
            Self::DockRight => "dock.right",
        }
    }

    /// The size `plugin snapshot` renders when none is given.
    pub fn default_snapshot_size(self, size: i32) -> (i32, i32) {
        match self {
            Self::DockLeft | Self::DockRight => (size, 720),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Surface {
    pub id: String,
    pub slot: Slot,
    /// Relative to the plugin folder.
    pub qml: String,
    pub size: i32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub surfaces: Vec<Surface>,
    #[serde(skip)]
    pub dir: PathBuf,
}

impl Manifest {
    pub fn qml_path(&self, surface: &Surface) -> PathBuf {
        self.dir.join(&surface.qml)
    }
}

/// One thing wrong with a manifest. `hint` says how to fix it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub field: String,
    pub message: String,
    pub hint: String,
}

fn problem(field: impl Into<String>, message: impl Into<String>, hint: impl Into<String>) -> Problem {
    Problem { field: field.into(), message: message.into(), hint: hint.into() }
}

pub fn valid_plugin_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 48
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn valid_surface_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A relative path that stays inside the plugin folder.
fn inside(relative: &str) -> bool {
    let path = Path::new(relative);
    !relative.is_empty()
        && path.is_relative()
        && path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Reads and checks `<dir>/plugin.json`.
pub fn load(dir: &Path) -> Result<Manifest, Vec<Problem>> {
    let file = dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&file).map_err(|e| {
        vec![problem(
            MANIFEST_FILE,
            format!("cannot read {}: {e}", file.display()),
            "Every plugin folder needs a plugin.json; `super-desktop plugin new <id>` writes one.",
        )]
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|e| {
        vec![problem(
            MANIFEST_FILE,
            format!("not valid JSON: {e}"),
            "Fix the JSON syntax (no comments, no trailing commas).",
        )]
    })?;
    validate(dir, &value)
}

/// Checks a parsed manifest. Unknown fields are ignored.
pub fn validate(dir: &Path, value: &Value) -> Result<Manifest, Vec<Problem>> {
    let mut problems = Vec::new();
    let Some(object) = value.as_object() else {
        return Err(vec![problem("", "the manifest must be a JSON object", "Start from `{\"id\": ..., \"api\": 1, \"surfaces\": [...]}`.")]);
    };
    let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");

    let id = object.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
    if !valid_plugin_id(&id) {
        problems.push(problem(
            "id",
            format!("invalid plugin id {id:?}"),
            "Use 1-48 characters: lowercase letters, digits and '-', starting with a letter or digit.",
        ));
    } else if !folder.is_empty() && folder != id {
        problems.push(problem(
            "id",
            format!("id {id:?} does not match the folder name {folder:?}"),
            "Rename the folder or the id so they are the same.",
        ));
    }

    match object.get("api").and_then(Value::as_u64) {
        Some(API_VERSION) => {}
        Some(other) if other > API_VERSION => problems.push(problem(
            "api",
            format!("plugin API {other} needs a newer SUPER DESKTOP (this one has API {API_VERSION})"),
            "Update SUPER DESKTOP (Settings → Updates) or set \"api\": 1.",
        )),
        _ => problems.push(problem("api", "missing or invalid \"api\"", "Set \"api\": 1.")),
    }

    let name = object.get("name").and_then(Value::as_str).unwrap_or(&id).trim().to_owned();
    let description = object.get("description").and_then(Value::as_str).unwrap_or("").to_owned();
    let enabled = match object.get("enabled") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            problems.push(problem("enabled", "\"enabled\" must be true or false", "Remove it or use a boolean."));
            true
        }
    };

    let mut surfaces = Vec::new();
    match object.get("surfaces").and_then(Value::as_array) {
        None => problems.push(problem(
            "surfaces",
            "missing \"surfaces\" array",
            "Add \"surfaces\": [{\"id\": \"main\", \"slot\": \"dock.right\", \"qml\": \"Dock.qml\"}].",
        )),
        Some(list) if list.is_empty() || list.len() > MAX_SURFACES => problems.push(problem(
            "surfaces",
            format!("{} surfaces; a plugin has 1 to {MAX_SURFACES}", list.len()),
            "Declare at least one surface and at most eight.",
        )),
        Some(list) => {
            let mut seen: Vec<String> = Vec::new();
            for (index, entry) in list.iter().enumerate() {
                let field = |name: &str| format!("surfaces[{index}].{name}");
                let surface_id = entry.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
                if !valid_surface_id(&surface_id) {
                    problems.push(problem(field("id"), format!("invalid surface id {surface_id:?}"), "Use 1-32 characters: lowercase letters, digits and '-'."));
                } else if seen.contains(&surface_id) {
                    problems.push(problem(field("id"), format!("duplicate surface id {surface_id:?}"), "Give every surface its own id."));
                }
                seen.push(surface_id.clone());
                let slot_text = entry.get("slot").and_then(Value::as_str).unwrap_or("");
                let slot = Slot::parse(slot_text);
                if slot.is_none() {
                    problems.push(problem(
                        field("slot"),
                        format!("unknown slot {slot_text:?}"),
                        format!("Use one of: {}.", Slot::SUPPORTED.join(", ")),
                    ));
                }
                let qml = entry.get("qml").and_then(Value::as_str).unwrap_or("").to_owned();
                if !inside(&qml) || !qml.ends_with(".qml") {
                    problems.push(problem(field("qml"), format!("invalid qml path {qml:?}"), "Give a .qml file relative to the plugin folder, e.g. \"Dock.qml\"."));
                } else if !dir.join(&qml).is_file() {
                    problems.push(problem(field("qml"), format!("{qml} does not exist in the plugin folder"), "Create the file or fix the path."));
                }
                let size = match entry.get("size") {
                    None => DOCK_SIZE_DEFAULT,
                    Some(v) => match v.as_i64() {
                        Some(n) if (i64::from(DOCK_SIZE_MIN)..=i64::from(DOCK_SIZE_MAX)).contains(&n) => n as i32,
                        _ => {
                            problems.push(problem(
                                field("size"),
                                format!("size {v} is out of range"),
                                format!("Use a whole number of logical pixels from {DOCK_SIZE_MIN} to {DOCK_SIZE_MAX}."),
                            ));
                            DOCK_SIZE_DEFAULT
                        }
                    },
                };
                if let Some(slot) = slot {
                    surfaces.push(Surface { id: surface_id, slot, qml, size });
                }
            }
        }
    }

    if problems.is_empty() {
        Ok(Manifest { id, name, description, enabled, surfaces, dir: dir.to_path_buf() })
    } else {
        Err(problems)
    }
}

/// A plugin folder and what its manifest says.
pub struct Found {
    pub dir: PathBuf,
    pub folder: String,
    pub manifest: Result<Manifest, Vec<Problem>>,
}

/// Every folder under `root`, sorted by name (that is the dock order too).
/// Hidden folders are skipped.
pub fn discover(root: &Path) -> Vec<Found> {
    let mut found: Vec<Found> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let folder = entry.file_name().to_str()?.to_owned();
            (!folder.starts_with('.')).then(|| {
                let dir = entry.path();
                Found { manifest: load(&dir), dir, folder }
            })
        })
        .collect();
    found.sort_by(|a, b| a.folder.cmp(&b.folder));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plugin_dir(name: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("sd-plugins-manifest-{}-{name}", std::process::id()));
        let dir = root.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::write(dir.join(file), "import QtQuick\nItem {}\n").unwrap();
        }
        dir
    }

    #[test]
    fn plugins_manifest_accepts_a_dock() {
        let dir = plugin_dir("clock", &["Dock.qml"]);
        let m = validate(&dir, &json!({"id": "clock", "api": 1, "surfaces": [
            {"id": "main", "slot": "dock.right", "qml": "Dock.qml", "size": 72}
        ], "future": true})).unwrap();
        assert_eq!(m.name, "clock");
        assert!(m.enabled);
        assert_eq!(m.surfaces[0].slot, Slot::DockRight);
        assert_eq!(m.surfaces[0].size, 72);
    }

    #[test]
    fn plugins_manifest_defaults_size() {
        let dir = plugin_dir("left", &["A.qml"]);
        let m = validate(&dir, &json!({"id": "left", "api": 1, "surfaces": [
            {"id": "a", "slot": "dock.left", "qml": "A.qml"}
        ]})).unwrap();
        assert_eq!(m.surfaces[0].size, DOCK_SIZE_DEFAULT);
    }

    #[test]
    fn plugins_manifest_reports_every_problem_with_a_hint() {
        let dir = plugin_dir("bad", &[]);
        let problems = validate(&dir, &json!({"id": "Other", "api": 2, "surfaces": [
            {"id": "x", "slot": "sidebar", "qml": "../evil.qml", "size": 9000},
            {"id": "x", "slot": "dock.left", "qml": "Missing.qml"}
        ]})).unwrap_err();
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert!(fields.contains(&"id"));
        assert!(fields.contains(&"api"));
        assert!(fields.contains(&"surfaces[0].slot"));
        assert!(fields.contains(&"surfaces[0].qml"));
        assert!(fields.contains(&"surfaces[0].size"));
        assert!(fields.contains(&"surfaces[1].id"));
        assert!(fields.contains(&"surfaces[1].qml"));
        assert!(problems.iter().all(|p| !p.hint.is_empty()));
        let slot = problems.iter().find(|p| p.field == "surfaces[0].slot").unwrap();
        assert!(slot.hint.contains("dock.left"));
    }

    #[test]
    fn plugins_manifest_id_must_match_folder() {
        let dir = plugin_dir("folder-a", &["A.qml"]);
        let problems = validate(&dir, &json!({"id": "folder-b", "api": 1, "surfaces": [
            {"id": "a", "slot": "dock.left", "qml": "A.qml"}
        ]})).unwrap_err();
        assert_eq!(problems[0].field, "id");
    }

    #[test]
    fn plugins_manifest_refuses_paths_outside_the_folder() {
        assert!(!inside("/etc/passwd"));
        assert!(!inside("../x.qml"));
        assert!(!inside("a/../../x.qml"));
        assert!(inside("ui/Dock.qml"));
        assert!(inside("./Dock.qml"));
    }

    #[test]
    fn plugins_discover_sorts_and_skips_hidden() {
        let root = std::env::temp_dir().join(format!("sd-plugins-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for name in ["zeta", "alpha", ".git"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        let found = discover(&root);
        let names: Vec<&str> = found.iter().map(|f| f.folder.as_str()).collect();
        assert_eq!(names, ["alpha", "zeta"]);
        assert!(found[0].manifest.is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
