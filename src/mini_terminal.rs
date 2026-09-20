use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Align, Button, EventControllerFocus, GestureClick, GestureDrag, Label, Orientation, Overlay};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use vte4::prelude::*;
use vte4::{PtyFlags, Terminal as VteTerminal};

use crate::state::TerminalData;
use crate::tmux::{
    capture_pane_text, ensure_session_with_inventory, extract_composer_draft,
    extract_last_prompt, get_agent_config, get_opencode_user_text_by_id,
    inspect_status_with_screen, preview_from_screen, resolve_own_opencode_id,
    tmux_bin, truncate_prompt_title,
};

pub const CARD_WIDTH: i32 = 380;
pub const CARD_HEIGHT: i32 = 240;
pub const NEW_TERM_WIDTH: i32 = 640;
pub const NEW_TERM_HEIGHT: i32 = 480;
pub const MIN_CARD_WIDTH: i32 = 320;
pub const MIN_CARD_HEIGHT: i32 = 180;
pub const ICON_SIZE: i32 = 128;
pub const EXPAND_RATIO: f64 = 0.80;

pub fn expanded_rect(screen_w: i32, screen_h: i32) -> (f64, f64, f64, f64) {
    let w = (screen_w as f64 * EXPAND_RATIO).round();
    let h = (screen_h as f64 * EXPAND_RATIO).round();
    let x = ((screen_w as f64 - w) / 2.0).round();
    let y = ((screen_h as f64 - h) / 2.0).round();
    (x, y, w, h)
}

pub fn clamp_card_size(w: i32, h: i32, screen_w: i32, screen_h: i32) -> (i32, i32) {
    let max_w = ((screen_w as f64) * 0.70).round() as i32;
    let max_h = ((screen_h as f64) * 0.75).round() as i32;
    (
        w.clamp(MIN_CARD_WIDTH, max_w.max(MIN_CARD_WIDTH)),
        h.clamp(MIN_CARD_HEIGHT, max_h.max(MIN_CARD_HEIGHT)),
    )
}

/// Each card remembers two independent spots: the expanded card position
/// (`x`/`y`) and the iconified position (`icon_x`/`icon_y`). This returns the
/// spot the widget should occupy right now, so minimizing/restoring can jump
/// between them without either one clobbering the other.
pub fn displayed_pos(data: &TerminalData) -> (f64, f64) {
    if data.iconified {
        (
            data.icon_x.unwrap_or(data.x) as f64,
            data.icon_y.unwrap_or(data.y) as f64,
        )
    } else {
        (data.x as f64, data.y as f64)
    }
}

/// Records a card position in the slot matching its current mode, so moving an
/// icon can never overwrite the expanded card position (and vice versa).
pub fn set_displayed_pos(data: &mut TerminalData, x: i32, y: i32) {
    if data.iconified {
        data.icon_x = Some(x);
        data.icon_y = Some(y);
    } else {
        data.x = x;
        data.y = y;
    }
}

/// A card that was never moved as an icon seeds its icon spot from the card
/// position on the first minimize, so the icon does not jump; after that the
/// saved icon spot is where every minimize returns to.
fn seed_icon_pos(data: &mut TerminalData) {
    if data.icon_x.is_none() {
        data.icon_x = Some(data.x);
    }
    if data.icon_y.is_none() {
        data.icon_y = Some(data.y);
    }
}

pub struct MiniTerminalCard {
    pub container: Overlay,
    pub data: Rc<RefCell<TerminalData>>,
    expanded: Rc<RefCell<bool>>,
    screen_w: i32,
    _screen_h: i32,
    header: gtk4::Box,
    footer: gtk4::Box,
    title_label: Label,
    title_prefix: String,
    /// Cached opencode session id for the USER-text DB lookup (None = unresolved yet).
    opencode_session: Rc<RefCell<Option<String>>>,
    /// Next time the cached id may be re-resolved against live tmux/DB state.
    /// A guess made before a neighbouring console closed otherwise sticks
    /// forever and keeps showing that console's prompt in this card's title.
    oc_recheck_at: Rc<RefCell<std::time::Instant>>,
    status_badge: Label,
    refresh_in_flight: Rc<Cell<bool>>,
    compact_status: Label,
    preview_label: Label,
    icon_box: gtk4::Box,
    _icon_label: Label,
    _icon_name_label: Label,
    meta_label: Label,
    hint_label: Label,
    _iconify_btn: Button,
    restore_btn: Button,
    expand_btn: Button,
    compact_restore_btn: Button,
    _compact_kill_btn: Button,
    compact_top_bar: gtk4::Box,
    preview_box: gtk4::Box,
    vte: Rc<RefCell<Option<VteTerminal>>>,
    session_task: Arc<crate::session_task::SessionTask>,
    visual_pos: Rc<RefCell<(f64, f64)>>,
    on_toggle: Rc<dyn Fn(&TerminalData)>,
    on_session_persist: Rc<dyn Fn(&TerminalData)>,
}

