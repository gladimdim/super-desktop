use gtk4::{gdk, style_context_add_provider_for_display, CssProvider, STYLE_PROVIDER_PRIORITY_APPLICATION};
use std::cell::RefCell;

use crate::theme::{current_theme, reload_theme, OmarchyTheme};

thread_local! {
    static CSS_PROVIDER: RefCell<Option<CssProvider>> = RefCell::new(None);
}

/// Run GTK's own theme in the Omarchy theme's light or dark variant.
/// Without a desktop settings portal GTK falls back to its light variant,
/// whose white buttons, entries and popovers then show the dark palette's
/// light text on white.
fn match_gtk_variant(theme: &OmarchyTheme) {
    if let Some(settings) = gtk4::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(theme.mode != "light");
    }
}

pub fn generate_css(theme: &OmarchyTheme) -> String {
    let mut css = theme_css(theme);
    css.push_str(&icon_radius_css());
    css.push_str(&crate::welcome_tour::css(theme));
    css.push_str(&crate::attachment_view::css(theme));
    css
}

/// One class per corner radius an iconified card can have, since GTK takes a
/// border radius from CSS only: an icon drawn smaller or larger than 128 px
/// (a remote PC's, fitted into the view) rounds its corners in proportion
/// instead of turning into a circle. See `mini_terminal::icon_radius_class`.
fn icon_radius_css() -> String {
    (1..=crate::mini_terminal::MAX_ICON_RADIUS)
        .map(|radius| {
            format!(
                ".mini-terminal.term-compact.term-icon-radius-{radius} {{ border-radius: {radius}px; }}\n"
            )
        })
        .collect()
}

