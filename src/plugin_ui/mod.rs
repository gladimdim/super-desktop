//! The overlay side of agent plugins: QML surfaces drawn by `sd-qml-host`
//! (`surface`), the docks they live in (`docks`) and the calls they can make
//! (`api`). The display-free half is `super_desktop::plugins`.
pub mod api;
pub mod docks;
pub mod surface;

use serde_json::{json, Value};

/// The theme values a plugin sees as `sd.theme`.
pub fn theme_json(theme: &crate::theme::OmarchyTheme) -> Value {
    json!({
        "name": theme.name,
        "mode": theme.mode,
        "accent": theme.accent,
        "background": theme.background,
        "darkBackground": theme.dark_background,
        "surface": theme.lighter_background,
        "foreground": theme.foreground,
        "muted": theme.muted,
        "selection": theme.selection,
        "red": theme.red,
        "green": theme.green,
        "yellow": theme.yellow,
        "blue": theme.blue,
        "magenta": theme.magenta,
        "cyan": theme.cyan,
        "orange": theme.orange,
        "fontFamily": theme.font_family,
        "fontSize": theme.font_size,
    })
}

/// Called whenever the overlay applies a theme.
pub fn apply_theme(theme: &crate::theme::OmarchyTheme) {
    surface::set_theme(theme_json(theme));
}