impl MiniTerminalCard {
    pub fn new<FDragUpdate, FDragEnd, FToggle, FClose, FResizeGhost, FResizeEnd, FRaise, FSessionSave>(
        mut term_data: TerminalData,
        on_drag_update: FDragUpdate,
        on_drag_end: FDragEnd,
        on_toggle: FToggle,
        on_close: FClose,
        on_resize_ghost: FResizeGhost,
        on_resize_end: FResizeEnd,
        on_raise: FRaise,
        on_session_persist: FSessionSave,
        screen_w: i32,
        screen_h: i32,
        startup_inventory: Option<Arc<crate::tmux::SessionInventory>>,
    ) -> Self
    where
        FDragUpdate: Fn(gtk4::Widget, f64, f64) + 'static,
        FDragEnd: Fn(gtk4::Widget, &TerminalData) + 'static,
        FToggle: Fn(&TerminalData) + 'static,
        FClose: Fn(String) + 'static,
        FResizeGhost: Fn(f64, f64, i32, i32, bool) + 'static,
        FResizeEnd: Fn() + 'static,
        FRaise: Fn(gtk4::Widget) + 'static,
        FSessionSave: Fn(&TerminalData) + 'static,
    {
        if term_data.iconified {
            term_data.width = ICON_SIZE;
            term_data.height = ICON_SIZE;
        } else {
            let (cw, ch) = clamp_card_size(term_data.width, term_data.height, screen_w, screen_h);
            term_data.width = cw;
            term_data.height = ch;
        }

        if term_data.restored_width < MIN_CARD_WIDTH || term_data.restored_height < MIN_CARD_HEIGHT {
            term_data.restored_width = CARD_WIDTH;
            term_data.restored_height = CARD_HEIGHT;
        }

        let initial_w = term_data.width;
        let initial_h = term_data.height;

        let data = Rc::new(RefCell::new(term_data));
        let expanded = Rc::new(RefCell::new(false));
        let vte = Rc::new(RefCell::new(None));
        let on_toggle: Rc<dyn Fn(&TerminalData)> = Rc::new(on_toggle);
        let on_close = Rc::new(on_close);
        let on_drag_update = Rc::new(on_drag_update);
        let on_drag_end = Rc::new(on_drag_end);
        let on_resize_ghost = Rc::new(on_resize_ghost);
        let on_resize_end = Rc::new(on_resize_end);
        let on_raise_rc = Rc::new(on_raise);
        let visual_pos = Rc::new(RefCell::new(displayed_pos(&data.borrow())));

        let root = Overlay::new();

        // Raise on click anywhere on container
        let click_raise = GestureClick::new();
        click_raise.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let container_weak_click = root.downgrade();
        let on_raise_click = Rc::clone(&on_raise_rc);
        click_raise.connect_pressed(move |_, _, _, _| {
            if let Some(c) = container_weak_click.upgrade() {
                on_raise_click(c.upcast());
            }
        });
        root.add_controller(click_raise);

        // Raise when focus enters container or any child widget (like VTE terminal)
        let focus_raise = EventControllerFocus::new();
        let container_weak_focus = root.downgrade();
        let on_raise_focus = Rc::clone(&on_raise_rc);
        focus_raise.connect_enter(move |_| {
            if let Some(c) = container_weak_focus.upgrade() {
                on_raise_focus(c.upcast());
            }
        });
        root.add_controller(focus_raise);

        let body = gtk4::Box::new(Orientation::Vertical, 0);

        let agent_type = data.borrow().agent_type.clone();
        let cfg = get_agent_config(&agent_type);

        root.set_size_request(initial_w, initial_h);
        root.add_css_class("mini-terminal");
        root.add_css_class(&format!("agent-card-{}", agent_type));

        // Header for normal card
        let header = gtk4::Box::new(Orientation::Horizontal, 6);
        header.add_css_class("term-header");

        let grip = Label::new(Some("⋮⋮"));
        grip.add_css_class("note-header-grip");
        header.append(&grip);

        // Group color tag dots (header + icon bar, kept in sync).
        // Click a dot -> 8-color picker popover, no text.
        let tag_sync: Rc<RefCell<Vec<glib::WeakRef<Button>>>> =
            Rc::new(RefCell::new(Vec::new()));
        let tag_data = Rc::clone(&data);
        let tag_root = root.downgrade();
        let tag_save = Rc::clone(&on_drag_end);
        let tag_sync_h = Rc::clone(&tag_sync);
        let header_tag = crate::tag::make_tag_dot(data.borrow().tag, move |next| {
            tag_data.borrow_mut().tag = next;
            for w in tag_sync_h.borrow().iter() {
                if let Some(b) = w.upgrade() {
                    crate::tag::apply_tag(&b, next);
                }
            }
            if let Some(r) = tag_root.upgrade() {
                tag_save(r.upcast(), &tag_data.borrow());
            }
        });
        tag_sync.borrow_mut().push(header_tag.downgrade());
        header.append(&header_tag);

        let title = Label::new(Some(&format!("{} {}", cfg.icon, cfg.name)));
        title.add_css_class("term-title");
        title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        title.set_single_line_mode(true);
        // Take every spare pixel up to the status badge; overflow ellipsizes.
        title.set_hexpand(true);
        title.set_halign(Align::Start);
        header.append(&title);

        let status_badge = Label::new(Some("● IDLE"));
        status_badge.add_css_class("term-status-badge");
        status_badge.add_css_class("status-idle");
        status_badge.set_halign(Align::End);
        header.append(&status_badge);
        header.append(&crate::asset_view::button(data.borrow().session_name.clone()));

        // Iconify button: iconifies the window into 128x128 size
        let iconify_btn = Button::with_label("🗕");
        iconify_btn.set_tooltip_text(Some("Iconify to 128×128"));
        iconify_btn.add_css_class("term-btn");
        header.append(&iconify_btn);

        // Restore / Expand larger size button
        let restore_btn = Button::with_label("🗖");
        restore_btn.set_tooltip_text(Some(&format!(
            "Restore larger size ({}×{})",
            data.borrow().restored_width,
            data.borrow().restored_height
        )));
        restore_btn.add_css_class("term-btn");
        header.append(&restore_btn);

        // Expand to 80% overlay button
        let expand_btn = Button::with_label("⛶");
        expand_btn.set_tooltip_text(Some("Expand to 80% overlay"));
        expand_btn.add_css_class("term-btn");

        let on_toggle_btn = Rc::clone(&on_toggle);
        let data_expand = Rc::clone(&data);
        expand_btn.connect_clicked(move |_| {
            on_toggle_btn(&data_expand.borrow());
        });
        header.append(&expand_btn);

        // Kill session button
        let kill_btn = Button::with_label("✕");
        kill_btn.set_tooltip_text(Some("Kill Session"));
        kill_btn.add_css_class("term-btn");

        let sess_name = data.borrow().session_name.clone();
        let on_close_header = Rc::clone(&on_close);
        kill_btn.connect_clicked(move |_| {
            on_close_header(sess_name.clone());
        });
        header.append(&kill_btn);
        body.append(&header);

        // Preview box
        let preview_box = gtk4::Box::new(Orientation::Vertical, 0);
        preview_box.add_css_class("term-preview-box");
        preview_box.set_vexpand(true);

        let preview_label = Label::new(Some("Connecting to session..."));
        preview_label.set_wrap(true);
        preview_label.set_wrap_mode(gtk4::pango::WrapMode::Char);
        preview_label.set_halign(Align::Start);
        preview_label.set_valign(Align::Start);
        preview_label.set_vexpand(true);
        preview_label.set_hexpand(true);
        preview_label.add_css_class("term-preview-text");
        preview_box.append(&preview_label);

        // Centered icon and agent name for iconified 128x128 mode
        let icon_box = gtk4::Box::new(Orientation::Vertical, 2);
        icon_box.add_css_class("term-icon-box");
        icon_box.set_hexpand(true);
        icon_box.set_vexpand(true);
        icon_box.set_halign(Align::Center);
        icon_box.set_valign(Align::Center);

        let icon_label = Label::new(Some(cfg.icon));
        icon_label.add_css_class("term-agent-icon");
        let attrs = gtk4::pango::AttrList::new();
        attrs.insert(gtk4::pango::AttrSize::new(38 * gtk4::pango::SCALE));
        icon_label.set_attributes(Some(&attrs));
        icon_box.append(&icon_label);

        let icon_name_label = Label::new(Some(cfg.name));
        icon_name_label.add_css_class("term-agent-name");
        icon_box.append(&icon_name_label);

        icon_box.set_visible(false);
        preview_box.append(&icon_box);
        body.append(&preview_box);

        // Footer for normal card
        let footer = gtk4::Box::new(Orientation::Horizontal, 6);
        footer.add_css_class("term-footer");

        let meta_label = Label::new(Some("PID: - • Foot/Tmux"));
        meta_label.add_css_class("term-meta");
        meta_label.set_halign(Align::Start);
        footer.append(&meta_label);

        let hint_label = Label::new(Some("Double-click to expand • drag any edge to resize"));
        hint_label.add_css_class("term-hint");
        hint_label.set_hexpand(true);
        hint_label.set_halign(Align::End);
        footer.append(&hint_label);
        body.append(&footer);

        root.set_child(Some(&body));

        // Compact top bar overlay for 128x128 iconified mode
        let compact_top_bar = gtk4::Box::new(Orientation::Horizontal, 4);
        compact_top_bar.add_css_class("term-compact-top-bar");
        compact_top_bar.set_halign(Align::Fill);
        compact_top_bar.set_valign(Align::Start);

        let compact_status = Label::new(Some("●"));
        compact_status.add_css_class("term-compact-status");
        compact_status.add_css_class("status-idle");
        compact_status.set_halign(Align::Start);
        compact_status.set_valign(Align::Center);
        compact_top_bar.append(&compact_status);

        // Group color tag dot for 128x128 icon mode (synced with header dot)
        let tag_data_c = Rc::clone(&data);
        let tag_root_c = root.downgrade();
        let tag_save_c = Rc::clone(&on_drag_end);
        let tag_sync_c = Rc::clone(&tag_sync);
        let compact_tag = crate::tag::make_tag_dot(data.borrow().tag, move |next| {
            tag_data_c.borrow_mut().tag = next;
            for w in tag_sync_c.borrow().iter() {
                if let Some(b) = w.upgrade() {
                    crate::tag::apply_tag(&b, next);
                }
            }
            if let Some(r) = tag_root_c.upgrade() {
                tag_save_c(r.upcast(), &tag_data_c.borrow());
            }
        });
        tag_sync.borrow_mut().push(compact_tag.downgrade());
        compact_top_bar.append(&compact_tag);

        let top_bar_spacer = gtk4::Box::new(Orientation::Horizontal, 0);
        top_bar_spacer.set_hexpand(true);
        compact_top_bar.append(&top_bar_spacer);

        let compact_actions = gtk4::Box::new(Orientation::Horizontal, 2);
        compact_actions.add_css_class("term-compact-actions");

        // Button to expand the window into its previous "larger" size set by user
        let compact_restore_btn = Button::with_label("🗖");
        compact_restore_btn.set_tooltip_text(Some(&format!(
            "Expand to larger size ({}×{})",
            data.borrow().restored_width,
            data.borrow().restored_height
        )));
        compact_restore_btn.add_css_class("term-btn");
        compact_restore_btn.add_css_class("term-compact-btn");
        compact_actions.append(&compact_restore_btn);

        let compact_kill_btn = Button::with_label("✕");
        compact_kill_btn.set_tooltip_text(Some("Kill Session"));
        compact_kill_btn.add_css_class("term-btn");
        compact_kill_btn.add_css_class("term-compact-btn");
        compact_actions.append(&compact_kill_btn);

        compact_top_bar.append(&compact_actions);
        root.add_overlay(&compact_top_bar);

        let title_prefix = format!("{} {}", cfg.icon, cfg.name);
        // Seed from persisted state so rebooted cards resume the SAME agent
        // session without waiting for the DB mapping to re-resolve.
        let opencode_session = Rc::new(RefCell::new(data.borrow().agent_session_id.clone()));
        let on_session_persist: Rc<dyn Fn(&TerminalData)> = Rc::new(on_session_persist);
        let card = Self {
            session_task: Arc::new(crate::session_task::SessionTask::default()),
            container: root,
            data: Rc::clone(&data),
            expanded: Rc::clone(&expanded),
            screen_w,
            _screen_h: screen_h,
            header,
            footer,
            title_label: title.clone(),
            title_prefix,
            opencode_session,
            // Recheck immediately on the first refresh so a stale persisted
            // guess heals fast, then at most every 30s.
            oc_recheck_at: Rc::new(RefCell::new(std::time::Instant::now())),
            status_badge,
            refresh_in_flight: Rc::new(Cell::new(false)),
            compact_status,
            preview_label,
            icon_box,
            _icon_label: icon_label,
            _icon_name_label: icon_name_label,
            meta_label,
            hint_label,
            _iconify_btn: iconify_btn.clone(),
            restore_btn: restore_btn.clone(),
            expand_btn,
            compact_restore_btn: compact_restore_btn.clone(),
            _compact_kill_btn: compact_kill_btn.clone(),
            compact_top_bar,
            preview_box,
            vte,
            visual_pos,
            on_toggle: Rc::clone(&on_toggle),
            on_session_persist: Rc::clone(&on_session_persist),
        };

        // Hover-focus: entering the card raises it and focuses VTE,
        // exactly like clicking inside the terminal.
        let hover = gtk4::EventControllerMotion::new();
        let container_weak_hover = card.container.downgrade();
        let vte_hover = Rc::clone(&card.vte);
        let on_raise_hover = Rc::clone(&on_raise_rc);
        hover.connect_enter(move |_, _, _| {
            if let Some(c) = container_weak_hover.upgrade() {
                on_raise_hover(c.upcast());
            }
            // Perf: grabbing focus re-triggers :focus-within CSS + cursor
            // redraw, so skip it when the VTE is already focused. During a
            // 120Hz drag across cards this fires constantly.
            if let Some(t) = vte_hover.borrow().as_ref() {
                if !t.has_focus() {
                    t.grab_focus();
                }
            }
        });
        card.container.add_controller(hover);

        // Actions
        let iconify_action: Rc<dyn Fn()> = {
            let expanded = Rc::clone(&expanded);
            let data = Rc::clone(&data);
            let vte = Rc::clone(&card.vte);
            let preview_box = card.preview_box.clone();
            let container = card.container.clone();
            let header = card.header.clone();
            let footer = card.footer.clone();
            let preview_label = card.preview_label.clone();
            let icon_box = card.icon_box.clone();
            let compact_top_bar = card.compact_top_bar.clone();
            let expand_btn = card.expand_btn.clone();
            let hint_label = card.hint_label.clone();
            let compact_restore_btn = card.compact_restore_btn.clone();
            let on_save = Rc::clone(&on_drag_end);
            Rc::new(move || {
                if *expanded.borrow() {
                    *expanded.borrow_mut() = false;
                    container.remove_css_class("term-expanded");
                    expand_btn.set_label("⛶");
                    expand_btn.set_tooltip_text(Some("Expand to 80% overlay"));
                    hint_label.set_label("Double-click to expand • drag any edge to resize");
                }
                remove_vte(&vte, &preview_box);
                {
                    let mut d = data.borrow_mut();
                    if d.width > ICON_SIZE || d.height > ICON_SIZE {
                        d.restored_width = d.width;
                        d.restored_height = d.height;
                    }
                    d.width = ICON_SIZE;
                    d.height = ICON_SIZE;
                    d.iconified = true;
                    // Minimizing returns to the remembered icon spot.
                    seed_icon_pos(&mut d);
                    compact_restore_btn.set_tooltip_text(Some(&format!(
                        "Expand to larger size ({}×{})",
                        d.restored_width, d.restored_height
                    )));
                }
                container.set_size_request(ICON_SIZE, ICON_SIZE);
                apply_layout(
                    false,
                    ICON_SIZE,
                    ICON_SIZE,
                    true,
                    screen_w,
                    &container,
                    &header,
                    &footer,
                    &preview_label,
                    &icon_box,
                    &compact_top_bar,
                    false,
                );
                on_save(container.clone().upcast(), &data.borrow());
            })
        };

        let restore_action: Rc<dyn Fn()> = {
            let session_task = Arc::clone(&card.session_task);
            let expanded = Rc::clone(&expanded);
            let data = Rc::clone(&data);
            let vte = Rc::clone(&card.vte);
            let preview_box = card.preview_box.clone();
            let container = card.container.clone();
            let header = card.header.clone();
            let footer = card.footer.clone();
            let preview_label = card.preview_label.clone();
            let icon_box = card.icon_box.clone();
            let compact_top_bar = card.compact_top_bar.clone();
            let expand_btn = card.expand_btn.clone();
            let restore_btn = card.restore_btn.clone();
            let hint_label = card.hint_label.clone();
            let on_save = Rc::clone(&on_drag_end);
            let on_toggle_restore = Rc::clone(&on_toggle);
            Rc::new(move || {
                if *expanded.borrow() {
                    *expanded.borrow_mut() = false;
                    container.remove_css_class("term-expanded");
                    expand_btn.set_label("⛶");
                    expand_btn.set_tooltip_text(Some("Expand to 80% overlay"));
                    hint_label.set_label("Double-click to expand • drag any edge to resize");
                }
                let (nw, nh) = {
                    let d = data.borrow();
                    let rw = if d.restored_width >= MIN_CARD_WIDTH {
                        d.restored_width
                    } else {
                        CARD_WIDTH
                    };
                    let rh = if d.restored_height >= MIN_CARD_HEIGHT {
                        d.restored_height
                    } else {
                        CARD_HEIGHT
                    };
                    clamp_card_size(rw, rh, screen_w, screen_h)
                };
                {
                    let mut d = data.borrow_mut();
                    d.width = nw;
                    d.height = nh;
                    d.iconified = false;
                    d.restored_width = nw;
                    d.restored_height = nh;
                    restore_btn.set_tooltip_text(Some(&format!("Restore size ({}×{})", nw, nh)));
                }
                container.set_size_request(nw, nh);

                if vte.borrow().is_none() {
                    spawn_vte(&vte, &preview_box, &data, false, &on_toggle_restore, &expanded, &session_task, None);
                } else if let Some(term) = vte.borrow().as_ref() {
                    let theme = crate::theme::current_theme();
                    let font = gtk4::pango::FontDescription::from_string(&format!("{} 10", theme.font_family));
                    term.set_font(Some(&font));
                }

                apply_layout(
                    false,
                    nw,
                    nh,
                    false,
                    screen_w,
                    &container,
                    &header,
                    &footer,
                    &preview_label,
                    &icon_box,
                    &compact_top_bar,
                    vte.borrow().is_some(),
                );
                on_save(container.clone().upcast(), &data.borrow());
            })
        };

        // Wire up buttons
        iconify_btn.connect_clicked({
            let action = Rc::clone(&iconify_action);
            move |_| action()
        });

        restore_btn.connect_clicked({
            let action = Rc::clone(&restore_action);
            move |_| action()
        });

        compact_restore_btn.connect_clicked({
            let action = Rc::clone(&restore_action);
            move |_| action()
        });

        let on_close_compact = Rc::clone(&on_close);
        let sess_compact = card.data.borrow().session_name.clone();
        compact_kill_btn.connect_clicked(move |_| {
            on_close_compact(sess_compact.clone());
        });

        // Double-click gestures
        let header_click = GestureClick::new();
        let on_toggle_header = Rc::clone(&card.on_toggle);
        let data_header = Rc::clone(&card.data);
        header_click.connect_released(move |_, n_press, _, _| {
            if n_press == 2 {
                on_toggle_header(&data_header.borrow());
            }
        });
        card.header.add_controller(header_click);

        let preview_click = GestureClick::new();
        let on_toggle_preview = Rc::clone(&card.on_toggle);
        let data_preview = Rc::clone(&card.data);
        let expanded_preview = Rc::clone(&card.expanded);
        let restore_click = Rc::clone(&restore_action);
        let vte_preview = Rc::clone(&card.vte);
        preview_click.connect_released(move |_, n_press, _, _| {
            if n_press == 2 {
                if *expanded_preview.borrow() {
                    return;
                }
                if vte_preview.borrow().is_some() {
                    return;
                }
                if data_preview.borrow().iconified {
                    restore_click();
                } else {
                    on_toggle_preview(&data_preview.borrow());
                }
            }
        });
        card.preview_box.add_controller(preview_click);

        // Container-level double-click to restore iconified card
        let card_click = GestureClick::new();
        let data_card_click = Rc::clone(&card.data);
        let restore_card_click = Rc::clone(&restore_action);
        let expanded_card_click = Rc::clone(&card.expanded);
        card_click.connect_released(move |_, n_press, _, _| {
            if n_press == 2 && !*expanded_card_click.borrow() && data_card_click.borrow().iconified {
                restore_card_click();
            }
        });
        card.container.add_controller(card_click);

        // Move drag gestures:
        // When normal: drags from header bar.
        // When iconified: drags from anywhere on the 128x128 container.
        attach_move_drag(
            &card.header,
            &card.container,
            Rc::clone(&card.data),
            Rc::clone(&card.expanded),
            Rc::clone(&card.visual_pos),
            Rc::clone(&on_drag_update),
            Rc::clone(&on_drag_end),
            Rc::clone(&on_raise_rc),
            false,
        );
        attach_move_drag(
            &card.container,
            &card.container,
            Rc::clone(&card.data),
            Rc::clone(&card.expanded),
            Rc::clone(&card.visual_pos),
            Rc::clone(&on_drag_update),
            Rc::clone(&on_drag_end),
            Rc::clone(&on_raise_rc),
            true,
        );

        // Eight border/corner resize targets make every edge behave like a
        // conventional desktop window. Compact and expanded cards do not
        // resize: restore/collapse them first.
        let resize_limits = crate::card_resize::Limits {
            min_width: MIN_CARD_WIDTH,
            min_height: MIN_CARD_HEIGHT,
            max_width: ((screen_w as f64) * 0.70).round() as i32,
            max_height: ((screen_h as f64) * 0.75).round() as i32,
            left: 10.0,
            top: 70.0,
            right: (screen_w - 10) as f64,
            bottom: (screen_h - 10) as f64,
        };
        let data_start = Rc::clone(&card.data);
        let expanded_start = Rc::clone(&card.expanded);
        let get_start: Rc<dyn Fn() -> Option<crate::card_resize::Rect>> = Rc::new(move || {
            let d = data_start.borrow();
            if *expanded_start.borrow() || d.iconified {
                return None;
            }
            Some(crate::card_resize::Rect {
                x: d.x as f64,
                y: d.y as f64,
                width: d.width,
                height: d.height,
            })
        });
        let root_begin = card.container.clone();
        let on_raise_resize = Rc::clone(&on_raise_rc);
        let on_ghost_begin = Rc::clone(&on_resize_ghost);
        let data_begin = Rc::clone(&card.data);
        let on_begin: Rc<dyn Fn()> = Rc::new(move || {
            root_begin.add_css_class("term-resizing");
            on_raise_resize(root_begin.clone().upcast());
            let d = data_begin.borrow();
            on_ghost_begin(d.x as f64, d.y as f64, d.width, d.height, false);
        });
        let on_ghost_preview = Rc::clone(&on_resize_ghost);
        let on_preview: Rc<dyn Fn(crate::card_resize::Rect)> = Rc::new(move |rect| {
            on_ghost_preview(rect.x, rect.y, rect.width, rect.height, false);
        });
        let root_commit = card.container.clone();
        let data_commit = Rc::clone(&card.data);
        let visual_commit = Rc::clone(&card.visual_pos);
        let restore_btn_commit = card.restore_btn.clone();
        let on_drag_end_resize = Rc::clone(&on_drag_end);
        let on_ghost_end = Rc::clone(&on_resize_end);
        let on_commit: Rc<dyn Fn(crate::card_resize::Rect)> = Rc::new(move |rect| {
            on_ghost_end();
            root_commit.remove_css_class("term-resizing");
            {
                let mut d = data_commit.borrow_mut();
                d.x = rect.x as i32;
                d.y = rect.y as i32;
                d.width = rect.width;
                d.height = rect.height;
                d.restored_width = rect.width;
                d.restored_height = rect.height;
            }
            *visual_commit.borrow_mut() = (rect.x, rect.y);
            root_commit.set_size_request(rect.width, rect.height);
            restore_btn_commit.set_tooltip_text(Some(&format!(
                "Restore size ({}×{})",
                rect.width, rect.height
            )));
            on_drag_end_resize(root_commit.clone().upcast(), &data_commit.borrow());
        });
        crate::card_resize::attach_resize_borders(
            &card.container,
            resize_limits,
            get_start,
            on_begin,
            on_preview,
            on_commit,
        );

        if !card.data.borrow().iconified && card.data.borrow().width >= MIN_CARD_WIDTH {
            card.attach_vte_with_inventory(startup_inventory);
        }
        card.apply_chrome();
        card.refresh_status();
        card
    }

