use gtk4::{gdk, style_context_add_provider_for_display, CssProvider, STYLE_PROVIDER_PRIORITY_APPLICATION};
use std::cell::RefCell;

use crate::theme::{current_theme, reload_theme, OmarchyTheme};

thread_local! {
    static CSS_PROVIDER: RefCell<Option<CssProvider>> = RefCell::new(None);
}

pub fn generate_css(theme: &OmarchyTheme) -> String {
    format!(
r#"
/* ================= Base Window & Backdrop ================= */
window.super-desktop-window {{
    background-color: {win_bg};
}}

/* ================= Top Floating HUD Bar ================= */
.hud-bar {{
    background-color: {hud_bg};
    border: 1px solid {hud_border};
    border-radius: 9999px;
    padding: 6px 16px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.55), 0 0 1px {hud_glow};
}}

.hud-title {{
    color: {accent};
    font-weight: 800;
    font-size: 13px;
    letter-spacing: 0.5px;
}}

.hud-badge {{
    background-color: {badge_bg};
    color: {accent};
    border-radius: 9999px;
    padding: 3px 9px;
    font-size: 11px;
    font-weight: 600;
}}

.hud-button {{
    background-color: {btn_bg};
    color: {foreground};
    border: 1px solid {btn_border};
    border-radius: 9999px;
    padding: 5px 12px;
    font-size: 12px;
    font-weight: 600;
    transition: all 180ms ease;
}}

.hud-button:hover {{
    background-color: {btn_hover_bg};
    color: {bright_foreground};
    border-color: {accent};
}}

.hud-button:active {{
    background-color: {badge_bg};
    border-color: {accent};
}}

.hud-button-danger {{
    background-color: {danger_bg};
    color: {bright_red};
    border-color: {danger_border};
}}

.hud-button-danger:hover {{
    background-color: {danger_border};
    color: #ffffff;
}}

.hud-shortcut {{
    color: {dark_foreground};
    font-size: 11px;
    font-weight: 500;
}}

/* ================= Sticky Notes ================= */
.sticky-note {{
    background-color: {note_bg};
    border: 1px solid {note_border};
    border-radius: 14px;
    box-shadow: 0 10px 28px rgba(0, 0, 0, 0.45), 0 2px 6px rgba(0, 0, 0, 0.25);
    transition: box-shadow 200ms ease;
}}

.sticky-note:hover {{
    box-shadow: 0 14px 38px rgba(0, 0, 0, 0.6), 0 0 14px {term_hover_glow};
}}

.sticky-note:focus-within {{
    box-shadow: 0 14px 38px rgba(0, 0, 0, 0.6), 0 0 0 2px {accent};
}}

.note-header {{
    background-color: {note_hdr_bg};
    padding: 7px 10px 6px 12px;
    border-top-left-radius: 13px;
    border-top-right-radius: 13px;
    border-bottom: 1px solid {note_hdr_divider};
}}

.note-header-grip {{
    color: {dark_foreground};
    font-size: 13px;
    opacity: 0.75;
}}

.note-header-title {{
    color: {accent};
    font-weight: 700;
    font-size: 12px;
    letter-spacing: 0.3px;
}}

.note-header-btn {{
    background: transparent;
    border: none;
    color: {light_foreground};
    padding: 2px 6px;
    border-radius: 6px;
    font-size: 11px;
    opacity: 0.85;
    transition: opacity 150ms ease, background-color 150ms ease, color 150ms ease;
}}

.note-header-btn:hover {{
    opacity: 1.0;
    color: {bright_foreground};
    background-color: {btn_hover_bg};
}}

.note-content-area {{
    background-color: {note_content_bg};
    padding: 8px 12px 12px 12px;
    border-bottom-left-radius: 13px;
    border-bottom-right-radius: 13px;
}}

.note-textview {{
    background: transparent;
}}

.note-textview text {{
    background: transparent;
    color: {foreground};
    font-family: '{font_family}', sans-serif;
    font-size: 13px;
    line-height: 1.45;
}}

.note-textview text:selected {{
    background-color: {selection};
    color: {bright_foreground};
}}

/* ================= Mini Terminal Cards ================= */
.mini-terminal {{
    background-color: {term_card_bg};
    border-radius: 14px;
    padding: 0;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.55), 0 0 1px {term_card_border};
    transition: box-shadow 180ms ease;
}}

