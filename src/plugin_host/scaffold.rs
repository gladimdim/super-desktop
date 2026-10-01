//! `super-desktop plugin new`: a starter plugin that validates and passes its
//! own test scenario as written, with the skills and agent instructions in
//! it, so the next coding agent follows the standard from the first edit.
use std::path::{Path, PathBuf};

const SDK: &str = include_str!("../../skills/super-desktop-plugin/sdk/python/sd_plugin.py");
const RENDERER_LIB: &str = include_str!("../../skills/super-desktop-plugin/examples/center-magnify/src/lib.rs");
const RENDERER_BUILD: &str = include_str!("../../skills/super-desktop-plugin/examples/center-magnify/build.sh");
const SKILL_URL: &str = "https://github.com/gladimdim/super-desktop/tree/master/skills/super-desktop-plugin";

fn title(id: &str) -> String {
    id.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn create(id: &str, kind: &str, dir: &Path) -> Result<(), String> {
    if !super::manifest::validate_text(&manifest(id, "process"), None).report.ok {
        return Err(format!("`{id}` is not a valid plugin id: 3–40 lowercase letters, digits and hyphens, starting with a letter"));
    }
    if !matches!(kind, "process" | "renderer") {
        return Err(format!("--kind is process or renderer, not `{kind}`"));
    }
    if dir.exists() && std::fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(true) {
        return Err(format!("{} exists and is not empty", dir.display()));
    }
    let mut files: Vec<(PathBuf, String, bool)> = vec![
        (dir.join(super::manifest::FILE_NAME), manifest(id, kind), false),
        (dir.join("README.md"), readme(id, kind), false),
        (dir.join("AGENTS.md"), agents(id, kind), false),
        (dir.join("CLAUDE.md"), "@AGENTS.md\n".into(), false),
        (dir.join(".gitignore"), "__pycache__/\n*.pyc\ntarget/\n".into(), false),
    ];
    match kind {
        "process" => {
            files.push((dir.join("main.py"), main_py(id), false));
            files.push((dir.join("sd_plugin.py"), SDK.into(), false));
            files.push((dir.join("tests/smoke.json"), scenario(id), false));
        }
        _ => {
            files.push((dir.join("Cargo.toml"), cargo_toml(id), false));
            files.push((dir.join("src/lib.rs"), RENDERER_LIB.replace("Center Magnify", &title(id)), false));
            files.push((dir.join("build.sh"), RENDERER_BUILD.replace("center_magnify", &id.replace('-', "_")), true));
        }
    }
    for (path, text, executable) in files {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        if executable {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
        }
    }
    let skills = copy_skills(dir);
    println!("Created {} ({kind} plugin) in {}", title(id), dir.display());
    match skills {
        Ok(()) => println!("Skills for coding agents: .agents/skills (also .claude/skills)"),
        Err(why) => println!("Skills not copied ({why}); agents can read them at {SKILL_URL}"),
    }
    println!("Next:");
    match kind {
        "process" => println!(
            "  super-desktop plugin validate {d}\n  super-desktop plugin test {d}\n  super-desktop plugin link {d} && super-desktop plugin activate {id}",
            d = dir.display()
        ),
        _ => println!(
            "  {d}/build.sh   (needs: rustup target add wasm32-unknown-unknown)\n  super-desktop plugin validate {d}",
            d = dir.display()
        ),
    }
    Ok(())
}

/// Both skills into `.agents/skills`, and `.claude/skills` as relative links
/// to them (one copy, two places agents look).
fn copy_skills(dir: &Path) -> Result<(), String> {
    let source = super::manifest::skills_dir().ok_or("this build has no skills folder")?;
    let root = source.parent().ok_or("no skills root")?;
    for name in ["super-desktop-plugin", "super-desktop-plugin-review"] {
        let from = root.join(name);
        if !from.is_dir() {
            continue;
        }
        let to = dir.join(".agents/skills").join(name);
        copy_dir(&from, &to, &["examples"]).map_err(|e| e.to_string())?;
        let link_dir = dir.join(".claude/skills");
        std::fs::create_dir_all(&link_dir).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(Path::new("../../.agents/skills").join(name), link_dir.join(name)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Copy a folder, skipping `skip` names and build output.
fn copy_dir(from: &Path, to: &Path, skip: &[&str]) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if skip.contains(&name_str.as_ref()) || name_str == "target" || name_str == "__pycache__" {
            continue;
        }
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to.join(&name), &[])?;
        } else {
            std::fs::copy(entry.path(), to.join(&name))?;
        }
    }
    Ok(())
}