    pub fn is_expanded(&self) -> bool {
        *self.expanded.borrow()
    }

    pub fn is_compact(&self) -> bool {
        !self.is_expanded() && self.data.borrow().iconified
    }

    pub fn size(&self, screen_w: i32, screen_h: i32) -> (f64, f64) {
        if self.is_expanded() {
            let (_, _, w, h) = expanded_rect(screen_w, screen_h);
            (w, h)
        } else {
            (
                self.data.borrow().width as f64,
                self.data.borrow().height as f64,
            )
        }
    }

    pub fn expand(&self, screen_w: i32, screen_h: i32) {
        if *self.expanded.borrow() {
            return;
        }
        *self.expanded.borrow_mut() = true;

        let (x, y, w, h) = expanded_rect(screen_w, screen_h);
        *self.visual_pos.borrow_mut() = (x, y);
        self.container.set_size_request(w as i32, h as i32);
        self.container.add_css_class("term-expanded");
        self.expand_btn.set_label("🗕");
        self.expand_btn
            .set_tooltip_text(Some("Collapse back to overlay card"));
        self.hint_label
            .set_label("Double-click header to collapse");

        if self.vte.borrow().is_none() {
            self.attach_vte();
        } else if let Some(term) = self.vte.borrow().as_ref() {
            let theme = crate::theme::current_theme();
            let font = gtk4::pango::FontDescription::from_string(&format!("{} 11", theme.font_family));
            term.set_font(Some(&font));
        }

        self.apply_chrome();
        self.focus_terminal();
    }

