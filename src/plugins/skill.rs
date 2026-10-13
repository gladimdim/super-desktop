//! The plugin skill, built into the binary so every plugin folder gets the
//! version that matches the running SUPER DESKTOP, and the `plugin new`
//! template.
use super::manifest::{self, Slot};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Files of `skills/super-desktop-plugin/`, by relative path.
pub const SKILL_FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("../../skills/super-desktop-plugin/SKILL.md")),
    ("examples/clock-dock/plugin.json", include_str!("../../skills/super-desktop-plugin/examples/clock-dock/plugin.json")),
    ("examples/clock-dock/Dock.qml", include_str!("../../skills/super-desktop-plugin/examples/clock-dock/Dock.qml")),
    ("examples/launcher-dock/plugin.json", include_str!("../../skills/super-desktop-plugin/examples/launcher-dock/plugin.json")),
    ("examples/launcher-dock/Dock.qml", include_str!("../../skills/super-desktop-plugin/examples/launcher-dock/Dock.qml")),
    ("examples/launcher-dock/DockButton.qml", include_str!("../../skills/super-desktop-plugin/examples/launcher-dock/DockButton.qml")),
    ("examples/pomodoro/plugin.json", include_str!("../../skills/super-desktop-plugin/examples/pomodoro/plugin.json")),
    ("examples/pomodoro/Toolbar.qml", include_str!("../../skills/super-desktop-plugin/examples/pomodoro/Toolbar.qml")),
    ("examples/pomodoro/Panel.qml", include_str!("../../skills/super-desktop-plugin/examples/pomodoro/Panel.qml")),
    ("examples/pomodoro/Settings.qml", include_str!("../../skills/super-desktop-plugin/examples/pomodoro/Settings.qml")),
];

/// Where the skill goes inside a plugin folder. Claude Code reads project
/// skills from here; AGENTS.md points every other agent at it.
pub const SKILL_DIR: &str = ".claude/skills/super-desktop-plugin";
const STAMP: &str = ".sd-skill-version";

fn stamp() -> String {
    format!("{} api{}", env!("CARGO_PKG_VERSION"), manifest::API_VERSION)
}

/// Writes (or refreshes) the skill into `plugin_dir`.
pub fn install(plugin_dir: &Path) -> io::Result<()> {
    let dir = plugin_dir.join(SKILL_DIR);
    if fs::read_to_string(dir.join(STAMP)).is_ok_and(|text| text.trim() == stamp()) {
        return Ok(());
    }
    for (relative, contents) in SKILL_FILES {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
    }
    fs::write(dir.join(STAMP), stamp())
}

const AGENTS_MD: &str = "# SUPER DESKTOP plugin: {name}

This folder is a SUPER DESKTOP plugin (QML). SUPER DESKTOP reloads it on
screen every time a file here is saved.

Before changing anything, read `.claude/skills/super-desktop-plugin/SKILL.md`
and follow its loop:

1. edit the QML or plugin.json;
2. `super-desktop plugin check {id} --json` and fix every error and warning;
3. `super-desktop plugin snapshot {id} --out /tmp/{id}.png --json` (add
   `--surface <id>` for each surface), then open the PNG and look at it;
4. `super-desktop plugin logs {id}` for runtime errors.

Keep the plugin id `{id}` (it must equal the folder name). You may change
the slots, add surfaces and files, and rewrite the starting QML entirely.
";

const DOCK_QML: &str = r#"import QtQuick
import QtQuick.Layouts

// {name}: a dock at the edge of SUPER DESKTOP. Replace this with your own.
Rectangle {
    id: root
    color: Qt.alpha(sd.theme.background, 0.85)
    radius: 14
    border.color: Qt.alpha(sd.theme.accent, 0.5)
    border.width: 1

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 8
        spacing: 8

        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: width
            radius: 12
            color: area.containsMouse ? sd.theme.accent : Qt.alpha(sd.theme.surface, 0.9)
            Text {
                anchors.centerIn: parent
                text: "✨"
                font.pixelSize: parent.width * 0.4
            }
            MouseArea {
                id: area
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: sd.call("notify", { title: "{name}", body: "Hello from your new plugin" })
            }
        }

        Item { Layout.fillHeight: true }
    }
}
"#;

const TOOLBAR_QML: &str = r#"import QtQuick

// {name}: an item in SUPER DESKTOP's top bar. Replace this with your own.
Rectangle {
    id: root
    radius: height / 2
    color: area.containsMouse ? sd.theme.accent : Qt.alpha(sd.theme.surface, 0.9)

    Text {
        anchors.centerIn: parent
        text: "✨ {name}"
        color: area.containsMouse ? sd.theme.background : sd.theme.foreground
        font.family: sd.theme.fontFamily
        font.pixelSize: Math.max(10, root.height * 0.42)
        elide: Text.ElideRight
        width: Math.min(implicitWidth, root.width - 12)
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: {CLICK}
    }
}
"#;

