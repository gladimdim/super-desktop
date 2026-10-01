//! Plugins: the host side of plugin API 1.
//!
//! The contract is in `skills/super-desktop-plugin/` (schemas and references);
//! this module follows it. Plugins describe contributions in their manifest
//! and talk JSON-RPC over stdio; the host draws everything, so turning a
//! plugin off is removing its entries and stopping its process.
//!
//! GTK-free parts (manifest, store, process, api) are usable from the CLI and
//! tests; the GTK parts live in `plugin_ui`.
pub mod api;
pub mod llm;
pub mod manifest;
pub mod process;
pub mod rpc;
pub mod store;
pub mod ui_model;
pub mod version_range;

use std::path::PathBuf;

pub const API_VERSION: u32 = 1;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `~/.config/super-desktop/plugins.json`: what is installed and active.
pub fn config_file() -> PathBuf {
    home().join(".config/super-desktop/plugins.json")
}

/// `~/.config/super-desktop/plugins/<id>/settings.json` lives here.
pub fn settings_dir(id: &str) -> PathBuf {
    home().join(".config/super-desktop/plugins").join(id)
}

/// Installed (copied) plugin code: `~/.local/share/super-desktop/plugins/<id>/`.
pub fn code_dir(id: &str) -> PathBuf {
    home().join(".local/share/super-desktop/plugins").join(id)
}

/// A plugin's writable data directory, storage and log.
pub fn data_dir(id: &str) -> PathBuf {
    home().join(".local/state/super-desktop/plugins").join(id)
}

pub fn log_file(id: &str) -> PathBuf {
    data_dir(id).join("plugin.log")
}
