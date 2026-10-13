//! `plugin.json`: what a plugin folder adds, and the checks that tell an
//! agent exactly what to fix.
use serde::Serialize;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

pub const API_VERSION: u64 = 1;
pub const MANIFEST_FILE: &str = "plugin.json";
pub const MAX_SURFACES: usize = 8;
pub const DOCK_SIZE_DEFAULT: i32 = 64;
pub const MAX_TITLE: usize = 60;

/// Where a surface is placed in the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Slot {
    #[serde(rename = "dock.left")]
    DockLeft,
    #[serde(rename = "dock.right")]
    DockRight,
    #[serde(rename = "toolbar.item")]
    ToolbarItem,
    #[serde(rename = "panel")]
    Panel,
    #[serde(rename = "settings.page")]
    SettingsPage,
    #[serde(rename = "card")]
    Card,
    #[serde(rename = "fullscreen")]
    Fullscreen,
}

/// What `size` and `height` mean for a slot, and their limits.
pub struct SlotSizes {
    /// `(min, default, max)` of `size`, or `None` when the slot ignores it.
    pub size: Option<(i32, i32, i32)>,
    pub height: Option<(i32, i32, i32)>,
}

impl Slot {
    pub const SUPPORTED: &'static [&'static str] = &["dock.left", "dock.right", "toolbar.item", "panel", "settings.page", "card", "fullscreen"];

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dock.left" => Some(Self::DockLeft),
            "dock.right" => Some(Self::DockRight),
            "toolbar.item" => Some(Self::ToolbarItem),
            "panel" => Some(Self::Panel),
            "settings.page" => Some(Self::SettingsPage),
            "card" => Some(Self::Card),
            "fullscreen" => Some(Self::Fullscreen),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::DockLeft => "dock.left",
            Self::DockRight => "dock.right",
            Self::ToolbarItem => "toolbar.item",
            Self::Panel => "panel",
            Self::SettingsPage => "settings.page",
            Self::Card => "card",
            Self::Fullscreen => "fullscreen",
        }
    }

    pub fn sizes(self) -> SlotSizes {
        match self {
            Self::DockLeft | Self::DockRight => SlotSizes { size: Some((32, DOCK_SIZE_DEFAULT, 480)), height: None },
            Self::ToolbarItem => SlotSizes { size: Some((24, 96, 240)), height: None },
            Self::Panel => SlotSizes { size: Some((200, 480, 1600)), height: Some((120, 360, 1200)) },
            Self::SettingsPage => SlotSizes { size: None, height: Some((60, 240, 1200)) },
            Self::Card => SlotSizes { size: Some((200, 640, 4000)), height: Some((120, 420, 3000)) },
            Self::Fullscreen => SlotSizes { size: None, height: None },
        }
    }

    /// The size `plugin snapshot` renders when none is given.
    pub fn default_snapshot_size(self, size: i32, height: i32) -> (i32, i32) {
        match self {
            Self::DockLeft | Self::DockRight => (size, 720),
            Self::ToolbarItem => (size, 36),
            Self::Panel => (size, height),
            Self::SettingsPage => (560, height),
            Self::Card => (size, height),
            Self::Fullscreen => (1280, 720),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Surface {
    pub id: String,
    pub slot: Slot,
    /// Relative to the plugin folder.
    pub qml: String,
    /// Width in logical px (docks, toolbar items, panels); 0 when unused.
    pub size: i32,
    /// Height in logical px (panels, cards, settings pages); 0 when unused.
    pub height: i32,
    /// Panel header and settings section title; empty means the plugin name.
    pub title: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub surfaces: Vec<Surface>,
    pub shortcuts: Vec<Shortcut>,
    #[serde(skip)]
    pub dir: PathBuf,
}

/// A key combination that, while the overlay is shown, opens, closes or
/// toggles one of the plugin's panels, cards or full-screen surfaces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Shortcut {
    /// As written, e.g. "Ctrl+Alt+T".
    pub keys: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
    /// The key's name, lowercase for letters ("t", "f5", "space").
    pub key: String,
    /// "panel.toggle", "card.open", "fullscreen.toggle"…
    pub action: String,
    pub surface: String,
}

pub const MAX_SHORTCUTS: usize = 8;
const SHORTCUT_ACTIONS: &[&str] = &[
    "panel.open", "panel.close", "panel.toggle", "card.open", "card.close", "card.toggle",
    "fullscreen.open", "fullscreen.close", "fullscreen.toggle",
];

/// Parses "Ctrl+Alt+T". A shortcut needs Super, or two of Ctrl/Alt/Shift
/// (one of them Ctrl or Alt), so it never takes keys people type.
pub fn parse_keys(text: &str) -> Result<(bool, bool, bool, bool, String), String> {
    let parts: Vec<&str> = text.split('+').map(str::trim).collect();
    let Some((key, modifiers)) = parts.split_last() else { return Err("empty".into()) };
    let (mut ctrl, mut alt, mut shift, mut super_key) = (false, false, false, false);
    for m in modifiers {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "super" | "meta" | "win" => super_key = true,
            other => return Err(format!("unknown modifier {other:?}")),
        }
    }
    let mut key = key.to_ascii_lowercase();
    if key == "enter" {
        key = "return".into();
    }
    let named = ["space", "return", "enter", "tab", "backspace", "delete", "insert", "home", "end",
        "page_up", "page_down", "up", "down", "left", "right"];
    let valid_key = (key.chars().count() == 1 && key.chars().all(|c| c.is_ascii_alphanumeric() || ",.;'[]/\\-=`".contains(c)))
        || named.contains(&key.as_str())
        || key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=12).contains(&n));
    if !valid_key {
        return Err(format!("unknown key {key:?}"));
    }
    let strong = super_key || (ctrl as u8 + alt as u8 + shift as u8 >= 2 && (ctrl || alt));
    if !strong {
        return Err("needs Super, or two of Ctrl, Alt and Shift".into());
    }
    Ok((ctrl, alt, shift, super_key, key))
}

