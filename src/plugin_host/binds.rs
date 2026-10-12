//! Which plugin shortcuts should be live, from the active plugins and the
//! user's overrides. Shared by the daemon and the CLI (which rewrites
//! `bindings.lua` itself when the daemon is not running).
use super::manifest::Manifest;
use super::store::Store;
use crate::shortcut::{PluginBind, PLUGIN_DESCRIPTION};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Default)]
pub struct Desired {
    /// Global shortcuts, written to `bindings.lua`.
    pub global: Vec<PluginBind>,
    /// Overlay shortcuts: normalized combo → (plugin, command).
    pub overlay: BTreeMap<String, (String, String)>,
    /// Shortcut id → why it is not active.
    pub problems: BTreeMap<String, String>,
}

pub fn normalize(combo: &str) -> String {
    combo.split('+').map(|part| part.trim().to_ascii_uppercase()).collect::<Vec<_>>().join(" + ")
}

/// `plugins` in a stable order (the first to claim a combination keeps it).
pub fn desired(plugins: &[(String, Arc<Manifest>)], store: &Store) -> Desired {
    let mut out = Desired::default();
    let mut taken: BTreeMap<String, String> = BTreeMap::new();
    for (id, manifest) in plugins {
        let overrides = store.get(id).map(|p| p.shortcuts.clone()).unwrap_or_default();
        for shortcut in &manifest.contributes.shortcuts {
            let combo = match overrides.get(&shortcut.id) {
                Some(None) => continue,
                Some(Some(combo)) => combo.clone(),
                None => shortcut.default.clone(),
            };
            if !super::manifest::is_combo(&combo) {
                out.problems.insert(shortcut.id.clone(), format!("`{combo}` is not a key combination"));
                continue;
            }
            let key = normalize(&combo);
            if let Some(owner) = taken.get(&key) {
                out.problems.insert(shortcut.id.clone(), format!("{combo} is already used by {owner}"));
                continue;
            }
            taken.insert(key.clone(), manifest.name.clone());
            if shortcut.scope == "overlay" {
                out.overlay.insert(key, (id.clone(), shortcut.command.clone()));
                continue;
            }
            let title = manifest.contributes.commands.iter().find(|c| c.id == shortcut.command).map(|c| c.title.as_str()).unwrap_or(&shortcut.command);
            out.global.push(PluginBind {
                combo,
                description: format!("{PLUGIN_DESCRIPTION}: {} — {title}", manifest.name),
                command: format!("super-desktop plugin run {id} {}", shortcut.command),
            });
        }
    }
    out
}

/// From disk, for the CLI: every active plugin whose manifest still loads.
pub fn desired_from_store(store: &Store) -> Desired {
    let plugins: Vec<(String, Arc<Manifest>)> = store
        .plugins
        .iter()
        .filter(|p| p.active)
        .filter_map(|p| super::cli::installed_manifest(p).ok().map(|m| (p.id.clone(), Arc::new(m))))
        .collect();
    desired(&plugins, store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, shortcuts: serde_json::Value) -> Arc<Manifest> {
        let value = serde_json::json!({
            "manifestVersion": 1, "id": id, "name": id.to_uppercase(), "version": "0.1.0", "description": "d",
            "engines": {"superDesktop": ">=1.0.0", "pluginApi": "1"},
            "main": {"command": ["python3", "main.py"]},
            "permissions": ["shortcuts.global", "shortcuts.overlay"],
            "contributes": {"commands": [{"id": format!("{id}.go"), "title": "Go"}], "shortcuts": shortcuts}
        });
        Arc::new(serde_json::from_value(value).unwrap())
    }

    #[test]
    fn plugin_shortcut_overrides_and_first_claim_wins() {
        let a = manifest("aaa", serde_json::json!([
            {"id": "aaa.g", "command": "aaa.go", "default": "SUPER + SHIFT + G", "scope": "global"},
            {"id": "aaa.o", "command": "aaa.go", "default": "CTRL + K", "scope": "overlay"}
        ]));
        let b = manifest("bbb", serde_json::json!([{"id": "bbb.g", "command": "bbb.go", "default": "SUPER + SHIFT + G", "scope": "global"}]));
        let mut store = Store::default();
        let d = desired(&[("aaa".into(), Arc::clone(&a)), ("bbb".into(), Arc::clone(&b))], &store);
        assert_eq!(d.global.len(), 1);
        assert_eq!(d.global[0].command, "super-desktop plugin run aaa aaa.go");
        assert!(d.problems["bbb.g"].contains("already used by AAA"));
        assert_eq!(d.overlay["CTRL + K"], ("aaa".into(), "aaa.go".into()));
        // The user turns aaa's off: bbb's takes the combination.
        store.plugins.push(crate::plugin_host::store::Installed {
            id: "aaa".into(), dir: "/x".into(), source: crate::plugin_host::store::Source::Linked { path: "/x".into() },
            version: "0.1.0".into(), active: true, granted: vec![], shortcuts: [("aaa.g".to_string(), None)].into(), renderer_off: false, extra: Default::default(),
        });
        let d = desired(&[("aaa".into(), a), ("bbb".into(), b)], &store);
        assert_eq!(d.global.len(), 1);
        assert_eq!(d.global[0].command, "super-desktop plugin run bbb bbb.go");
    }
}