fn manifest(id: &str, kind: &str) -> String {
    let name = title(id);
    let value = if kind == "renderer" {
        serde_json::json!({
            "$schema": "https://raw.githubusercontent.com/gladimdim/super-desktop/master/skills/super-desktop-plugin/schemas/manifest.schema.json",
            "manifestVersion": 1, "id": id, "name": name, "version": "0.1.0",
            "description": "Decides where and how big cards are drawn.",
            "engines": {"superDesktop": format!(">={}", crate::updates::running()), "pluginApi": "1"},
            "permissions": ["layout.renderer"],
            "contributes": {"renderer": {"id": format!("{id}.renderer"), "wasm": "renderer.wasm"}}
        })
    } else {
        serde_json::json!({
            "$schema": "https://raw.githubusercontent.com/gladimdim/super-desktop/master/skills/super-desktop-plugin/schemas/manifest.schema.json",
            "manifestVersion": 1, "id": id, "name": name, "version": "0.1.0",
            "description": "Describe in one sentence what the user gets.",
            "engines": {"superDesktop": format!(">={}", crate::updates::running()), "pluginApi": "1"},
            "main": {"command": ["python3", "main.py"]},
            "permissions": ["ui.toolbar", "ui.popup"],
            "contributes": {
                "commands": [{"id": format!("{id}.open"), "title": format!("Open {name}")}],
                "toolbar": [{"id": format!("{id}.button"), "icon": "🧩", "label": name, "tooltip": format!("Open {name}"), "command": format!("{id}.open")}],
                "views": [{"id": format!("{id}.panel"), "title": name, "kind": "panel", "width": 480, "height": 320}],
                "settings": [{"key": "greeting", "type": "string", "title": "Greeting", "default": "Hello"}]
            }
        })
    };
    serde_json::to_string_pretty(&value).expect("json") + "\n"
}

fn main_py(id: &str) -> String {
    format!(
        r#""""{name}: a SUPER DESKTOP plugin. See AGENTS.md before changing it."""
from sd_plugin import Plugin

plugin = Plugin()
VIEW = "{id}.panel"
state = {{"clicks": 0, "handle": None}}


def model(greeting):
    # Groups, a spacer before the actions, one primary button: see the skill's
    # references/ui.md ("Look"). Colours come from the user's Omarchy theme.
    return {{"type": "column", "id": "root", "gap": 12, "children": [
        {{"type": "group", "id": "header", "tone": "accent", "children": [
            {{"type": "row", "id": "top", "gap": 10, "children": [
                {{"type": "label", "id": "message", "text": greeting, "style": "title"}},
                {{"type": "spacer", "id": "top-space"}},
                {{"type": "badge", "id": "status", "text": "ready", "tone": "success"}},
            ]}},
        ]}},
        {{"type": "group", "id": "body", "title": "Clicks", "subtitle": "Press the button; the count updates in place.", "children": [
            {{"type": "row", "id": "actions", "gap": 10, "children": [
                {{"type": "label", "id": "count", "text": "Not clicked yet", "style": "muted"}},
                {{"type": "spacer", "id": "actions-space"}},
                {{"type": "button", "id": "again", "label": "Click me", "tone": "primary"}},
            ]}},
        ]}},
    ]}}


@plugin.command("{id}.open")
def open_panel(context):
    greeting = plugin.call("settings.get").get("greeting", "Hello")
    state["handle"] = plugin.call("ui.open", view=VIEW, model=model(greeting))["handle"]


@plugin.view(VIEW)
def on_view(handle, node, event, value):
    if node == "again" and event == "click":
        state["clicks"] += 1
        plugin.call("ui.patch", handle=handle, ops=[
            {{"op": "set", "id": "count", "props": {{"text": f"Clicked {{state['clicks']}} time(s)"}}}},
        ])
        plugin.call("contrib.update", id="{id}.button", badge=str(state["clicks"]))


@plugin.on("view.closed")
def closed(handle):
    if handle == state["handle"]:
        state["handle"] = None


plugin.run()
"#,
        name = title(id)
    )
}