    pub fn collapse(&self) {
        if !*self.expanded.borrow() {
            return;
        }
        *self.expanded.borrow_mut() = false;
        *self.visual_pos.borrow_mut() = displayed_pos(&self.data.borrow());

        let iconified = self.data.borrow().iconified;
        if iconified {
            self.detach_vte();
        } else if let Some(term) = self.vte.borrow().as_ref() {
            let theme = crate::theme::current_theme();
            let font = gtk4::pango::FontDescription::from_string(&format!("{} 10", theme.font_family));
            term.set_font(Some(&font));
        }

        let w = self.data.borrow().width;
        let h = self.data.borrow().height;
        self.container.set_size_request(w, h);
        self.container.remove_css_class("term-expanded");
        self.expand_btn.set_label("⛶");
        self.expand_btn
            .set_tooltip_text(Some("Expand to 80% overlay"));
        self.hint_label
            .set_label("Double-click to expand • drag any edge to resize");
        self.apply_chrome();
        self.refresh_status();
    }

    pub fn focus_terminal(&self) {
        if let Some(term) = self.vte.borrow().as_ref() {
            term.grab_focus();
        }
    }

    fn apply_chrome(&self) {
        let vte_attached = self.vte.borrow().is_some();
        apply_layout(
            self.is_expanded(),
            self.data.borrow().width,
            self.data.borrow().height,
            self.data.borrow().iconified,
            self.screen_w,
            &self.container,
            &self.header,
            &self.footer,
            &self.preview_label,
            &self.icon_box,
            &self.compact_top_bar,
            vte_attached,
        );
    }