/// One `.tag-dot-N` class per group color in `tag::TAG_COLORS`.
fn tag_dot_css() -> String {
    crate::tag::TAG_COLORS
        .iter()
        .enumerate()
        .map(|(index, color)| {
            format!(".tag-dot-{} {{ background-color: {color}; border-color: {color}; }}", index + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn theme_css(theme: &OmarchyTheme) -> String {
    format!(
r#"
/* ================= Base Window & Backdrop ================= */
.asset-drawer {{
    color: {foreground};
}}
.asset-drawer .asset-group {{
    padding: 8px;
    border: 1px solid {btn_border};
    border-radius: 6px;
}}
.asset-drawer .asset-group-title {{
    font-size: 18px;
    font-weight: bold;
}}
.asset-drawer textview, .asset-drawer textview text {{
    background-color: {note_content_bg};
    color: {foreground};
}}
.asset-drawer button, .asset-drawer entry {{
    background-color: {btn_bg};
    background-image: none;
    color: {foreground};
    border: 1px solid {btn_border};
    border-radius: 5px;
}}
.asset-drawer button:checked {{
    border-color: {accent};
    box-shadow: inset 0 -2px {accent};
}}
window.super-desktop-window {{
    background-color: {win_bg};
}}

/* ============== Hot corner (almost-invisible input surface) ============== */
/* The 8x8 layer surface in the top-left corner that receives the pointer for
   the hot-corner gesture (see hotcorner.rs).

   It paints 1/255 of black — not `transparent`, on purpose, and this is
   measured, not stylistic: GTK treats a FULLY transparent window as
   click-through, so it both refuses the pointer (no dwell, no gesture) and
   ignores the size request (the surface silently becomes 200x200, which would
   then swallow clicks far outside the corner). An 8-bit alpha of 1 is the
   smallest value that is not zero, so this is the least ink that keeps the
   surface real: a 0.4% darkening over 8x8 pixels, invisible on any wallpaper,
   and it must stay that way — 0.002 still rounds to nothing. */
window.sd-hot-corner {{
    background-color: rgba(0, 0, 0, 0.004);
    background-image: none;
    box-shadow: none;
}}

/* ================= Full-width Top Dock ================= */
.hud-bar {{
    background-color: {hud_bg};
    border: none;
    border-bottom: 1px solid {hud_border};
    border-radius: 0;
    padding: 6px 16px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.38), inset 0 -1px {hud_glow};
}}

.hud-bar.hud-size-medium {{
    padding: 4px 12px;
}}

.hud-bar.hud-size-small {{
    padding: 2px 10px;
}}

.hud-size-medium .hud-button,
.hud-size-medium menubutton.machine-selector > button {{ padding: 4px 10px; font-size: 11px; }}
.hud-size-medium .hud-gear,
.hud-size-medium .hud-icon-btn {{ min-width: 32px; min-height: 32px; padding: 0; }}
.hud-size-medium .hud-title {{ font-size: 12px; }}
.hud-size-medium .hud-shortcut {{ font-size: 10px; }}
.hud-size-medium entry.ws-entry {{ min-height: 24px; padding: 2px 8px; font-size: 12px; }}

.hud-size-small .hud-button,
.hud-size-small menubutton.machine-selector > button {{ padding: 2px 8px; font-size: 10px; }}
.hud-size-small .hud-gear,
.hud-size-small .hud-icon-btn {{ min-width: 28px; min-height: 28px; padding: 0; }}
.hud-size-small .hud-title {{ font-size: 11px; }}
.hud-size-small .hud-shortcut {{ font-size: 9px; padding-top: 2px; padding-bottom: 2px; }}
.hud-size-small .ws-subtitle {{ font-size: 8px; padding-bottom: 0; }}
.hud-size-small .ws-icon {{ font-size: 10px; }}
.hud-size-small entry.ws-entry {{ min-height: 20px; padding: 1px 7px; font-size: 11px; }}
.hud-size-small .ws-menu-btn {{ padding: 1px 6px; font-size: 10px; }}

.hud-title {{
    color: {accent};
    font-weight: 800;
    font-size: 13px;
    letter-spacing: 0.5px;
}}

.hud-button,
menubutton.machine-selector > button {{
    background-color: transparent;
    background-image: none;
    color: {light_foreground};
    border: none;
    border-radius: 6px;
    box-shadow: none;
    padding: 6px 10px;
    font-size: 12px;
    font-weight: 600;
    transition: background-color 120ms ease, color 120ms ease, box-shadow 120ms ease;
}}

.hud-button:hover,
menubutton.machine-selector > button:hover {{
    background-color: {btn_hover_bg};
    color: {bright_foreground};
    box-shadow: inset 0 -2px {accent};
}}

.hud-button:active,
menubutton.machine-selector > button:active,
menubutton.machine-selector > button:checked {{
    background-color: {badge_bg};
    color: {accent};
    box-shadow: inset 0 -2px {accent};
}}

.hud-action-primary {{
    color: {accent};
    font-weight: 700;
}}

/* MenuButton wraps a real button: style that node, avoiding nested pills. */
menubutton.machine-selector {{
    padding: 0;
    background: transparent;
    border: none;
    box-shadow: none;
}}
menubutton.machine-selector > button:focus-visible,
button.machine-peer:focus-visible {{
    outline: 1px solid {accent};
    outline-offset: -2px;
}}
button.machine-peer {{
    padding: 8px 10px;
    border-radius: 8px;
}}
button.machine-peer-selected {{
    color: {accent};
    background-color: {badge_bg};
}}

/* ================= Remote PC live consoles ================= */

.remote-canvas {{
    background-color: {darker_background};
    border-radius: 12px;
}}

/* That PC's screen edge, drawn over its cards (remote_terminal.rs). */
.remote-canvas-border {{
    border: 2px solid {accent};
    border-radius: 12px;
}}

/* Icon-only HUD chrome (⚙ settings, arrange, Hide): symbolic SVGs recolored by
   the Omarchy palette through `color`, oversized into a clear hit target so they
   read as icons rather than one more pill among the labels. */
.hud-gear,
.hud-icon-btn {{
    padding: 0;
    min-width: 36px;
    min-height: 36px;
}}
.hud-gear image,
.hud-icon-btn image {{
    min-width: 20px;
    min-height: 20px;
}}

.hud-button-danger {{
    background-color: transparent;
    color: {red_text};
}}

.hud-button-danger:hover {{
    background-color: {danger_bg};
    color: {red_text};
    box-shadow: inset 0 -2px {bright_red};
}}

.hud-shortcut {{
    color: {dark_foreground};
    font-size: 11px;
    font-weight: 500;
}}

.top-bar-size-active {{
    background-color: {badge_bg};
    color: {accent};
    border-color: {accent};
}}

/* ================= Top Bar: Workspace Folder Field ================= */
/* The folder new harness cards start in, plus its ▾ re-use history. */
.ws-bar {{
    margin-left: 2px;
}}

.ws-subtitle {{
    color: {dark_foreground};
    font-size: 9px;
    font-weight: 500;
    letter-spacing: 0.15px;
    padding: 0 2px 1px 2px;
}}

.ws-icon {{
    color: {dark_foreground};
    font-size: 12px;
}}

entry.ws-entry {{
    background-color: transparent;
    background-image: none;
    color: {foreground};
    border: none;
    border-bottom: 1px solid {btn_border};
    border-radius: 0;
    box-shadow: none;
    padding: 3px 6px;
    min-height: 28px;
    min-width: 0;
    font-size: 13px;
    font-weight: 600;
    transition: border-color 150ms ease;
}}

entry.ws-entry:focus {{
    border-color: {accent};
    box-shadow: inset 0 -1px {accent};
}}

/* The text is not a folder that exists: keep the old folder in use and say so
   instead of starting cards somewhere the user did not ask for. */
entry.ws-entry.ws-entry-invalid {{
    border-color: {bright_red};
    color: {red_text};
}}

.ws-menu-btn {{
    background-color: transparent;
    background-image: none;
    color: {dark_foreground};
    border: none;
    border-radius: 5px;
    box-shadow: none;
    padding: 2px 6px;
    min-height: 0;
    font-size: 11px;
    font-weight: 700;
}}

.ws-menu-btn:hover {{
    background-color: {btn_hover_bg};
    color: {bright_foreground};
}}

.ws-resize-handle {{
    color: {dark_foreground};
    min-width: 10px;
    min-height: 22px;
    margin-left: 1px;
    font-size: 13px;
}}

.ws-resize-handle:hover {{
    color: {accent};
}}

popover.ws-pop {{
    background-color: {hud_bg};
    border: 1px solid {hud_border};
    border-radius: 12px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.55), 0 0 1px {hud_glow};
    padding: 0;
}}

popover.ws-pop > contents {{
    background-color: transparent;
    border-radius: 12px;
    padding: 0;
}}

.ws-pop-box {{
    padding: 6px;
    min-width: 280px;
}}

.ws-row-pick {{
    background-color: transparent;
    border: none;
    box-shadow: none;
    padding: 4px 6px;
    border-radius: 8px;
}}

.ws-row-pick:hover {{
    background-color: {btn_hover_bg};
}}

.ws-row-mark {{
    color: {accent};
    font-size: 11px;
    font-weight: 700;
    min-width: 10px;
}}

.ws-row-name {{
    color: {foreground};
    font-size: 12px;
    font-weight: 700;
}}

/* The folder currently in use stands out from the rest of the history. */
.ws-row-active .ws-row-name {{
    color: {accent};
}}

/* ↑/↓ highlight in the autocomplete list. */
.ws-row-selected {{
    background-color: {badge_bg};
    border-radius: 8px;
}}

.ws-row-selected .ws-row-name {{
    color: {accent};
}}

.ws-row-path {{
    color: {dark_foreground};
    font-size: 11px;
    font-weight: 500;
}}

.ws-del {{
    background-color: transparent;
    border: none;
    box-shadow: none;
    color: {dark_foreground};
    padding: 2px 6px;
    border-radius: 8px;
    font-size: 11px;
    font-weight: 700;
}}

.ws-del:hover {{
    background-color: {danger_bg};
    color: {red_text};
}}

.ws-empty {{
    color: {dark_foreground};
    font-size: 11px;
    padding: 4px 6px;
}}

/* ================= Sticky Notes ================= */
.sticky-note {{
    background-color: {note_bg};
    border: 1px solid {note_border};
    border-radius: 14px;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.45);
    transition: border-color 150ms ease;
}}

.sticky-note:hover {{
    border-color: {accent};
}}

.sticky-note:focus-within {{
    border-color: {accent};
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.45), 0 0 0 2px {accent};
}}

/* Perf: while a card is dragged at 120Hz, kill every animated effect so
   each frame is a plain translated blit with no shadow re-raster. */
.sticky-note.dragging,
.mini-terminal.dragging {{
    transition: none;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.5);
}}

/* Slide-in/out runs on the frame clock (up to the monitor Hz). Dropping the
   shadow while cards are flying saves a GSK blur per widget per frame. */
.sliding .sticky-note,
.sliding .mini-terminal,
.sliding .hud-bar {{
    transition: none;
    box-shadow: none;
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
    transition: opacity 150ms ease, background-color 150ms ease;
}}

.note-header-btn:hover {{
    opacity: 1.0;
    color: {bright_foreground};
    background-color: {btn_hover_bg};
}}

/* ================= Group Color Tags (notes + terminals) ================= */
.tag-dot {{
    min-width: 14px;
    min-height: 14px;
    border-radius: 9999px;
    padding: 0;
    border: 1.5px solid {btn_border};
}}

.tag-dot:hover {{
    border-color: {bright_foreground};
}}

.tag-dot-none {{
    background-color: transparent;
    border-style: dashed;
    opacity: 0.55;
}}

{tag_dots}

.tag-pop-box {{
    padding: 8px;
}}

.tag-swatch {{
    min-width: 22px;
    min-height: 22px;
    border-radius: 9999px;
    padding: 0;
}}

.tag-swatch:hover {{
    border-color: #ffffff;
}}

.tag-swatch.tag-selected {{
    border: 2px solid #ffffff;
    box-shadow: 0 0 8px rgba(255, 255, 255, 0.45);
}}

/* ================= Provider Usage Hover Card ================= */
popover.usage-pop {{
    background-color: {hud_bg};
    border: 1px solid {hud_border};
    border-radius: 14px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.55), 0 0 1px {hud_glow};
    padding: 0;
}}

