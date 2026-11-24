pub(in crate::mini_terminal) use gtk4::gdk;
pub(in crate::mini_terminal) use gtk4::glib;
pub(in crate::mini_terminal) use gtk4::prelude::*;
pub(in crate::mini_terminal) use gtk4::{
    Align, Button, EventControllerFocus, EventControllerKey, GestureClick, GestureDrag, Label,
    Orientation, Overlay,
};
pub(in crate::mini_terminal) use std::cell::{Cell, RefCell};
pub(in crate::mini_terminal) use std::rc::Rc;
pub(in crate::mini_terminal) use std::sync::Arc;
pub(in crate::mini_terminal) use std::time::{Duration, Instant};
pub(in crate::mini_terminal) use vte4::prelude::*;
pub(in crate::mini_terminal) use vte4::{PtyFlags, Terminal as VteTerminal};

pub(in crate::mini_terminal) use crate::card_source::{CardSource, RemoteSession};
pub(in crate::mini_terminal) use crate::state::TerminalData;
pub(in crate::mini_terminal) use crate::tmux::{
    ensure_session_with_inventory, get_agent_config,
    tmux_bin,
};


mod chrome;
mod geometry;
mod interaction;
mod lifecycle;
mod refresh;
mod vte_runtime;
#[cfg(test)]
mod tests;

pub(in crate::mini_terminal) use chrome::{
    CardAction, CardChrome, CompactChrome, GeometryAction, IconSpot,
};
#[allow(unused_imports)]
pub use geometry::{
    clamp_card_size, displayed_pos, expanded_rect, icon_side, max_card_size, min_card_size,
    set_displayed_pos, workspace_limits, CARD_HEIGHT, CARD_HINT, CARD_WIDTH, EXPAND_RATIO,
    ICON_SIZE, MAX_ICON_RADIUS, MIN_CARD_HEIGHT, MIN_CARD_WIDTH, NEW_TERM_HEIGHT, NEW_TERM_WIDTH,
};
pub(in crate::mini_terminal) use geometry::{
    card_hint, card_hint_or, clamp_card_size_at, icon_radius_class, inside_hover_margin,
    seed_icon_pos, set_font_px, CARD_BORDER, MAX_ICON_SCALE, MIN_NAME_FONT_PX,
};
pub(crate) use interaction::{dragged_origin, placed_origin};
#[allow(unused_imports)]
pub(in crate::mini_terminal) use interaction::{
    CardActivity, MoveDrag, ACTIVE_FOR, RESIZE_HOVER_LOCK,
};
pub use refresh::observe_hidden_status;
pub(in crate::mini_terminal) use refresh::forget_session_notifications;
#[allow(unused_imports)]
pub use interaction::{HoverRaiseLock, NEW_HARNESS_HOVER_LOCK};
#[allow(unused_imports)]
pub use refresh::{run_status_refresh, PendingRefresh};
#[cfg(test)]
pub(in crate::mini_terminal) use refresh::{format_card_title, status_view_texts};
pub(in crate::mini_terminal) use refresh::{apply_status_view, reveal_notice};
#[allow(unused_imports)]
pub use vte_runtime::{apply_vte_colors, apply_vte_theme, vte_font};
pub(in crate::mini_terminal) use vte_runtime::{
    freeze_terminal, remove_vte, set_card_font, spawn_vte, thaw_terminal, SlideFrameWatch,
    SlideLook, VteDrawing,
};