    pub fn attach_vte(&self) {
        self.attach_vte_with_inventory(None);
    }

    fn attach_vte_with_inventory(&self, inventory: Option<Arc<crate::tmux::SessionInventory>>) {
        if self.vte.borrow().is_some() {
            return;
        }
        spawn_vte(
            &self.vte,
            &self.preview_box,
            &self.data,
            self.is_expanded(),
            &self.on_toggle,
            &self.expanded,
            &self.session_task,
            inventory,
        );
    }

    pub fn close_session(&self) {
        self.session_task.close();
        self.detach_vte();
        let task = Arc::clone(&self.session_task);
        let session = self.data.borrow().session_name.clone();
        gtk4::gio::spawn_blocking(move || {
            task.finish_close(|| crate::tmux::kill_session(&session));
        });
    }

    pub fn detach_vte(&self) {
        remove_vte(&self.vte, &self.preview_box);
    }

    pub fn apply_theme(&self, theme: &crate::theme::OmarchyTheme) {
        if let Some(term) = self.vte.borrow().as_ref() {
            let font_size = if self.is_expanded() { 11 } else { 10 };
            let font = gtk4::pango::FontDescription::from_string(&format!("{} {}", theme.font_family, font_size));
            term.set_font(Some(&font));

            let fg = gdk::RGBA::parse(&theme.foreground).ok();
            let bg = gdk::RGBA::parse(&theme.darker_background).ok();
            let palette = theme.get_ansi_palette();
            let palette_refs: Vec<&gdk::RGBA> = palette.iter().collect();
            term.set_colors(fg.as_ref(), bg.as_ref(), &palette_refs);

            if let Ok(cursor) = gdk::RGBA::parse(&theme.accent) {
                term.set_color_cursor(Some(&cursor));
            }
        }
    }

