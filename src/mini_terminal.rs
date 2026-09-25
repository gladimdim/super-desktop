use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Button, EventControllerFocus, EventControllerKey, GestureClick, GestureDrag, Label,
    Orientation, Overlay,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vte4::prelude::*;
use vte4::{PtyFlags, Terminal as VteTerminal};

use crate::card_source::{CardSource, RemoteSession};
use crate::state::TerminalData;
use crate::tmux::{
    ensure_session_with_inventory, get_agent_config,
    tmux_bin,
};

pub const CARD_WIDTH: i32 = 380;
pub const CARD_HEIGHT: i32 = 240;
pub const NEW_TERM_WIDTH: i32 = 640;
pub const NEW_TERM_HEIGHT: i32 = 480;
pub const MIN_CARD_WIDTH: i32 = 320;
pub const MIN_CARD_HEIGHT: i32 = 180;
pub const ICON_SIZE: i32 = 128;
/// What every card's footer says when nothing more important is happening.
pub const CARD_HINT: &str = "Double-click to expand • drag any edge to resize";

/// The smallest a card may be in a given workspace's own pixel space.
///
/// A local card's minimum is absolute; a remote card is drawn fitted into the
/// viewer's canvas, so the same host card is smaller on a smaller screen and
/// its minimum has to follow.
pub fn min_card_size(scale: f64) -> (i32, i32) {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    (
        ((MIN_CARD_WIDTH as f64) * scale).round() as i32,
        ((MIN_CARD_HEIGHT as f64) * scale).round() as i32,
    )
}

/// The footer hint: what the card's source wants to say, or the standing
/// invitation to expand and resize.
fn card_hint(message: &Rc<RefCell<Option<String>>>) -> String {
    card_hint_or(message, CARD_HINT)
}

fn card_hint_or(message: &Rc<RefCell<Option<String>>>, fallback: &str) -> String {
    message
        .borrow()
        .clone()
        .unwrap_or_else(|| fallback.to_string())
}

/// Resize bounds for a card in a `width × height` workspace drawn at `scale`
/// times this machine's pixels: the local rules (10 px margins, 70 px under
/// the dock, at most 70% × 75% of the workspace), scaled with the workspace.
pub fn workspace_limits((width, height): (i32, i32), scale: f64) -> crate::card_resize::Limits {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let (min_width, min_height) = min_card_size(scale);
    crate::card_resize::Limits {
        min_width,
        min_height,
        max_width: ((width as f64) * 0.70).round() as i32,
        max_height: ((height as f64) * 0.75).round() as i32,
        left: 10.0 * scale,
        top: 70.0 * scale,
        right: width as f64 - 10.0 * scale,
        bottom: height as f64 - 10.0 * scale,
    }
}

/// Clamp a card size in a workspace whose smallest card is `scale` times this
/// machine's: one rule for the local workspace (scale 1) and a fitted one.
pub fn clamp_card_size_at(w: i32, h: i32, screen_w: i32, screen_h: i32, scale: f64) -> (i32, i32) {
    let (min_w, min_h) = min_card_size(scale);
    let max_w = ((screen_w as f64) * 0.70).round() as i32;
    let max_h = ((screen_h as f64) * 0.75).round() as i32;
    (
        w.clamp(min_w, max_w.max(min_w)),
        h.clamp(min_h, max_h.max(min_h)),
    )
}

pub const EXPAND_RATIO: f64 = 0.80;
/// Logical pixels outside the last active card that do not activate a neighbour.
const HOVER_DEAD_ZONE: f32 = 20.0;

fn inside_hover_margin(x: f32, y: f32, width: i32, height: i32) -> bool {
    x >= -HOVER_DEAD_ZONE && y >= -HOVER_DEAD_ZONE
        && x <= width as f32 + HOVER_DEAD_ZONE
        && y <= height as f32 + HOVER_DEAD_ZONE
}
/// Newly opened harnesses ignore hover-raise on other cards for this long
/// so the pointer can travel to the new card without burying it.
pub const NEW_HARNESS_HOVER_LOCK: Duration = Duration::from_secs(4);
/// Keep a resize release over another card from immediately switching terminals.
const RESIZE_HOVER_LOCK: Duration = Duration::from_secs(1);

/// Shared across every terminal card. After `lock()`, pointer-enter on any
/// other card skips raise/focus until the hold expires or a click on another
/// card calls `on_click` (clicks stay an intentional switch).
#[derive(Clone)]
pub struct HoverRaiseLock {
    until: Rc<Cell<Option<Instant>>>,
    owner: Rc<RefCell<Option<String>>>,
    hover_owner: Rc<RefCell<Option<(String, glib::WeakRef<gtk4::Widget>)>>>,
}

impl HoverRaiseLock {
    pub fn new() -> Self {
        Self {
            until: Rc::new(Cell::new(None)),
            owner: Rc::new(RefCell::new(None)),
            hover_owner: Rc::new(RefCell::new(None)),
        }
    }

    pub fn lock(&self, session_name: &str) {
        self.lock_for(session_name, NEW_HARNESS_HOVER_LOCK);
    }

    pub fn lock_for(&self, session_name: &str, duration: Duration) {
        self.until.set(Some(Instant::now() + duration));
        *self.owner.borrow_mut() = Some(session_name.to_string());
    }

    pub fn release(&self) {
        self.until.set(None);
        self.owner.borrow_mut().take();
    }

