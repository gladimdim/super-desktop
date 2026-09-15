use gtk4::{gdk, style_context_add_provider_for_display, CssProvider, STYLE_PROVIDER_PRIORITY_APPLICATION};

pub const APP_CSS: &str = r#"
/* ================= Base Window & Backdrop ================= */
window.super-desktop-window {
    background-color: rgba(10, 14, 24, 0.72);
}

/* ================= Top Floating HUD Bar ================= */
.hud-bar {
    background-color: rgba(20, 26, 43, 0.92);
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 9999px;
    padding: 6px 16px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.5), 0 0 1px rgba(255, 255, 255, 0.2);
}

.hud-title {
    color: #38BDF8;
    font-weight: 800;
    font-size: 13px;
    letter-spacing: 0.5px;
}

.hud-badge {
    background-color: rgba(56, 189, 248, 0.15);
    color: #7DD3FC;
    border-radius: 9999px;
    padding: 3px 9px;
    font-size: 11px;
    font-weight: 600;
}

.hud-button {
    background-color: rgba(255, 255, 255, 0.08);
    color: #E2E8F0;
    border: 1px solid rgba(255, 255, 255, 0.10);
    border-radius: 9999px;
    padding: 5px 12px;
    font-size: 12px;
    font-weight: 600;
    transition: all 180ms ease;
}

.hud-button:hover {
    background-color: rgba(255, 255, 255, 0.18);
    color: #FFFFFF;
    border-color: rgba(255, 255, 255, 0.3);
}

.hud-button:active {
    background-color: rgba(56, 189, 248, 0.25);
    border-color: #38BDF8;
}

.hud-button-danger {
    background-color: rgba(239, 68, 68, 0.18);
    color: #FCA5A5;
    border-color: rgba(239, 68, 68, 0.3);
}

.hud-button-danger:hover {
    background-color: rgba(239, 68, 68, 0.35);
    color: #FFFFFF;
}

.hud-shortcut {
    color: #94A3B8;
    font-size: 11px;
    font-weight: 500;
}

/* ================= Sticky Notes ================= */
.sticky-note {
    border-radius: 14px;
    box-shadow: 0 10px 28px rgba(0, 0, 0, 0.4), 0 2px 6px rgba(0, 0, 0, 0.25);
    transition: box-shadow 200ms ease;
}

.sticky-note:focus-within {
    box-shadow: 0 14px 38px rgba(0, 0, 0, 0.5), 0 0 0 2px rgba(255, 255, 255, 0.25);
}

.note-header {
    padding: 7px 10px 6px 12px;
    border-top-left-radius: 14px;
    border-top-right-radius: 14px;
}

.note-header-grip {
    font-size: 13px;
    opacity: 0.65;
}

.note-header-btn {
    background: transparent;
    border: none;
    padding: 2px 6px;
    border-radius: 6px;
    opacity: 0.75;
    transition: opacity 150ms ease, background-color 150ms ease;
}

.note-header-btn:hover {
    opacity: 1.0;
    background-color: rgba(0, 0, 0, 0.12);
}

.note-content-area {
    padding: 8px 12px 12px 12px;
    border-bottom-left-radius: 14px;
    border-bottom-right-radius: 14px;
}

.note-textview text {
    background: transparent;
    font-size: 13px;
    line-height: 1.4;
}