popover.usage-pop > contents {{
    background-color: transparent;
    border-radius: 14px;
    padding: 0;
}}

.usage-pop-box {{
    padding: 10px 12px 12px 12px;
}}

/* Header pill: the HUD button mirrored, so the card reads as the button
   itself unfolding downward. */
.usage-head {{
    background-color: {btn_bg};
    border: 1px solid {btn_border};
    border-radius: 9999px;
    padding: 5px 12px;
}}

.usage-head-name {{
    color: {foreground};
    font-size: 12px;
    font-weight: 700;
}}

separator.usage-sep {{
    background-color: {term_hdr_divider};
    min-height: 1px;
    margin: 2px 0;
}}

.usage-launch {{
    color: {dark_foreground};
    font-size: 11px;
    font-style: italic;
}}

.usage-title {{
    color: {accent};
    font-weight: 800;
    font-size: 13px;
}}

.usage-tier {{
    background-color: {badge_bg};
    color: {accent};
    border-radius: 9999px;
    padding: 2px 9px;
    font-size: 11px;
    font-weight: 600;
}}

.usage-status {{
    color: {bright_foreground};
    font-size: 12px;
    font-weight: 600;
}}

.usage-limit-label {{
    color: {foreground};
    font-size: 12px;
    font-weight: 600;
}}

.usage-left {{
    font-size: 12px;
    font-weight: 800;
}}

.usage-left.ok {{ color: {usage_ok}; }}
.usage-left.warn {{ color: {usage_warn}; }}
.usage-left.crit {{ color: {usage_crit}; }}

progressbar.usage-bar > trough {{
    min-height: 6px;
    border-radius: 9999px;
    background-color: {usage_trough};
    border: none;
}}

progressbar.usage-bar > trough > progress {{
    min-height: 6px;
    border-radius: 9999px;
}}

progressbar.usage-bar.ok > trough > progress {{ background-color: {usage_ok}; }}
progressbar.usage-bar.warn > trough > progress {{ background-color: {usage_warn}; }}
progressbar.usage-bar.crit > trough > progress {{ background-color: {usage_crit}; }}

.usage-meta {{
    color: {light_foreground};
    font-size: 11px;
}}

.usage-warn {{
    color: {usage_warn};
    font-size: 11px;
}}

.usage-src {{
    color: {dark_foreground};
    font-size: 10px;
}}

.note-content-area {{
    background-color: {note_content_bg};
    /* At least the 14px resize corners (card_resize::place_zone): a selection
       started in the text's corner must not become a resize. */
    padding: 8px 14px 14px 14px;
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
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.5);
    transition: border-color 150ms ease;
}}

.mini-terminal:hover {{
    border-color: {accent};
}}

.mini-terminal:focus-within {{
    border-color: {accent};
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.5), 0 0 0 1.5px {accent};
}}

.mini-terminal.term-expanded {{
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.6), 0 0 0 1px {term_expand_glow};
}}

.mini-terminal.term-resizing,
.mini-terminal.term-compact.term-resizing {{
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.5);
    outline: 2.5px dashed {accent};
    outline-offset: 2px;
    transition: none;
}}

/* ================= Window Resize Ghost & Rulers ================= */
.term-resize-ghost {{
    border: 3.5px dashed {accent};
    border-radius: 14px;
    background-color: {ghost_bg};
    box-shadow: 0 0 18px {ghost_shadow}, 0 0 0 1.5px rgba(0, 0, 0, 0.85);
}}

.term-resize-ghost.ghost-icon {{
    border-radius: 18px;
    border-color: {bright_blue};
    border-width: 3.5px;
    border-style: dashed;
    background-color: {ghost_icon_bg};
    box-shadow: 0 0 18px {ghost_icon_shadow}, 0 0 0 1.5px rgba(0, 0, 0, 0.85);
}}

.term-ghost-label {{
    color: {accent};
    font-family: '{font_family}', monospace;
    font-size: 14px;
    font-weight: 800;
    letter-spacing: 0.5px;
    background-color: {ghost_label_bg};
    border-radius: 10px;
    padding: 8px 18px;
    border: 2px solid {ghost_label_border};
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.7);
}}

.term-resize-ghost.ghost-icon .term-ghost-label {{
    color: {bright_blue};
    border-color: {ghost_icon_border};
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.7);
}}

.term-resize-ghost.ghost-note .term-ghost-label {{
    color: {bright_yellow};
    border-color: {bright_yellow};
}}

/* ================= Buried Terminal Ghosts =================
   A terminal covered by another card, while the user is
   working in neither, keeps a dotted outline so the desk still
   shows that it is there. It is drawn over the card that hides
   it and never takes a click. */
