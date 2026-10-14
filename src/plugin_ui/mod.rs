//! The overlay side of agent plugins: QML surfaces drawn by `sd-qml-host`
//! (`surface`), the places they live in (`slots`), the calls they can make
//! (`api`), ✨ Create Plugin (`create`, `agent`), Settings → Plugins
//! (`settings_page`) and its Get plugins list (`browse`). The display-free half is `super_desktop::plugins`.
pub mod agent;
pub mod api;
pub mod browse;
pub mod cards;
pub mod create;
pub mod llm;
pub mod settings_page;
pub mod slots;
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

/// Styles for plugin frames, Settings → Plugins and ✨ Create Plugin.
pub fn css(theme: &crate::theme::OmarchyTheme) -> String {
    use crate::theme::OmarchyTheme;
    format!(
        ".plugin-surface-error {{ background-color: {error_bg}; color: {foreground}; border: 1px solid {error_border}; border-radius: 10px; padding: 8px; font-size: 11px; }}
.plugin-create-request-frame {{ border: 1px solid {border}; border-radius: 6px; }}
.plugin-settings-row {{ padding: 6px 0 10px 0; border-bottom: 1px solid {border}; }}
.plugin-toolbar-item {{ border-radius: 999px; }}
.plugin-browse-row {{ padding: 8px 0 10px 0; border-bottom: 1px solid {border}; }}
.plugin-browse-category {{ margin-top: 6px; }}
.plugin-popup {{ background-color: {background}; border: 1px solid {border}; border-radius: 12px; box-shadow: 0 12px 32px rgba(0, 0, 0, 0.55); }}
",
        error_bg = OmarchyTheme::hex_to_rgba(&theme.background, 0.92),
        error_border = OmarchyTheme::hex_to_rgba(&theme.red, 0.75),
        foreground = theme.foreground,
        background = theme.background,
        border = theme.rgba_muted(0.30),
    )
}