/* Colors */
.note-yellow { background-color: #FEF08A; color: #1E293B; }
.note-yellow .note-header { background-color: #FDE047; border-bottom: 1px solid rgba(202, 138, 4, 0.25); }
.note-yellow text { color: #1E293B; }

.note-mint { background-color: #BBF7D0; color: #064E3B; }
.note-mint .note-header { background-color: #86EFAC; border-bottom: 1px solid rgba(22, 163, 74, 0.25); }
.note-mint text { color: #064E3B; }

.note-sky { background-color: #BAE6FD; color: #0C4A6E; }
.note-sky .note-header { background-color: #7DD3FC; border-bottom: 1px solid rgba(2, 132, 199, 0.25); }
.note-sky text { color: #0C4A6E; }

.note-rose { background-color: #FBCFE8; color: #831843; }
.note-rose .note-header { background-color: #F472B6; border-bottom: 1px solid rgba(219, 39, 119, 0.25); }
.note-rose text { color: #831843; }

.note-purple { background-color: #E9D5FF; color: #581C87; }
.note-purple .note-header { background-color: #D8B4FE; border-bottom: 1px solid rgba(124, 58, 237, 0.25); }
.note-purple text { color: #581C87; }

.note-dark { background-color: rgba(26, 31, 48, 0.95); color: #F1F5F9; border: 1px solid rgba(255, 255, 255, 0.12); }
.note-dark .note-header { background-color: rgba(36, 43, 66, 0.95); border-bottom: 1px solid rgba(255, 255, 255, 0.08); }
.note-dark text { color: #F1F5F9; }
.note-dark .note-header-btn:hover { background-color: rgba(255, 255, 255, 0.15); }

/* ================= Mini Terminal Cards ================= */
.mini-terminal {
    background-color: rgba(18, 24, 38, 0.94);
    border-radius: 14px;
    padding: 0;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.55), 0 0 1px rgba(255, 255, 255, 0.18);
    transition: transform 180ms ease, box-shadow 180ms ease;
}

.mini-terminal:hover {
    box-shadow: 0 14px 38px rgba(0, 0, 0, 0.65), 0 0 14px rgba(56, 189, 248, 0.25);
}

.agent-card-antigravity { border: 1.5px solid rgba(99, 102, 241, 0.7); }
.agent-card-claude { border: 1.5px solid rgba(249, 115, 22, 0.7); }
.agent-card-codex { border: 1.5px solid rgba(16, 185, 129, 0.7); }
.agent-card-opencode { border: 1.5px solid rgba(6, 182, 212, 0.7); }
.agent-card-grok { border: 1.5px solid rgba(236, 72, 153, 0.7); }
.agent-card-aider { border: 1.5px solid rgba(139, 92, 246, 0.7); }
.agent-card-shell { border: 1.5px solid rgba(148, 163, 184, 0.4); }

.term-header {
    background-color: rgba(26, 33, 52, 0.85);
    border-top-left-radius: 13px;
    border-top-right-radius: 13px;
    padding: 7px 10px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

.term-title {
    color: #F8FAFC;
    font-weight: 700;
    font-size: 12px;
}

.term-status-badge {
    border-radius: 9999px;
    padding: 2px 7px;
    font-size: 10px;
    font-weight: 600;
}

.status-active {
    background-color: rgba(34, 197, 94, 0.18);
    color: #4ADE80;
    border: 1px solid rgba(34, 197, 94, 0.35);
}

.status-busy {
    background-color: rgba(250, 204, 21, 0.18);
    color: #FDE047;
    border: 1px solid rgba(250, 204, 21, 0.35);
}

.status-exited {
    background-color: rgba(148, 163, 184, 0.15);
    color: #94A3B8;
    border: 1px solid rgba(148, 163, 184, 0.25);
}

.term-btn {
    background: transparent;
    border: none;
    color: #94A3B8;
    padding: 3px 6px;
    border-radius: 6px;
    font-size: 11px;
    transition: all 150ms ease;
}

.term-btn:hover {
    color: #FFFFFF;
    background-color: rgba(255, 255, 255, 0.12);
}

.term-preview-box {
    background-color: #0B0E17;
    margin: 8px;
    padding: 8px;
    border-radius: 8px;
    border: 1px solid rgba(255, 255, 255, 0.06);
}

.term-preview-text {
    font-family: 'JetBrainsMono Nerd Font', 'Fira Code', 'DejaVu Sans Mono', monospace;
    font-size: 9.5px;
    line-height: 1.35;
    color: #38BDF8;
}

.term-footer {
    padding: 2px 10px 8px 10px;
}

.term-meta {
    color: #64748B;
    font-size: 10px;
    font-weight: 500;
}

.term-hint {
    color: #94A3B8;
    font-size: 9.5px;
    font-style: italic;
    opacity: 0.8;
}
"#;

pub fn apply_styles() {
    let provider = CssProvider::new();
    provider.load_from_data(APP_CSS);
    if let Some(display) = gdk::Display::default() {
        style_context_add_provider_for_display(
            &display,
            &provider,
            STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