const PANEL_QML: &str = r#"import QtQuick
import QtQuick.Layouts

// {name}: a floating panel; the top-bar item opens and closes it.
Rectangle {
    id: root
    color: sd.theme.background

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 16
        spacing: 12

        Text {
            text: "{name}"
            color: sd.theme.accent
            font.family: sd.theme.fontFamily
            font.pixelSize: 20
            font.bold: true
        }
        Text {
            Layout.fillWidth: true
            text: "Your panel goes here."
            color: sd.theme.foreground
            font.family: sd.theme.fontFamily
            wrapMode: Text.Wrap
        }
        Item { Layout.fillHeight: true }
    }
}
"#;

const SETTINGS_QML: &str = r#"import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// {name}: its section in Settings → Plugins. Values saved with
// sd.setValue are seen by every surface of the plugin as sd.values.
Item {
    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 4
        spacing: 8

        CheckBox {
            text: "Example option"
            checked: sd.values.example === true
            onToggled: sd.setValue("example", checked)
        }
        Text {
            text: "Saved: " + JSON.stringify(sd.values)
            color: sd.theme.muted
            font.pixelSize: 11
        }
    }
}
"#;

/// What `plugin new` and Create Plugin start from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    DockLeft,
    DockRight,
    /// One item in the top bar.
    ToolbarItem,
    /// A top-bar item that opens and closes a floating panel.
    Panel,
    /// A section in Settings → Plugins.
    SettingsPage,
}

impl Template {
    pub const NAMES: &'static [&'static str] = &["dock.left", "dock.right", "toolbar.item", "panel", "settings.page"];

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "dock.left" => Self::DockLeft,
            "dock.right" => Self::DockRight,
            "toolbar.item" => Self::ToolbarItem,
            "panel" => Self::Panel,
            "settings.page" => Self::SettingsPage,
            _ => return None,
        })
    }

    /// `(plugin.json surfaces, files)` for the template.
    fn files(self, name: &str) -> (serde_json::Value, Vec<(&'static str, String)>) {
        let fill = |qml: &str| qml.replace("{name}", name);
        match self {
            Self::DockLeft | Self::DockRight => {
                let slot = if self == Self::DockLeft { Slot::DockLeft } else { Slot::DockRight };
                (
                    serde_json::json!([{"id": "main", "slot": slot.name(), "qml": "Dock.qml", "size": manifest::DOCK_SIZE_DEFAULT}]),
                    vec![("Dock.qml", fill(DOCK_QML))],
                )
            }
            Self::ToolbarItem => (
                serde_json::json!([{"id": "main", "slot": "toolbar.item", "qml": "Toolbar.qml", "size": 120}]),
                vec![("Toolbar.qml", fill(TOOLBAR_QML).replace(
                    "{CLICK}",
                    &format!("sd.call(\"notify\", {{ title: \"{name}\", body: \"Hello from your new plugin\" }})"),
                ))],
            ),
            Self::Panel => (
                serde_json::json!([
                    {"id": "button", "slot": "toolbar.item", "qml": "Toolbar.qml", "size": 120},
                    {"id": "main", "slot": "panel", "qml": "Panel.qml", "size": 480, "height": 360}
                ]),
                vec![
                    ("Toolbar.qml", fill(TOOLBAR_QML).replace("{CLICK}", "sd.call(\"panel.toggle\", { surface: \"main\" })")),
                    ("Panel.qml", fill(PANEL_QML)),
                ],
            ),
            Self::SettingsPage => (
                serde_json::json!([{"id": "settings", "slot": "settings.page", "qml": "Settings.qml", "height": 120}]),
                vec![("Settings.qml", fill(SETTINGS_QML))],
            ),
        }
    }
}