    pub fn refresh_status(&self) {
        if self.refresh_in_flight.get() {
            return;
        }

        let sess_name = self.data.borrow().session_name.clone();
        let agent_type = self.data.borrow().agent_type.clone();
        let want_preview = self.vte.borrow().is_none() && !self.is_compact();
        let preview_lines = if want_preview {
            let h = self.data.borrow().height;
            Some(((h - 70) / 13).clamp(6, 28) as usize)
        } else {
            None
        };

        let status_badge = self.status_badge.downgrade();
        let compact_status = self.compact_status.downgrade();
        let preview_label = self.preview_label.downgrade();
        let meta_label = self.meta_label.downgrade();
        let title_label = self.title_label.downgrade();
        let title_prefix = self.title_prefix.clone();
        let opencode_cache = Rc::clone(&self.opencode_session);
        let cached_oc_id: Option<String> = opencode_cache.borrow().clone();
        // Fall back to the persisted id (loaded from state.json at startup)
        // when the in-memory cache is still empty.
        let cached_oc_id = cached_oc_id.or_else(|| self.data.borrow().agent_session_id.clone());
        // Re-resolve the mapping at most every 30s (immediately on the first
        // refresh). A guess made before a neighbouring console closed
        // otherwise sticks forever, showing that console's prompt here.
        // Skipped entirely for non-opencode cards: no tmux/DB probing.
        let need_resolve = agent_type == "opencode" && {
            let mut slot = self.oc_recheck_at.borrow_mut();
            let now = std::time::Instant::now();
            if cached_oc_id.is_none() || now >= *slot {
                *slot = now + std::time::Duration::from_secs(30);
                true
            } else {
                false
            }
        };
        let data_weak = Rc::downgrade(&self.data);
        let data_snapshot: Option<TerminalData> =
            data_weak.upgrade().map(|d| d.borrow().clone());
        let on_persist = Rc::clone(&self.on_session_persist);
        let in_flight = Rc::downgrade(&self.refresh_in_flight);
        self.refresh_in_flight.set(true);

        glib::MainContext::default().spawn_local(async move {
            if in_flight.upgrade().is_none() {
                return;
            }
            let handle = gtk4::gio::spawn_blocking(move || {
                // Single screen capture feeds both detectors. Priority for the
                // title is strictly USER-entered text:
                //   1. composer draft (typed, not yet submitted),
                //   2. last submitted user message (opencode session DB),
                //   3. shell-history prompt (`~ ❯ cmd` on plain shells),
                //   4. default `icon + agent name` (never agent output).
                let screen = capture_pane_text(&sess_name);
                let status = inspect_status_with_screen(
                    &sess_name,
                    &agent_type,
                    screen.as_deref().unwrap_or(""),
                );
                let preview = preview_lines.map(|lines| match screen.as_deref() {
                    Some(screen) => preview_from_screen(screen, lines),
                    None if status.status == "EXITED" => "Session offline or ended.".into(),
                    None => "Ready. Waiting for input...".into(),
                });
                let history = screen
                    .as_deref()
                    .and_then(extract_last_prompt)
                    .map(|s| truncate_prompt_title(&s));
                let draft = screen.as_deref().and_then(extract_composer_draft);
                // `resolve_own_opencode_id` only ever returns THIS pane's own
                // session (own `--session` flag, else a claims-aware match),
                // so `db_text` is the prompt typed INTO this harness — never
                // another terminal's input. A stale cached guess heals here.
                let mut oc_id = cached_oc_id;
                if need_resolve {
                    let fresh = resolve_own_opencode_id(&sess_name, oc_id.as_deref());
                    if fresh != oc_id {
                        oc_id = fresh;
                    }
                }
                let db_text = if agent_type == "opencode" && draft.is_none() {
                    oc_id.as_deref().and_then(get_opencode_user_text_by_id)
                } else {
                    None
                };
                (status, preview, history, draft, db_text, oc_id)
            });
            let Ok((status_info, preview, history, draft, db_text, oc_id)) = handle.await else {
                if let Some(flag) = in_flight.upgrade() {
                    flag.set(false);
                }
                return;
            };

            if let (Some(badge), Some(compact)) = (status_badge.upgrade(), compact_status.upgrade()) {
                apply_status_view(&badge, &compact, status_info.status);
            }

            if let (Some(label), Some(meta)) = (preview_label.upgrade(), meta_label.upgrade()) {
                if let Some(preview) = preview.as_deref() {
                    if label.label().as_str() != preview {
                        label.set_label(preview);
                    }
                }
                let meta_text = format!("PID: {} • {}", status_info.pid, status_info.cmd);
                if meta.label() != meta_text {
                    meta.set_label(&meta_text);
                }
            }

            if let Some(title) = title_label.upgrade() {
                let user_text = draft.or(db_text).or(history);
                let new_title = format_card_title(&title_prefix, user_text.as_deref());
                if title.label().as_str() != new_title {
                    title.set_label(&new_title);
                    title.set_tooltip_text(Some(&new_title));
                }
            }
            if *opencode_cache.borrow() != oc_id {
                *opencode_cache.borrow_mut() = oc_id.clone();
            }
            // Persist the tmux-pane -> opencode-session mapping to state.json
            // on first resolution (and when a stale guess heals) so a later
            // reboot resumes THIS card with `opencode --session <id>` instead
            // of sharing the latest session.
            if let Some(new_id) = oc_id {
                let needs_save = data_snapshot
                    .as_ref()
                    .map(|d| d.agent_session_id.as_deref() != Some(new_id.as_str()))
                    .unwrap_or(true);
                if needs_save {
                    if let Some(d) = data_weak.upgrade() {
                        d.borrow_mut().agent_session_id = Some(new_id.clone());
                        let snapshot = d.borrow().clone();
                        on_persist(&snapshot);
                    }
                }
            }
            if let Some(flag) = in_flight.upgrade() {
                flag.set(false);
            }
        });
    }
}

fn status_view_texts(status: &str) -> (&'static str, &'static str, &'static str) {
    match status {
        "BUSY" | "WORKING" => ("● WORKING", "●", "status-busy"),
        "EXITED" => ("○ EXITED", "○", "status-exited"),
        _ => ("● IDLE", "●", "status-idle"),
    }
}

/// Build the header title: `prefix` (`icon + agent name`) plus the last
/// prompt snippet when available. Keeps the default prefix when prompt is None/empty.
fn format_card_title(prefix: &str, last_prompt: Option<&str>) -> String {
    match last_prompt {
        Some(prompt) if !prompt.trim().is_empty() => format!("{prefix} • {prompt}"),
        _ => prefix.to_string(),
    }
}

fn apply_status_view(badge: &Label, compact: &Label, status: &str) {
    let (badge_text, compact_text, css_class) = status_view_texts(status);
    let changed = badge.label().as_str() != badge_text || compact.label().as_str() != compact_text;
    if changed {
        for cls in ["status-active", "status-idle", "status-busy", "status-exited"] {
            if badge.has_css_class(cls) {
                badge.remove_css_class(cls);
            }
            if compact.has_css_class(cls) {
                compact.remove_css_class(cls);
            }
        }
        badge.add_css_class(css_class);
        compact.add_css_class(css_class);
        badge.set_label(badge_text);
        compact.set_label(compact_text);
    }
}

fn remove_vte(vte: &Rc<RefCell<Option<VteTerminal>>>, preview_box: &gtk4::Box) {
    if let Some(term) = vte.borrow_mut().take() {
        preview_box.remove(&term);
    }
}