impl Manifest {
    pub fn qml_path(&self, surface: &Surface) -> PathBuf {
        self.dir.join(&surface.qml)
    }

    pub fn surface(&self, id: &str) -> Option<&Surface> {
        self.surfaces.iter().find(|s| s.id == id)
    }

    /// A surface's title, or the plugin name.
    pub fn title_of<'a>(&'a self, surface: &'a Surface) -> &'a str {
        if surface.title.is_empty() { &self.name } else { &surface.title }
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
                let mut number = |name: &str, range: Option<(i32, i32, i32)>| -> i32 {
                    let Some((min, default, max)) = range else { return 0 };
                    match entry.get(name) {
                        None => default,
                        Some(v) => match v.as_i64() {
                            Some(n) if (i64::from(min)..=i64::from(max)).contains(&n) => n as i32,
                            _ => {
                                problems.push(problem(
                                    field(name),
                                    format!("{name} {v} is out of range for {}", slot.map_or("this slot", Slot::name)),
                                    format!("Use a whole number of logical pixels from {min} to {max}."),
                                ));
                                default
                            }
                        },
                    }
                };
                let sizes = slot.map(Slot::sizes);
                let size = number("size", sizes.as_ref().and_then(|s| s.size));
                let height = number("height", sizes.as_ref().and_then(|s| s.height));
                let title = match entry.get("title") {
                    None => String::new(),
                    Some(Value::String(t)) if t.chars().count() <= MAX_TITLE => t.trim().to_owned(),
                    Some(_) => {
                        problems.push(problem(field("title"), "title must be text of at most 60 characters", "Shorten it."));
                        String::new()
                    }
                };
                if let Some(slot) = slot {
                    surfaces.push(Surface { id: surface_id, slot, qml, size, height, title });
                }
            }
        }
    }

    let mut shortcuts = Vec::new();
    match object.get("shortcuts") {
        None => {}
        Some(Value::Array(list)) if list.len() <= MAX_SHORTCUTS => {
            for (index, entry) in list.iter().enumerate() {
                let field = |name: &str| format!("shortcuts[{index}].{name}");
                let keys = entry.get("keys").and_then(Value::as_str).unwrap_or("").to_owned();
                let action = entry.get("action").and_then(Value::as_str).unwrap_or("").to_owned();
                let surface = entry.get("surface").and_then(Value::as_str).unwrap_or("").to_owned();
                let parsed = parse_keys(&keys);
                if let Err(e) = &parsed {
                    problems.push(problem(field("keys"), format!("{keys:?}: {e}"), "Write it like \"Ctrl+Alt+T\" or \"Super+F5\": Super, or two of Ctrl/Alt/Shift, then one key."));
                }
                if !SHORTCUT_ACTIONS.contains(&action.as_str()) {
                    problems.push(problem(field("action"), format!("unknown action {action:?}"), format!("Use one of: {}.", SHORTCUT_ACTIONS.join(", "))));
                } else {
                    let kind = action.split('.').next().unwrap_or("");
                    let wanted = Slot::parse(kind).unwrap_or(Slot::Panel);
                    if !surfaces.iter().any(|s: &Surface| s.id == surface && s.slot == wanted) {
                        problems.push(problem(field("surface"), format!("no {kind} surface {surface:?}"), format!("Name one of this plugin's \"{kind}\" surfaces.")));
                    }
                }
                if let Ok((ctrl, alt, shift, super_key, key)) = parsed {
                    shortcuts.push(Shortcut { keys, ctrl, alt, shift, super_key, key, action, surface });
                }
            }
        }
        Some(_) => problems.push(problem("shortcuts", format!("\"shortcuts\" must be a list of at most {MAX_SHORTCUTS}"), "Use [{\"keys\": \"Ctrl+Alt+T\", \"action\": \"card.toggle\", \"surface\": \"main\"}].")),
    }

    if problems.is_empty() {
        Ok(Manifest { id, name, description, enabled, surfaces, shortcuts, dir: dir.to_path_buf() })
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
        assert_eq!(m.surfaces[0].height, 0);
    }

    #[test]
    fn plugins_manifest_sizes_follow_the_slot() {
        let dir = plugin_dir("slots", &["A.qml"]);
        let m = validate(&dir, &json!({"id": "slots", "api": 1, "name": "Slots", "surfaces": [
            {"id": "bar", "slot": "toolbar.item", "qml": "A.qml"},
            {"id": "win", "slot": "panel", "qml": "A.qml", "size": 640, "height": 400, "title": "Stats"},
            {"id": "prefs", "slot": "settings.page", "qml": "A.qml", "size": 99999}
        ]})).unwrap();
        assert_eq!((m.surfaces[0].size, m.surfaces[0].height), (96, 0));
        assert_eq!((m.surfaces[1].size, m.surfaces[1].height), (640, 400));
        assert_eq!(m.title_of(&m.surfaces[1]), "Stats");
        // A settings page ignores size; its height defaults.
        assert_eq!((m.surfaces[2].size, m.surfaces[2].height), (0, 240));
        assert_eq!(m.title_of(&m.surfaces[2]), "Slots");
        let problems = validate(&dir, &json!({"id": "slots", "api": 1, "surfaces": [
            {"id": "win", "slot": "panel", "qml": "A.qml", "height": 50}
        ]})).unwrap_err();
        assert_eq!(problems[0].field, "surfaces[0].height");
        assert!(problems[0].hint.contains("120"));
    }

    #[test]
    fn plugins_manifest_reports_every_problem_with_a_hint() {
        let dir = plugin_dir("bad", &[]);
        let problems = validate(&dir, &json!({"id": "Other", "api": 2, "surfaces": [
            {"id": "x", "slot": "sidebar", "qml": "../evil.qml", "size": 9000},
            {"id": "x", "slot": "dock.left", "qml": "Missing.qml", "size": 9000, "title": 5}
        ]})).unwrap_err();
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert!(fields.contains(&"id"));
        assert!(fields.contains(&"api"));
        assert!(fields.contains(&"surfaces[0].slot"));
        assert!(fields.contains(&"surfaces[0].qml"));
        assert!(!fields.contains(&"surfaces[0].size")); // unknown slot: nothing to check it against
        assert!(fields.contains(&"surfaces[1].id"));
        assert!(fields.contains(&"surfaces[1].qml"));
        assert!(fields.contains(&"surfaces[1].size"));
        assert!(fields.contains(&"surfaces[1].title"));
        assert!(problems.iter().all(|p| !p.hint.is_empty()));
        let slot = problems.iter().find(|p| p.field == "surfaces[0].slot").unwrap();
        assert!(slot.hint.contains("dock.left"));
    }

    #[test]
    fn plugins_manifest_shortcuts_need_strong_keys_and_a_target() {
        assert_eq!(parse_keys("Ctrl+Alt+T").unwrap(), (true, true, false, false, "t".into()));
        assert!(parse_keys("Super+F5").is_ok());
        assert!(parse_keys("Ctrl+Shift+space").is_ok());
        assert!(parse_keys("Ctrl+T").is_err(), "one modifier would take keys people type");
        assert!(parse_keys("Shift+Alt+x").is_ok());
        assert!(parse_keys("Alt+Hyper+x").is_err());
        assert!(parse_keys("Ctrl+Alt+F13").is_err());
        let dir = plugin_dir("keys", &["A.qml"]);
        let m = validate(&dir, &json!({"id": "keys", "api": 1,
            "surfaces": [{"id": "term", "slot": "card", "qml": "A.qml"}],
            "shortcuts": [{"keys": "Ctrl+Alt+T", "action": "card.toggle", "surface": "term"}]})).unwrap();
        assert_eq!(m.shortcuts[0].key, "t");
        let problems = validate(&dir, &json!({"id": "keys", "api": 1,
            "surfaces": [{"id": "term", "slot": "card", "qml": "A.qml"}],
            "shortcuts": [{"keys": "T", "action": "panel.toggle", "surface": "term"}, {"keys": "Ctrl+Alt+X", "action": "run", "surface": "term"}]})).unwrap_err();
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert_eq!(fields, ["shortcuts[0].keys", "shortcuts[0].surface", "shortcuts[1].action"]);
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
