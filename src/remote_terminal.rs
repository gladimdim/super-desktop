//! Live remote consoles, drawn at the host's own card geometry.
//!
//! The consoles here are the *same* card widget the local workspace uses
//! (`MiniTerminalCard`): same chrome, same header buttons, same drag, resize,
//! expand and close gestures. Only the source differs, and this view supplies
//! it: a host session streamed over the pinned WebSocket instead of a tmux
//! session on this machine, and a snapshot instead of `state.json`.
//!
//! Every control therefore means the same thing in both workspaces, and on a
//! remote card it means it *on the host*: minimize, maximize, close and resize
//! are one typed command each, carrying the card revision this view drew, so a
//! concurrent host edit is answered with a conflict instead of being
//! overwritten. The host's snapshot is the truth this view mirrors.
use crate::desktop_protocol::{
    CardLayout, CommandReply, DesktopCard, LocalWorkspaceSnapshot, WorkspaceSnapshot,
    WorkspaceCommand, MAX_REMOTE_VIEWERS,
};
use crate::command_feedback::{self, CardCommand, Geometry, Outcome};
use crate::mini_terminal::MiniTerminalCard;
use crate::peer_client::{self, Peer};
use crate::remote_workspace::{self, ViewMode, ViewTransform};
use crate::state::TerminalData;
use gtk4::{glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Header height at 100% scale. Every other card dimension comes from the
/// host's logical rectangle, multiplied by this view's scale.
const HEADER_HEIGHT: f64 = 30.0;

/// Told whenever the view's mode or zoom changes, so the toggle can follow.
pub type ModeCallback = Rc<dyn Fn(ViewMode)>;

/// The host's workspace, rendered live inside the viewer's canvas.
pub struct RemoteCanvas {
    /// `message` (status/errors) or `canvas` (live cards).
    pub area: gtk4::Stack,
    /// The viewport. In Fit mode it never scrolls; in 100% mode it pans over
    /// the host's workspace. It asks for no size of its own, so neither a
    /// large host nor an off-screen card can enlarge the overlay window.
    scroller: gtk4::ScrolledWindow,
    /// Sized to exactly the host workspace at the current scale (the
    /// scroll range), and centered in the viewport when it is smaller.
    page: gtk4::Overlay,
    frame: gtk4::Box,
    /// The cards, in canvas pixels (`host × scale`). An overlay child of
    /// `page` that is never measured, so card extents never become the
    /// scroll range or the window's size.
    canvas: gtk4::Fixed,
    /// Fit or 100% (with its zoom) for the PC on screen now.
    mode: Cell<ViewMode>,
    /// The mode chosen for each PC this session, by machine id. Not written
    /// to disk: the plan asks for Fit by default and never persists view
    /// geometry, and a restart returns to This PC anyway.
    modes: RefCell<HashMap<String, ViewMode>>,
    on_mode: RefCell<Option<ModeCallback>>,
    /// Where the pointer is over the viewport, for zooming around it.
    pointer: Cell<Option<(f64, f64)>>,
    /// The viewport size the cards were last laid out for.
    laid_out_for: Cell<(i32, i32)>,
    /// Commands a regression test captured instead of sending.
    #[cfg(test)]
    outbox: RefCell<Option<Vec<WorkspaceCommand>>>,
    cards: RefCell<HashMap<String, Rc<MiniTerminalCard>>>,
    /// Where each card was last placed inside the fitted canvas, as
    /// `(x, y, width)`; the overlay's slide-out animates between these.
    placed: RefCell<HashMap<String, (f64, f64, f64)>>,
    /// Cards a gesture owns right now: a snapshot must not pull a card back to
    /// where the host last reported it while the user is still moving it.
    gesturing: RefCell<HashSet<String>>,
    /// Drops the host has not confirmed yet, in host pixels. A refresh that
    /// still has the old origin keeps the card where it was released.
    pending_moves: RefCell<HashMap<String, (i32, i32)>>,
    /// Cards whose next placement is a refused edit snapping back to the host.
    snapping: RefCell<HashSet<String>>,
    /// Cards gliding to a host position right now, with their destination in
    /// this canvas's pixels.
    glides: RefCell<HashMap<String, (f64, f64)>>,
    /// The host's stacking order as this view last imposed it, so a local
    /// click-raise is not undone by every poll.
    stacking: RefCell<Vec<String>>,
    /// Shared with the cards' hover handlers, so a card opened elsewhere does
    /// not steal the pointer path.
    hover_lock: crate::mini_terminal::HoverRaiseLock,
    snapshot: RefCell<Option<WorkspaceSnapshot>>,
    peer: RefCell<Option<Peer>>,
    /// The host advertises `workspace-layout-v1`, so it accepts commands. A
    /// host that does not keeps its own layout: a drop is dropped rather than
    /// guessed at, and no command is ever sent.
    layout_writable: Cell<bool>,
    /// Asked for a fresh snapshot after a command whose effect the answer
    /// cannot describe, so the viewer does not wait out the poll.
    on_changed: RefCell<Option<Rc<dyn Fn()>>>,
    message: gtk4::Label,
}

impl RemoteCanvas {
    pub fn new() -> Rc<Self> {
        let area = gtk4::Stack::new();
        area.set_transition_type(gtk4::StackTransitionType::None);
        area.set_hexpand(true);
        area.set_vexpand(true);
        let message = gtk4::Label::new(Some("Connecting…"));
        message.set_wrap(true);
        message.set_justify(gtk4::Justification::Center);
        message.add_css_class("term-preview-text");
        area.add_named(&message, Some("message"));
        // viewport → page (host-sized, centered) → frame (background) with
        // the card canvas laid over it.
        let frame = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        frame.add_css_class("remote-canvas");
        let canvas = gtk4::Fixed::new();
        canvas.set_halign(gtk4::Align::Start);
        canvas.set_valign(gtk4::Align::Start);
        let page = gtk4::Overlay::new();
        page.set_halign(gtk4::Align::Center);
        page.set_valign(gtk4::Align::Center);
        page.set_child(Some(&frame));
        page.add_overlay(&canvas);
        page.set_measure_overlay(&canvas, false);
        page.set_clip_overlay(&canvas, false);
        let scroller = gtk4::ScrolledWindow::new();
        scroller.set_policy(gtk4::PolicyType::External, gtk4::PolicyType::External);
        scroller.set_propagate_natural_width(false);
        scroller.set_propagate_natural_height(false);
        scroller.set_min_content_width(0);
        scroller.set_min_content_height(0);
        scroller.set_hexpand(true);
        scroller.set_vexpand(true);
        // Hover focuses a card's terminal; that must never scroll the view
        // under the pointer. Panning is the user's, not focus's.
        let viewport = gtk4::Viewport::new(None::<&gtk4::Adjustment>, None::<&gtk4::Adjustment>);
        viewport.set_scroll_to_focus(false);
        viewport.set_child(Some(&page));
        scroller.set_child(Some(&viewport));
        area.add_named(&scroller, Some("canvas"));
        area.set_visible_child_name("message");
        let view = Rc::new(Self {
            area,
            scroller,
            page,
            frame,
            canvas,
            mode: Cell::new(ViewMode::Fit),
            modes: RefCell::new(HashMap::new()),
            on_mode: RefCell::new(None),
            pointer: Cell::new(None),
            laid_out_for: Cell::new((0, 0)),
            #[cfg(test)]
            outbox: RefCell::new(None),
            cards: RefCell::new(HashMap::new()),
            placed: RefCell::new(HashMap::new()),
            gesturing: RefCell::new(HashSet::new()),
            pending_moves: RefCell::new(HashMap::new()),
            snapping: RefCell::new(HashSet::new()),
            glides: RefCell::new(HashMap::new()),
            stacking: RefCell::new(Vec::new()),
            hover_lock: crate::mini_terminal::HoverRaiseLock::new(),
            snapshot: RefCell::new(None),
            peer: RefCell::new(None),
            layout_writable: Cell::new(false),
            on_changed: RefCell::new(None),
            message,
        });
        // The viewer's own monitor can change while the overlay stays mapped
        // (display switch, dock), so the layout follows the viewport's actual
        // allocation: its adjustments change page size on every resize.
        for adjustment in [view.scroller.hadjustment(), view.scroller.vadjustment()] {
            let weak = Rc::downgrade(&view);
            adjustment.connect_changed(move |_| {
                let Some(view) = weak.upgrade() else {
                    return;
                };
                if view.laid_out_for.get() == view.viewport_size() {
                    return;
                }
                // Never relayout inside the allocation that reported it.
                let weak = Rc::downgrade(&view);
                glib::idle_add_local_once(move || {
                    if let Some(view) = weak.upgrade() {
                        if view.laid_out_for.get() != view.viewport_size() {
                            view.relayout();
                        }
                    }
                });
            });
        }
        view.bind_pan_and_zoom();
        view
    }

    /// Pan (drag on empty canvas, scrollbars, touchpad or wheel scroll) and
    /// zoom (Ctrl+scroll, pinch) for the 100% mode.
    fn bind_pan_and_zoom(self: &Rc<Self>) {
        let motion = gtk4::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |_, x, y| {
            if let Some(view) = weak.upgrade() {
                view.pointer.set(Some((x, y)));
            }
        });
        let weak = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(view) = weak.upgrade() {
                view.pointer.set(None);
            }
        });
        self.scroller.add_controller(motion);

        // Ctrl+scroll zooms around the pointer. Captured before the viewport
        // and the emulators see it; a plain scroll pans (or scrolls a
        // terminal's history when over one) as usual.
        let scroll = gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        scroll.connect_scroll(move |controller, _, dy| {
            let Some(view) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if !controller
                .current_event_state()
                .contains(gtk4::gdk::ModifierType::CONTROL_MASK)
                || !view.has_workspace()
            {
                return glib::Propagation::Proceed;
            }
            let steps = match controller.unit() {
                gtk4::gdk::ScrollUnit::Surface => dy / 40.0,
                _ => dy,
            };
            let zoom = view.transform().scale * 1.1f64.powf(-steps);
            view.set_mode(ViewMode::Actual { zoom }, view.pointer.get());
            glib::Propagation::Stop
        });
        self.scroller.add_controller(scroll);

        // Touchpad pinch or touchscreen zoom, around the fingers.
        let pinch = gtk4::GestureZoom::new();
        pinch.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let start = Rc::new(Cell::new(1.0));
        let weak = Rc::downgrade(self);
        let start_begin = Rc::clone(&start);
        pinch.connect_begin(move |gesture, _| {
            let Some(view) = weak.upgrade() else {
                return;
            };
            if !view.has_workspace() {
                gesture.set_state(gtk4::EventSequenceState::Denied);
                return;
            }
            start_begin.set(view.transform().scale);
        });
        let weak = Rc::downgrade(self);
        pinch.connect_scale_changed(move |gesture, scale| {
            let Some(view) = weak.upgrade() else {
                return;
            };
            if !view.has_workspace() {
                return;
            }
            gesture.set_state(gtk4::EventSequenceState::Claimed);
            let anchor = gesture.bounding_box_center();
            view.set_mode(ViewMode::Actual { zoom: start.get() * scale }, anchor);
        });
        self.scroller.add_controller(pinch);

        // Dragging empty canvas pans in 100% mode. A drag that starts on a
        // card (its header, edges or terminal) or on a scrollbar is that
        // widget's own gesture and is left alone.
        let drag = gtk4::GestureDrag::new();
        drag.set_button(0);
        let origin = Rc::new(Cell::new((0.0, 0.0)));
        let weak = Rc::downgrade(self);
        let origin_begin = Rc::clone(&origin);
        drag.connect_drag_begin(move |gesture, x, y| {
            let Some(view) = weak.upgrade() else {
                return;
            };
            let button = gesture.current_button();
            let pannable = matches!(view.mode.get(), ViewMode::Actual { .. })
                && (button == 1 || button == 2)
                && view.is_empty_canvas(x, y);
            if !pannable {
                gesture.set_state(gtk4::EventSequenceState::Denied);
                return;
            }
            origin_begin.set((
                view.scroller.hadjustment().value(),
                view.scroller.vadjustment().value(),
            ));
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_update(move |gesture, dx, dy| {
            let Some(view) = weak.upgrade() else {
                return;
            };
            if dx.abs() > 2.0 || dy.abs() > 2.0 {
                gesture.set_state(gtk4::EventSequenceState::Claimed);
            }
            let (x, y) = origin.get();
            view.scroller.hadjustment().set_value(x - dx);
            view.scroller.vadjustment().set_value(y - dy);
        });
        self.scroller.add_controller(drag);
    }

    /// Whether a viewport point is on the workspace but not on any card.
    fn is_empty_canvas(&self, x: f64, y: f64) -> bool {
        let Some(picked) = self.scroller.pick(x, y, gtk4::PickFlags::DEFAULT) else {
            return false;
        };
        let viewport = self.scroller.child();
        picked == *self.canvas.upcast_ref::<gtk4::Widget>()
            || picked == *self.page.upcast_ref::<gtk4::Widget>()
            || picked == *self.frame.upcast_ref::<gtk4::Widget>()
            || viewport.is_some_and(|viewport| picked == viewport)
    }

    fn has_workspace(&self) -> bool {
        self.snapshot.borrow().is_some()
    }

    /// The current mode (Fit, or 100% with its zoom).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn mode(&self) -> ViewMode {
        self.mode.get()
    }

    /// Told whenever the mode or the zoom changes.
    pub fn set_on_mode_changed(&self, callback: ModeCallback) {
        *self.on_mode.borrow_mut() = Some(callback);
    }

    /// Switch between Fit and 100% (or change the zoom), keeping the host
    /// point under `anchor` — the pointer, or the viewport's center — where
    /// it is. Remembered for this PC for the rest of the session.
    pub fn set_mode(self: &Rc<Self>, mode: ViewMode, anchor: Option<(f64, f64)>) {
        let mode = match mode {
            ViewMode::Fit => ViewMode::Fit,
            ViewMode::Actual { zoom } => ViewMode::Actual {
                zoom: remote_workspace::clamp_zoom(zoom),
            },
        };
        if let Some(peer) = self.peer.borrow().as_ref() {
            self.modes
                .borrow_mut()
                .insert(peer.machine_id.clone(), mode);
        }
        let host = self.host_size();
        let before = self.transform();
        self.mode.set(mode);
        if let Some((host_width, host_height)) = host {
            let (width, height) = self.viewport_size();
            let anchor = anchor.unwrap_or((f64::from(width) / 2.0, f64::from(height) / 2.0));
            let after = before.rezoomed(mode, host_width, host_height, anchor);
            self.relayout();
            self.show_pan(&after);
        }
        self.announce_mode();
    }

    fn announce_mode(&self) {
        let callback = self.on_mode.borrow().clone();
        if let Some(callback) = callback {
            callback(self.mode.get());
        }
    }

    /// Scroll the viewport to a transform's pan. The scroll range is set
    /// here as well, so the value is not clamped against the previous zoom's
    /// range before the viewport's next allocation catches up.
    fn show_pan(&self, transform: &ViewTransform) {
        for (adjustment, value, content, page) in [
            (
                self.scroller.hadjustment(),
                transform.pan_x,
                transform.content_width,
                transform.view_width,
            ),
            (
                self.scroller.vadjustment(),
                transform.pan_y,
                transform.content_height,
                transform.view_height,
            ),
        ] {
            let page = f64::from(page);
            adjustment.configure(
                value,
                0.0,
                f64::from(content).max(page),
                page * 0.1,
                page * 0.9,
                page,
            );
        }
    }

    /// The workspace revision this view last held. A folder change is judged
    /// against it, because the folder is one field of the workspace.
    pub fn snapshot_revision(&self) -> Option<u64> {
        self.snapshot
            .borrow()
            .as_ref()
            .map(|snapshot| snapshot.local.revision)
    }

    /// Called after a command whose effect this view cannot apply by itself
    /// (close, expand): the owner asks for a fresh snapshot instead of waiting
    /// out the poll.
    pub fn set_on_changed(&self, callback: Rc<dyn Fn()>) {
        *self.on_changed.borrow_mut() = Some(callback);
    }

    /// Number of rendered cards. Used by regression tests, which cannot open a
    /// real remote connection.
    #[cfg(test)]
    pub fn card_count(&self) -> usize {
        self.cards.borrow().len()
    }

    /// The host card ids on screen, sorted.
    #[cfg(test)]
    pub fn card_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.cards.borrow().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Cards whose stream has completed the host's `attached` handshake.
    #[cfg(test)]
    pub fn live_streams(&self) -> usize {
        self.cards
            .borrow()
            .values()
            .filter(|card| card.remote_session().is_some_and(|session| session.is_attached()))
            .count()
    }

    /// The host epoch of the snapshot on screen.
    #[cfg(test)]
    pub fn snapshot_epoch(&self) -> Option<String> {
        self.snapshot
            .borrow()
            .as_ref()
            .map(|snapshot| snapshot.local.epoch.clone())
    }

    /// Whether the host accepts layout commands, as its last answer said.
    #[cfg(test)]
    pub fn layout_is_writable(&self) -> bool {
        self.layout_writable.get()
    }

    /// The revision this view holds for one card, which is what a gesture
    /// sends with its command.
    #[cfg(test)]
    pub fn card_revision(&self, card_id: &str) -> Option<u64> {
        self.card(card_id).map(|card| card.revision)
    }

    /// The host position this view holds for one card.
    #[cfg(test)]
    pub fn card_position(&self, card_id: &str) -> Option<(i32, i32)> {
        self.card(card_id).map(|card| (card.layout.x, card.layout.y))
    }

    /// The rendered card widget for a host card, so a check can look at the
    /// chrome the user actually sees.
    #[cfg(test)]
    pub fn card_widget(&self, card_id: &str) -> Option<Rc<MiniTerminalCard>> {
        self.cards.borrow().get(card_id).cloned()
    }

    /// Capture commands instead of sending them, so a check can see exactly
    /// what a gesture would have told the host.
    #[cfg(test)]
    pub fn capture_commands(&self) {
        *self.outbox.borrow_mut() = Some(Vec::new());
    }

    #[cfg(test)]
    pub fn captured(&self) -> Vec<WorkspaceCommand> {
        self.outbox.borrow().clone().unwrap_or_default()
    }

    /// The viewport widget, for checks against real GTK geometry.
    #[cfg(test)]
    pub fn viewport(&self) -> &gtk4::ScrolledWindow {
        &self.scroller
    }

    /// A header drag the user made in viewport pixels: grabbed at `grab`,
    /// released at `release`. The points go through GTK's own translation
    /// from the viewport into the card canvas — the one a real pointer's
    /// events take, pan and all — and the drop then runs the card's real
    /// commit path.
    #[cfg(test)]
    pub fn drag_card_in_view(self: &Rc<Self>, card_id: &str, grab: (f64, f64), release: (f64, f64)) {
        let card = self.card_widget(card_id).expect("card on screen");
        let point = |(x, y): (f64, f64)| {
            self.scroller
                .compute_point(&self.canvas, &gtk4::graphene::Point::new(x as f32, y as f32))
                .map(|p| (f64::from(p.x()), f64::from(p.y())))
                .expect("viewport and canvas share a root")
        };
        let (from, to) = (point(grab), point(release));
        let data = {
            let mut data = card.data.borrow_mut();
            let (x, y) = crate::mini_terminal::displayed_pos(&data);
            crate::mini_terminal::set_displayed_pos(
                &mut data,
                (x + to.0 - from.0).round() as i32,
                (y + to.1 - from.1).round() as i32,
            );
            data.clone()
        };
        self.commit_card_layout(card_id, &data);
    }

    /// The host's own card as the last snapshot reported it. Every command
    /// needs it: its revision is what the owner checks.
    fn card(&self, card_id: &str) -> Option<DesktopCard> {
        self.snapshot
            .borrow()
            .as_ref()?
            .local
            .cards
            .iter()
            .find(|card| card.card_id == card_id)
            .cloned()
    }

    /// Stop every stream while the overlay is hidden.
    ///
    /// The host keeps its sessions and every card keeps its last frame, so the
    /// hide animation still has something to move and the next show reconnects
    /// without a failure backoff.
    pub fn suspend(&self) {
        for card in self.cards.borrow().values() {
            if let Some(session) = card.remote_session() {
                session.suspend();
            }
        }
    }

    /// Cards that follow the overlay's slide-out, as
    /// `(widget, x, y, width, offset)`: the card's rest position and width
    /// inside the fitted canvas, plus that canvas's position on the viewer's
    /// screen, so a card can find its nearest real screen edge.
    pub fn slide_cards(&self) -> Vec<(gtk4::Widget, f64, f64, f64, f64)> {
        // The canvas is centered in (or panned across) the viewer's area, so
        // its left edge is what turns canvas coordinates into screen ones.
        let offset = self.transform().origin().0;
        let placed = self.placed.borrow().clone();
        self.cards
            .borrow()
            .iter()
            .filter_map(|(id, card)| {
                let (x, y, width) = placed.get(id).copied()?;
                Some((
                    card.container.clone().upcast::<gtk4::Widget>(),
                    x,
                    y,
                    width,
                    offset,
                ))
            })
            .collect()
    }

    /// Show a status line instead of live cards.
    pub fn show_message(&self, text: &str) {
        self.message.set_text(text);
        self.area.set_visible_child_name("message");
    }

    /// Drop every card and its stream. Used when leaving this remote PC, when
    /// the host takes the card away, or when the overlay is hidden: the host
    /// reaps exactly the tmux clients these views held.
    pub fn clear(&self) {
        let cards = std::mem::take(&mut *self.cards.borrow_mut());
        for (_, card) in cards {
            card.close_session();
            self.canvas.remove(&card.container);
        }
        self.placed.borrow_mut().clear();
        self.gesturing.borrow_mut().clear();
        self.pending_moves.borrow_mut().clear();
        self.glides.borrow_mut().clear();
        self.stacking.borrow_mut().clear();
        *self.snapshot.borrow_mut() = None;
        *self.peer.borrow_mut() = None;
        // Another PC decides for itself whether it accepts layout commands.
        self.layout_writable.set(false);
        // The next PC starts at its own mode, unscrolled; its remembered mode
        // is restored by its first snapshot.
        self.scroller.hadjustment().set_value(0.0);
        self.scroller.vadjustment().set_value(0.0);
        self.laid_out_for.set((0, 0));
    }

    /// Render a fresh host snapshot. Cards that did not change keep their
    /// widget, their scrollback and their live stream.
    ///
    /// `layout_writable` is the host's own `workspace-layout-v1` answer: only a
    /// host that accepts commands gets drags, resizes and button presses.
    pub fn apply(
        self: &Rc<Self>,
        peer: &Peer,
        incoming: &WorkspaceSnapshot,
        layout_writable: bool,
    ) {
        {
            let mut pending = self.pending_moves.borrow_mut();
            pending.retain(|id, pos| {
                incoming
                    .local
                    .cards
                    .iter()
                    .find(|card| &card.card_id == id)
                    .is_some_and(|card| !card.expanded && shown_origin(card) != *pos)
            });
        }
        self.layout_writable.set(layout_writable);
        let arriving = self.peer.borrow().as_ref().map(|known| &known.machine_id)
            != Some(&peer.machine_id);
        *self.peer.borrow_mut() = Some(peer.clone());
        if arriving {
            // Each PC keeps the mode last chosen for it this session.
            let mode = self
                .modes
                .borrow()
                .get(&peer.machine_id)
                .copied()
                .unwrap_or_default();
            self.mode.set(mode);
            self.announce_mode();
        }
        // A poll that was already running when we applied a command still
        // carries the older revision. Revisions only grow inside one epoch, so
        // the newer of the two is the truth, and the next gesture is not
        // refused as stale because of a slow snapshot.
        let snapshot = self.newest(incoming);
        *self.snapshot.borrow_mut() = Some(snapshot.clone());
        let local = &snapshot.local;

        let stale: Vec<String> = self
            .cards
            .borrow()
            .keys()
            .filter(|id| !local.cards.iter().any(|card| &card.card_id == *id))
            .cloned()
            .collect();
        for id in stale {
            if let Some(card) = self.cards.borrow_mut().remove(&id) {
                card.close_session();
                self.canvas.remove(&card.container);
            }
            self.placed.borrow_mut().remove(&id);
            self.gesturing.borrow_mut().remove(&id);
            self.pending_moves.borrow_mut().remove(&id);
            self.glides.borrow_mut().remove(&id);
        }

        let ordered = stacked(local);
        let live = live_cards(&ordered);
        let scale = self.scale();
        for host in &ordered {
            // Bound before the match: the borrow in a match scrutinee lives for
            // the whole match, and the empty arm inserts into the same map.
            let existing = self.cards.borrow().get(&host.card_id).cloned();
            let card = match existing {
                Some(card) => card,
                None => {
                    let card = self.build_card(peer, host, scale);
                    self.canvas.put(&card.container, 0.0, 0.0);
                    self.cards
                        .borrow_mut()
                        .insert(host.card_id.clone(), Rc::clone(&card));
                    card
                }
            };
            // The host owns the mode: an iconified or expanded card is drawn
            // that way because the host says so, not because this viewer
            // decided it.
            card.mirror_host_mode(host.layout.iconified, host.expanded);
            let live_now = live.contains(&host.card_id);
            let message = self.card_message(&card, host, live_now);
            card.apply_host_state(&host.title, host.session_alive, message);
            self.drive_stream(&card, host, live_now);
        }
        self.relayout();
    }

    /// Re-attach any console that should be live but lost its stream.
    ///
    /// A stream that ends (a network blip, the host reaping its tmux client)
    /// is retried by the next snapshot this view draws, after the session's
    /// own backoff. With live workspace events a quiet host sends no further
    /// snapshot, so the owner calls this on its regular tick instead: it reads
    /// nothing from the network and changes nothing about the layout, it only
    /// asks each live card's session to attach again when its backoff allows.
    pub fn retry_streams(self: &Rc<Self>) {
        let Some(snapshot) = self.snapshot.borrow().clone() else {
            return;
        };
        let ordered = stacked(&snapshot.local);
        let live = live_cards(&ordered);
        for host in ordered {
            if !live.contains(&host.card_id) {
                continue;
            }
            let card = self.cards.borrow().get(&host.card_id).cloned();
            if let Some(card) = card {
                self.drive_stream(&card, host, true);
            }
        }
    }

    /// What to say about one card, in the viewer's own words: the stream's own
    /// report first, then why this view is not streaming it.
    fn card_message(
        &self,
        card: &Rc<MiniTerminalCard>,
        host: &DesktopCard,
        live: bool,
    ) -> Option<&'static str> {
        if host.session_alive == Some(false) {
            return Some("Host session ended");
        }
        card.remote_session()
            .and_then(|session| session.message())
            .or(if live {
                None
            } else {
                Some("Preview only · at most 8 live consoles")
            })
    }

    /// Attach, keep, or release this card's stream, according to the host's own
    /// state and this view's attachment budget.
    fn drive_stream(self: &Rc<Self>, card: &Rc<MiniTerminalCard>, host: &DesktopCard, live: bool) {
        let Some(session) = card.remote_session().cloned() else {
            return;
        };
        if host.session_alive == Some(false) {
            // The host's session is gone: release the stream and keep the last
            // frame, exactly like a stream that ended on its own.
            session.stop();
            return;
        }
        if host.layout.iconified {
            // An icon is not a terminal. `mirror_host_mode` already released
            // this, and the budget belongs to a card that can use it.
            return;
        }
        if !live {
            session.detach();
            return;
        }
        // Building the emulator starts the stream (see `spawn_vte`); a card
        // that still has its emulator from an earlier attachment only needs a
        // new stream.
        card.attach_vte();
        session.attach();
    }

    /// One remote console, built by the same widget the local workspace uses.
    fn build_card(
        self: &Rc<Self>,
        peer: &Peer,
        host: &DesktopCard,
        scale: f64,
    ) -> Rc<MiniTerminalCard> {
        let id = host.card_id.clone();
        let canvas_for_drag = self.canvas.clone();
        let canvas_for_raise = self.canvas.clone();
        // The resize preview needs the card, which does not exist until the
        // widget is built; this slot closes that loop.
        let slot: Rc<RefCell<Option<Rc<MiniTerminalCard>>>> = Rc::new(RefCell::new(None));

        let weak_update = Rc::downgrade(self);
        let id_update = id.clone();
        let on_drag_update = move |widget: gtk4::Widget, x: f64, y: f64| {
            // The gesture owns this card's geometry until it ends: a snapshot
            // that arrives mid-drag must not pull the card back.
            if let Some(view) = weak_update.upgrade() {
                view.gesturing.borrow_mut().insert(id_update.clone());
            }
            canvas_for_drag.move_(&widget, x, y);
        };
        let weak_end = Rc::downgrade(self);
        let id_end = id.clone();
        let on_drag_end = move |_: gtk4::Widget, data: &TerminalData| {
            if let Some(view) = weak_end.upgrade() {
                view.commit_card_layout(&id_end, data);
            }
        };
        let weak_toggle = Rc::downgrade(self);
        let id_toggle = id.clone();
        let on_toggle = move |_data: &TerminalData| {
            if let Some(view) = weak_toggle.upgrade() {
                view.toggle_expanded(&id_toggle);
            }
        };
        let weak_close = Rc::downgrade(self);
        let on_close = move |session: String| {
            if let Some(view) = weak_close.upgrade() {
                view.close_card(&session);
            }
        };
        let weak_resize = Rc::downgrade(self);
        let id_resize = id.clone();
        let resize_slot = Rc::clone(&slot);
        let canvas_for_resize = self.canvas.clone();
        let on_resize_ghost = move |x: f64, y: f64, width: i32, height: i32, _icon: bool| {
            // The local workspace previews a resize with a dashed outline; a
            // remote card has the host's card to draw instead, so it resizes as
            // the edge is dragged. The commit at the end is what the host is
            // told about.
            if let Some(card) = resize_slot.borrow().as_ref() {
                canvas_for_resize.move_(&card.container, x, y);
                card.container.set_size_request(width, height);
            }
            if let Some(view) = weak_resize.upgrade() {
                view.gesturing.borrow_mut().insert(id_resize.clone());
            }
        };
        let weak_resize_end = Rc::downgrade(self);
        let id_resize_end = id.clone();
        let on_resize_end = move || {
            if let Some(view) = weak_resize_end.upgrade() {
                view.gesturing.borrow_mut().remove(&id_resize_end);
            }
        };
        let canvas_for_raise = canvas_for_raise.clone();
        let on_raise = move |widget: gtk4::Widget| {
            crate::window::raise_canvas_child(&canvas_for_raise, &widget);
        };
        let on_session_persist = |_data: &TerminalData| {};
        let on_interaction = || {};

        let (screen_w, screen_h) = self.fitted_size();
        let card = Rc::new(MiniTerminalCard::new(
            terminal_data(host, scale),
            on_drag_update,
            on_drag_end,
            on_toggle,
            on_close,
            on_resize_ghost,
            on_resize_end,
            on_raise,
            on_session_persist,
            on_interaction,
            screen_w,
            screen_h,
            None,
            None,
            self.hover_lock.clone(),
            crate::card_source::CardSource::Remote {
                peer: peer.clone(),
                card_id: id,
                scale,
            },
        ));
        *slot.borrow_mut() = Some(Rc::clone(&card));
        card
    }

    /// Ask the host to store the geometry a gesture just ended with.
    ///
    /// The gesture worked in this canvas's own pixels; the host owns host
    /// pixels, so the drop is converted back and clamped there. The card's
    /// revision travels with it, so a concurrent host edit is refused as a
    /// conflict rather than overwritten.
    fn commit_card_layout(self: &Rc<Self>, card_id: &str, data: &TerminalData) {
        self.gesturing.borrow_mut().remove(card_id);
        let Some(host) = self.card(card_id) else {
            self.relayout();
            return;
        };
        let scale = self.scale();
        let Some(layout) = host_layout(&host, data, scale) else {
            self.relayout();
            return;
        };
        // Keep the card where it was released until the host confirms it, so a
        // poll that started before this command cannot pull it back.
        let origin = if layout.iconified {
            (
                layout.icon_x.unwrap_or(layout.x),
                layout.icon_y.unwrap_or(layout.y),
            )
        } else {
            (layout.x, layout.y)
        };
        if shown_origin(&host) != origin {
            self.pending_moves
                .borrow_mut()
                .insert(card_id.to_string(), origin);
        }
        self.send(
            card_id,
            WorkspaceCommand::SetLayout {
                card_id: card_id.to_string(),
                expected_revision: host.revision,
                layout,
            },
        );
    }

    /// The card's maximize button or its header double-click: the host owns
    /// whether a card is expanded, so this asks for the opposite of what the
    /// host last reported.
    fn toggle_expanded(self: &Rc<Self>, card_id: &str) {
        let Some(host) = self.card(card_id) else {
            return;
        };
        self.send(
            card_id,
            WorkspaceCommand::SetExpanded {
                card_id: card_id.to_string(),
                expected_revision: host.revision,
                expanded: !host.expanded,
            },
        );
    }

    /// A remote console's close button: the host removes its own card, its
    /// widget and its session, and the next snapshot no longer carries it.
    fn close_card(self: &Rc<Self>, session_name: &str) {
        let Some(host) = self.snapshot.borrow().as_ref().and_then(|snapshot| {
            snapshot
                .local
                .cards
                .iter()
                .find(|card| card.session_name == session_name)
                .cloned()
        }) else {
            return;
        };
        self.send(
            &host.card_id,
            WorkspaceCommand::CloseTerminal {
                card_id: host.card_id.clone(),
                expected_revision: host.revision,
            },
        );
    }

    /// Send one command for a card and take the host's answer as the truth.
    ///
    /// Nothing here is retried. An accepted command and a conflict both carry
    /// the host's published revision and geometry, so this view redraws what
    /// the host really has (a conflict glides the card there); the owner is
    /// asked for a fresh snapshot as well, because a close changes the set of
    /// cards rather than one card's state. Whatever happened is said on the
    /// card itself (see `command_feedback`), and an unknown outcome is only
    /// ever refreshed, never sent again.
    fn send(self: &Rc<Self>, card_id: &str, command: WorkspaceCommand) {
        let (Some(peer), Some(epoch)) = (
            self.peer.borrow().clone(),
            self.snapshot
                .borrow()
                .as_ref()
                .map(|snapshot| snapshot.local.epoch.clone()),
        ) else {
            self.relayout();
            return;
        };
        let Some(kind) = CardCommand::of(&command) else {
            self.relayout();
            return;
        };
        #[cfg(test)]
        if let Some(outbox) = self.outbox.borrow_mut().as_mut() {
            outbox.push(command);
            return;
        }
        let machine = peer.machine_id.clone();
        let request = peer_client::request(&peer, &epoch, command);
        let id = card_id.to_string();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let reply =
                gtk4::gio::spawn_blocking(move || peer_client::command(&peer, &request)).await;
            let Some(view) = weak.upgrade() else {
                return;
            };
            // The user may have moved on to another PC (or away and back)
            // while this was in flight: its answer describes cards that are
            // no longer the ones on screen.
            if view.peer.borrow().as_ref().map(|peer| &peer.machine_id) != Some(&machine) {
                return;
            }
            view.finish(&id, kind, &reply);
        });
    }

    /// Apply one command's answer: geometry first, then the notice, then a
    /// refresh when the decision asks for one.
    fn finish<E>(
        self: &Rc<Self>,
        card_id: &str,
        kind: CardCommand,
        reply: &Result<Result<CommandReply, peer_client::PeerError>, E>,
    ) {
        let feedback = command_feedback::for_card(kind, Outcome::of(reply));
        match (reply, feedback.geometry) {
            (Ok(Ok(reply)), Geometry::Adopt | Geometry::SnapToHost) => {
                self.adopt(card_id, reply, feedback.geometry == Geometry::SnapToHost);
            }
            (Ok(Ok(reply)), Geometry::Revert) => self.adopt(card_id, reply, false),
            _ => {
                self.pending_moves.borrow_mut().remove(card_id);
                self.gesturing.borrow_mut().remove(card_id);
                self.relayout();
            }
        }
        if let Some(notice) = feedback.notice {
            if let Some(card) = self.cards.borrow().get(card_id) {
                card.show_notice(notice);
            }
        }
        if feedback.refresh {
            if let Some(changed) = self.on_changed.borrow().clone() {
                changed();
            }
        }
    }

    /// The snapshot to draw: the host's, with anything we already know to be
    /// newer than it kept. Inside one epoch a card's revision only grows, so a
    /// lower one is a snapshot that was read before our last command.
    fn newest(&self, incoming: &WorkspaceSnapshot) -> WorkspaceSnapshot {
        let mut snapshot = incoming.clone();
        let known = self.snapshot.borrow().clone();
        let Some(known) = known else {
            return snapshot;
        };
        if known.local.epoch != snapshot.local.epoch {
            // A restarted host owns its revisions again; nothing carries over.
            return snapshot;
        }
        for card in &mut snapshot.local.cards {
            let Some(newer) = known
                .local
                .cards
                .iter()
                .find(|known| known.card_id == card.card_id && known.revision > card.revision)
            else {
                continue;
            };
            card.revision = newer.revision;
            card.layout = newer.layout.clone();
            card.expanded = newer.expanded;
        }
        snapshot
    }

    /// Take the host's own answer as the new truth for one card, so the next
    /// gesture is based on the state the host really published.
    ///
    /// Both an accepted command and a conflict carry that state: on a conflict
    /// the card snaps to the geometry the host actually has instead of keeping
    /// an edit the host refused.
    fn adopt(self: &Rc<Self>, card_id: &str, reply: &CommandReply, glide: bool) {
        use crate::desktop_protocol::CommandResult;
        let published = match &reply.result {
            CommandResult::Applied {
                card_revision,
                layout,
                expanded,
                ..
            }
            | CommandResult::Conflict {
                card_revision,
                layout,
                expanded,
                ..
            } => Some((*card_revision, layout.clone(), *expanded)),
            CommandResult::Rejected { .. } => None,
        };
        if let Some((revision, layout, expanded)) = published {
            if let Some(snapshot) = self.snapshot.borrow_mut().as_mut() {
                snapshot.local.revision = reply.revision;
                if let Some(card) = snapshot
                    .local
                    .cards
                    .iter_mut()
                    .find(|card| card.card_id == card_id)
                {
                    if let Some(revision) = revision {
                        card.revision = revision;
                    }
                    if let Some(layout) = layout {
                        card.layout = layout;
                    }
                    if let Some(expanded) = expanded {
                        card.expanded = expanded;
                    }
                }
            }
        }
        self.pending_moves.borrow_mut().remove(card_id);
        self.gesturing.borrow_mut().remove(card_id);
        if glide {
            self.snapping.borrow_mut().insert(card_id.to_string());
        }
        self.relayout();
        self.snapping.borrow_mut().remove(card_id);
    }

    /// Move a card to its host position: at once, or — for a card the host
    /// just refused to move — as a short glide from where the user left it,
    /// so the snap back reads as the host's answer rather than a glitch.
    fn place(self: &Rc<Self>, id: &str, card: &Rc<MiniTerminalCard>, x: f64, y: f64) {
        if let Some(target) = self.glides.borrow_mut().get_mut(id) {
            // Already gliding: a newer answer only moves the destination.
            *target = (x, y);
            return;
        }
        let from = self.canvas.child_position(&card.container);
        let far = (from.0 - x).abs() > 1.0 || (from.1 - y).abs() > 1.0;
        let animate = self.snapping.borrow().contains(id)
            && far
            && card.container.is_mapped()
            && gtk4::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations());
        if !animate {
            self.canvas.move_(&card.container, x, y);
            return;
        }
        self.glides.borrow_mut().insert(id.to_string(), (x, y));
        let weak = Rc::downgrade(self);
        let id = id.to_string();
        let started: Cell<Option<i64>> = Cell::new(None);
        card.container.add_tick_callback(move |widget, clock| {
            let Some(view) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(to) = view.glides.borrow().get(&id).copied() else {
                return glib::ControlFlow::Break;
            };
            // A new gesture owns the card from its first frame.
            if view.gesturing.borrow().contains(&id) {
                view.glides.borrow_mut().remove(&id);
                return glib::ControlFlow::Break;
            }
            let now = clock.frame_time();
            let start = started.get().unwrap_or(now);
            started.set(Some(start));
            let (x, y, done) = glide_step(from, to, now - start);
            view.canvas.move_(widget, x, y);
            if done {
                view.glides.borrow_mut().remove(&id);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    /// Snapshot plus drops the host has not echoed yet.
    fn presented_snapshot(&self) -> Option<WorkspaceSnapshot> {
        let mut snapshot = self.snapshot.borrow().clone()?;
        let pending = self.pending_moves.borrow().clone();
        for card in &mut snapshot.local.cards {
            if card.expanded {
                continue;
            }
            if let Some(&(x, y)) = pending.get(&card.card_id) {
                if card.layout.iconified {
                    card.layout.icon_x = Some(x);
                    card.layout.icon_y = Some(y);
                } else {
                    card.layout.x = x;
                    card.layout.y = y;
                }
            }
        }
        Some(snapshot)
    }

    /// Place and size every card at the host's own geometry, fitted to this
    /// canvas. A card a gesture owns keeps the position the gesture set.
    fn relayout(self: &Rc<Self>) {
        let Some(snapshot) = self.presented_snapshot() else {
            return;
        };
        let local = &snapshot.local;
        let transform = self.transform();
        let scale = transform.scale;
        self.laid_out_for.set(self.viewport_size());
        // The page is exactly the host workspace at this scale: that is the
        // scroll range in 100% mode, and it is centered when smaller.
        self.frame
            .set_size_request(transform.content_width, transform.content_height);
        let policy = match self.mode.get() {
            // Fit shows everything: nothing to scroll, and no scrollbar.
            ViewMode::Fit => gtk4::PolicyType::External,
            ViewMode::Actual { .. } => gtk4::PolicyType::Automatic,
        };
        self.scroller.set_policy(policy, policy);
        if matches!(self.mode.get(), ViewMode::Fit) {
            self.show_pan(&transform);
        }
        let cards = self.cards.borrow().clone();
        for (id, card) in &cards {
            let Some(host) = local.cards.iter().find(|host| &host.card_id == id) else {
                continue;
            };
            if self.gesturing.borrow().contains(id) {
                continue;
            }
            let rect = remote_workspace::card_rect(host, local.canvas.width, local.canvas.height);
            let (x, y) = (rect.x * scale, rect.y * scale);
            let width = (rect.width * scale).round().max(24.0) as i32;
            let height = (rect.height * scale).round().max(24.0) as i32;
            {
                let mut data = card.data.borrow_mut();
                crate::mini_terminal::set_displayed_pos(
                    &mut data,
                    x.round() as i32,
                    y.round() as i32,
                );
                data.width = width;
                data.height = height;
                data.restored_width =
                    (f64::from(host.layout.restored_width) * scale).round().max(1.0) as i32;
                data.restored_height =
                    (f64::from(host.layout.restored_height) * scale).round().max(1.0) as i32;
                data.tag = host.layout.tag;
                data.workspace_dir = Some(host.workspace.clone());
            }
            card.set_workspace_size(transform.content_width, transform.content_height);
            card.adopt_host_geometry(width, height);
            // The font follows the host's grid, not this machine's theme: the
            // emulator has to line up with the host's own columns and rows.
            let header = if card.header_visible() {
                ((HEADER_HEIGHT * scale).round() as i32).clamp(16, (height / 2).max(16))
            } else {
                0
            };
            card.set_fit(width as f64, f64::from((height - header).max(1)), scale);
            self.place(id, card, x, y);
            self.placed
                .borrow_mut()
                .insert(id.clone(), (x, y, width as f64));
        }
        // The host owns stacking. Re-impose it only when it changed, so a local
        // click-raise is not undone by every poll.
        let order: Vec<String> = {
            let mut ordered: Vec<&DesktopCard> = local.cards.iter().collect();
            ordered.sort_by_key(|card| (card.expanded, card.stacking_order));
            ordered.iter().map(|card| card.card_id.clone()).collect()
        };
        if *self.stacking.borrow() != order {
            for id in &order {
                if let Some(card) = cards.get(id) {
                    crate::window::raise_canvas_child(&self.canvas, &card.container);
                }
            }
            *self.stacking.borrow_mut() = order;
        }
        self.area.set_visible_child_name("canvas");
    }

    /// The host workspace's logical size, once a snapshot has said it.
    fn host_size(&self) -> Option<(u32, u32)> {
        self.snapshot
            .borrow()
            .as_ref()
            .map(|snapshot| (snapshot.local.canvas.width, snapshot.local.canvas.height))
    }

    /// The viewport's allocated logical size.
    fn viewport_size(&self) -> (i32, i32) {
        (self.scroller.width(), self.scroller.height())
    }

    /// The one host ↔ viewer mapping for the current mode, viewport and pan.
    /// Fit follows the plan's rule (`min(1, Vw/Hw, Vh/Hh)`, centered, never
    /// enlarged); 100% is one host pixel per viewer pixel times the zoom.
    pub fn transform(&self) -> ViewTransform {
        let (host_width, host_height) = self.host_size().unwrap_or((1, 1));
        let (width, height) = self.viewport_size();
        ViewTransform::new(
            self.mode.get(),
            host_width,
            host_height,
            width,
            height,
            (
                self.scroller.hadjustment().value(),
                self.scroller.vadjustment().value(),
            ),
        )
    }

    fn scale(&self) -> f64 {
        self.transform().scale
    }

    /// The whole host canvas in this view's pixels. A card's own gestures are
    /// bounded by it, exactly as a local card is bounded by the screen.
    fn fitted_size(&self) -> (i32, i32) {
        if self.host_size().is_none() {
            return (0, 0);
        }
        let transform = self.transform();
        (transform.content_width, transform.content_height)
    }
}

/// The card data a remote console is built from. Its geometry is this canvas's
/// own pixels; the host's is what `relayout` fits into them.
fn terminal_data(host: &DesktopCard, scale: f64) -> TerminalData {
    TerminalData {
        id: host.card_id.clone(),
        session_name: host.session_name.clone(),
        agent_type: host.agent_type.clone(),
        // A remote card starts no process here, so it has no command of its own.
        command: String::new(),
        x: (f64::from(host.layout.x) * scale).round() as i32,
        y: (f64::from(host.layout.y) * scale).round() as i32,
        width: 1,
        height: 1,
        restored_width: (f64::from(host.layout.restored_width) * scale).round().max(1.0) as i32,
        restored_height: (f64::from(host.layout.restored_height) * scale)
            .round()
            .max(1.0) as i32,
        iconified: host.layout.iconified,
        icon_x: host.layout.icon_x.map(|x| (f64::from(x) * scale).round() as i32),
        icon_y: host.layout.icon_y.map(|y| (f64::from(y) * scale).round() as i32),
        created_at: 0.0,
        tag: host.layout.tag,
        agent_session_id: None,
        workspace_dir: Some(host.workspace.clone()),
    }
}

/// The layout to ask the host for, translated out of this canvas's pixels.
///
/// Moving, resizing and iconifying are the same command because the card's own
/// gestures already wrote all of them into `data`: one drop carries whatever
/// the user changed.
fn host_layout(host: &DesktopCard, data: &TerminalData, scale: f64) -> Option<CardLayout> {
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    const MAX: i32 = 32768;
    let to_host = |value: f64| (value / scale).round() as i32;
    let mut layout = host.layout.clone();
    if data.iconified {
        layout.iconified = true;
        let (x, y) = (
            data.icon_x.unwrap_or(data.x),
            data.icon_y.unwrap_or(data.y),
        );
        layout.icon_x = Some(to_host(f64::from(x)).clamp(-MAX, MAX));
        layout.icon_y = Some(to_host(f64::from(y)).clamp(-MAX, MAX));
    } else {
        layout.iconified = false;
        layout.x = to_host(f64::from(data.x)).clamp(-MAX, MAX);
        layout.y = to_host(f64::from(data.y)).clamp(-MAX, MAX);
        let width = to_host(f64::from(data.width)).clamp(1, MAX) as u32;
        let height = to_host(f64::from(data.height)).clamp(1, MAX) as u32;
        layout.width = width;
        layout.height = height;
        layout.restored_width = width;
        layout.restored_height = height;
    }
    Some(layout)
}

/// How long a refused card takes to glide back to the host's position.
const GLIDE_MICROS: i64 = 220_000;

/// One frame of the snap-back glide: ease-out from `from` to `to`, `elapsed`
/// microseconds in. Returns the position and whether the glide is over.
fn glide_step(from: (f64, f64), to: (f64, f64), elapsed: i64) -> (f64, f64, bool) {
    let t = (elapsed.max(0) as f64 / GLIDE_MICROS as f64).min(1.0);
    let eased = 1.0 - (1.0 - t).powi(3);
    (
        from.0 + (to.0 - from.0) * eased,
        from.1 + (to.1 - from.1) * eased,
        t >= 1.0,
    )
}

/// Where the card is drawn on the host: the icon spot when minimized, otherwise
/// the saved card origin.
fn shown_origin(card: &DesktopCard) -> (i32, i32) {
    if card.layout.iconified {
        (
            card.layout.icon_x.unwrap_or(card.layout.x),
            card.layout.icon_y.unwrap_or(card.layout.y),
        )
    } else {
        (card.layout.x, card.layout.y)
    }
}

/// The host's cards back to front, with expanded ones on top, as the host
/// stacks them.
fn stacked(local: &LocalWorkspaceSnapshot) -> Vec<&DesktopCard> {
    let mut ordered: Vec<&DesktopCard> = local.cards.iter().collect();
    ordered.sort_by_key(|card| (card.expanded, card.stacking_order));
    ordered
}

/// Topmost first: when the host shows more consoles than the attach budget
/// allows, the ones in front are the ones with live output.
fn live_cards(ordered: &[&DesktopCard]) -> Vec<String> {
    ordered
        .iter()
        .filter(|card| streamable(card))
        .take(MAX_REMOTE_VIEWERS)
        .map(|card| card.card_id.clone())
        .collect()
}

/// Whether this card should hold one of the viewer's attachments. An icon is
/// not a terminal, and a session the host reports as gone has nothing to show.
fn streamable(card: &DesktopCard) -> bool {
    !card.layout.iconified && card.session_alive != Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_visible_live_consoles_get_a_stream() {
        let mut card = crate::remote_workspace::fixture().local.cards.remove(0);
        assert!(streamable(&card));
        card.layout.iconified = true;
        assert!(!streamable(&card));
        card.layout.iconified = false;
        card.session_alive = Some(false);
        assert!(!streamable(&card));
    }

    #[test]
    fn a_gesture_is_translated_out_of_the_view_s_own_pixels() {
        let host = crate::remote_workspace::fixture().local.cards.remove(0);
        let mut data = terminal_data(&host, 0.5);
        data.x = 350;
        data.y = 100;
        data.width = 320;
        data.height = 240;
        let layout = host_layout(&host, &data, 0.5).unwrap();
        // At half scale everything doubles on its way back to host pixels.
        assert_eq!((layout.x, layout.y), (700, 200));
        assert_eq!((layout.width, layout.height), (640, 480));
        // A scale that cannot be inverted is refused rather than guessed at.
        assert!(host_layout(&host, &data, 0.0).is_none());

        // An icon moves in its own slot and keeps the saved card origin.
        data.iconified = true;
        data.icon_x = Some(20);
        data.icon_y = Some(40);
        let layout = host_layout(&host, &data, 0.5).unwrap();
        assert!(layout.iconified);
        assert_eq!((layout.icon_x, layout.icon_y), (Some(40), Some(80)));
        assert_eq!((layout.x, layout.y), (host.layout.x, host.layout.y));
    }

    #[test]
    fn a_refused_card_glides_back_to_the_host_and_settles_there() {
        let (x, y, done) = glide_step((0.0, 0.0), (100.0, 50.0), 0);
        assert_eq!((x, y, done), (0.0, 0.0, false));
        let (x, _, done) = glide_step((0.0, 0.0), (100.0, 50.0), GLIDE_MICROS / 2);
        // Ease-out: past halfway at half time, never overshooting.
        assert!(x > 50.0 && x < 100.0 && !done);
        assert_eq!(glide_step((0.0, 0.0), (100.0, 50.0), GLIDE_MICROS * 3), (100.0, 50.0, true));
    }

    #[test]
    fn command_results_reach_the_card_chrome() {
        crate::gtk_test::run_in_child_process("remote_terminal::tests::feedback_inner");
    }

    #[test]
    fn feedback_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        use crate::desktop_protocol::CommandResult;
        gtk4::init().unwrap();
        let canvas = RemoteCanvas::new();
        let refreshes = Rc::new(Cell::new(0));
        canvas.set_on_changed(Rc::new({
            let refreshes = Rc::clone(&refreshes);
            move || refreshes.set(refreshes.get() + 1)
        }));
        let mut snapshot = crate::remote_workspace::fixture();
        snapshot.local.cards[0].session_alive = Some(false);
        let peer = peer_client::test_peer('a');
        canvas.apply(&peer, &snapshot, true);
        let card = canvas.card_widget("card-one").unwrap();
        assert_eq!(card.notice_text(), None);

        // A drop the host refused: the card takes the host's own geometry and
        // says why it moved.
        let mut host_layout = snapshot.local.cards[0].layout.clone();
        host_layout.x = 300;
        host_layout.y = 400;
        let reply = |result| CommandReply {
            request_id: "m1".into(),
            machine_id: peer.machine_id.clone(),
            epoch: "host-one".into(),
            revision: 5,
            result,
        };
        let conflict: Result<_, ()> = Ok(Ok(reply(CommandResult::Conflict {
            card_id: "card-one".into(),
            card_revision: Some(4),
            layout: Some(host_layout),
            expanded: Some(false),
        })));
        canvas.finish("card-one", CardCommand::Layout, &conflict);
        assert_eq!(canvas.card_position("card-one"), Some((300, 400)));
        assert_eq!(canvas.card_revision("card-one"), Some(4));
        assert_eq!(
            card.notice_text().as_deref(),
            Some("Changed on that PC · showing its layout")
        );
        assert!(card.notice_has_class("term-notice-info"));
        assert_eq!(refreshes.get(), 1);

        // No answer at all: nothing is claimed, the view refreshes to find
        // out, and the command is not sent again.
        let unknown: Result<Result<CommandReply, peer_client::PeerError>, ()> =
            Ok(Err(peer_client::PeerError(command_feedback::OUTCOME_UNKNOWN)));
        canvas.finish("card-one", CardCommand::Close, &unknown);
        assert_eq!(
            card.notice_text().as_deref(),
            Some("Result unknown · check before retrying")
        );
        assert!(card.notice_has_class("term-notice-warning"));
        assert!(!card.notice_has_class("term-notice-info"));
        assert_eq!(refreshes.get(), 2);
        assert_eq!(canvas.card_position("card-one"), Some((300, 400)));

        // An unreachable host: said on the card, left to the regular poll.
        let unreachable: Result<Result<CommandReply, peer_client::PeerError>, ()> =
            Ok(Err(peer_client::PeerError("connection_failed_or_pin_mismatch")));
        canvas.finish("card-one", CardCommand::Expand, &unreachable);
        assert_eq!(
            card.notice_text().as_deref(),
            Some("Cannot reach that PC · change not applied")
        );
        assert_eq!(refreshes.get(), 2);
        assert_eq!(canvas.card_count(), 1);
    }

    #[test]
    fn a_drag_at_100_percent_with_pan_sends_host_geometry() {
        crate::gtk_test::run_in_child_process("remote_terminal::tests::pan_zoom_inner");
    }

    #[test]
    fn pan_zoom_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        use crate::desktop_protocol::CommandResult;
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let canvas = RemoteCanvas::new();
        canvas.capture_commands();
        let window = gtk4::Window::new();
        window.set_default_size(960, 600);
        window.set_resizable(false);
        window.set_child(Some(&canvas.area));
        window.present();
        let pump = || {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(150);
            while std::time::Instant::now() < until {
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        // Host 1920×1080 with one card at (100, 200), 640×480. Its session is
        // gone, so this check opens no socket.
        let mut snapshot = crate::remote_workspace::fixture();
        snapshot.local.cards[0].session_alive = Some(false);
        let peer = peer_client::test_peer('a');
        canvas.apply(&peer, &snapshot, true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while canvas.viewport().width() != 960 || canvas.viewport().height() != 600 {
            pump();
            assert!(std::time::Instant::now() < deadline, "viewport never allocated");
        }
        // The first layout ran before the viewport had its size; the
        // allocation itself triggers the refit.
        pump();
        let card = canvas.card_widget("card-one").unwrap();
        // Where GTK really drew the card, in viewport pixels (through the
        // window, the root both share). The card's own 1 px border is inside
        // the tolerance.
        let drawn_card = |card: &Rc<MiniTerminalCard>| {
            let origin = gtk4::graphene::Point::new(0.0, 0.0);
            // A move queues a new allocation, and GTK reports no transform
            // until it has run: let the frame finish first.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let card_at = loop {
                if let Some(point) = card.container.compute_point(&window, &origin) {
                    break point;
                }
                assert!(std::time::Instant::now() < deadline, "card never allocated");
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            };
            let view_at = canvas.viewport().compute_point(&window, &origin).unwrap();
            (
                f64::from(card_at.x() - view_at.x()),
                f64::from(card_at.y() - view_at.y()),
            )
        };
        let drawn = || drawn_card(&card);
        let near = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() <= 1.5 && (a.1 - b.1).abs() <= 1.5;

        // Fit (the default): half scale, the transform matches GTK's layout,
        // and a drop doubles on its way back to host pixels.
        assert_eq!(canvas.mode(), ViewMode::Fit);
        let t = canvas.transform();
        assert_eq!(t.scale, 0.5);
        assert!(near(drawn(), t.host_to_view(100.0, 200.0)), "{:?}", drawn());
        let at = t.host_to_view(100.0, 200.0);
        canvas.drag_card_in_view("card-one", (at.0 + 20.0, at.1 + 8.0), (at.0 + 120.0, at.1 + 58.0));
        match canvas.captured().last() {
            Some(WorkspaceCommand::SetLayout { card_id, expected_revision, layout }) => {
                assert_eq!(card_id, "card-one");
                assert_eq!(*expected_revision, 1);
                assert_eq!((layout.x, layout.y), (300, 300));
                assert_eq!((layout.width, layout.height), (640, 480));
            }
            other => panic!("expected a layout command, got {other:?}"),
        }

        // The host refuses: the card goes back to the host's own geometry.
        let reply = |result| CommandReply {
            request_id: "m1".into(),
            machine_id: peer.machine_id.clone(),
            epoch: "host-one".into(),
            revision: 2,
            result,
        };
        let refuse = |canvas: &Rc<RemoteCanvas>, revision: u64| {
            let host = snapshot.local.cards[0].layout.clone();
            let conflict: Result<_, ()> = Ok(Ok(reply(CommandResult::Conflict {
                card_id: "card-one".into(),
                card_revision: Some(revision),
                layout: Some(host),
                expanded: Some(false),
            })));
            canvas.finish("card-one", CardCommand::Layout, &conflict);
        };
        refuse(&canvas, 2);
        assert_eq!(canvas.card_position("card-one"), Some((100, 200)));

        // 100%: one host pixel per viewer pixel, and switching sends nothing.
        let sent = canvas.captured().len();
        canvas.set_mode(ViewMode::Actual { zoom: 1.0 }, Some((0.0, 0.0)));
        pump();
        assert_eq!(canvas.captured().len(), sent, "a view change is not a host change");
        let t = canvas.transform();
        assert_eq!(t.scale, 1.0);
        assert_eq!((t.content_width, t.content_height), (1920, 1080));
        // The emulator is fitted at 100%: a crisp font for the host's grid,
        // never a scaled bitmap and never a new grid for the host.
        assert_eq!(card.fit_scale(), 1.0);
        // The card's own resize bounds follow the workspace it now sits in.
        let limits = card.resize_limits();
        assert_eq!(limits.right, 1910.0);
        assert_eq!(limits.bottom, 1070.0);

        // Pan like a scrollbar or a drag on empty canvas would.
        canvas.viewport().hadjustment().set_value(60.0);
        canvas.viewport().vadjustment().set_value(150.0);
        pump();
        let t = canvas.transform();
        assert_eq!((t.pan_x, t.pan_y), (60.0, 150.0));
        assert!(near(drawn(), (40.0, 50.0)), "{:?}", drawn());
        assert!(near(drawn(), t.host_to_view(100.0, 200.0)));
        // Grab the header where it is drawn, drop it 300 → and 100 ↓.
        canvas.drag_card_in_view("card-one", (60.0, 58.0), (360.0, 158.0));
        match canvas.captured().last() {
            Some(WorkspaceCommand::SetLayout { expected_revision, layout, .. }) => {
                // The revision the conflict published travels with the drop.
                assert_eq!(*expected_revision, 2);
                assert_eq!((layout.x, layout.y), (400, 300));
                assert_eq!((layout.width, layout.height), (640, 480));
            }
            other => panic!("expected a layout command, got {other:?}"),
        }
        // Conflict feedback still works at 100% with a pan: the host's own
        // geometry is adopted and said on the card.
        refuse(&canvas, 3);
        pump();
        assert_eq!(canvas.card_position("card-one"), Some((100, 200)));
        assert_eq!(
            card.notice_text().as_deref(),
            Some("Changed on that PC · showing its layout")
        );

        // Zoom around a point keeps the host point under it, and a drop at
        // 200% halves on its way back to host pixels.
        let anchor = (400.0, 300.0);
        let under = canvas.transform().view_to_host(anchor.0, anchor.1);
        canvas.set_mode(ViewMode::Actual { zoom: 2.0 }, Some(anchor));
        pump();
        let t = canvas.transform();
        assert_eq!(t.scale, 2.0);
        let after = t.view_to_host(anchor.0, anchor.1);
        assert!((after.0 - under.0).abs() <= 1.0 && (after.1 - under.1).abs() <= 1.0);
        let at = t.host_to_view(100.0, 200.0);
        assert!(near(drawn(), at), "{:?} vs {at:?}", drawn());
        assert_eq!(card.fit_scale(), 2.0);
        canvas.drag_card_in_view("card-one", (at.0 + 30.0, at.1 + 10.0), (at.0 + 230.0, at.1 - 90.0));
        match canvas.captured().last() {
            Some(WorkspaceCommand::SetLayout { expected_revision, layout, .. }) => {
                assert_eq!(*expected_revision, 3);
                assert_eq!((layout.x, layout.y), (200, 150));
                assert_eq!((layout.width, layout.height), (640, 480));
            }
            other => panic!("expected a layout command, got {other:?}"),
        }

        // The mode belongs to this PC for the session: another PC starts in
        // Fit, and coming back restores 200%.
        canvas.clear();
        let other = peer_client::test_peer('b');
        let elsewhere = snapshot.clone();
        canvas.apply(&other, &elsewhere, true);
        assert_eq!(canvas.mode(), ViewMode::Fit);
        canvas.clear();
        canvas.apply(&peer, &snapshot, true);
        assert_eq!(canvas.mode(), ViewMode::Actual { zoom: 2.0 });
        // Back to Fit: nothing to pan, everything visible again.
        canvas.set_mode(ViewMode::Fit, None);
        pump();
        let t = canvas.transform();
        assert_eq!((t.scale, t.pan_x, t.pan_y), (0.5, 0.0, 0.0));
        // The card was rebuilt for the new visit.
        let card = canvas.card_widget("card-one").unwrap();
        assert!(near(drawn_card(&card), t.host_to_view(100.0, 200.0)));
        window.close();
    }

    #[test]
    fn remote_consoles_are_the_same_widget_the_local_workspace_uses() {
        crate::gtk_test::run_in_child_process("remote_terminal::tests::widget_inner");
    }

    #[test]
    fn widget_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let canvas = RemoteCanvas::new();
        // A host whose session is gone renders as a card but is never streamed,
        // so this check opens no socket.
        let mut snapshot = crate::remote_workspace::fixture();
        snapshot.local.cards[0].session_alive = Some(false);
        canvas.apply(&peer_client::test_peer('a'), &snapshot, true);

        let card = canvas.card_widget("card-one").expect("one remote console");
        // The same widget, with the same chrome and gestures the local
        // workspace shows: that is what makes one UI serve both.
        assert!(card.remote_session().is_some());
        assert!(card.data.borrow().session_name.starts_with("sd_term_"));
        // It says what the host said, not what this machine's tmux knows: the
        // title comes from the snapshot, and the badge marks it as remote.
        assert_eq!(canvas.card_count(), 1);
        assert!(card.footer_text().contains("Host session ended"));

        // The host's card leaves: the widget goes with it, and the stream that
        // fed it is released.
        snapshot.local.cards.clear();
        canvas.apply(&peer_client::test_peer('a'), &snapshot, true);
        assert_eq!(canvas.card_count(), 0);
    }
}