fn spawn_vte(
    vte: &Rc<RefCell<Option<VteTerminal>>>,
    preview_box: &gtk4::Box,
    data: &Rc<RefCell<TerminalData>>,
    is_expanded: bool,
    on_toggle: &Rc<dyn Fn(&TerminalData)>,
    expanded_ref: &Rc<RefCell<bool>>,
    session_task: &Arc<crate::session_task::SessionTask>,
    inventory: Option<Arc<crate::tmux::SessionInventory>>,
) {
    if session_task.is_closed() { return; }
    remove_vte(vte, preview_box);

    let term = VteTerminal::new();
    term.set_hexpand(true);
    term.set_vexpand(true);
    term.set_input_enabled(true);
    term.set_scroll_on_keystroke(true);
    term.set_scroll_on_output(true);
    term.set_scrollback_lines(5000);
    term.add_css_class("term-vte");
    term.set_can_focus(true);
    term.set_focusable(true);

    let font_size = if is_expanded { 11 } else { 10 };
    let theme = crate::theme::current_theme();
    let font = gtk4::pango::FontDescription::from_string(&format!("{} {}", theme.font_family, font_size));
    term.set_font(Some(&font));

    let fg = gdk::RGBA::parse(&theme.foreground).ok();
    let bg = gdk::RGBA::parse(&theme.darker_background).ok();
    let palette = theme.get_ansi_palette();
    let palette_refs: Vec<&gdk::RGBA> = palette.iter().collect();
    term.set_colors(fg.as_ref(), bg.as_ref(), &palette_refs);

    if let Ok(cursor) = gdk::RGBA::parse(&theme.accent) {
        term.set_color_cursor(Some(&cursor));
    }

    let term_click = GestureClick::new();
    let term_weak = term.downgrade();
    term_click.connect_pressed(move |_, _, _, _| {
        if let Some(t) = term_weak.upgrade() {
            t.grab_focus();
        }
    });
    term.add_controller(term_click);

    let term_hover = gtk4::EventControllerMotion::new();
    let term_weak = term.downgrade();
    term_hover.connect_enter(move |_, _, _| {
        if let Some(t) = term_weak.upgrade() {
            if !t.has_focus() {
                t.grab_focus();
            }
        }
    });
    term.add_controller(term_hover);

    let session = data.borrow().session_name.clone();
    let agent_type = data.borrow().agent_type.clone();
    let cmd = data.borrow().command.clone();
    let agent_session_id = data.borrow().agent_session_id.clone();
    // The folder this card was created in (the top bar's field at that time),
    // not the field's current value: every harness scopes its resume and its
    // history to the cwd, so a card restored elsewhere would come back
    // attached to a different project. Cards from before this existed have
    // `None` and keep the old behaviour ($HOME).
    let workspace_dir = data.borrow().workspace_dir.clone();

    // Display the widget immediately, but do not block GTK on tmux, filesystem
    // probes, or agent resume lookup. A weak widget + identity check prevents
    // a late completion attaching a removed/replaced VTE after minimize/close.
    let weak_term = term.downgrade();
    let weak_slot = Rc::downgrade(vte);
    let task = Arc::clone(session_task);
    glib::MainContext::default().spawn_local(async move {
        if task.is_closed() { return; }
        let prepare_task = Arc::clone(&task);
        let prepare_session = session.clone();
        let prepared = gtk4::gio::spawn_blocking(move || {
            let cwd = crate::tmux::resolve_workspace_dir(workspace_dir.as_deref());
            let ready = prepare_task.prepare(|| ensure_session_with_inventory(
                &prepare_session, &agent_type, Some(&cmd), agent_session_id.as_deref(), Some(&cwd),
                inventory.as_deref(),
            ));
            (ready, cwd, tmux_bin())
        }).await;
        let Ok((true, cwd, tmux)) = prepared else { return; };
        crate::startup::mark("terminal session prepared");
        if task.is_closed() { return; }
        let (Some(term), Some(slot)) = (weak_term.upgrade(), weak_slot.upgrade()) else { return; };
        if slot.borrow().as_ref() != Some(&term) { return; }
        let argv = [tmux.as_str(), "-2", "attach-session", "-t", session.as_str()];

        let mut env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
        // Attaching from inside a tmux client nests sessions and routes input
        // to the OUTER session. Unset these for launches from a terminal too.
        env_map.remove("TMUX");
        env_map.remove("TMUX_PANE");
        env_map.insert("TERM".to_string(), "xterm-256color".to_string());
        env_map.insert("COLORTERM".to_string(), "truecolor".to_string());
        let env: Vec<String> = env_map
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let env_refs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();

        term.spawn_async(
            PtyFlags::DEFAULT,
            Some(cwd.as_str()),
            &argv,
            &env_refs,
            glib::SpawnFlags::DEFAULT,
            || {},
            -1,
            None::<&gtk4::gio::Cancellable>,
            |result| {
                if let Err(err) = result {
                    eprintln!("SUPER DESKTOP: failed to attach tmux in overlay: {err}");
                } else {
                    crate::startup::mark("terminal attach process spawned");
                }
            },
        );
    });

    let expand_time = std::time::Instant::now();
    let on_toggle_child = Rc::clone(on_toggle);
    let data_child = Rc::clone(data);
    let expanded_child = Rc::clone(expanded_ref);
    term.connect_child_exited(move |_, status| {
        if *expanded_child.borrow() && expand_time.elapsed() > std::time::Duration::from_millis(1500) && status == 0 {
            on_toggle_child(&data_child.borrow());
        }
    });

    preview_box.append(&term);
    *vte.borrow_mut() = Some(term);
}

fn apply_layout(
    expanded: bool,
    _width: i32,
    _height: i32,
    iconified: bool,
    _screen_w: i32,
    root: &Overlay,
    header: &gtk4::Box,
    footer: &gtk4::Box,
    preview_label: &Label,
    icon_box: &gtk4::Box,
    compact_top_bar: &gtk4::Box,
    vte_attached: bool,
) {
    let compact = !expanded && iconified;
    header.set_visible(!compact);
    footer.set_visible(!compact);
    preview_label.set_visible(!compact && !expanded && !vte_attached);
    icon_box.set_visible(compact);
    compact_top_bar.set_visible(compact && !expanded);
    crate::card_resize::set_resize_borders_visible(root, !expanded && !compact);

    if compact {
        root.add_css_class("term-compact");
    } else {
        root.remove_css_class("term-compact");
    }
}