pub struct MiniTerminalCard {
    pub container: Overlay,
    pub data: Rc<RefCell<TerminalData>>,
    pub(in crate::mini_terminal) expanded: Rc<RefCell<bool>>,
    pub(in crate::mini_terminal) chrome: CardChrome,
    pub(in crate::mini_terminal) title_label: Label,
    pub(in crate::mini_terminal) title_prefix: String,
    pub(in crate::mini_terminal) brand_images: Vec<gtk4::Image>,
    /// Cached opencode session id for the USER-text DB lookup (None = unresolved yet).
    pub(in crate::mini_terminal) opencode_session: Rc<RefCell<Option<String>>>,
    /// Next time the cached id may be re-resolved against live tmux/DB state.
    /// A guess made before a neighbouring console closed otherwise sticks
    /// forever and keeps showing that console's prompt in this card's title.
    pub(in crate::mini_terminal) oc_recheck_at: Rc<RefCell<std::time::Instant>>,
    pub(in crate::mini_terminal) status_badge: Label,
    pub(in crate::mini_terminal) current_status: Rc<RefCell<String>>,
    pub(in crate::mini_terminal) refresh_in_flight: Rc<Cell<bool>>,
    pub(in crate::mini_terminal) meta_label: Label,
    /// A brief line about the last command this card sent to another PC
    /// (see `command_feedback`). Floats under the header, never takes input,
    /// never changes the card's size, and dismisses itself.
    pub(in crate::mini_terminal) notice: gtk4::Revealer,
    pub(in crate::mini_terminal) notice_label: Label,
    /// Bumped by every notice, so an older timer cannot hide a newer one.
    pub(in crate::mini_terminal) notice_generation: Rc<Cell<u64>>,
    pub(in crate::mini_terminal) preview_box: gtk4::Box,
    /// **↓ Jump to newest**, over the terminal while its pane is scrolled back.
    pub(in crate::mini_terminal) jump: crate::scrollback::JumpButton,
    pub(in crate::mini_terminal) vte: Rc<RefCell<Option<VteTerminal>>>,
    /// Whether that terminal draws: the overlay is mapped and no card hides it.
    pub(in crate::mini_terminal) vte_drawing: VteDrawing,
    /// The terminal's screen as a texture, shown in its place while the
    /// overlay slides (see `freeze_for_slide`).
    pub(in crate::mini_terminal) slide_frame: gtk4::Picture,
    /// Whether that texture still shows the terminal as it is, so a show can
    /// reuse the one taken at the hide (see `keep_slide_frame_for_show`).
    pub(in crate::mini_terminal) slide_watch: SlideFrameWatch,
    pub(in crate::mini_terminal) session_task: Arc<crate::session_task::SessionTask>,
    pub(in crate::mini_terminal) visual_pos: Rc<RefCell<(f64, f64)>>,
    pub(in crate::mini_terminal) on_toggle: Rc<dyn Fn(&TerminalData)>,
    pub(in crate::mini_terminal) on_raise: Rc<dyn Fn(gtk4::Widget)>,
    pub(in crate::mini_terminal) on_session_persist: Rc<dyn Fn(&TerminalData)>,
    pub(in crate::mini_terminal) hover_lock: HoverRaiseLock,
    pub keyboard_digit: Cell<Option<u8>>,
    /// Pointer and typing signals for the overlap ghosts (`user_is_active`).
    pub(in crate::mini_terminal) activity: Rc<CardActivity>,
    /// Who runs this card's session: this machine's tmux, or a host that
    /// streams it here. Everything else about the card is the same either way.
    pub(in crate::mini_terminal) source: CardSource,
    /// This card's host session, when its source is remote.
    pub(in crate::mini_terminal) remote: Option<Rc<RemoteSession>>,
    /// The body size and fit scale a remote view sized this card to, so its
    /// font follows the host's grid.
    pub(in crate::mini_terminal) fit: Rc<Cell<(f64, f64, f64)>>,
    /// What that font was last fitted to, so drawing this card again at the
    /// same size leaves its emulator alone.
    pub(in crate::mini_terminal) font_fit: Rc<RefCell<Option<crate::card_source::FontFit>>>,
    /// The workspace this card is bounded by, in its own pixels. A local card
    /// is bounded by this machine's screen; a remote one by the host's
    /// workspace as the view currently draws it, which changes with the
    /// Fit/100% mode and the zoom.
    pub(in crate::mini_terminal) workspace: Rc<Cell<(i32, i32)>>,
    /// Set after construction: see [`CardAction`]. A remote layout command must
    /// not grow a second copy of an action that could drift from the button's.
    pub(in crate::mini_terminal) iconify_action: CardAction,
    pub(in crate::mini_terminal) restore_action: CardAction,
    pub(in crate::mini_terminal) first_icon_spot: IconSpot,
    pub(in crate::mini_terminal) cli_mode_change: Rc<Cell<bool>>,
    pub(in crate::mini_terminal) cli_tag_buttons: Rc<RefCell<Vec<glib::WeakRef<Button>>>>,
    pub(in crate::mini_terminal) geometry_commit: GeometryAction,
    /// The header buttons running plugins add (a local card only).
    pub(in crate::mini_terminal) plugin_buttons: Option<crate::plugin_ui::card_buttons::ButtonRow>,
    pub(in crate::mini_terminal) files_btn: Option<Button>,
    pub(in crate::mini_terminal) attach_btn: Option<Button>,
    pub(in crate::mini_terminal) history_btn: Option<Button>,
    pub(in crate::mini_terminal) on_open_slash: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    pub(in crate::mini_terminal) on_restart: Rc<RefCell<Option<Rc<dyn Fn(bool)>>>>,
}