/* Keyboard selection dims output without changing any card geometry. */
.terminal-picker-ghost .term-preview-box {{
    opacity: 0.60;
}}
.terminal-picker-overlay {{
    color: {bright_foreground};
}}
.term-overlap-ghost {{
    border: 2px dotted {overlap_ghost_border};
    border-radius: 14px;
    background-color: transparent;
    box-shadow: 0 0 12px {overlap_ghost_glow};
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

/* An iconified card's logo, name, bar and buttons are sized in Rust, in
   proportion to the icon (`CompactChrome::fit` in mini_terminal.rs), and so is
   its corner radius (`icon_radius_css`). */
.term-agent-icon {{
    color: {bright_foreground};
}}

.term-agent-name {{
    color: {light_foreground};
    font-weight: 700;
    margin-top: 3px;
}}

.term-compact-status {{
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
    padding: 0;
}}

.term-btn.term-compact-btn {{
    min-width: 0;
    min-height: 0;
    padding: 0;
}}

.tag-dot.term-compact-tag {{
    min-width: 0;
    min-height: 0;
}}

/* Invisible Windows-style resize targets on all four edges and corners.
   Their size is set in Rust; keeping the CSS inert avoids paint work. */
.card-resize-zone {{
    background: transparent;
    border: none;
    padding: 0;
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
.agent-card-reasonix {{ border: 1.5px solid {agent_bright_blue}; }}
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
    transition: background-color 150ms ease;
}}

/* A card's panel buttons (Files, 📎, history): tighter than the header's
   other buttons, so all three fit a default-size card. */
.term-panel-btns .term-btn {{
    padding: 3px 4px;
    min-width: 0;
}}

.term-btn:hover {{
    color: {bright_foreground};
    background-color: {btn_hover_bg};
}}

.term-btn:disabled {{
    opacity: 0.4;
    background: transparent;
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

/* Command feedback on a remote card (and under the remote top bar): a brief
   line saying what the host did with the last command. The pill floats over
   the card body and never takes input; the tone classes colour it and the
   bar's note alike. */
.term-notice {{
    background-color: {compact_bg};
    border: 1px solid {btn_border};
    border-radius: 9999px;
    padding: 3px 10px;
    font-size: 10px;
    font-weight: 600;
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.35);
}}

/* ↓ Jump to newest, over a card whose pane is scrolled back through its
   history (scrollback.rs): the theme's accent, so it reads over any output. */
button.term-jump-newest {{
    background-image: none;
    background-color: {accent};
    color: {darker_background};
    border: none;
    border-radius: 9999px;
    padding: 3px 12px;
    min-height: 0;
    font-size: 11px;
    font-weight: 700;
    box-shadow: 0 2px 10px rgba(0, 0, 0, 0.45);
}}
button.term-jump-newest:hover {{
    background-color: {bright_foreground};
}}
.term-notice-info {{
    color: {accent};
}}
.term-notice.term-notice-info {{
    border-color: {ghost_label_border};
}}
.term-notice-warning {{
    color: {bright_yellow};
}}
.term-notice.term-notice-warning {{
    border-color: {status_busy_border};
}}
.term-notice-error {{
    color: {usage_crit};
}}
.term-notice.term-notice-error {{
    border-color: {usage_crit};
}}

/* Remote top bar: only "Connected" / "Disconnected". */
.remote-connected {{
    color: {accent};
}}
.remote-disconnected {{
    color: {usage_crit};
}}

/* A paired PC that is connecting or cannot be reached (connection_panel.rs):
   the likeliest cause, the fix, and a checklist of each link between the two
   PCs in the invitation's `invite-step` rows. */
.remote-status {{
    background-color: {term_card_bg};
    border: 1px solid {btn_border};
    border-radius: 14px;
    padding: 22px 24px 16px 24px;
    margin: 16px;
    box-shadow: 0 10px 32px rgba(0, 0, 0, 0.45);
}}
.invite-step-mark.remote-status-mark {{
    min-width: 46px;
    min-height: 46px;
    font-size: 21px;
}}
.remote-status-title {{
    color: {bright_foreground};
    font-size: 18px;
    font-weight: 800;
}}
.remote-status-hint {{
    color: {light_foreground};
    font-size: 12px;
    margin-bottom: 4px;
}}
.remote-status-checks .invite-step-detail {{
    font-family: '{font_family}', monospace;
}}
.remote-status-footer {{
    color: {dark_foreground};
    font-size: 10.5px;
}}
/* The GTK theme's button image would cover the pill's own colours here. */
.remote-status button.launcher-btn {{
    background-image: none;
    box-shadow: none;
}}

/* ================= Settings and Android Connection Pages ================= */
/* `harness_settings` owns the card chrome (`mini-terminal` + `harness-panel`
   + `term-header`) and swaps dedicated destination pages into its body. */
.launcher-head-badge {{
    background-color: {badge_bg};
    border-radius: 9px;
    padding: 3px 8px;
    font-size: 15px;
}}

.launcher-subtitle {{
    color: {dark_foreground};
    font-size: 10px;
    font-weight: 500;
    margin-top: 1px;
}}

.launcher-scroll,
.launcher-scroll > viewport {{
    background: transparent;
}}

.launcher-body {{
    padding: 12px 14px 14px 14px;
}}

/* Settings destinations use the same restrained card language as Android.
   The hub keeps these choices separate instead of making one long form. */
.settings-entry,
.android-settings-entry {{
    background: {launcher_section_bg};
    border: 1px solid {launcher_section_border};
    border-radius: 6px;
    padding: 14px;
    color: {foreground};
    box-shadow: none;
}}
.settings-entry:hover,
.android-settings-entry:hover {{
    border-color: {accent};
    background: {badge_bg};
}}
.settings-entry-icon {{ color: {accent}; font-size: 24px; }}
.settings-entry-title {{ color: {foreground}; font-weight: 700; font-size: 13px; }}
.settings-entry-summary {{ color: {dark_foreground}; font-size: 10.5px; }}
.settings-entry-arrow {{ color: {dark_foreground}; font-size: 22px; }}

/* PC setup is a centered overlay card, with one decision or task per page. */
.pc-wizard {{
    border: 1px solid {hud_border};
}}
.pc-wizard-page {{
    padding: 24px 28px;
}}
.pc-wizard-page > .settings-entry-title {{
    font-size: 18px;
}}
.pc-wizard-page > .settings-entry-summary {{
    font-size: 12px;
    line-height: 1.45;
}}
.pc-wizard-page .settings-entry {{
    padding: 18px;
    margin-top: 8px;
}}
.pc-wizard-page .settings-entry-title {{
    font-size: 13px;
}}
.pc-wizard-page .ws-entry {{
    min-height: 34px;
}}
.pc-wizard-status {{
    color: {light_foreground};
    font-size: 12px;
    min-height: 20px;
}}
.pc-wizard-code {{
    color: {accent};
    font-size: 30px;
    font-weight: 800;
    letter-spacing: 5px;
    margin: 12px 0;
}}
.pc-wizard-request {{
    background: {launcher_section_bg};
    border: 1px solid {accent};
    border-radius: 8px;
    padding: 16px;
    margin-top: 14px;
}}

/* Pairing request panel: the one place a device is approved or rejected. */
.pairing-panel {{
    border: 1px solid {accent};
}}
.pairing-device-icon {{ color: {accent}; }}
.pairing-device-name {{
    color: {bright_foreground};
    font-size: 17px;
    font-weight: 800;
}}
.pairing-details {{
    border-radius: 8px;
    padding: 8px 12px;
}}
.pairing-decisions {{ margin-top: 6px; }}
.pairing-decisions button {{ padding: 8px 18px; }}
.pairing-queue {{
    color: {bright_yellow};
    font-size: 11px;
    font-weight: 700;
}}
.pairing-result-mark {{
    color: {bright_green};
    font-size: 34px;
    font-weight: 800;
}}
.pairing-result-mark.pairing-result-rejected {{ color: {red_text}; }}

/* Invitation flow: a checklist of prerequisites, the invitation, then what
   happened to the request it produced. */
.invite-checklist {{
    border-radius: 8px;
    padding: 10px 12px;
}}
.invite-step {{ padding: 3px 0; }}
.invite-step-mark {{
    border-radius: 9999px;
    min-width: 20px;
    min-height: 20px;
    font-size: 11px;
    font-weight: 800;
}}
.invite-step-mark.invite-working {{ color: {dark_foreground}; background-color: {btn_bg}; }}
.invite-step-mark.invite-ok {{
    color: {bright_green};
    background-color: {status_active_bg};
    border: 1px solid {status_active_border};
}}
.invite-step-mark.invite-warn {{
    color: {bright_yellow};
    background-color: {status_busy_bg};
    border: 1px solid {status_busy_border};
}}
.invite-step-mark.invite-fail {{
    color: {red_text};
    background-color: {danger_bg};
    border: 1px solid {danger_border};
}}
.invite-step-title {{ color: {foreground}; font-size: 12px; font-weight: 700; }}
.invite-step-detail {{ color: {light_foreground}; font-size: 11px; }}
.invite-card {{
    border-radius: 8px;
    padding: 12px;
}}
.invite-expiry {{ color: {accent}; }}
.invite-qr {{ margin: 4px 0; }}
.invite-outcome {{ margin-top: 2px; }}

/* Connections overview and its sub-pages. */
.connections-pending {{
    background-color: {badge_bg};
    border: 1px solid {accent};
    border-radius: 8px;
    padding: 10px 12px;
}}
.connections-pending-text {{
    color: {bright_foreground};
    font-size: 11.5px;
    font-weight: 700;
}}
/* Settings → Updates (updates.rs): the commits an update brings, and the
   hub entry's chip once a newer version is known. */
.update-changes {{
    font-family: '{font_family}', monospace;
    font-size: 11px;
    color: {foreground};
    background-color: {launcher_section_bg};
    border: 1px solid {launcher_section_border};
    border-radius: 8px;
    padding: 8px 10px;
}}
.term-status-badge.update-available {{
    color: {darker_background};
    background-color: {accent};
    border-color: {accent};
    font-weight: 700;
}}

.connections-warning-chip {{
    background-color: {danger_bg};
    color: {bright_yellow};
    border: 1px solid {danger_border};
}}
.connections-empty {{ padding-bottom: 6px; }}
.connections-action-note {{ margin-top: 2px; }}

/* This is deliberately the first child of the Settings hub: without an open
   8759/tcp rule the Android page can look configured while every phone fails
   to reach the bridge. */
.settings-firewall-warning {{
    background-color: {danger_bg};
    border: 1px solid {danger_border};
    border-radius: 8px;
    padding: 10px 12px;
}}
.settings-firewall-warning-title {{
    color: {bright_yellow};
    font-size: 11.5px;
    font-weight: 800;
}}
.settings-firewall-warning-text {{
    color: {light_foreground};
    font-size: 10.5px;
}}
.android-connection-count {{ color: {accent}; padding: 4px 9px; }}
.android-page .launcher-section {{
    border-radius: 6px;
    padding: 14px;
}}
.android-page .launcher-section-head {{ margin-bottom: 8px; }}
.android-page .launcher-section-title {{ font-size: 13px; }}
.android-page .launcher-btn {{ border-radius: 4px; padding: 7px 12px; }}
.android-page .launcher-section-body {{ border-spacing: 8px; }}
/* Flat device groups inherit the current Omarchy palette in both modes. */
.connections-device {{
    padding: 12px 0;
    border-bottom: 1px solid {launcher_section_border};
}}
.connections-device-name {{ color: {foreground}; font-weight: 600; font-size: 12px; }}
.connections-device-detail {{ color: {dark_foreground}; font-size: 10px; }}
.connections-device .launcher-btn {{ background: transparent; }}
.android-empty {{ color: {dark_foreground}; padding: 12px 0; }}

.launcher-section {{
    background-color: {launcher_section_bg};
    border: 1px solid {launcher_section_border};
    border-radius: 12px;
    padding: 9px 12px 11px 12px;
}}

.launcher-section-num {{
    background-color: {badge_bg};
    color: {accent};
    border-radius: 9999px;
    min-width: 17px;
    min-height: 15px;
    padding: 1px 0;
    font-size: 10px;
    font-weight: 800;
}}

.launcher-section-title {{
    color: {accent};
    font-size: 12px;
    font-weight: 800;
    letter-spacing: 0.4px;
}}

.launcher-status-text {{
    color: {light_foreground};
    font-size: 11.5px;
}}

.launcher-row {{
    padding: 3px 0;
}}

.launcher-key {{
    color: {dark_foreground};
    font-size: 11.5px;
    font-weight: 600;
}}

.launcher-value {{
    color: {bright_foreground};
    font-family: '{font_family}', monospace;
    font-size: 11.5px;
}}

separator.launcher-sep {{
    background-color: {term_hdr_divider};
    min-height: 1px;
}}

.launcher-note {{
    color: {bright_yellow};
    font-size: 10.5px;
}}

/* Failed start/stop: the overlay is the only place the user sees it */
.launcher-note-error {{
    color: {red_text};
    font-weight: 700;
}}

.launcher-hint,
.launcher-footer {{
    color: {dark_foreground};
    font-size: 10px;
    font-style: italic;
}}

.launcher-actions {{
    margin-top: 2px;
}}

/* Buttons keep the HUD pill language, sized for a panel */
/* GTK's own theme paints buttons with a background image (white in its
   light variant, which GTK uses when the desktop reports no dark
   preference); it covers `background-color`, so clear it here. */
.launcher-btn {{
    background-color: {btn_bg};
    background-image: none;
    box-shadow: none;
    text-shadow: none;
    color: {foreground};
    border: 1px solid {btn_border};
    border-radius: 9px;
    padding: 6px 12px;
    font-size: 11.5px;
    font-weight: 600;
    transition: background-color 150ms ease, border-color 150ms ease;
}}

.launcher-btn:hover {{
    background-color: {btn_hover_bg};
    color: {bright_foreground};
    border-color: {accent};
}}

.launcher-btn-primary {{
    background-color: {badge_bg};
    color: {primary_text};
    border-color: {accent};
}}

.launcher-btn-danger {{
    background-color: {danger_bg};
    color: {danger_text};
    border-color: {danger_border};
}}

.launcher-btn-danger:hover {{
    background-color: {danger_border};
    color: {danger_hover_text};
}}

.launcher-online {{
    background-color: {status_active_bg};
    color: {bright_green};
    border: 1px solid {status_active_border};
}}

.launcher-offline {{
    background-color: {status_exited_bg};
    color: {light_foreground};
    border: 1px solid {status_exited_border};
}}

/* ============ Shortcut recorder (⚙ Settings · section 1) ============ */
/* The recorded combination reads as a key, not as body text: mono, boxed and
   selectable, so it can be compared with the HUD hint at a glance. */
.shortcut-row {{
    margin-top: 2px;
}}

.shortcut-combo {{
    background-color: {launcher_pin_bg};
    color: {bright_foreground};
    border: 1px solid {launcher_section_border};
    border-radius: 6px;
    padding: 5px 10px;
    font-family: '{font_family}', monospace;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.5px;
}}

/* Armed: whatever is pressed next becomes the shortcut. */
.shortcut-recording {{
    color: {bright_yellow};
    border: 1px solid {accent};
    font-style: italic;
    font-weight: 600;
}}

/* ================= Harness Settings Card ================= */
/* The only overlay card: it holds the settings page (rows read
   `[logo] name …… resolved command [ON/OFF]`) and, on navigation, the 📱
   launcher page. `.harness-pages` / `.harness-page` are markers for which
   child of the card is showing; they need no rules of their own. */
.harness-panel {{
    border: 1px solid {hud_border};
}}

.harness-rows {{
    margin-top: 2px;
}}

.harness-row {{
    padding: 4px 0;
}}

.harness-icon {{
    font-size: 13px;
}}

.harness-name {{
    color: {foreground};
    font-size: 12px;
    font-weight: 700;
}}

.harness-cmd {{
    color: {dark_foreground};
    font-family: '{font_family}', monospace;
    font-size: 10.5px;
}}

.harness-retired {{
    color: {dark_foreground};
    border: 1px solid {btn_border};
    border-radius: 9999px;
    padding: 0 6px;
    font-size: 10px;
}}

/* The line under OpenClaw's row: its status/title plugin, and the setup. */
.harness-plugin-notice {{
    border-left: 2px solid {btn_border};
    margin: 0 0 4px 24px;
    padding: 2px 0 2px 10px;
}}

.harness-plugin-text {{
    color: {light_foreground};
    font-size: 10.5px;
}}

.harness-plugin-connected {{
    background-color: {status_active_bg};
    color: {bright_green};
    border: 1px solid {status_active_border};
    border-radius: 9999px;
    padding: 1px 8px;
    font-size: 10px;
    font-weight: 700;
}}

.harness-toggle {{
    background-color: {btn_bg};
    color: {light_foreground};
    border: 1px solid {btn_border};
    border-radius: 9999px;
    padding: 3px 12px;
    min-width: 52px;
    font-size: 10.5px;
    font-weight: 800;
    letter-spacing: 0.6px;
    transition: background-color 150ms ease, border-color 150ms ease;
}}

.harness-toggle:hover {{
    background-color: {btn_hover_bg};
    border-color: {accent};
}}

/* Same chip language as .launcher-online / .launcher-offline */
.harness-toggle-on {{
    background-color: {status_active_bg};
    color: {bright_green};
    border: 1px solid {status_active_border};
}}

.harness-toggle-off {{
    background-color: {status_exited_bg};
    color: {dark_foreground};
    border: 1px solid {status_exited_border};
}}

"#,
        // The Mac overlay uses an opaque native window: a translucent root
        // can leave black/stale regions when terminal surfaces disappear.
        win_bg = theme.rgba_darker_bg(if cfg!(target_os = "macos") { 1.0 } else { 0.72 }),
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
        red_text = theme.readable_text(&theme.background, 1.0, &[&theme.bright_red, &theme.red]),
        danger_bg = OmarchyTheme::hex_to_rgba(&theme.red, 0.20),
        danger_text = theme.readable_text(&theme.red, 0.20, &[&theme.bright_red, &theme.red]),
        danger_hover_text = theme.readable_text(&theme.red, 0.40, &["#ffffff"]),
        primary_text = theme.readable_text(&theme.accent, 0.15, &[&theme.accent]),
        bright_red = theme.bright_red,
        tag_dots = tag_dot_css(),
        danger_border = OmarchyTheme::hex_to_rgba(&theme.red, 0.40),
        note_bg = theme.rgba_dark_bg(0.94),
        note_border = theme.rgba_muted(0.35),
        note_hdr_bg = theme.rgba_lighter_bg(0.85),
        note_hdr_divider = theme.rgba_muted(0.30),
        note_content_bg = theme.rgba_dark_bg(0.90),
        selection = theme.selection,
        term_card_bg = theme.rgba_dark_bg(0.94),
        term_expand_glow = theme.rgba_accent(0.35),
        ghost_bg = theme.rgba_dark_bg(0.60),
        ghost_shadow = theme.rgba_accent(0.55),
        bright_blue = theme.bright_blue,
        ghost_icon_bg = theme.rgba_darker_bg(0.70),
        ghost_icon_shadow = OmarchyTheme::hex_to_rgba(&theme.bright_blue, 0.60),
        font_family = theme.font_family,
        ghost_label_bg = theme.rgba_darker_bg(0.88),
        ghost_label_border = theme.rgba_accent(0.85),
        ghost_icon_border = OmarchyTheme::hex_to_rgba(&theme.bright_blue, 0.85),
        overlap_ghost_border = theme.accent,
        overlap_ghost_glow = theme.rgba_accent(0.35),
        compact_bg = theme.rgba_dark_bg(0.94),
        compact_actions_bg = theme.rgba_lighter_bg(0.85),
        darker_background = theme.darker_background,
        agent_blue = OmarchyTheme::hex_to_rgba(&theme.blue, 0.70),
        agent_orange = OmarchyTheme::hex_to_rgba(&theme.orange, 0.70),
        agent_green = OmarchyTheme::hex_to_rgba(&theme.green, 0.70),
        agent_cyan = OmarchyTheme::hex_to_rgba(&theme.cyan, 0.70),
        agent_magenta = OmarchyTheme::hex_to_rgba(&theme.magenta, 0.70),
        agent_bright_magenta = OmarchyTheme::hex_to_rgba(&theme.bright_magenta, 0.70),
        agent_bright_blue = OmarchyTheme::hex_to_rgba(&theme.bright_blue, 0.70),
        agent_shell = theme.rgba_muted(0.55),
        launcher_section_bg = theme.rgba_darker_bg(0.35),
        launcher_section_border = theme.rgba_muted(0.25),
        launcher_pin_bg = theme.rgba_darker_bg(0.55),
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
        usage_ok = theme.bright_green,
        usage_warn = theme.bright_yellow,
        usage_crit = theme.bright_red,
        usage_trough = theme.rgba_muted(0.25),
    )
}

/// Register the vendored `assets/icons` tree with the display's icon theme so
/// `Button::from_icon_name("sd-*-symbolic")` resolves (HUD gear / arrange).
fn register_app_icons() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    if let Some(root) = crate::brand::find_icons_root() {
        gtk4::IconTheme::for_display(&display).add_search_path(root);
    }
}