fn attach_move_drag<FUpdate, FEnd, FRaise>(
    source: &impl IsA<gtk4::Widget>,
    root: &Overlay,
    data: Rc<RefCell<TerminalData>>,
    expanded: Rc<RefCell<bool>>,
    visual_pos: Rc<RefCell<(f64, f64)>>,
    on_drag_update: Rc<FUpdate>,
    on_drag_end: Rc<FEnd>,
    on_raise: Rc<FRaise>,
    iconified_only: bool,
) where
    FUpdate: Fn(gtk4::Widget, f64, f64) + 'static,
    FEnd: Fn(gtk4::Widget, &TerminalData) + 'static,
    FRaise: Fn(gtk4::Widget) + 'static,
{
    let drag = GestureDrag::new();
    let start_pos = Rc::new(RefCell::new((0.0, 0.0)));
    let grab_offset: Rc<RefCell<Option<(f64, f64)>>> = Rc::new(RefCell::new(None));

    let data_begin = Rc::clone(&data);
    let start_pos_begin = Rc::clone(&start_pos);
    let grab_offset_begin = Rc::clone(&grab_offset);
    let visual_begin = Rc::clone(&visual_pos);
    let expanded_begin = Rc::clone(&expanded);
    let root_weak_drag = root.downgrade();
    let on_raise_drag = Rc::clone(&on_raise);
    drag.connect_drag_begin(move |gesture, _, _| {
        if iconified_only && !data_begin.borrow().iconified {
            return;
        }
        if !iconified_only && data_begin.borrow().iconified {
            return;
        }
        if let Some(r) = root_weak_drag.upgrade() {
            on_raise_drag(r.upcast());
        }
        let (init_x, init_y) = if *expanded_begin.borrow() {
            *visual_begin.borrow()
        } else {
            // Drag from wherever the card is actually drawn (icon spot when
            // minimized), otherwise the icon would jump on first motion.
            let d = data_begin.borrow();
            displayed_pos(&d)
        };
        *start_pos_begin.borrow_mut() = (init_x, init_y);
        *grab_offset_begin.borrow_mut() = gesture
            .current_event()
            .and_then(|e| e.position())
            .map(|(mx, my)| (mx - init_x, my - init_y));
    });

    let root_weak = root.downgrade();
    let start_pos_update = Rc::clone(&start_pos);
    let grab_offset_update = Rc::clone(&grab_offset);
    let visual_update = Rc::clone(&visual_pos);
    let on_update = Rc::clone(&on_drag_update);
    let data_update = Rc::clone(&data);
    let expanded_update = Rc::clone(&expanded);
    drag.connect_drag_update(move |gesture, offset_x, offset_y| {
        if iconified_only && !data_update.borrow().iconified {
            return;
        }
        if !iconified_only && data_update.borrow().iconified {
            return;
        }
        if *expanded_update.borrow() {
            return;
        }
        if offset_x.abs() > 2.0 || offset_y.abs() > 2.0 {
            gesture.set_state(gtk4::EventSequenceState::Claimed);
        }
        if let Some(c) = root_weak.upgrade() {
            let (nx, ny) = match (
                *grab_offset_update.borrow(),
                gesture.current_event().and_then(|e| e.position()),
            ) {
                (Some((gx, gy)), Some((mx, my))) => (mx - gx, my - gy),
                _ => {
                    let (sx, sy) = *start_pos_update.borrow();
                    (sx + offset_x, sy + offset_y)
                }
            };
            *visual_update.borrow_mut() = (nx, ny);
            set_displayed_pos(&mut data_update.borrow_mut(), nx.round() as i32, ny.round() as i32);
            on_update(c.upcast(), nx, ny);
        }
    });

    let root_weak = root.downgrade();
    let data_end = Rc::clone(&data);
    let start_pos_end = Rc::clone(&start_pos);
    let grab_offset_end = Rc::clone(&grab_offset);
    let visual_end = Rc::clone(&visual_pos);
    let expanded_end = Rc::clone(&expanded);
    let on_end = Rc::clone(&on_drag_end);
    drag.connect_drag_end(move |gesture, offset_x, offset_y| {
        if iconified_only && !data_end.borrow().iconified {
            return;
        }
        if !iconified_only && data_end.borrow().iconified {
            return;
        }
        if *expanded_end.borrow() {
            return;
        }
        if let Some(c) = root_weak.upgrade() {
            let (nx_f, ny_f) = match (
                *grab_offset_end.borrow(),
                gesture.current_event().and_then(|e| e.position()),
            ) {
                (Some((gx, gy)), Some((mx, my))) => (mx - gx, my - gy),
                _ => {
                    let (sx, sy) = *start_pos_end.borrow();
                    (sx + offset_x, sy + offset_y)
                }
            };
            let nx = nx_f.round() as i32;
            let ny = ny_f.round() as i32;
            set_displayed_pos(&mut data_end.borrow_mut(), nx, ny);
            *visual_end.borrow_mut() = (nx as f64, ny as f64);
            on_end(c.upcast(), &data_end.borrow());
        }
    });

    source.add_controller(drag);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term_data(iconified: bool) -> TerminalData {
        TerminalData {
            id: "t".to_string(),
            session_name: "s".to_string(),
            agent_type: "shell".to_string(),
            command: "/usr/bin/bash".to_string(),
            x: 100,
            y: 200,
            width: CARD_WIDTH,
            height: CARD_HEIGHT,
            restored_width: CARD_WIDTH,
            restored_height: CARD_HEIGHT,
            iconified,
            icon_x: None,
            icon_y: None,
            created_at: 0.0,
            tag: 0,
            agent_session_id: None,
            workspace_dir: None,
        }
    }

    #[test]
    fn test_icon_keeps_its_own_position() {
        // A card that was never minimized draws at the card position.
        let mut d = term_data(false);
        assert_eq!(displayed_pos(&d), (100.0, 200.0));

        // First minimize seeds the icon spot where the card was (no jump).
        d.iconified = true;
        seed_icon_pos(&mut d);
        assert_eq!(displayed_pos(&d), (100.0, 200.0));

        // Dragging the icon moves only the icon spot.
        set_displayed_pos(&mut d, 500, 300);
        assert_eq!((d.x, d.y), (100, 200));
        assert_eq!(displayed_pos(&d), (500.0, 300.0));

        // Restoring returns the card to its own spot.
        d.iconified = false;
        assert_eq!(displayed_pos(&d), (100.0, 200.0));

        // Moving the card leaves the remembered icon spot alone, so the next
        // minimize goes back to it.
        set_displayed_pos(&mut d, 900, 400);
        assert_eq!((d.x, d.y), (900, 400));
        assert_eq!((d.icon_x, d.icon_y), (Some(500), Some(300)));
        d.iconified = true;
        assert_eq!(displayed_pos(&d), (500.0, 300.0));
    }

    #[test]
    fn closing_before_gtk_dispatch_cancels_terminal_preparation() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process("mini_terminal::tests::closing_before_gtk_dispatch_cancels_terminal_preparation");
            return;
        }
        if gtk4::init().is_err() { return; }
        let mut data = term_data(false);
        data.session_name = format!("test_sd_cancel_prepare_{}", std::process::id());
        let session = data.session_name.clone();
        let slot = Rc::new(RefCell::new(None));
        let preview = gtk4::Box::new(Orientation::Vertical, 0);
        let task = Arc::new(crate::session_task::SessionTask::default());
        let toggle: Rc<dyn Fn(&TerminalData)> = Rc::new(|_| {});
        spawn_vte(&slot, &preview, &Rc::new(RefCell::new(data)), false,
            &toggle, &Rc::new(RefCell::new(false)), &task, None);
        assert!(slot.borrow().is_some(), "placeholder exists before async setup");
        task.close();
        remove_vte(&slot, &preview);
        let context = glib::MainContext::default();
        while context.pending() { context.iteration(false); }
        assert!(slot.borrow().is_none());
        assert!(!crate::tmux::session_exists(&session), "cancelled setup must not create a session");
    }

    #[test]
    fn test_icon_position_round_trips_and_defaults_to_card_position() {
        // State files written before icon_x/icon_y existed must still load.
        let legacy = r#"{"id":"t","session_name":"s","agent_type":"shell",
            "command":"bash","x":10,"y":20,"created_at":0.0}"#;
        let mut old: TerminalData = serde_json::from_str(legacy).unwrap();
        assert_eq!((old.icon_x, old.icon_y), (None, None));
        old.iconified = true;
        assert_eq!(displayed_pos(&old), (10.0, 20.0));

        // New fields survive a save/load cycle.
        let mut d = term_data(true);
        set_displayed_pos(&mut d, 42, 43);
        let json = serde_json::to_string(&d).unwrap();
        let back: TerminalData = serde_json::from_str(&json).unwrap();
        assert_eq!((back.icon_x, back.icon_y), (Some(42), Some(43)));
        assert_eq!((back.x, back.y), (100, 200));
    }

    #[test]
    fn test_expanded_rect_centered() {
        let (x, y, w, h) = expanded_rect(1920, 1080);
        assert_eq!(w, 1536.0);
        assert_eq!(h, 864.0);
        assert_eq!(x, 192.0);
        assert_eq!(y, 108.0);
    }

    #[test]
    fn test_new_terminal_default_size_is_640x480() {
        assert_eq!((NEW_TERM_WIDTH, NEW_TERM_HEIGHT), (640, 480));
        // Unclamped on a typical screen.
        let (w, h) = clamp_card_size(NEW_TERM_WIDTH, NEW_TERM_HEIGHT, 1920, 1080);
        assert_eq!((w, h), (640, 480));
    }

    #[test]
    fn test_clamp_card_size_bounds() {
        let (w, h) = clamp_card_size(10, 10, 1920, 1080);
        assert_eq!((w, h), (MIN_CARD_WIDTH, MIN_CARD_HEIGHT));
        let (w, h) = clamp_card_size(99999, 99999, 1920, 1080);
        assert_eq!(w, 1344);
        assert_eq!(h, 810);
    }

    #[test]
    fn test_status_view_texts_mapping() {
        assert_eq!(status_view_texts("WORKING"), ("● WORKING", "●", "status-busy"));
        assert_eq!(status_view_texts("BUSY"), ("● WORKING", "●", "status-busy"));
        assert_eq!(status_view_texts("EXITED"), ("○ EXITED", "○", "status-exited"));
        assert_eq!(status_view_texts("IDLE"), ("● IDLE", "●", "status-idle"));
        assert_eq!(status_view_texts("weird"), ("● IDLE", "●", "status-idle"));
    }

    #[test]
    fn test_format_card_title_with_and_without_prompt() {
        assert_eq!(
            format_card_title("⚡ Claude Code", Some("fix login bug")),
            "⚡ Claude Code • fix login bug"
        );
        assert_eq!(format_card_title("⚡ Claude Code", None), "⚡ Claude Code");
        assert_eq!(format_card_title("⚡ Claude Code", Some("")), "⚡ Claude Code");
        assert_eq!(format_card_title("⚡ Claude Code", Some("   ")), "⚡ Claude Code");
    }
}