impl MiniTerminalCard {
    pub fn new<FDragUpdate, FDragEnd, FToggle, FClose, FResizeGhost, FResizeEnd, FRaise, FSessionSave, FInteraction>(
        mut term_data: TerminalData,
        on_drag_update: FDragUpdate,
        on_drag_end: FDragEnd,
        on_toggle: FToggle,
        on_close: FClose,
        on_resize_ghost: FResizeGhost,
        on_resize_end: FResizeEnd,
        on_raise: FRaise,
        on_session_persist: FSessionSave,
        on_interaction: FInteraction,
        screen_w: i32,
        screen_h: i32,
        startup_inventory: Option<Arc<crate::tmux::SessionInventory>>,
        // Custom launchers loaded once for a restoration batch; `None` reads
        // state.json for this one card.
        custom_harnesses: Option<Rc<Vec<crate::custom_harness::CustomHarness>>>,
        hover_lock: HoverRaiseLock,
        source: CardSource,
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
        // Called when the user's attention on this card changed (pointer in or
        // out, focus claimed or lost): the overlap ghosts are recomputed from
        // that state.
        FInteraction: Fn() + 'static,
    {
        let scale = source.scale();
        if term_data.iconified {
            term_data.width = icon_side(scale);
            term_data.height = icon_side(scale);
        } else {
            let (cw, ch) =
                clamp_card_size_at(term_data.width, term_data.height, screen_w, screen_h, scale);
            term_data.width = cw;
            term_data.height = ch;
        }

        // A remote card's rectangle is the host's, already fitted to this
        // canvas: it is never replaced by this machine's defaults.
        let (min_w, min_h) = min_card_size(scale);
        if !source.is_remote()
            && (term_data.restored_width < min_w || term_data.restored_height < min_h)
        {
            term_data.restored_width = CARD_WIDTH;
            term_data.restored_height = CARD_HEIGHT;
        }

        let initial_w = term_data.width;
        let initial_h = term_data.height;

        let data = Rc::new(RefCell::new(term_data));
        let expanded = Rc::new(RefCell::new(false));
        let vte = Rc::new(RefCell::new(None));
        let fit: Rc<Cell<(f64, f64, f64)>> = Rc::new(Cell::new((0.0, 0.0, scale)));
        let font_fit = Rc::new(RefCell::new(None));
        let source_message: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let on_toggle: Rc<dyn Fn(&TerminalData)> = Rc::new(on_toggle);
        let on_close = Rc::new(on_close);
        let on_drag_update = Rc::new(on_drag_update);
        let on_drag_end: Rc<dyn Fn(gtk4::Widget, &TerminalData)> = Rc::new(on_drag_end);
        let on_resize_ghost = Rc::new(on_resize_ghost);
        let on_resize_end = Rc::new(on_resize_end);
        let on_raise_rc: Rc<dyn Fn(gtk4::Widget)> = Rc::new(on_raise);
        // Pointer and typing attention on this card, for the overlap ghosts
        // (`user_is_active`). The callback tells the overlay to redraw them.
        let activity = Rc::new(CardActivity::new(Rc::new(on_interaction)));
        let visual_pos = Rc::new(RefCell::new(displayed_pos(&data.borrow())));

        let root = Overlay::new();

        // Raise on click anywhere on container. A click on a different card
        // during a new-harness hold is an intentional switch, so drop the lock.
        let click_raise = GestureClick::new();
        click_raise.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let container_weak_click = root.downgrade();
        let on_raise_click = Rc::clone(&on_raise_rc);
        let click_lock = hover_lock.clone();
        let click_session = data.borrow().session_name.clone();
        click_raise.connect_pressed(move |_, _, _, _| {
            click_lock.on_click(&click_session);
            if let Some(c) = container_weak_click.upgrade() {
                click_lock.note_hover(&click_session, &c);
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
        let custom = match custom_harnesses {
            Some(list) => list.iter().find(|item| item.id == agent_type).cloned(),
            None => crate::state::load_state().custom_harnesses.into_iter()
                .find(|item| item.id == agent_type),
        };
        let display_name = custom.as_ref().map_or(cfg.name, |item| item.name.as_str());
        let display_icon = custom.as_ref().map_or(cfg.icon, |item| item.icon.as_str());

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
        let synced_tag_dot = || {
            let tag_data = Rc::clone(&data);
            let tag_root = root.downgrade();
            let tag_save = Rc::clone(&on_drag_end);
            let tag_sync_dot = Rc::clone(&tag_sync);
            let dot = crate::tag::make_tag_dot(data.borrow().tag, move |next| {
                tag_data.borrow_mut().tag = next;
                for w in tag_sync_dot.borrow().iter() {
                    if let Some(b) = w.upgrade() {
                        crate::tag::apply_tag(&b, next);
                    }
                }
                if let Some(r) = tag_root.upgrade() {
                    tag_save(r.upcast(), &tag_data.borrow());
                }
            });
            tag_sync.borrow_mut().push(dot.downgrade());
            dot
        };
        let header_tag = synced_tag_dot();
        header.append(&header_tag);

        let logo = crate::brand::logo_path(&agent_type, crate::theme::current_theme().mode == "light");
        let mut brand_images = Vec::new();
        if let Some(path) = &logo {
            let image = gtk4::Image::from_file(path);
            image.set_pixel_size(18);
            header.append(&image);
            brand_images.push(image);
        }
        let title_prefix = if logo.is_some() { display_name.to_string() }
            else { format!("{} {}", display_icon, display_name) };
        let title = Label::new(Some(&title_prefix));
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
        // Plugin buttons act on this machine's cards only: a remote card
        // belongs to its host.
        let plugin_buttons = (!source.is_remote()).then(|| {
            let row = crate::plugin_ui::card_buttons::ButtonRow::new(
                &data.borrow().id,
                crate::shell_title::is_regular(&agent_type),
            );
            header.append(&row);
            row
        });
        // File previews read this machine's own sessions, so a remote card
        // never gets that button. The panel buttons sit closer together than
        // the rest of the header: it sets the card's minimum width, and these
        // three must fit a default-size card without widening it.
        let (files_btn, attach_btn, history_btn) = if !source.is_remote() {
            let panels = gtk4::Box::new(Orientation::Horizontal, 0);
            panels.add_css_class("term-panel-btns");
            let f_btn = crate::asset_view::button(
                data.borrow().session_name.clone(),
                display_name.to_string(),
            );
            panels.append(&f_btn);
            // Attachments are files on this machine, typed into its own
            // terminal. A card from before folders were recorded started in
            // $HOME.
            let folder = data.borrow().workspace_dir.clone()
                .unwrap_or_else(crate::state::home_dir_string);
            let a_btn = crate::attachment_view::button(
                data.borrow().session_name.clone(),
                display_name.to_string(),
                folder,
            );
            panels.append(&a_btn);
            // Prompt history reads this machine's journal and agent files too.
            let h_btn = crate::prompt_history_view::button(
                data.borrow().session_name.clone(),
                display_name.to_string(),
                &agent_type,
            );
            panels.append(&h_btn);
            header.append(&panels);
            (Some(f_btn), Some(a_btn), Some(h_btn))
        } else {
            (None, None, None)
        };
        let on_open_slash: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let on_restart: Rc<RefCell<Option<Rc<dyn Fn(bool)>>>> = Rc::new(RefCell::new(None));
        if !source.is_remote() {
            let right_click = GestureClick::new();
            right_click.set_button(gdk::BUTTON_SECONDARY);
            let h_weak = header.downgrade();
            let pop_agent = agent_type.clone();
            let pop_data = Rc::clone(&data);
            let pop_slash = Rc::clone(&on_open_slash);
            let pop_restart = Rc::clone(&on_restart);
            let pop_f = files_btn.clone();
            let pop_a = attach_btn.clone();
            let pop_h = history_btn.clone();
            right_click.connect_pressed(move |_, _, _, _| {
                let Some(hdr) = h_weak.upgrade() else { return };
                let pop = gtk4::Popover::new();
                pop.set_parent(&hdr);
                pop.set_position(gtk4::PositionType::Bottom);
                pop.set_has_arrow(true);
                pop.set_autohide(true);
                let col = gtk4::Box::new(Orientation::Vertical, 2);
                col.set_margin_top(4);
                col.set_margin_bottom(4);
                col.set_margin_start(4);
                col.set_margin_end(4);

                let add_btn = |label: &str, sensitive: bool, act: Box<dyn FnOnce()>| {
                    let b = Button::with_label(label);
                    b.set_has_frame(false);
                    b.set_sensitive(sensitive);
                    if let Some(lbl) = b.child().and_downcast::<Label>() {
                        lbl.set_xalign(0.0);
                    }
                    let pop_close = pop.clone();
                    let act_cell = RefCell::new(Some(act));
                    b.connect_clicked(move |_| {
                        pop_close.popdown();
                        if let Some(f) = act_cell.borrow_mut().take() {
                            f();
                        }
                    });
                    col.append(&b);
                };

                if !crate::slash_commands::commands_for(&pop_agent).is_empty() {
                    let slash_ref = Rc::clone(&pop_slash);
                    add_btn("Slash commands…", true, Box::new(move || {
                        if let Some(cb) = slash_ref.borrow().clone() {
                            cb();
                        }
                    }));
                }
                let r1 = Rc::clone(&pop_restart);
                add_btn("Restart fresh", true, Box::new(move || {
                    if let Some(cb) = r1.borrow().clone() {
                        cb(false);
                    }
                }));
                let can_resume = pop_data.borrow().agent_session_id.is_some()
                    || matches!(pop_agent.as_str(), "claude" | "opencode" | "codex" | "aider" | "reasonix");
                let r2 = Rc::clone(&pop_restart);
                add_btn("Resume conversation", can_resume, Box::new(move || {
                    if let Some(cb) = r2.borrow().clone() {
                        cb(true);
                    }
                }));
                if let Some(btn) = pop_h.clone() {
                    let sensitive = btn.is_sensitive();
                    add_btn("Prompt history", sensitive, Box::new(move || {
                        btn.emit_clicked();
                    }));
                }
                if let Some(btn) = pop_f.clone() {
                    add_btn("Files & links", true, Box::new(move || {
                        btn.emit_clicked();
                    }));
                }
                if let Some(btn) = pop_a.clone() {
                    add_btn("Attach files", true, Box::new(move || {
                        btn.emit_clicked();
                    }));
                }
                pop.set_child(Some(&col));
                pop.connect_closed(|p| p.unparent());
                pop.popup();
            });
            header.add_controller(right_click);
        }

        // Iconify button: iconifies the window into 128x128 size
        let iconify_btn = Button::with_label("🗕");
        iconify_btn.set_tooltip_text(Some("Iconify to 128×128"));
        iconify_btn.add_css_class("term-btn");
        header.append(&iconify_btn);

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

        let icon_label = Label::new(Some(display_icon));
        icon_label.add_css_class("term-agent-icon");
        let icon_logo = logo.as_ref().map(|path| {
            let image = gtk4::Image::from_file(path);
            icon_box.append(&image);
            brand_images.push(image.clone());
            image
        });
        if icon_logo.is_none() {
            icon_box.append(&icon_label);
        }

        let icon_name_label = Label::new(Some(display_name));
        icon_name_label.add_css_class("term-agent-name");
        // A long custom name shortens instead of widening the icon.
        icon_name_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        icon_name_label.set_single_line_mode(true);
        icon_box.append(&icon_name_label);

        icon_box.set_visible(false);
        preview_box.append(&icon_box);
        let slide_frame = gtk4::Picture::new();
        slide_frame.set_can_shrink(true);
        slide_frame.set_content_fit(gtk4::ContentFit::Fill);
        slide_frame.set_hexpand(true);
        slide_frame.set_vexpand(true);
        slide_frame.set_can_target(false);
        slide_frame.set_visible(false);
        preview_box.append(&slide_frame);
        // The terminal area, with Jump to newest over its bottom edge. Only a
        // local card has a pane here to scroll back.
        let preview_area = Overlay::new();
        preview_area.set_vexpand(true);
        preview_area.set_child(Some(&preview_box));
        let jump = crate::scrollback::JumpButton::new(&data.borrow().session_name, {
            let vte = Rc::clone(&vte);
            move || vte.borrow().is_some()
        });
        preview_area.add_overlay(&jump.button);
        if !source.is_remote() {
            jump.watch(&preview_box);
        }
        body.append(&preview_area);

        // Footer for normal card
        let footer = gtk4::Box::new(Orientation::Horizontal, 6);
        footer.add_css_class("term-footer");

        let meta_label = Label::new(Some("PID: - • Foot/Tmux"));
        meta_label.add_css_class("term-meta");
        meta_label.set_halign(Align::Start);
        footer.append(&meta_label);

        let hint_label = Label::new(Some(CARD_HINT));
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
        let compact_tag = synced_tag_dot();
        compact_tag.add_css_class("term-compact-tag");
        compact_top_bar.append(&compact_tag);

        // Right-aligned by its own expansion: a spacer would add two gaps the
        // bar of a small icon has no room for.
        let compact_actions = gtk4::Box::new(Orientation::Horizontal, 2);
        compact_actions.add_css_class("term-compact-actions");
        compact_actions.set_hexpand(true);
        compact_actions.set_halign(Align::End);

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
        let compact = CompactChrome {
            card: root.clone(),
            top_bar: compact_top_bar,
            status: compact_status,
            tag: compact_tag,
            actions: compact_actions,
            restore: compact_restore_btn.clone(),
            close: compact_kill_btn.clone(),
            icon_box,
            logo: icon_logo,
            glyph: icon_label,
            name: icon_name_label,
            side: Rc::new(Cell::new(0)),
        };

        // Command feedback. An overlay child is not part of the card's own
        // size request, so a long line wraps inside the card instead of
        // enlarging it, and it lets every click through to the chrome below.
        let notice_label = Label::new(None);
        notice_label.add_css_class("term-notice");
        // At most two lines, then an ellipsis: a small fitted or iconified card
        // keeps a bounded notice rather than one tall column of characters.
        notice_label.set_wrap(true);
        notice_label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        notice_label.set_lines(2);
        notice_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        notice_label.set_justify(gtk4::Justification::Center);
        let notice = gtk4::Revealer::new();
        notice.set_transition_type(gtk4::RevealerTransitionType::Crossfade);
        notice.set_transition_duration(180);
        notice.set_child(Some(&notice_label));
        notice.set_halign(Align::Center);
        notice.set_valign(Align::Start);
        notice.set_margin_start(8);
        notice.set_margin_end(8);
        notice.set_can_target(false);
        notice.set_can_focus(false);
        notice.set_visible(false);
        notice.connect_child_revealed_notify(|revealer| {
            if !revealer.reveals_child() && !revealer.is_child_revealed() {
                revealer.set_visible(false);
            }
        });
        root.add_overlay(&notice);


        // Seed from persisted state so rebooted cards resume the SAME agent
        // session without waiting for the DB mapping to re-resolve.
        let opencode_session = Rc::new(RefCell::new(data.borrow().agent_session_id.clone()));
        let on_session_persist: Rc<dyn Fn(&TerminalData)> = Rc::new(on_session_persist);
        // A remote card owns the stream that feeds it. The widget is the same
        // either way; this is the only place a session lives.
        let remote = match &source {
            CardSource::Remote { peer, card_id, .. } => {
                let feed = Rc::clone(&vte);
                let fit_for_grid = Rc::clone(&fit);
                let font_fit_for_grid = Rc::clone(&font_fit);
                let slot = Rc::clone(&source_message);
                let used = Rc::clone(&vte);
                let used_feed = Rc::clone(&used);
                let message_data = Rc::clone(&data);
                let message_expanded = Rc::clone(&expanded);
                let message_preview = preview_label.clone();
                let message_hint = hint_label.clone();
                Some(RemoteSession::new(crate::card_source::RemoteView {
                    peer: peer.clone(),
                    card_id: card_id.clone(),
                    vte: feed,
                    on_grid: Rc::new(move |grid| {
                        let Some(terminal) = used.borrow().as_ref().cloned() else {
                            return;
                        };
                        let (width, height, scale) = fit_for_grid.get();
                        crate::card_source::fit_font(
                            &terminal,
                            width,
                            height,
                            Some(grid),
                            scale,
                            &font_fit_for_grid,
                        );
                    }),
                    on_message: Rc::new(move |text| {
                        *slot.borrow_mut() = text.map(str::to_string);
                        let compact =
                            !*message_expanded.borrow() && message_data.borrow().iconified;
                        if !compact && !*message_expanded.borrow() && used_feed.borrow().is_none()
                        {
                            message_preview.set_text(text.unwrap_or("Connecting…"));
                            message_preview.set_visible(true);
                        }
                        message_hint.set_label(text.unwrap_or(CARD_HINT));
                    }),
                }))
            }
            CardSource::Local => None,
        };

        let card = Self {
            session_task: Arc::new(crate::session_task::SessionTask::default()),
            chrome: CardChrome {
                root: root.clone(),
                header,
                footer,
                preview_label,
                compact,
                expand_btn,
                close_btn: kill_btn.clone(),
                hint_label,
                source_message,
            },
            container: root,
            data: Rc::clone(&data),
            expanded: Rc::clone(&expanded),
            title_label: title.clone(),
            title_prefix,
            brand_images,
            opencode_session,
            // Recheck immediately on the first refresh so a stale persisted
            // guess heals fast, then at most every 30s.
            oc_recheck_at: Rc::new(RefCell::new(std::time::Instant::now())),
            status_badge,
            current_status: Rc::new(RefCell::new("IDLE".into())),
            refresh_in_flight: Rc::new(Cell::new(false)),
            meta_label,
            notice,
            notice_label,
            notice_generation: Rc::new(Cell::new(0)),
            preview_box,
            jump,
            vte,
            vte_drawing: VteDrawing::new(),
            slide_frame,
            slide_watch: SlideFrameWatch::default(),
            visual_pos,
            on_toggle: Rc::clone(&on_toggle),
            on_raise: Rc::clone(&on_raise_rc),
            on_session_persist: Rc::clone(&on_session_persist),
            hover_lock: hover_lock.clone(),
            keyboard_digit: Cell::new(None),
            activity,
            source,
            remote,
            fit,
            font_fit,
            workspace: Rc::new(Cell::new((screen_w, screen_h))),
            iconify_action: Rc::new(RefCell::new(None)),
            restore_action: Rc::new(RefCell::new(None)),
            first_icon_spot: Rc::new(RefCell::new(None)),
            cli_mode_change: Rc::new(Cell::new(false)),
            cli_tag_buttons: Rc::clone(&tag_sync),
            geometry_commit: Rc::new(RefCell::new(None)),
            plugin_buttons,
            files_btn,
            attach_btn,
            history_btn,
            on_open_slash: Rc::clone(&on_open_slash),
            on_restart: Rc::clone(&on_restart),
        };

        // Hover-focus: entering the card raises it and focuses VTE,
        // exactly like clicking inside the terminal. Skipped while a newly
        // opened harness holds the lock, otherwise the pointer path to that
        // card raises whatever it crosses and hides the new one.
        let hover = gtk4::EventControllerMotion::new();
        let container_weak_hover = card.container.downgrade();
        let vte_hover = Rc::clone(&card.vte);
        let on_raise_hover = Rc::clone(&on_raise_rc);
        let hover_lock_enter = hover_lock.clone();
        let hover_session = card.data.borrow().session_name.clone();
        let pointer_enter = Rc::clone(&card.activity);
        let accepted = Rc::new(Cell::new(false));
        let accepted_hover = Rc::clone(&accepted);
        let activate: Rc<dyn Fn(f64, f64)> = Rc::new(move |x, y| {
            pointer_enter.set_pointer_inside(true);
            if accepted_hover.get() { return; }
            let Some(c) = container_weak_hover.upgrade() else { return; };
            if !hover_lock_enter.allows_hover_at(&hover_session, &c, x, y) { return; }
            // Editing a sticky note: neither raise over it nor take its keyboard.
            // Not marked accepted, so hover works again once the note is left.
            if crate::sticky_note::note_has_focus(&c) { return; }
            accepted_hover.set(true);
            hover_lock_enter.note_hover(&hover_session, &c);
            on_raise_hover(c.upcast());
            if let Some(t) = vte_hover.borrow().as_ref() {
                if !t.has_focus() { t.grab_focus(); }
            }
        });
        let activate_enter = Rc::clone(&activate);
        hover.connect_enter(move |_, x, y| activate_enter(x, y));
        // Enter may land in the margin. Retry as the pointer moves past it,
        // without repeatedly raising an already accepted card.
        hover.connect_motion(move |_, x, y| activate(x, y));
        let pointer_leave = Rc::clone(&card.activity);
        hover.connect_leave(move |_| {
            accepted.set(false);
            pointer_leave.set_pointer_inside(false);
        });
        card.container.add_controller(hover);

        // Actions
        let iconify_action: Rc<dyn Fn()> = {
            let expanded = Rc::clone(&expanded);
            let data = Rc::clone(&data);
            let vte = Rc::clone(&card.vte);
            let preview_box = card.preview_box.clone();
            let chrome = card.chrome.clone();
            let remote = card.remote.clone();
            let fit = Rc::clone(&card.fit);
            let on_save = Rc::clone(&on_drag_end);
            let jump = card.jump.clone();
            let first_icon_spot = Rc::clone(&card.first_icon_spot);
            Rc::new(move || {
                if expanded.replace(false) {
                    chrome.paint_expanded(false);
                }
                remove_vte(&vte, &preview_box);
                jump.hide();
                // The icon of a remote card is the host's, at the view's scale.
                let side = icon_side(fit.get().2);
                // An icon that never had a spot of its own takes the first free
                // one in the icon rows; after that it keeps where it was left.
                let first_spot = {
                    let d = data.borrow();
                    let provider = first_icon_spot.borrow().clone();
                    match provider {
                        Some(provider) if d.icon_x.is_none() || d.icon_y.is_none() => {
                            let session = d.session_name.clone();
                            drop(d);
                            provider(&session, side)
                        }
                        _ => None,
                    }
                };
                {
                    let mut d = data.borrow_mut();
                    if d.width > side || d.height > side {
                        d.restored_width = d.width;
                        d.restored_height = d.height;
                    }
                    d.width = side;
                    d.height = side;
                    d.iconified = true;
                    if let Some((x, y)) = first_spot {
                        (d.icon_x, d.icon_y) = (Some(x), Some(y));
                    }
                    // Minimizing returns to the remembered icon spot.
                    seed_icon_pos(&mut d);
                    chrome.compact.restore.set_tooltip_text(Some(&format!(
                        "Expand to larger size ({}×{})",
                        d.restored_width, d.restored_height
                    )));
                }
                chrome.root.set_size_request(side, side);
                chrome.apply(false, side, side, true, false);
                on_save(chrome.root.clone().upcast(), &data.borrow());
                // A remote icon is not a terminal: release the stream. The next
                // snapshot reattaches if the host restores the card.
                if let Some(session) = &remote {
                    session.detach();
                }
            })
        };

        let restore_action: Rc<dyn Fn()> = {
            let cli_mode_change = Rc::clone(&card.cli_mode_change);
            let session_task = Arc::clone(&card.session_task);
            let expanded = Rc::clone(&expanded);
            let data = Rc::clone(&data);
            let vte = Rc::clone(&card.vte);
            let preview_box = card.preview_box.clone();
            let chrome = card.chrome.clone();
            let fit = Rc::clone(&card.fit);
            let font_fit = Rc::clone(&card.font_fit);
            let workspace = Rc::clone(&card.workspace);
            let on_save = Rc::clone(&on_drag_end);
            let on_toggle_restore = Rc::clone(&on_toggle);
            let hover_lock_restore = card.hover_lock.clone();
            let activity_restore = Rc::clone(&card.activity);
            let remote_restore = card.remote.clone();
            Rc::new(move || {
                if expanded.replace(false) {
                    chrome.paint_expanded(false);
                }
                let (min_w, min_h) = min_card_size(fit.get().2);
                let (nw, nh) = {
                    let d = data.borrow();
                    let rw = if d.restored_width >= min_w {
                        d.restored_width
                    } else {
                        CARD_WIDTH
                    };
                    let rh = if d.restored_height >= min_h {
                        d.restored_height
                    } else {
                        CARD_HEIGHT
                    };
                    let (screen_w, screen_h) = workspace.get();
                    clamp_card_size_at(rw, rh, screen_w, screen_h, fit.get().2)
                };
                {
                    let mut d = data.borrow_mut();
                    d.width = nw;
                    d.height = nh;
                    d.iconified = false;
                    d.restored_width = nw;
                    d.restored_height = nh;
                }
                chrome.root.set_size_request(nw, nh);

                if vte.borrow().is_none() {
                    spawn_vte(&vte, &preview_box, &data, false, &on_toggle_restore, &expanded, &session_task, None, hover_lock_restore.clone(), &activity_restore, remote_restore.clone(), &fit, &font_fit, !cli_mode_change.get());
                } else if let Some(term) = vte.borrow().as_ref() {
                    set_card_font(term, false);
                }

                chrome.apply(false, nw, nh, false, vte.borrow().is_some());
                on_save(chrome.root.clone().upcast(), &data.borrow());
            })
        };

        // Published after construction so remote commands reach these exact
        // actions; see the struct fields.
        *card.iconify_action.borrow_mut() = Some(Rc::clone(&iconify_action));
        *card.restore_action.borrow_mut() = Some(Rc::clone(&restore_action));

        // Wire up buttons
        iconify_btn.connect_clicked({
            let action = Rc::clone(&iconify_action);
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
        card.chrome.header.add_controller(header_click);

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
        let move_drag = MoveDrag {
            root: card.container.downgrade(),
            data: Rc::clone(&card.data),
            expanded: Rc::clone(&card.expanded),
            visual_pos: Rc::clone(&card.visual_pos),
            on_update: on_drag_update,
            on_end: Rc::clone(&on_drag_end),
            on_raise: Rc::clone(&on_raise_rc),
        };
        move_drag.attach(&card.chrome.header, false);
        move_drag.attach(&card.container, true);

        // Eight border/corner resize targets make every edge behave like a
        // conventional desktop window. Compact and expanded cards do not
        // resize: restore/collapse them first.
        // Read when a resize begins, so a remote view that changed its scale
        // (Fit/100%, zoom) bounds the edge in its current pixels. A local card
        // is at scale 1 on this machine's screen, exactly as before.
        let resize_limits: Rc<dyn Fn() -> crate::card_resize::Limits> = {
            let workspace = Rc::clone(&card.workspace);
            let fit = Rc::clone(&card.fit);
            Rc::new(move || workspace_limits(workspace.get(), fit.get().2))
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
            on_drag_end_resize(root_commit.clone().upcast(), &data_commit.borrow());
        });
        *card.geometry_commit.borrow_mut() = Some(Rc::clone(&on_commit));
        // Install the hold before committing geometry: the allocation change
        // can itself deliver pointer-enter to the terminal under the release.
        // Keep this on the gesture path, not incoming remote geometry updates.
        let resize_lock = card.hover_lock.clone();
        let resize_session = card.data.borrow().session_name.clone();
        let resize_root = card.container.downgrade();
        let resize_vte = Rc::clone(&card.vte);
        let resize_activity = Rc::clone(&card.activity);
        let raise_after_resize = Rc::clone(&on_raise_rc);
        let on_resize_commit: Rc<dyn Fn(crate::card_resize::Rect)> = Rc::new(move |rect| {
            resize_lock.lock_for(&resize_session, RESIZE_HOVER_LOCK);
            if let Some(root) = resize_root.upgrade() {
                resize_lock.note_hover(&resize_session, &root);
                raise_after_resize(root.upcast());
            }
            if let Some(term) = resize_vte.borrow().as_ref() {
                if !term.has_focus() { term.grab_focus(); }
            }
            resize_activity.note_activity(Instant::now());
            on_commit(rect);
        });
        crate::card_resize::attach_resize_borders_with(
            &card.container,
            resize_limits,
            get_start,
            on_begin,
            on_preview,
            on_resize_commit,
        );

        if !card.data.borrow().iconified && card.data.borrow().width >= MIN_CARD_WIDTH {
            card.attach_vte_with_inventory(startup_inventory);
        }
        card.apply_chrome();
        card.refresh_status();
        card
    }

}