fn scenario(id: &str) -> String {
    let value = serde_json::json!({
        "settings": {"greeting": "Hi from the test"},
        "steps": [
            {"run": format!("{id}.open")},
            {"expect": {"view": format!("{id}.panel"), "node": "message", "props": {"text": "Hi from the test"}}},
            {"click": {"view": format!("{id}.panel"), "node": "again"}},
            {"expect": {"view": format!("{id}.panel"), "node": "count", "props": {"text": "Clicked 1 time(s)"}}},
            {"expect": {"contrib": format!("{id}.button"), "badge": "1"}}
        ]
    });
    serde_json::to_string_pretty(&value).expect("json") + "\n"
}

fn cargo_toml(id: &str) -> String {
    format!(
        "[package]\nname = \"{id}\"\nversion = \"0.1.0\"\nedition = \"2021\"\npublish = false\n\n[lib]\ncrate-type = [\"cdylib\", \"rlib\"]\n\n[dependencies]\n\n[profile.release]\nopt-level = \"s\"\nlto = true\ncodegen-units = 1\npanic = \"abort\"\nstrip = true\n\n# Keep this crate out of any enclosing workspace.\n[workspace]\n"
    )
}

fn readme(id: &str, kind: &str) -> String {
    let what = if kind == "renderer" { "a window renderer" } else { "a toolbar button that opens a panel" };
    format!(
        "# {name}\n\nA [SUPER DESKTOP](https://github.com/gladimdim/super-desktop) plugin: {what}.\n\n## Install\n\n```sh\nsuper-desktop plugin link .\nsuper-desktop plugin activate {id}\n```\n\n## Permissions\n\nList each permission in `super-desktop-plugin.json` here and say why the plugin needs it.\n\n## What it sends where\n\nNothing leaves this computer. (Say so precisely if that changes, for example when the plugin uses the AI provider.)\n",
        name = title(id)
    )
}

fn agents(id: &str, kind: &str) -> String {
    let test = if kind == "renderer" { "`cargo test` and `./build.sh`" } else { "`super-desktop plugin test .`" };
    format!(
        "# Agent instructions\n\nThis is a SUPER DESKTOP plugin (`{id}`). Follow the `super-desktop-plugin`\nskill in `.agents/skills/super-desktop-plugin/SKILL.md` (also at\n{SKILL_URL}); review with `super-desktop-plugin-review` before a release.\n\n- Start with `super-desktop plugin describe --json`: use only what it lists.\n- The manifest is `super-desktop-plugin.json`; run\n  `super-desktop plugin validate . --json` after every change and fix every error.\n- Run {test} before committing.\n- Never print to stdout from plugin code: it is the protocol channel.\n- Ask for the fewest permissions; never edit SUPER DESKTOP's or Hyprland's files.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_new_writes_valid_starters() {
        let base = std::env::temp_dir().join(format!("sd-scaffold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for kind in ["process", "renderer"] {
            let dir = base.join(kind);
            create("my-thing", kind, &dir).unwrap();
            let report = super::super::manifest::load_dir(&dir).report;
            let errors: Vec<_> = report.errors.iter().filter(|e| !(kind == "renderer" && e.path == "contributes.renderer.wasm")).collect();
            assert!(errors.is_empty(), "{kind}: {errors:#?}");
            assert!(dir.join(".agents/skills/super-desktop-plugin/SKILL.md").exists());
            assert!(dir.join(".claude/skills/super-desktop-plugin/SKILL.md").exists(), "the .claude link resolves");
            assert!(!dir.join(".agents/skills/super-desktop-plugin/examples").exists(), "examples are not copied");
            assert!(create("my-thing", kind, &dir).unwrap_err().contains("not empty"));
        }
        assert!(create("Bad_Id", "process", &base.join("bad")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