    fn active_owner(&self) -> Option<String> {
        let until = self.until.get()?;
        if Instant::now() >= until {
            self.release();
            return None;
        }
        self.owner.borrow().clone()
    }

    /// True when this card may raise and take focus from a pointer enter.
    pub fn allows_hover(&self, session_name: &str) -> bool {
        match self.active_owner() {
            None => true,
            Some(owner) => owner == session_name,
        }
    }

    fn note_hover(&self, session: &str, widget: &impl IsA<gtk4::Widget>) {
        *self.hover_owner.borrow_mut() = Some((session.to_owned(), widget.as_ref().downgrade()));
    }

    fn allows_hover_at(&self, session: &str, source: &impl IsA<gtk4::Widget>, x: f64, y: f64) -> bool {
        if !self.allows_hover(session) {
            return false;
        }
        let owner = self.hover_owner.borrow();
        let Some((name, weak)) = owner.as_ref() else { return true; };
        if name == session { return true; }
        let Some(widget) = weak.upgrade().filter(|widget| widget.is_mapped()) else { return true; };
        // Translate against the live allocation, so dragging, resizing and output
        // scaling never leave a stale exclusion rectangle behind.
        let Some(point) = source.as_ref().compute_point(&widget, &gtk4::graphene::Point::new(x as f32, y as f32)) else { return true; };
        !inside_hover_margin(point.x(), point.y(), widget.width(), widget.height())
    }

    /// A click on a different card is an intentional switch: drop the hold.
    pub fn on_click(&self, session_name: &str) {
        if self.hover_owner.borrow().as_ref().is_some_and(|(owner, _)| owner != session_name) {
            self.hover_owner.borrow_mut().take();
        }
        if let Some(owner) = self.active_owner() {
            if owner != session_name {
                self.release();
            }
        }
    }
}

/// How long a card stays "in use" after the last sign of the user working in it
/// (a keystroke, a launch, an expand).
pub const ACTIVE_FOR: Duration = Duration::from_secs(3);

/// Why a card counts as the one the user is working in, plus the hook that
/// tells the overlay when that changed.
///
/// GTK keyboard focus cannot answer this question in this overlay: hovering a
/// card raises it *and* grabs focus, nothing releases that focus when the
/// pointer moves on, and a launched or expanded card holds it until something
/// else claims it. "Focused" therefore meant "active forever", and the dotted
/// outlines of the cards it covered never came back. Both signals here expire
/// on their own instead — the pointer immediately, keystrokes and deliberate
/// launches/expands after [`ACTIVE_FOR`] (the overlay's 1s refresh re-reads
/// that).
pub struct CardActivity {
    pointer_inside: Cell<bool>,
    last_activity: Cell<Option<Instant>>,
    on_change: Rc<dyn Fn()>,
}

impl CardActivity {
    pub fn new(on_change: Rc<dyn Fn()>) -> Self {
        Self {
            pointer_inside: Cell::new(false),
            last_activity: Cell::new(None),
            on_change,
        }
    }

    /// True while the user is working in this card.
    pub fn is_active(&self, now: Instant) -> bool {
        self.pointer_inside.get()
            || self
                .last_activity
                .get()
                .is_some_and(|at| now.saturating_duration_since(at) < ACTIVE_FOR)
    }

    /// The pointer entered or left the card. Notifies on a real change only, so
    /// a pointer that keeps crossing cards does not redraw the outlines twice.
    pub fn set_pointer_inside(&self, inside: bool) {
        if self.pointer_inside.replace(inside) != inside {
            (self.on_change)();
        }
    }

    /// The user did something with this card: a keystroke meant for it, a
    /// launch, an expand. It is active now, and stays so while that keeps
    /// happening.
    ///
    /// Notifies when this is what made it active again (the first sign after a
    /// pause), not on every repeat: the outlines are already in the right state.
    pub fn note_activity(&self, now: Instant) {
        if !self.is_active(now) {
            (self.on_change)();
        }
        self.last_activity.set(Some(now));
    }
}

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