.mini-terminal:hover {{
    box-shadow: 0 14px 38px rgba(0, 0, 0, 0.65), 0 0 14px {term_hover_glow};
}}

.mini-terminal:focus-within {{
    box-shadow: 0 14px 42px rgba(0, 0, 0, 0.7), 0 0 0 1.5px {accent};
}}

.mini-terminal.term-expanded {{
    box-shadow: 0 28px 80px rgba(0, 0, 0, 0.72), 0 0 0 1px {term_expand_glow};
}}

.mini-terminal.term-resizing,
.mini-terminal.term-compact.term-resizing {{
    opacity: 0.45;
    box-shadow: 0 14px 42px rgba(0, 0, 0, 0.7), 0 0 24px {term_hover_glow};
    outline: 2.5px dashed {accent};
    outline-offset: 2px;
    transition: opacity 120ms ease;
}}

/* ================= Window Resize Ghost & Rulers ================= */
.term-resize-ghost {{
    border: 3.5px dashed {accent};
    border-radius: 14px;
    background-color: {ghost_bg};
    box-shadow: 0 0 36px {ghost_shadow}, 0 0 0 1.5px rgba(0, 0, 0, 0.85), inset 0 0 24px {ghost_inset};
}}

.term-resize-ghost.ghost-icon {{
    border-radius: 18px;
    border-color: {bright_blue};
    border-width: 3.5px;
    border-style: dashed;
    background-color: {ghost_icon_bg};
    box-shadow: 0 0 36px {ghost_icon_shadow}, 0 0 0 1.5px rgba(0, 0, 0, 0.85), inset 0 0 24px {ghost_icon_inset};
}}

.term-ghost-label {{
    color: {accent};
    font-family: '{font_family}', monospace;
    font-size: 14px;
    font-weight: 800;
    letter-spacing: 0.5px;
    text-shadow: 0 0 12px {ghost_shadow};
    background-color: {ghost_label_bg};
    border-radius: 10px;
    padding: 8px 18px;
    border: 2px solid {ghost_label_border};
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.75), 0 0 14px {ghost_shadow};
}}

.term-resize-ghost.ghost-icon .term-ghost-label {{
    color: {bright_blue};
    text-shadow: 0 0 12px {ghost_icon_shadow};
    border-color: {ghost_icon_border};
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.75), 0 0 14px {ghost_icon_shadow};
}}

.mini-terminal.term-compact {{
    border-radius: 18px;
    background-color: {compact_bg};
}}

.mini-terminal.term-compact .term-preview-box {{
    margin: 4px;
    padding: 0;
    background: transparent;
    border: none;
}}

.term-agent-icon {{
    color: {bright_foreground};
    font-size: 38px;
}}

.term-agent-name {{
    color: {light_foreground};
    font-size: 11px;
    font-weight: 700;
    margin-top: 3px;
}}

.term-compact-top-bar {{
    margin: 6px 8px 0 8px;
}}

.term-compact-status {{
    font-size: 11px;
    padding: 0;
    background: transparent;
    border: none;
}}

.term-compact-status.status-idle,
.term-compact-status.status-active,
.term-compact-status.status-busy,
.term-compact-status.status-exited {{
    background-color: transparent;
    border: none;
}}

.term-compact-actions {{
    background-color: {compact_actions_bg};
    border-radius: 6px;
    padding: 1px 2px;
}}

.term-compact-btn {{
    min-width: 20px;
    min-height: 20px;
    padding: 1px 4px;
    font-size: 11px;
}}

.term-resize-handle {{
    color: {dark_foreground};
    font-size: 12px;
    padding: 2px 5px 0 8px;
    min-width: 18px;
    min-height: 16px;
}}