/// Creates `<root>/<id>` with a working plugin, AGENTS.md (with the user's
/// `request`, when given) and the skill.
pub fn new_plugin(root: &Path, id: &str, name: &str, template: Template, request: &str) -> Result<PathBuf, String> {
    if !manifest::valid_plugin_id(id) {
        return Err(format!("invalid plugin id {id:?}: use 1-48 lowercase letters, digits and '-'"));
    }
    let dir = root.join(id);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    let name = if name.trim().is_empty() { id } else { name.trim() };
    // QML string literals: keep the name from breaking out of them.
    let qml_name: String = name.chars().filter(|c| !matches!(c, '"' | '\\' | '\n' | '\r')).collect();
    let (surfaces, files) = template.files(&qml_name);
    let write = || -> io::Result<()> {
        fs::create_dir_all(&dir)?;
        let manifest = serde_json::json!({
            "id": id,
            "name": name,
            "description": request.lines().next().unwrap_or("").chars().take(200).collect::<String>(),
            "api": manifest::API_VERSION,
            "surfaces": surfaces,
        });
        fs::write(dir.join(manifest::MANIFEST_FILE), serde_json::to_string_pretty(&manifest)? + "\n")?;
        for (file, contents) in &files {
            fs::write(dir.join(file), contents)?;
        }
        let mut agents = AGENTS_MD.replace("{name}", name).replace("{id}", id);
        if !request.trim().is_empty() {
            agents.push_str("\n## What the user asked for\n\n");
            agents.push_str(request.trim());
            agents.push_str("\n\nThe starting files are only a placeholder: build this.\n");
        }
        fs::write(dir.join("AGENTS.md"), agents)?;
        fs::write(dir.join("CLAUDE.md"), "@AGENTS.md\n")?;
        install(&dir)
    };
    write().map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// A free plugin id made from `text` (a name or the request's first words).
pub fn unique_id(root: &Path, text: &str) -> String {
    let mut base = String::new();
    for word in text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).take(4) {
        if !base.is_empty() {
            base.push('-');
        }
        base.push_str(&word.to_ascii_lowercase());
    }
    base.truncate(40);
    let base = base.trim_matches('-').to_owned();
    let base = if base.is_empty() { "my-plugin".to_owned() } else { base };
    if !root.join(&base).exists() {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|id| !root.join(id).exists()).expect("a free id")
}

/// The prompt that starts an agent on a new plugin.
pub fn first_prompt(id: &str, request: &str) -> String {
    format!(
        "Build the SUPER DESKTOP plugin in this folder ({id}). Read AGENTS.md and .claude/skills/super-desktop-plugin/SKILL.md first, then follow the check and snapshot loop until it works. The user asked for: {}",
        request.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_new_plugin_is_valid_and_has_the_skill() {
        let root = std::env::temp_dir().join(format!("sd-plugins-new-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = new_plugin(&root, "my-dock", "My dock", Template::DockLeft, "A clock").unwrap();
        let manifest = manifest::load(&dir).unwrap();
        assert_eq!(manifest.surfaces[0].slot, Slot::DockLeft);
        assert!(fs::read_to_string(dir.join("AGENTS.md")).unwrap().contains("## What the user asked for\n\nA clock"));
        assert!(dir.join(SKILL_DIR).join("SKILL.md").is_file());
        assert!(dir.join(SKILL_DIR).join("examples/launcher-dock/Dock.qml").is_file());
        assert!(fs::read_to_string(dir.join("AGENTS.md")).unwrap().contains("plugin check my-dock"));
        assert!(new_plugin(&root, "my-dock", "", Template::DockLeft, "").is_err());
        assert!(new_plugin(&root, "Bad Id", "", Template::DockLeft, "").is_err());
        for (index, name) in Template::NAMES.iter().enumerate() {
            let template = Template::parse(name).unwrap();
            let dir = new_plugin(&root, &format!("t{index}"), "Quote \" name", template, "").unwrap();
            manifest::load(&dir).unwrap_or_else(|p| panic!("{name}: {p:?}"));
        }
        assert_eq!(unique_id(&root, "My Dock!"), "my-dock-2");
        assert_eq!(unique_id(&root, "  show CPU usage per core please "), "show-cpu-usage-per");
        assert_eq!(unique_id(&root, "🎮"), "my-plugin");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn plugins_examples_are_valid() {
        let root = std::env::temp_dir().join(format!("sd-plugins-examples-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (relative, contents) in SKILL_FILES {
            if let Some(rest) = relative.strip_prefix("examples/") {
                let path = root.join(rest);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, contents).unwrap();
            }
        }
        for example in ["clock-dock", "launcher-dock", "pomodoro"] {
            let m = manifest::load(&root.join(example)).unwrap_or_else(|p| panic!("{example}: {p:?}"));
            // Every QML file of an example loads cleanly, when the host is built.
            if let Some(host) = crate::plugins::paths::qml_host_binary() {
                for surface in &m.surfaces {
                    let out = std::process::Command::new(&host).arg("--check").arg(m.qml_path(surface)).output().unwrap();
                    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                    assert_eq!(report["ok"], true, "{example}/{}: {report}", surface.qml);
                    assert_eq!(report["warnings"], serde_json::json!([]), "{example}/{}: {report}", surface.qml);
                }
            }
        }
        // Every example file is in SKILL_FILES (and so reaches plugin folders).
        let on_disk = |dir: &str| std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/super-desktop-plugin/examples").join(dir))
            .unwrap().flatten().map(|e| format!("examples/{dir}/{}", e.file_name().to_string_lossy())).collect::<Vec<_>>();
        for example in ["clock-dock", "launcher-dock", "pomodoro"] {
            for file in on_disk(example) {
                assert!(SKILL_FILES.iter().any(|(path, _)| *path == file), "{file} is missing from SKILL_FILES");
            }
        }
        let _ = fs::remove_dir_all(&root);
    }
}
