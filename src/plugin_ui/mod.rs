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

/// Text on the accent: the theme's background or foreground, whichever
/// contrasts more with it.
fn on_color(theme: &crate::theme::OmarchyTheme) -> String {
    fn luminance(hex: &str) -> f64 {
        let hex = hex.trim_start_matches('#');
        let channel = |i: usize| {
            let v = f64::from(u8::from_str_radix(hex.get(i..i + 2).unwrap_or("80"), 16).unwrap_or(128)) / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
    }
    let contrast = |a: &str, b: &str| {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    };
    if contrast(&theme.background, &theme.accent) >= contrast(&theme.foreground, &theme.accent) {
        theme.background.clone()
    } else {
        theme.foreground.clone()
    }
}

/// Styles for plugin frames, Settings → Plugins and ✨ Create Plugin.
pub fn css(theme: &crate::theme::OmarchyTheme) -> String {
    use crate::theme::OmarchyTheme;
    format!(
        ".plugin-surface-error {{ background-color: {error_bg}; color: {foreground}; border: 1px solid {error_border}; border-radius: 10px; padding: 8px; font-size: 11px; }}
.plugin-create-request-frame {{ border: 1px solid {border}; border-radius: 6px; }}
.plugin-settings-row {{ padding: 6px 0 10px 0; border-bottom: 1px solid {border}; }}
.plugin-toolbar-item {{ border-radius: 999px; }}
.plugin-tabs {{ background-color: {chip_bg}; border-radius: 10px; padding: 3px; }}
.plugin-tab {{ background: none; box-shadow: none; border: none; border-radius: 8px; padding: 5px 16px; color: {text_soft}; font-weight: 700; font-size: 12px; }}
.plugin-tab:hover {{ color: {foreground}; }}
.plugin-tab:checked {{ background-color: {accent_soft}; color: {foreground}; }}
.plugin-search {{ min-height: 34px; border-radius: 10px; }}
.plugin-icon-button {{ background: {chip_bg}; box-shadow: none; border: none; border-radius: 10px; min-width: 34px; color: {text_soft}; }}
.plugin-icon-button:hover {{ color: {foreground}; background-color: {accent_soft}; }}
.plugin-chip {{ background: {chip_bg}; box-shadow: none; border: 1px solid transparent; border-radius: 999px; padding: 3px 14px; min-height: 0; color: {text_soft}; font-size: 11.5px; font-weight: 600; }}
.plugin-chip:hover {{ border-color: {accent_line}; color: {foreground}; }}
.plugin-chip:checked {{ background-color: {accent}; color: {on_accent}; }}
.plugin-group-title {{ color: {foreground}; font-size: 13px; font-weight: 800; margin-top: 4px; }}
.plugin-card {{ background-color: {card_bg}; border: 1px solid {border}; border-radius: 14px; }}
.plugin-card:hover {{ border-color: {accent_line}; }}
.plugin-card-preview {{ background-color: {preview_bg}; }}
.plugin-card-placeholder {{ font-size: 34px; opacity: 0.35; }}
.plugin-thumb {{ border-radius: 8px; border: 1px solid {border}; }}
.plugin-card-body {{ padding: 12px 14px 14px 14px; }}
.plugin-card-name {{ color: {foreground}; font-size: 13.5px; font-weight: 800; }}
.plugin-card-text {{ color: {text_soft}; font-size: 11.5px; }}
.plugin-card-meta {{ color: {text_hint}; font-size: 10.5px; }}
.plugin-subtle {{ color: {text_hint}; font-size: 10.5px; }}
.plugin-error {{ color: {error_text}; }}
.plugin-badge {{ background-color: {chip_bg}; color: {text_soft}; border-radius: 999px; padding: 1px 8px; font-size: 10px; font-weight: 700; }}
.plugin-action {{ background-color: {chip_bg}; background-image: none; box-shadow: none; border: 1px solid transparent; border-radius: 9px; padding: 5px 12px; color: {text_soft}; font-size: 11.5px; font-weight: 700; }}
.plugin-action:disabled {{ color: {text_hint}; }}
.plugin-action-primary {{ background-color: {accent}; color: {on_accent}; }}
.plugin-action-primary:hover {{ background-color: {accent_hover}; }}
.plugin-action-primary:disabled {{ background-color: {chip_bg}; color: {text_hint}; }}
.plugin-link {{ background: none; box-shadow: none; border: none; padding: 5px 6px; color: {text_hint}; font-size: 11px; font-weight: 600; }}
.plugin-link:hover {{ color: {foreground}; }}
.plugin-popup {{ background-color: {background}; border: 1px solid {border}; border-radius: 12px; box-shadow: 0 12px 32px rgba(0, 0, 0, 0.55); }}
",
        error_bg = OmarchyTheme::hex_to_rgba(&theme.background, 0.92),
        error_border = OmarchyTheme::hex_to_rgba(&theme.red, 0.75),
        foreground = theme.foreground,
        background = theme.background,
        text_soft = OmarchyTheme::hex_to_rgba(&theme.foreground, 0.78),
        text_hint = OmarchyTheme::hex_to_rgba(&theme.foreground, 0.55),
        chip_bg = OmarchyTheme::hex_to_rgba(&theme.foreground, 0.07),
        card_bg = OmarchyTheme::hex_to_rgba(&theme.foreground, 0.035),
        preview_bg = OmarchyTheme::hex_to_rgba(&theme.foreground, 0.05),
        accent = theme.accent,
        accent_soft = OmarchyTheme::hex_to_rgba(&theme.accent, 0.22),
        accent_line = OmarchyTheme::hex_to_rgba(&theme.accent, 0.55),
        accent_hover = OmarchyTheme::hex_to_rgba(&theme.accent, 0.85),
        on_accent = on_color(theme),
        error_text = theme.red,
        border = theme.rgba_muted(0.30),
    )
}