pub fn apply_styles() {
    register_app_icons();
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
        match_gtk_variant(&theme);
        let css = generate_css(&theme);
        provider.load_from_string(&css);
    });
}

pub fn reload_styles() -> OmarchyTheme {
    let theme = reload_theme();
    match_gtk_variant(&theme);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn workspace_background_matches_native_compositing_mode() {
        // GTK and AppKit must agree about whether the root can expose the desktop.
        for mode in ["dark", "light"] {
            let mut theme = OmarchyTheme::default();
            theme.mode = mode.into();
            let css = generate_css(&theme);
            let root = css.split("window.super-desktop-window {").nth(1).unwrap()
                .split('}').next().unwrap();
            let alpha = if cfg!(target_os = "macos") { 1.0 } else { 0.72 };
            assert!(root.contains(&theme.rgba_darker_bg(alpha)));
        }
    }

    #[test]
    fn test_generated_css_parses_without_errors() {
        // GTK is single-threaded, so the parse assertions run in their own
        // process (see `crate::gtk_test`).
        crate::gtk_test::run_in_child_process("styles::tests::css_gtk_parses_cleanly");
    }

    #[test]
    fn css_gtk_parses_cleanly() {
        if !crate::gtk_test::is_child() {
            return;
        }
        // A single bad property silently drops its rule at runtime, which is
        // how the launcher page would quietly lose its chrome. Parse the
        // generated stylesheet and fail on any CSS parser complaint.
        let _ = gtk4::init();
        let css = generate_css(&current_theme());
        let provider = CssProvider::new();
        let errors: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        {
            let errors = Rc::clone(&errors);
            provider.connect_parsing_error(move |_, _section, err| {
                errors.borrow_mut().push(err.to_string());
            });
        }
        provider.load_from_string(&css);

        let errors = errors.borrow();
        assert!(
            errors.is_empty(),
            "generated CSS must parse cleanly, got: {errors:#?}"
        );
        drop(errors);

        // Sanity check the wiring: a genuinely broken rule must be reported,
        // otherwise this test would pass no matter what the stylesheet says.
        let bogus = CssProvider::new();
        let caught: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        {
            let caught = Rc::clone(&caught);
            bogus.connect_parsing_error(move |_, _, _| *caught.borrow_mut() += 1);
        }
        bogus.load_from_string(".harness-panel { border-radius: not-a-length; }");
        assert!(
            *caught.borrow() > 0,
            "CSS parsing-error hook is not wired up"
        );
    }

    #[test]
    fn test_launcher_page_styles_exist() {
        // The launcher page of the ⚙ card (`launcher_settings`): a typo here
        // turns a section into unstyled text, and the card chrome comes from
        // `.harness-panel`.
        let css = generate_css(&current_theme());
        for class in [
            "launcher-scroll",
            "launcher-section",
            "launcher-section-num",
            "launcher-section-title",
            "launcher-key",
            "launcher-value",
            "launcher-btn",
            "launcher-online",
            "launcher-offline",
            "launcher-note",
            "launcher-note-error",
        ] {
            assert!(
                css.contains(&format!(".{class}")),
                "missing CSS rule for .{class}"
            );
        }
    }

    #[test]
    fn test_harness_settings_panel_styles_exist() {
        // The ⚙ card is built from the shared `.launcher-section` chrome plus
        // these; a typo in either turns a row into unstyled text.
        let css = generate_css(&current_theme());
        for class in [
            "harness-panel",
            "settings-entry",
            "settings-firewall-warning",
            "harness-rows",
            "harness-row",
            "harness-name",
            "harness-cmd",
            "harness-toggle",
            "harness-toggle-on",
            "harness-toggle-off",
            "harness-plugin-notice",
            "harness-plugin-text",
            "harness-plugin-connected",
            // Connections pages, the invitation flow and the approval panel.
            "pairing-panel",
            "pairing-device-name",
            "pairing-result-mark",
            "invite-checklist",
            "invite-step-mark",
            "invite-ok",
            "invite-warn",
            "invite-fail",
            "invite-card",
            "connections-pending",
            "connections-warning-chip",
        ] {
            assert!(
                css.contains(&format!(".{class}")),
                "missing CSS rule for .{class}"
            );
        }
    }

    #[test]
    fn test_update_styles_exist() {
        let css = generate_css(&current_theme());
        for rule in [".update-changes {", ".term-status-badge.update-available {"] {
            assert!(css.contains(rule), "missing CSS rule {rule}");
        }
    }

    #[test]
    fn test_jump_to_newest_style_exists() {
        let css = generate_css(&current_theme());
        assert!(css.contains("button.term-jump-newest {"), "missing CSS rule for .term-jump-newest");
    }

    #[test]
    fn test_remote_view_styles_exist() {
        // The outline of that PC's screen, and the panel it shows while it
        // is connecting or unreachable (its checklist rows are the
        // invitation's, already covered above).
        let css = generate_css(&current_theme());
        for class in [
            "remote-canvas-border",
            "remote-status",
            "remote-status-mark",
            "remote-status-title",
            "remote-status-hint",
            "remote-status-checks",
            "remote-status-footer",
        ] {
            assert!(
                css.contains(&format!(".{class}")),
                "missing CSS rule for .{class}"
            );
        }
    }

    #[test]
    fn test_workspace_field_styles_exist() {
        // The top bar's folder field + its ▾ history; an unstyled entry would
        // paint with the bare GTK theme colours on top of the HUD pill.
        let css = generate_css(&current_theme());
        for class in [
            "ws-bar",
            "ws-subtitle",
            "ws-icon",
            "ws-entry",
            "ws-entry-invalid",
            "ws-menu-btn",
            "ws-pop",
            "ws-pop-box",
            "ws-row-pick",
            "ws-row-name",
            "ws-row-path",
            "ws-row-mark",
            "ws-row-active",
            "ws-row-selected",
            "ws-del",
            "sliding",
            "ws-empty",
        ] {
            assert!(
                css.contains(&format!(".{class}")),
                "missing CSS rule for .{class}"
            );
        }
    }

    #[test]
    fn test_hot_corner_styles_exist() {
        // The corner surface is an input area, not a visual one: without this
        // rule GTK paints the window background and the user gets a stray
        // rectangle in the corner of the screen.
        let css = generate_css(&current_theme());
        assert!(
            css.contains("window.sd-hot-corner"),
            "missing the transparent hot-corner rule"
        );
        // 1/255 of black, NOT `transparent`: a fully transparent layer surface
        // is click-through in GTK and loses both the pointer and its size
        // (see the comment on the rule).
        assert!(
            css.contains("background-color: rgba(0, 0, 0, 0.004)"),
            "the hot corner must stay (just about) invisible, got:\n{css}"
        );
        assert!(
            !css.contains("window.sd-hot-corner {{\n    background-color: transparent"),
            "a fully transparent hot corner receives no pointer events"
        );
    }

    #[test]
    fn test_shortcut_recorder_styles_exist() {
        // Section 1 of the ⚙ panel: a boxed combination plus its armed state.
        // Unstyled, the recorder looks like a stray line of text.
        let css = generate_css(&current_theme());
        for class in ["shortcut-row", "shortcut-combo", "shortcut-recording"] {
            assert!(
                css.contains(&format!(".{class}")),
                "missing CSS rule for .{class}"
            );
        }
    }

    #[test]
    fn test_hud_gear_style_exists() {
        // The ⚙ settings / arrange / Hide toggles are icon-only symbolic SVGs:
        // without this rule they render as empty pills, not a sized icon.
        let css = generate_css(&current_theme());
        assert!(css.contains(".hud-gear"), "missing CSS rule for .hud-gear");
        assert!(
            css.contains(".hud-icon-btn"),
            "missing CSS rule for .hud-icon-btn"
        );
        assert!(
            css.contains(".hud-button-danger"),
            "missing CSS rule for .hud-button-danger"
        );
    }

    #[test]
    fn test_overlap_ghost_styles_exist() {
        // The dotted outline over a terminal that another card hides. Without
        // the rule the ghost box is invisible (or a solid rectangle painted
        // over the card that covers the terminal).
        let css = generate_css(&current_theme());
        assert!(
            css.contains(".term-overlap-ghost"),
            "missing CSS rule for .term-overlap-ghost"
        );
        assert!(
            css.contains("border: 2px dotted"),
            "the buried-card ghost must be a dotted outline, got:\n{css}"
        );
    }

    #[test]
    fn test_command_notice_styles_exist() {
        // The card's command feedback and the remote bar's note use these; an
        // unstyled tone would read as ordinary preview text.
        let css = generate_css(&current_theme());
        for class in [".term-notice ", ".term-notice-info", ".term-notice-warning", ".term-notice-error"] {
            assert!(css.contains(class), "missing CSS rule for {class}");
        }
        for class in crate::command_feedback::TONE_CLASSES {
            assert!(css.contains(&format!(".{class}")), "missing CSS rule for {class}");
        }
    }

    #[test]
    fn button_labels_are_readable_in_every_omarchy_theme() {
        crate::gtk_test::run_in_child_process("styles::tests::button_contrast_inner");
    }

    /// WCAG relative luminance of an sRGB colour (0..=1 channels).
    fn luminance([r, g, b]: [f64; 3]) -> f64 {
        let lin = |c: f64| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    fn contrast(a: [f64; 3], b: [f64; 3]) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// Every installed Omarchy theme (and the built-in palette), with GTK's
    /// own theme in the variant the app picks for it and in the other one
    /// (a desktop without a settings portal, or a theme switch in flight):
    /// each button's label must stand out from the button it is drawn on.
    #[test]
    fn button_contrast_inner() {
        use gtk4::prelude::*;
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let mut themes = vec![("built-in".to_string(), OmarchyTheme::default())];
        if let Ok(dirs) = std::fs::read_dir("/usr/share/omarchy/themes") {
            let mut dirs: Vec<_> = dirs.filter_map(Result::ok).map(|d| d.path()).collect();
            dirs.sort();
            for dir in dirs {
                if let Ok(colors) = std::fs::read_to_string(dir.join("colors.toml")) {
                    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
                    themes.push((name, crate::theme::from_colors_toml(&colors)));
                }
            }
        }
        // (panel classes, button classes)
        let cases: &[(&[&str], &[&str])] = &[
            (&["mini-terminal", "harness-panel"], &["launcher-btn"]),
            (&["mini-terminal", "harness-panel"], &["launcher-btn", "launcher-btn-primary"]),
            (&["mini-terminal", "harness-panel"], &["launcher-btn", "launcher-btn-danger"]),
            (&["mini-terminal", "harness-panel"], &["launcher-btn", "tour-next"]),
            (&["mini-terminal", "harness-panel"], &["launcher-btn", "tour-demo"]),
            (&["mini-terminal", "harness-panel"], &["term-btn"]),
            (&["mini-terminal", "harness-panel", "asset-drawer"], &[]),
            (&["mini-terminal", "harness-panel", "asset-drawer"], &["attach-browse-row"]),
            (&[], &["hud-button"]),
            (&[], &["hud-button", "hud-button-danger"]),
        ];
        let provider = CssProvider::new();
        let display = gdk::Display::default().unwrap();
        style_context_add_provider_for_display(&display, &provider, STYLE_PROVIDER_PRIORITY_APPLICATION);
        let settings = gtk4::Settings::default().unwrap();
        let shots = std::env::var_os("SD_CONTRAST_SHOTS").map(std::path::PathBuf::from);
        let renderer = gtk4::gsk::CairoRenderer::new();
        renderer.realize(None::<&gdk::Surface>).unwrap();
        let mut failures = Vec::new();
        for (name, theme) in &themes {
            let backdrop = gdk::RGBA::parse(theme.background.as_str()).ok()
                .map(|c| [f64::from(c.red()), f64::from(c.green()), f64::from(c.blue())])
                .unwrap();
            let matched = theme.mode != "light";
            for dark in [matched, !matched] {
                provider.load_from_string(&generate_css(theme));
                settings.set_gtk_application_prefer_dark_theme(dark);
                // Fresh widgets for every palette: nothing styled earlier
                // is measured.
                let window = gtk4::Window::new();
                let column = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
                let mut buttons = Vec::new();
                for (panel_classes, button_classes) in cases {
                    let panel = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
                    for class in *panel_classes {
                        panel.add_css_class(class);
                    }
                    let button = gtk4::Button::with_label("Skip tour");
                    for class in *button_classes {
                        button.add_css_class(class);
                    }
                    button.set_halign(gtk4::Align::Start);
                    button.set_margin_start(12);
                    panel.append(&button);
                    column.append(&panel);
                    buttons.push((format!("{panel_classes:?} {button_classes:?}"), button));
                }
                window.set_child(Some(&column));
                window.present();
                let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while column.width() == 0 {
                    assert!(std::time::Instant::now() < until, "never laid out");
                    crate::gtk_test::pump(20);
                }
                crate::gtk_test::pump(100);
                let paintable = gtk4::WidgetPaintable::new(Some(&column));
                let snapshot = gtk4::Snapshot::new();
                paintable.snapshot(&snapshot, f64::from(column.width()), f64::from(column.height()));
                let texture = renderer.render_texture(&snapshot.to_node().unwrap(), None);
                let (w, h) = (texture.width() as usize, texture.height() as usize);
                let mut bytes = vec![0u8; w * h * 4];
                texture.download(&mut bytes, w * 4);
                // Premultiplied BGRA over the theme's background.
                let pixel = |x: usize, y: usize| -> [f64; 3] {
                    let p = &bytes[(y * w + x) * 4..][..4];
                    let a = f64::from(p[3]) / 255.0;
                    let rgb = [p[2], p[1], p[0]];
                    std::array::from_fn(|i| f64::from(rgb[i]) / 255.0 + backdrop[i] * (1.0 - a))
                };
                let variant = if dark { "dark" } else { "light" };
                let before = failures.len();
                for (case, button) in &buttons {
                    let bounds = button.compute_bounds(&column).unwrap();
                    let (x0, y0) = (bounds.x() as usize + 3, bounds.y() as usize + 3);
                    let x1 = ((bounds.x() + bounds.width()) as usize).min(w) - 3;
                    let y1 = ((bounds.y() + bounds.height()) as usize).min(h) - 3;
                    // The button's fill is its most common colour; the label
                    // is the pixel that differs from it most.
                    let mut counts = std::collections::HashMap::new();
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let c = pixel(x, y).map(|v| (v * 255.0).round() as u8);
                            *counts.entry(c).or_insert(0) += 1;
                        }
                    }
                    let fill = counts.into_iter().max_by_key(|(_, n)| *n).unwrap().0.map(|v| f64::from(v) / 255.0);
                    let mut best = 1.0f64;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            best = best.max(contrast(fill, pixel(x, y)));
                        }
                    }
                    if best < 3.0 {
                        failures.push(format!("{name} (GTK {variant}): {case} label contrast {best:.2}"));
                    }
                }
                if let Some(dir) = shots.as_ref().filter(|_| failures.len() > before) {
                    std::fs::create_dir_all(dir).unwrap();
                    texture.save_to_png(dir.join(format!("{name}-{variant}.png"))).unwrap();
                }
                window.destroy();
            }
        }
        renderer.unrealize();
        assert!(failures.is_empty(), "{} unreadable buttons:\n{}", failures.len(), failures.join("\n"));
    }
}