.term-resize-handle:hover {{
    color: {accent};
}}

.term-vte {{
    background-color: {darker_background};
}}

/* Agent Card Themed Accents */
.agent-card-antigravity {{ border: 1.5px solid {agent_blue}; }}
.agent-card-claude {{ border: 1.5px solid {agent_orange}; }}
.agent-card-codex {{ border: 1.5px solid {agent_green}; }}
.agent-card-opencode {{ border: 1.5px solid {agent_cyan}; }}
.agent-card-grok {{ border: 1.5px solid {agent_magenta}; }}
.agent-card-aider {{ border: 1.5px solid {agent_bright_magenta}; }}
.agent-card-shell {{ border: 1.5px solid {agent_shell}; }}

.term-header {{
    background-color: {term_hdr_bg};
    border-top-left-radius: 13px;
    border-top-right-radius: 13px;
    padding: 7px 10px;
    border-bottom: 1px solid {term_hdr_divider};
}}

.term-title {{
    color: {bright_foreground};
    font-weight: 700;
    font-size: 12px;
}}

.term-status-badge {{
    border-radius: 9999px;
    padding: 2px 7px;
    font-size: 10px;
    font-weight: 600;
}}

.status-active {{
    background-color: {status_active_bg};
    color: {bright_green};
    border: 1px solid {status_active_border};
}}

.status-idle {{
    background-color: {status_idle_bg};
    color: {bright_green};
    border: 1px solid {status_idle_border};
}}

.status-busy {{
    background-color: {status_busy_bg};
    color: {bright_yellow};
    border: 1px solid {status_busy_border};
}}

.status-exited {{
    background-color: {status_exited_bg};
    color: {light_foreground};
    border: 1px solid {status_exited_border};
}}

.term-btn {{
    background: transparent;
    border: none;
    color: {light_foreground};
    padding: 3px 6px;
    border-radius: 6px;
    font-size: 11px;
    transition: all 150ms ease;
}}

.term-btn:hover {{
    color: {bright_foreground};
    background-color: {btn_hover_bg};
}}

.term-preview-box {{
    background-color: {darker_background};
    margin: 4px 8px 4px 8px;
    padding: 2px;
    border-radius: 8px;
    border: 1px solid {preview_border};
}}

.term-expanded .term-preview-box {{
    margin: 4px 8px 4px 8px;
    padding: 2px;
}}

.term-preview-text {{
    font-family: '{font_family}', monospace;
    font-size: 9.5px;
    line-height: 1.35;
    color: {preview_text};
}}

.term-footer {{
    padding: 2px 10px 8px 10px;
}}

.term-meta {{
    color: {dark_foreground};
    font-size: 10px;
    font-weight: 500;
}}

