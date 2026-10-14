//! Agent plugins: folders of QML that add surfaces (docks, toolbar items,
//! panels, cards, full-screen views, settings sections) to the
//! overlay. This half has no GTK: the manifest, where plugins live, the
//! protocol spoken with `sd-qml-host`, the host process itself, the public
//! plugin list (`registry`) and the `super-desktop plugin` commands. The overlay side is `plugin_ui`.
pub mod cli;
pub mod host;
pub mod install;
pub mod lint;
pub mod manifest;
pub mod paths;
pub mod protocol;
pub mod registry;
pub mod skill;
