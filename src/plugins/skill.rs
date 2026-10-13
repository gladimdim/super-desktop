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
3. `super-desktop plugin snapshot {id} --out /tmp/{id}.png --json`, then open
   the PNG and look at it;
4. `super-desktop plugin logs {id}` for runtime errors.

Keep the plugin id `{id}` (it must equal the folder name).
";

fn dock_qml(name: &str) -> String {
    format!(
        r#"import QtQuick
import QtQuick.Layouts

// {name}: a dock at the edge of SUPER DESKTOP. Replace this with your own.
Rectangle {{
    id: root
    color: Qt.alpha(sd.theme.background, 0.85)
    radius: 14
    border.color: Qt.alpha(sd.theme.accent, 0.5)
    border.width: 1

    ColumnLayout {{
        anchors.fill: parent
        anchors.margins: 8
        spacing: 8

        Rectangle {{
            Layout.fillWidth: true
            Layout.preferredHeight: width
            radius: 12
            color: area.containsMouse ? sd.theme.accent : Qt.alpha(sd.theme.surface, 0.9)
            Text {{
                anchors.centerIn: parent
                text: "✨"
                font.pixelSize: parent.width * 0.4
            }}
            MouseArea {{
                id: area
                anchors.fill: parent
                hoverEnabled: true
                onClicked: sd.call("notify", {{ title: "{name}", body: "Hello from your new plugin" }})
            }}
        }}

        Item {{ Layout.fillHeight: true }}
    }}
}}
"#
    )
}

/// Creates `<root>/<id>` with a working dock plugin, AGENTS.md and the skill.
pub fn new_plugin(root: &Path, id: &str, name: &str, slot: Slot) -> Result<PathBuf, String> {
    if !manifest::valid_plugin_id(id) {
        return Err(format!(
            "invalid plugin id {id:?}: use 1-48 lowercase letters, digits and '-'"
        ));
    }
    let dir = root.join(id);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    let name = if name.trim().is_empty() { id } else { name.trim() };
    let write = || -> io::Result<()> {
        fs::create_dir_all(&dir)?;
        let manifest = serde_json::json!({
            "id": id,
            "name": name,
            "description": "",
            "api": manifest::API_VERSION,
            "surfaces": [{"id": "main", "slot": slot.name(), "qml": "Dock.qml", "size": manifest::DOCK_SIZE_DEFAULT}],
        });
        fs::write(dir.join(manifest::MANIFEST_FILE), serde_json::to_string_pretty(&manifest)? + "\n")?;
        fs::write(dir.join("Dock.qml"), dock_qml(name))?;
        let agents = AGENTS_MD.replace("{name}", name).replace("{id}", id);
        fs::write(dir.join("AGENTS.md"), agents)?;
        fs::write(dir.join("CLAUDE.md"), "@AGENTS.md\n")?;
        install(&dir)
    };
    write().map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_new_plugin_is_valid_and_has_the_skill() {
        let root = std::env::temp_dir().join(format!("sd-plugins-new-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = new_plugin(&root, "my-dock", "My dock", Slot::DockLeft).unwrap();
        let manifest = manifest::load(&dir).unwrap();
        assert_eq!(manifest.surfaces[0].slot, Slot::DockLeft);
        assert!(dir.join(SKILL_DIR).join("SKILL.md").is_file());
        assert!(dir.join(SKILL_DIR).join("examples/launcher-dock/Dock.qml").is_file());
        assert!(fs::read_to_string(dir.join("AGENTS.md")).unwrap().contains("plugin check my-dock"));
        assert!(new_plugin(&root, "my-dock", "", Slot::DockLeft).is_err());
        assert!(new_plugin(&root, "Bad Id", "", Slot::DockLeft).is_err());
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
        for example in ["clock-dock", "launcher-dock"] {
            manifest::load(&root.join(example)).unwrap_or_else(|p| panic!("{example}: {p:?}"));
        }
        let _ = fs::remove_dir_all(&root);
    }
}