.term-hint {{
    color: {dark_foreground};
    font-size: 9.5px;
    font-style: italic;
    opacity: 0.8;
}}
"#,
        win_bg = theme.rgba_darker_bg(0.72),
        hud_bg = theme.rgba_dark_bg(0.92),
        hud_border = theme.rgba_accent(0.18),
        hud_glow = theme.rgba_accent(0.2),
        accent = theme.accent,
        badge_bg = theme.rgba_accent(0.15),
        foreground = theme.foreground,
        bright_foreground = theme.bright_foreground,
        dark_foreground = theme.dark_foreground,
        light_foreground = theme.light_foreground,
        btn_bg = theme.rgba_lighter_bg(0.85),
        btn_border = theme.rgba_muted(0.35),
        btn_hover_bg = theme.rgba_muted(0.45),
        danger_bg = OmarchyTheme::hex_to_rgba(&theme.red, 0.20),
        bright_red = theme.bright_red,
        danger_border = OmarchyTheme::hex_to_rgba(&theme.red, 0.40),
        note_bg = theme.rgba_dark_bg(0.94),
        note_border = theme.rgba_muted(0.35),
        note_hdr_bg = theme.rgba_lighter_bg(0.85),
        note_hdr_divider = theme.rgba_muted(0.30),
        note_content_bg = theme.rgba_dark_bg(0.90),
        selection = theme.selection,
        term_card_bg = theme.rgba_dark_bg(0.94),
        term_card_border = theme.rgba_muted(0.40),
        term_hover_glow = theme.rgba_accent(0.25),
        term_expand_glow = theme.rgba_accent(0.35),
        ghost_bg = theme.rgba_dark_bg(0.60),
        ghost_shadow = theme.rgba_accent(0.55),
        ghost_inset = theme.rgba_accent(0.20),
        bright_blue = theme.bright_blue,
        ghost_icon_bg = theme.rgba_darker_bg(0.70),
        ghost_icon_shadow = OmarchyTheme::hex_to_rgba(&theme.bright_blue, 0.60),
        ghost_icon_inset = OmarchyTheme::hex_to_rgba(&theme.bright_blue, 0.22),
        font_family = theme.font_family,
        ghost_label_bg = theme.rgba_darker_bg(0.88),
        ghost_label_border = theme.rgba_accent(0.85),
        ghost_icon_border = OmarchyTheme::hex_to_rgba(&theme.bright_blue, 0.85),
        compact_bg = theme.rgba_dark_bg(0.94),
        compact_actions_bg = theme.rgba_lighter_bg(0.85),
        darker_background = theme.darker_background,
        agent_blue = OmarchyTheme::hex_to_rgba(&theme.blue, 0.70),
        agent_orange = OmarchyTheme::hex_to_rgba(&theme.orange, 0.70),
        agent_green = OmarchyTheme::hex_to_rgba(&theme.green, 0.70),
        agent_cyan = OmarchyTheme::hex_to_rgba(&theme.cyan, 0.70),
        agent_magenta = OmarchyTheme::hex_to_rgba(&theme.magenta, 0.70),
        agent_bright_magenta = OmarchyTheme::hex_to_rgba(&theme.bright_magenta, 0.70),
        agent_shell = theme.rgba_muted(0.55),
        term_hdr_bg = theme.rgba_lighter_bg(0.85),
        term_hdr_divider = theme.rgba_muted(0.30),
        bright_green = theme.bright_green,
        status_active_bg = OmarchyTheme::hex_to_rgba(&theme.green, 0.18),
        status_active_border = OmarchyTheme::hex_to_rgba(&theme.green, 0.35),
        status_idle_bg = OmarchyTheme::hex_to_rgba(&theme.green, 0.18),
        status_idle_border = OmarchyTheme::hex_to_rgba(&theme.green, 0.35),
        bright_yellow = theme.bright_yellow,
        status_busy_bg = OmarchyTheme::hex_to_rgba(&theme.yellow, 0.18),
        status_busy_border = OmarchyTheme::hex_to_rgba(&theme.yellow, 0.35),
        status_exited_bg = theme.rgba_muted(0.20),
        status_exited_border = theme.rgba_muted(0.35),
        preview_border = theme.rgba_muted(0.25),
        preview_text = theme.bright_cyan,
    )
}

pub fn apply_styles() {
    CSS_PROVIDER.with(|cell| {
        let mut opt = cell.borrow_mut();
        let provider = opt.get_or_insert_with(|| {
            let p = CssProvider::new();
            if let Some(display) = gdk::Display::default() {
                style_context_add_provider_for_display(
                    &display,
                    &p,
                    STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }
            p
        });

        let theme = current_theme();
        let css = generate_css(&theme);
        provider.load_from_string(&css);
    });
}

pub fn reload_styles() -> OmarchyTheme {
    let theme = reload_theme();
    let css = generate_css(&theme);

    CSS_PROVIDER.with(|cell| {
        let mut opt = cell.borrow_mut();
        if let Some(provider) = opt.as_ref() {
            provider.load_from_string(&css);
        } else {
            let p = CssProvider::new();
            if let Some(display) = gdk::Display::default() {
                style_context_add_provider_for_display(
                    &display,
                    &p,
                    STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }
            p.load_from_string(&css);
            *opt = Some(p);
        }
    });

    theme
}