/// One of the card's own actions, stored after construction so a remote command
/// runs exactly the code the matching local button or gesture runs.
type CardAction = Rc<RefCell<Option<Rc<dyn Fn()>>>>;
type GeometryAction = Rc<RefCell<Option<Rc<dyn Fn(crate::card_resize::Rect)>>>>;

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
    brand_images: Vec<gtk4::Image>,
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
    expand_btn: Button,
    compact_restore_btn: Button,
    _compact_kill_btn: Button,
    compact_top_bar: gtk4::Box,
    /// A brief line about the last command this card sent to another PC
    /// (see `command_feedback`). Floats under the header, never takes input,
    /// never changes the card's size, and dismisses itself.
    notice: gtk4::Revealer,
    notice_label: Label,
    /// Bumped by every notice, so an older timer cannot hide a newer one.
    notice_generation: Rc<Cell<u64>>,
    preview_box: gtk4::Box,
    vte: Rc<RefCell<Option<VteTerminal>>>,
    session_task: Arc<crate::session_task::SessionTask>,
    visual_pos: Rc<RefCell<(f64, f64)>>,
    on_toggle: Rc<dyn Fn(&TerminalData)>,
    on_raise: Rc<dyn Fn(gtk4::Widget)>,
    on_session_persist: Rc<dyn Fn(&TerminalData)>,
    hover_lock: HoverRaiseLock,
    pub keyboard_digit: Cell<Option<u8>>,
    /// Pointer and typing signals for the overlap ghosts (`user_is_active`).
    activity: Rc<CardActivity>,
    /// Who runs this card's session: this machine's tmux, or a host that
    /// streams it here. Everything else about the card is the same either way.
    source: CardSource,
    /// This card's host session, when its source is remote.
    remote: Option<Rc<RemoteSession>>,
    /// The body size and fit scale a remote view sized this card to, so its
    /// font follows the host's grid.
    fit: Rc<Cell<(f64, f64, f64)>>,
    /// The workspace this card is bounded by, in its own pixels. A local card
    /// is bounded by this machine's screen; a remote one by the host's
    /// workspace as the view currently draws it, which changes with the
    /// Fit/100% mode and the zoom.
    workspace: Rc<Cell<(i32, i32)>>,
    /// The last thing the source said about this card's session. A chrome
    /// change of our own must not lose it.
    source_message: Rc<RefCell<Option<String>>>,
    /// Set after construction: see [`CardAction`]. A remote layout command must
    /// not grow a second copy of an action that could drift from the button's.
    iconify_action: CardAction,
    restore_action: CardAction,
    geometry_commit: GeometryAction,
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
            term_data.width = ICON_SIZE;
            term_data.height = ICON_SIZE;
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
        let source_message: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let on_toggle: Rc<dyn Fn(&TerminalData)> = Rc::new(on_toggle);
        let on_close = Rc::new(on_close);
        let on_drag_update = Rc::new(on_drag_update);
        let on_drag_end = Rc::new(on_drag_end);
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
        // File previews read this machine's own sessions, so a remote card
        // never gets that button.
        if !source.is_remote() {
            header.append(&crate::asset_view::button(data.borrow().session_name.clone()));
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
        let attrs = gtk4::pango::AttrList::new();
        attrs.insert(gtk4::pango::AttrSize::new(38 * gtk4::pango::SCALE));
        icon_label.set_attributes(Some(&attrs));
        if let Some(path) = &logo {
            let image = gtk4::Image::from_file(path);
            image.set_pixel_size(48);
            icon_box.append(&image);
            brand_images.push(image);
        } else {
            icon_box.append(&icon_label);
        }

        let icon_name_label = Label::new(Some(display_name));
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
            container: root,
            data: Rc::clone(&data),
            expanded: Rc::clone(&expanded),
            screen_w,
            _screen_h: screen_h,
            header,
            footer,
            title_label: title.clone(),
            title_prefix,
            brand_images,
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
            expand_btn,
            compact_restore_btn: compact_restore_btn.clone(),
            _compact_kill_btn: compact_kill_btn.clone(),
            compact_top_bar,
            notice,
            notice_label,
            notice_generation: Rc::new(Cell::new(0)),
            preview_box,
            vte,
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
            workspace: Rc::new(Cell::new((screen_w, screen_h))),
            source_message: Rc::clone(&source_message),
            iconify_action: Rc::new(RefCell::new(None)),
            restore_action: Rc::new(RefCell::new(None)),
            geometry_commit: Rc::new(RefCell::new(None)),
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
            let container = card.container.clone();
            let header = card.header.clone();
            let footer = card.footer.clone();
            let preview_label = card.preview_label.clone();
            let icon_box = card.icon_box.clone();
            let compact_top_bar = card.compact_top_bar.clone();
            let expand_btn = card.expand_btn.clone();
            let hint_label = card.hint_label.clone();
            let source_message = Rc::clone(&source_message);
            let compact_restore_btn = card.compact_restore_btn.clone();
            let remote = card.remote.clone();
            let on_save = Rc::clone(&on_drag_end);
            Rc::new(move || {
                if *expanded.borrow() {
                    *expanded.borrow_mut() = false;
                    container.remove_css_class("term-expanded");
                    expand_btn.set_label("⛶");
                    expand_btn.set_tooltip_text(Some("Expand to 80% overlay"));
                    hint_label.set_label(&card_hint(&source_message));
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
                // A remote icon is not a terminal: release the stream. The next
                // snapshot reattaches if the host restores the card.
                if let Some(session) = &remote {
                    session.detach();
                }
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
            let hint_label = card.hint_label.clone();
            let source_message = Rc::clone(&source_message);
            let fit = Rc::clone(&card.fit);
            let workspace = Rc::clone(&card.workspace);
            let on_save = Rc::clone(&on_drag_end);
            let on_toggle_restore = Rc::clone(&on_toggle);
            let hover_lock_restore = card.hover_lock.clone();
            let activity_restore = Rc::clone(&card.activity);
            let remote_restore = card.remote.clone();
            Rc::new(move || {
                if *expanded.borrow() {
                    *expanded.borrow_mut() = false;
                    container.remove_css_class("term-expanded");
                    expand_btn.set_label("⛶");
                    expand_btn.set_tooltip_text(Some("Expand to 80% overlay"));
                    hint_label.set_label(&card_hint(&source_message));
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
                container.set_size_request(nw, nh);

                if vte.borrow().is_none() {
                    spawn_vte(&vte, &preview_box, &data, false, &on_toggle_restore, &expanded, &session_task, None, hover_lock_restore.clone(), &activity_restore, remote_restore.clone(), &fit);
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

    pub fn desktop_presentation(&self) -> crate::workspace_model::CardPresentation {
        crate::workspace_model::CardPresentation {
            title: self.title_label.label().to_string(), expanded: self.is_expanded(),
        }
    }

    /// Expanded state and the title are presentation, not saved state, so a
    /// local card reports their changes to desktop event subscribers itself. A
    /// remote card mirrors another PC and never describes this one.
    fn notify_host_change(&self) {
        if self.remote.is_none() {
            crate::workspace_model::notify_changed();
        }
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

    /// Where this card is drawn on the overlay canvas, in whichever form is
    /// currently up: the 80% expanded card, the 128×128 icon, or the plain
    /// card. Shared by the slide animation and the overlap ghosts.
    pub fn canvas_rect(&self, screen_w: i32, screen_h: i32) -> crate::card_resize::Rect {
        let (width, height) = self.size(screen_w, screen_h);
        let (x, y) = if self.is_expanded() {
            let (x, y, _, _) = expanded_rect(screen_w, screen_h);
            (x, y)
        } else {
            displayed_pos(&self.data.borrow())
        };
        crate::card_resize::Rect {
            x,
            y,
            width: width.round() as i32,
            height: height.round() as i32,
        }
    }

    /// True while the user is working in this card, which keeps the dotted
    /// ghost outlines of the cards it covers (and its own) off the screen.
    ///
    /// Expanded counts, and so do the pointer being on the card and any
    /// keystroke meant for it — but that last one only for [`ACTIVE_FOR`] after
    /// the last sign of use. Signals that expire on their own are the point:
    /// GTK focus would keep a card "in use" forever, because hover-raise grabs
    /// it and nothing releases it when the user moves on (see [`CardActivity`]).
    pub fn user_is_active(&self) -> bool {
        self.is_expanded() || self.activity.is_active(Instant::now())
    }

    /// Drop the "the pointer is over this card" state.
    ///
    /// Called when the overlay is unmapped: an unmap does not always deliver a
    /// pointer leave, and a card that still believed the pointer was on it
    /// would suppress the outlines of everything it covers after the next show.
    pub fn forget_pointer(&self) {
        self.activity.set_pointer_inside(false);
    }

    pub fn expand(&self, screen_w: i32, screen_h: i32) {
        if *self.expanded.borrow() {
            return;
        }
        *self.expanded.borrow_mut() = true;
        self.notify_host_change();

        let (x, y, w, h) = expanded_rect(screen_w, screen_h);
        *self.visual_pos.borrow_mut() = (x, y);
        self.container.set_size_request(w as i32, h as i32);
        self.container.add_css_class("term-expanded");
        self.expand_btn.set_label("❐");
        self.expand_btn
            .set_tooltip_text(Some("Collapse back to overlay card"));
        self.hint_label
            .set_label(&card_hint_or(&self.source_message, "Double-click header to collapse"));

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
        self.notify_host_change();
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
            .set_label(&card_hint(&self.source_message));
        self.apply_chrome();
        self.refresh_status();
    }

    /// Iconify or restore this card through its own button action.
    ///
    /// Returns whether the state changed, so a caller can tell "already there"
    /// from "cannot". A remote command must not invent a state the local UI
    /// could not reach, which is why the action, not the geometry, is reused.
    pub fn set_iconified(&self, iconified: bool) -> bool {
        if self.data.borrow().iconified == iconified || self.is_expanded() {
            return false;
        }
        let action = if iconified {
            self.iconify_action.borrow().clone()
        } else {
            self.restore_action.borrow().clone()
        };
        match action {
            Some(action) => {
                action();
                true
            }
            None => false,
        }
    }

    /// Apply a geometry commit exactly like a local resize gesture: stored
    /// size, restored size, chrome and persisted state. Returns whether the
    /// card was ready to take one.
    pub fn apply_geometry(&self, rect: crate::card_resize::Rect) -> bool {
        let Some(commit) = self.geometry_commit.borrow().clone() else {
            return false;
        };
        commit(rect);
        true
    }

    /// An explicit keyboard selection restores compact cards and protects the
    /// selected terminal from the pointer left over a different card.
    pub fn select_with_keyboard(&self) {
        self.set_iconified(false);
        let session = self.data.borrow().session_name.clone();
        self.hover_lock.lock_for(&session, Duration::from_secs(1));
        self.hover_lock.note_hover(&session, &self.container);
        (self.on_raise)(self.container.clone().upcast());
        self.focus_terminal();
    }

    pub fn focus_terminal(&self) {
        // A launch or an expand is a deliberate "I am working here" (see
        // `CardActivity`): it keeps the outlines of the covered cards off the
        // screen for a moment, and expires on its own like everything else.
        self.activity.note_activity(Instant::now());
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

    /// What the card's footer is telling the user, so a view can check that the
    /// host's state reached the chrome.
    #[cfg(test)]
    pub fn footer_text(&self) -> String {
        self.hint_label.label().to_string()
    }

    /// Show a brief, non-blocking line about a command's result, just under
    /// the chrome, and dismiss it after the notice's own duration.
    pub fn show_notice(&self, notice: crate::command_feedback::Notice) {
        for class in crate::command_feedback::TONE_CLASSES {
            self.notice_label.remove_css_class(class);
        }
        self.notice_label.add_css_class(notice.tone.css_class());
        self.notice_label.set_text(notice.text);
        // Below whichever bar the card shows, so the title and buttons stay
        // readable; a card that has not been allocated yet uses a small gap.
        let top = if self.header.is_visible() {
            self.header.height()
        } else if self.compact_top_bar.is_visible() {
            self.compact_top_bar.height()
        } else {
            0
        };
        self.notice.set_margin_top(top + 4);
        self.notice.set_visible(true);
        self.notice.set_reveal_child(true);
        let generation = self.notice_generation.get().wrapping_add(1);
        self.notice_generation.set(generation);
        let current = Rc::clone(&self.notice_generation);
        let revealer = self.notice.downgrade();
        glib::timeout_add_local_once(notice.duration, move || {
            if current.get() != generation {
                return;
            }
            if let Some(revealer) = revealer.upgrade() {
                revealer.set_reveal_child(false);
                // Without a frame clock the crossfade never finishes.
                if !revealer.is_mapped() {
                    revealer.set_visible(false);
                }
            }
        });
    }

    /// The notice on screen right now, if any.
    #[cfg(test)]
    pub fn notice_text(&self) -> Option<String> {
        self.notice
            .reveals_child()
            .then(|| self.notice_label.text().to_string())
    }

    /// The tone class the notice carries, for checks of its colour.
    #[cfg(test)]
    pub fn notice_has_class(&self, class: &str) -> bool {
        self.notice_label.has_css_class(class)
    }

    /// Whether the header is on screen. A compact or expanded card is not
    /// showing it, and a view sizing a card's terminal has to know.
    pub fn header_visible(&self) -> bool {
        self.header.is_visible()
    }

    pub fn remote_session(&self) -> Option<&Rc<RemoteSession>> {
        self.remote.as_ref()
    }

    /// The workspace this card is bounded by changed size: a remote view
    /// switched between Fit and 100%, zoomed, or was resized.
    pub fn set_workspace_size(&self, width: i32, height: i32) {
        self.workspace.set((width.max(1), height.max(1)));
    }

    /// The scale this card's emulator font was last fitted at.
    #[cfg(test)]
    pub fn fit_scale(&self) -> f64 {
        self.fit.get().2
    }

    /// The bounds a resize of this card starts with right now.
    #[cfg(test)]
    pub fn resize_limits(&self) -> crate::card_resize::Limits {
        workspace_limits(self.workspace.get(), self.fit.get().2)
    }

    /// Remember the size and fit scale the owning view drew this card at, and
    /// size the emulator's font for the host's grid.
    pub fn set_fit(&self, body_width: f64, body_height: f64, scale: f64) {
        self.fit.set((body_width, body_height, scale));
        let Some(terminal) = self.vte.borrow().as_ref().cloned() else {
            return;
        };
        let grid = self.remote.as_ref().and_then(|session| session.grid());
        crate::card_source::fit_font(&terminal, body_width, body_height, grid, scale);
    }

    /// Show what the host reports about this card: its title, whether its
    /// session is alive, and anything to say about the stream.
    pub fn apply_host_state(&self, title: &str, alive: Option<bool>, message: Option<&str>) {
        let title = title.trim();
        if !title.is_empty() && self.title_label.label() != title {
            self.title_label.set_label(title);
        }
        apply_status_view(
            &self.status_badge,
            &self.compact_status,
            match alive {
                Some(false) => "EXITED",
                // The host's session is alive but its state is the host's
                // business: this card only claims that it is remote.
                _ => "REMOTE",
            },
        );
        let (workspace, session) = {
            let data = self.data.borrow();
            (data.workspace_dir.clone(), data.session_name.clone())
        };
        let meta = match workspace.as_deref().and_then(|dir| {
            std::path::Path::new(dir)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }) {
            Some(name) => format!("Remote · {name}"),
            None => format!("Remote · {session}"),
        };
        if self.meta_label.label() != meta {
            self.meta_label.set_label(&meta);
        }
        *self.source_message.borrow_mut() = message.map(str::to_string);
        self.paint_source_message();
    }

    /// Mirror the host's own card mode. The host decides whether a card is
    /// iconified or expanded, so this never asks for anything: it is what a
    /// snapshot does to a card.
    pub fn mirror_host_mode(&self, iconified: bool, expanded: bool) {
        let was_icon = self.data.borrow().iconified;
        {
            let mut data = self.data.borrow_mut();
            data.iconified = iconified;
        }
        *self.expanded.borrow_mut() = expanded;
        if iconified && !was_icon {
            // An icon is not a terminal: stop drawing and stop streaming.
            self.detach_vte();
            if let Some(session) = &self.remote {
                session.detach();
            }
        }
        self.apply_chrome();
        self.paint_source_message();
    }

    /// Adopt the size the owning view fitted this card to, without saving
    /// anything or asking for anything: the host's rectangle is the truth.
    pub fn adopt_host_geometry(&self, width: i32, height: i32) {
        self.container.set_size_request(width, height);
        *self.visual_pos.borrow_mut() = displayed_pos(&self.data.borrow());
        self.apply_chrome();
        self.paint_source_message();
    }

    /// The footer hint, or the body when there is nothing to show there,
    /// carries whatever the card's source is saying.
    fn paint_source_message(&self) {
        if !self.source.is_remote() {
            return;
        }
        let message = self.source_message.borrow().clone();
        if self.vte.borrow().is_none() && !self.is_compact() && !self.is_expanded() {
            self.preview_label
                .set_text(message.as_deref().unwrap_or("Connecting…"));
            self.preview_label.set_visible(true);
        }
        self.hint_label
            .set_label(&card_hint_or(&self.source_message, CARD_HINT));
    }

    pub fn attach_vte(&self) {
        self.attach_vte_with_inventory(None);
    }

    /// Unmap the VTE widget without detaching tmux. Hide uses this so a
    /// GPU-heavy local model cannot stall compositor frames on live terminals.
    pub fn set_vte_drawing(&self, drawing: bool) {
        if let Some(term) = self.vte.borrow().as_ref() {
            term.set_visible(drawing);
        }
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
            self.hover_lock.clone(),
            &self.activity,
            self.remote.clone(),
            &self.fit,
        );
    }

    pub fn close_session(&self) {
        if let Some(session) = &self.remote {
            // The host owns the session: this card only stops reading it.
            session.stop();
            self.detach_vte();
            return;
        }
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
        if let Some(path) = crate::brand::logo_path(&self.data.borrow().agent_type, theme.mode == "light") {
            for image in &self.brand_images { image.set_from_file(Some(&path)); }
        }
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

    /// Refresh this card alone (one worker, one tmux inventory).
    pub fn refresh_status(&self) {
        run_status_refresh(self.prepare_status_refresh().into_iter().collect());
    }

    /// The worker request for this card's status/title/preview, plus how to
    /// apply the answer on the main thread. `None` for a remote card or while
    /// a previous refresh of this card is still running.
    pub fn prepare_status_refresh(&self) -> Option<PendingRefresh> {
        // A remote card's title, badge and preview come from the host's
        // snapshot: probing this machine's tmux or agent databases for another
        // machine's session would describe the wrong session.
        if self.source.is_remote() {
            return None;
        }
        if self.refresh_in_flight.get() {
            return None;
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

        let request = crate::card_status::CardRequest {
            session: sess_name,
            agent: agent_type,
            preview_lines,
            oc_id: cached_oc_id,
            need_resolve,
        };
        let apply = Box::new(move |update: Option<crate::card_status::CardUpdate>| {
            let Some(crate::card_status::CardUpdate { status: status_info, preview, prompt, oc_id }) = update else {
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
                let user_text = prompt;
                let new_title = format_card_title(&title_prefix, user_text.as_deref());
                if title.label().as_str() != new_title {
                    title.set_label(&new_title);
                    title.set_tooltip_text(Some(&new_title));
                    // This refresh runs for local cards only; their title is
                    // part of the published workspace.
                    crate::workspace_model::notify_changed();
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
        Some(PendingRefresh { request, apply })
    }
}

/// One card's part of a batched status refresh.
pub struct PendingRefresh {
    request: crate::card_status::CardRequest,
    apply: Box<dyn FnOnce(Option<crate::card_status::CardUpdate>)>,
}

/// Refresh several cards with one worker and one tmux inventory, then apply
/// each answer on the main thread. Cards dropped meanwhile are skipped (the
/// appliers only hold weak references).
pub fn run_status_refresh(pending: Vec<PendingRefresh>) {
    if pending.is_empty() {
        return;
    }
    let (requests, appliers): (Vec<_>, Vec<_>) =
        pending.into_iter().map(|p| (p.request, p.apply)).unzip();
    glib::MainContext::default().spawn_local(async move {
        let handle = gtk4::gio::spawn_blocking(move || crate::card_status::refresh(requests));
        match handle.await {
            Ok(updates) if updates.len() == appliers.len() => {
                for (apply, update) in appliers.into_iter().zip(updates) {
                    apply(Some(update));
                }
            }
            _ => {
                for apply in appliers {
                    apply(None);
                }
            }
        }
    });
}

fn status_view_texts(status: &str) -> (&'static str, &'static str, &'static str) {
    match status {
        "BUSY" | "WORKING" => ("● WORKING", "●", "status-busy"),
        "FINISHED" => ("✓ FINISHED", "✓", "status-idle"),
        "WAITING" => ("◌ WAITING", "◌", "status-idle"),
        "ERROR" => ("⚠ ERROR", "⚠", "status-exited"),
        "UNKNOWN" => ("? UNKNOWN", "?", "status-idle"),
        "EXITED" => ("○ EXITED", "○", "status-exited"),
        // A live card whose session runs on another machine says so, rather
        // than claiming to be idle here.
        "REMOTE" => ("● REMOTE", "●", "status-idle"),
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

/// Apply the active Omarchy theme and a font size to a VTE terminal.
///
/// One source of truth for terminal rendering: a local card and the remote live
/// view must paint the host's bytes identically. Fractional sizes are supported
/// because a remote card fits the host's grid into the viewer's own screen.
pub fn apply_vte_theme(term: &VteTerminal, font_size: f64) {
    let theme = crate::theme::current_theme();
    let mut font = gtk4::pango::FontDescription::from_string(&theme.font_family);
    font.set_size((font_size * f64::from(gtk4::pango::SCALE)).round() as i32);
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

fn remove_vte(vte: &Rc<RefCell<Option<VteTerminal>>>, preview_box: &gtk4::Box) {
    if let Some(term) = vte.borrow_mut().take() {
        preview_box.remove(&term);
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_vte(
    vte: &Rc<RefCell<Option<VteTerminal>>>,
    preview_box: &gtk4::Box,
    data: &Rc<RefCell<TerminalData>>,
    is_expanded: bool,
    on_toggle: &Rc<dyn Fn(&TerminalData)>,
    expanded_ref: &Rc<RefCell<bool>>,
    session_task: &Arc<crate::session_task::SessionTask>,
    inventory: Option<Arc<crate::tmux::SessionInventory>>,
    hover_lock: HoverRaiseLock,
    activity: &Rc<CardActivity>,
    remote: Option<Rc<RemoteSession>>,
    fit: &Rc<Cell<(f64, f64, f64)>>,
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
    crate::terminal_clipboard::install(&term, data.borrow().agent_type == "codex");
    crate::terminal_links::install(&term);

    let font_size = if is_expanded { 11.0 } else { 10.0 };
    apply_vte_theme(&term, font_size);
    // The font of a remote card follows the host's own grid instead of this
    // machine's theme: the emulator must line up with the columns and rows the
    // host decided. See below, where the grid is known.

    let term_click = GestureClick::new();
    let term_weak = term.downgrade();
    let click_lock = hover_lock.clone();
    let click_session = data.borrow().session_name.clone();
    let click_activity = Rc::clone(activity);
    term_click.connect_pressed(move |_, _, _, _| {
        click_lock.on_click(&click_session);
        // Clicking a terminal is how the user says "I am working here": keep
        // the outlines of the cards it covers off the screen while that lasts
        // (see `CardActivity`), even if the pointer wanders off to type.
        click_activity.note_activity(Instant::now());
        if let Some(t) = term_weak.upgrade() {
            t.grab_focus();
        }
    });
    term.add_controller(term_click);

    // Keystrokes are how the overlay knows the user is working in *this* card
    // (see `CardActivity`): a terminal taking input keeps the dotted outlines
    // of the cards it covers off the screen until the typing stops. Capture
    // phase, and `Proceed`, so the terminal still receives every key.
    let keys_activity = Rc::clone(activity);
    let keys = EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    keys.connect_key_pressed(move |_, _, _, _| {
        keys_activity.note_activity(Instant::now());
        glib::Propagation::Proceed
    });
    term.add_controller(keys);

    let session = data.borrow().session_name.clone();
    let term_hover = gtk4::EventControllerMotion::new();
    let term_weak = term.downgrade();
    let hover_lock_vte = hover_lock.clone();
    let hover_session = session.clone();
    term_hover.connect_enter(move |_, x, y| {
        if let Some(t) = term_weak.upgrade() {
            if !hover_lock_vte.allows_hover_at(&hover_session, &t, x, y) { return; }
            if crate::sticky_note::note_has_focus(&t) { return; }
            if !t.has_focus() {
                t.grab_focus();
            }
        }
    });
    term.add_controller(term_hover);
    if let Some(session) = remote {
        // A remote card has no local process at all. The host's bytes are fed
        // into this emulator, and every committed keystroke goes to the host
        // session rather than to a PTY here.
        term.set_scrollback_lines(crate::card_source::REMOTE_SCROLLBACK);
        let input_session = Rc::clone(&session);
        crate::card_source::connect_host_input(&term, move |bytes| {
            input_session.input(bytes);
        });
        // Return never arrives as a commit on this PTY-less emulator: the input
        // method and the window's activate-default binding consume it, while
        // letters still come through `commit`. Catch it on the way in and send
        // the carriage return the host shell treats as "run this".
        let return_session = Rc::clone(&session);
        let keys = EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, keyval, _, state| {
            if !crate::card_source::is_submit_key(keyval, state) {
                return glib::Propagation::Proceed;
            }
            return_session.input(b"\r");
            glib::Propagation::Stop
        });
        term.add_controller(keys);
        // Size the font for the host's grid once this card has a body: until
        // then the theme font stands in and the view refits after layout.
        crate::card_source::pack_remote_emulator(&term);
        preview_box.append(&term);
        let (body_width, body_height, scale) = fit.get();
        if body_width > 1.0 && body_height > 1.0 {
            crate::card_source::fit_font(&term, body_width, body_height, session.grid(), scale);
        }
        *vte.borrow_mut() = Some(term);
        session.attach();
        return;
    }

    // Observe bytes VTE sends, not key labels or the response painted on screen.
    let prompt_session = session.clone();
    let prompt_input = RefCell::new(crate::prompt_history::InputTracker::default());
    crate::card_source::connect_host_input(&term, move |bytes| {
        for prompt in prompt_input.borrow_mut().feed(bytes) {
            crate::prompt_history::record(&prompt_session, &prompt);
        }
    });

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
    FRaise: Fn(gtk4::Widget) + 'static + ?Sized,
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
            &toggle, &Rc::new(RefCell::new(false)), &task, None, HoverRaiseLock::new(),
            &Rc::new(CardActivity::new(Rc::new(|| {}))), None,
            &Rc::new(Cell::new((0.0, 0.0, 1.0))));
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
    fn test_hover_raise_lock_blocks_other_cards_until_expiry_or_click() {
        assert!(NEW_HARNESS_HOVER_LOCK >= Duration::from_secs(3));
        assert!(NEW_HARNESS_HOVER_LOCK <= Duration::from_secs(5));

        let lock = HoverRaiseLock::new();
        assert!(lock.allows_hover("old"));
        assert!(lock.allows_hover("new"));

        lock.lock_for("new", Duration::from_secs(30));
        assert!(lock.allows_hover("new"), "the new harness may still take hover");
        assert!(!lock.allows_hover("old"), "other cards must not steal hover");

        lock.on_click("new");
        assert!(!lock.allows_hover("old"), "clicking the new card keeps the hold");

        lock.on_click("old");
        assert!(lock.allows_hover("old"), "clicking another card is an intentional switch");
        assert!(lock.allows_hover("new"));
    }

    #[test]
    fn hover_dead_zone_uses_live_widget_bounds_and_click_override() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process("mini_terminal::tests::hover_dead_zone_uses_live_widget_bounds_and_click_override");
            return;
        }
        if gtk4::init().is_err() { return; }
        let window = gtk4::Window::new();
        let fixed = gtk4::Fixed::new();
        window.set_child(Some(&fixed));
        let card = gtk4::Box::new(Orientation::Vertical, 0);
        card.set_size_request(100, 80);
        fixed.put(&card, 20.0, 20.0);
        window.set_default_size(300, 200);
        window.present();
        for _ in 0..30 {
            while glib::MainContext::default().iteration(false) {}
            if card.is_mapped() && card.width() > 0 { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(card.is_mapped());
        let lock = HoverRaiseLock::new();
        lock.note_hover("front", &card);
        let point = card.compute_point(&fixed, &gtk4::graphene::Point::new(card.width() as f32, 40.0)).unwrap();
        let x = f64::from(point.x());
        let y = f64::from(point.y());
        assert!(!lock.allows_hover_at("back", &fixed, x + 20.0, y));
        assert!(lock.allows_hover_at("back", &fixed, x + 21.0, y));
        assert!(lock.allows_hover_at("front", &fixed, x + 2.0, y));
        lock.on_click("front");
        assert!(!lock.allows_hover_at("back", &fixed, x + 2.0, y));
        lock.on_click("back");
        assert!(lock.allows_hover_at("back", &fixed, x + 2.0, y));
        lock.note_hover("front", &card);
        card.set_visible(false);
        assert!(lock.allows_hover_at("back", &fixed, x + 2.0, y));
        fixed.remove(&card);
        drop(card);
        assert!(lock.allows_hover_at("back", &fixed, x + 2.0, y));
        window.close();
    }

    #[test]
    fn test_hover_dead_zone_edges_and_corners() {
        for (x, y) in [(-20.0, 40.0), (120.0, 40.0), (50.0, -20.0),
            (50.0, 100.0), (-20.0, -20.0), (120.0, 100.0)] {
            assert!(inside_hover_margin(x, y, 100, 80));
        }
        for (x, y) in [(-20.01, 40.0), (120.01, 40.0), (50.0, -20.01), (50.0, 100.01)] {
            assert!(!inside_hover_margin(x, y, 100, 80));
        }
        assert!(inside_hover_margin(117.0, 40.0, 100, 80));
        assert!(!inside_hover_margin(117.0, 40.0, 90, 80));
    }

    #[test]
    fn resize_release_holds_hover_for_one_second_then_allows_other_cards() {
        assert_eq!(RESIZE_HOVER_LOCK, Duration::from_secs(1));
        let lock = HoverRaiseLock::new();
        let before_release = Instant::now();
        lock.lock_for("resized", RESIZE_HOVER_LOCK);
        assert!(lock.until.get().unwrap() >= before_release + Duration::from_secs(1));
        assert!(lock.allows_hover("resized"));
        assert!(!lock.allows_hover("under_pointer"));
        // Advance the deadline without a slow, timing-sensitive sleep.
        lock.until.set(Some(Instant::now()));
        assert!(lock.allows_hover("under_pointer"));
    }

    #[test]
    fn test_hover_raise_lock_expires() {
        let lock = HoverRaiseLock::new();
        lock.lock_for("new", Duration::ZERO);
        assert!(lock.allows_hover("old"));
        assert!(lock.allows_hover("new"));
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

    /// The outline rule hangs on this: a card counts as "in use" while the
    /// pointer is on it or it is taking keystrokes — and stops counting on its
    /// own. GTK focus cannot be used here (hover-raise grabs it and nothing
    /// releases it), which is what made the outlines vanish for good.
    #[test]
    fn a_card_is_active_only_while_the_user_is_in_it() {
        let changes = Rc::new(Cell::new(0));
        let counter = Rc::clone(&changes);
        let activity = CardActivity::new(Rc::new(move || counter.set(counter.get() + 1)));
        let start = Instant::now();

        assert!(!activity.is_active(start), "an untouched card is not in use");

        activity.set_pointer_inside(true);
        assert!(activity.is_active(start), "the pointer counts at once");
        activity.set_pointer_inside(true);
        assert_eq!(changes.get(), 1, "an unchanged pointer must not redraw");
        activity.set_pointer_inside(false);
        assert_eq!(changes.get(), 2);

        assert!(!activity.is_active(start));
        activity.note_activity(start);
        assert_eq!(changes.get(), 3, "typing makes the card active");
        assert!(activity.is_active(start + Duration::from_secs(1)));
        activity.note_activity(start + Duration::from_secs(2));
        assert_eq!(changes.get(), 3, "keystrokes while active change nothing");

        // A pause hands the card back, and the next key takes it again. The
        // window runs from the *last* keystroke, not from the first.
        let expired = start + Duration::from_secs(2) + ACTIVE_FOR;
        assert!(activity.is_active(expired - Duration::from_millis(1)));
        assert!(!activity.is_active(expired));
        activity.note_activity(expired);
        assert_eq!(changes.get(), 4);

        // The pointer wins over an expired keyboard state.
        activity.set_pointer_inside(true);
        assert!(activity.is_active(start + Duration::from_secs(600)));
    }
}
