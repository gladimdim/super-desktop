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
    CardLayout, CommandReply, DesktopCard, WorkspaceSnapshot, WorkspaceCommand, MAX_REMOTE_VIEWERS,
};
use crate::mini_terminal::MiniTerminalCard;
use crate::peer_client::{self, Peer};
use crate::remote_workspace;
use crate::state::TerminalData;
use gtk4::{glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Header height at 100% scale. Every other card dimension comes from the
/// host's logical rectangle, multiplied by this view's fit scale.
const HEADER_HEIGHT: f64 = 30.0;

/// The host's workspace, rendered live inside the viewer's canvas.
pub struct RemoteCanvas {
    /// `message` (status/errors) or `canvas` (live cards).
    pub area: gtk4::Stack,
    canvas: gtk4::Fixed,
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
        let holder = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        holder.set_halign(gtk4::Align::Center);
        holder.set_valign(gtk4::Align::Center);
        let canvas = gtk4::Fixed::new();
        canvas.add_css_class("remote-canvas");
        holder.append(&canvas);
        area.add_named(&holder, Some("canvas"));
        area.set_visible_child_name("message");
        let view = Rc::new(Self {
            area,
            canvas,
            cards: RefCell::new(HashMap::new()),
            placed: RefCell::new(HashMap::new()),
            gesturing: RefCell::new(HashSet::new()),
            pending_moves: RefCell::new(HashMap::new()),
            stacking: RefCell::new(Vec::new()),
            hover_lock: crate::mini_terminal::HoverRaiseLock::new(),
            snapshot: RefCell::new(None),
            peer: RefCell::new(None),
            layout_writable: Cell::new(false),
            on_changed: RefCell::new(None),
            message,
        });
        // The viewer's own monitor can change while the overlay stays mapped
        // (display switch, dock), so the fit follows the actual allocation.
        for property in ["width", "height"] {
            let weak = Rc::downgrade(&view);
            view.area.connect_notify_local(Some(property), move |_, _| {
                if let Some(view) = weak.upgrade() {
                    view.relayout();
                }
            });
        }
        view
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
        // The fitted canvas is centered in the viewer's area, so its left edge
        // is what turns canvas coordinates into screen coordinates.
        let offset = ((self.area.width() - self.canvas.width()) as f64 / 2.0).max(0.0);
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
        self.stacking.borrow_mut().clear();
        *self.snapshot.borrow_mut() = None;
        *self.peer.borrow_mut() = None;
        // Another PC decides for itself whether it accepts layout commands.
        self.layout_writable.set(false);
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
        *self.peer.borrow_mut() = Some(peer.clone());
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
        }

        // Topmost first: when the host shows more consoles than the attach
        // budget allows, the ones in front are the ones with live output.
        let mut ordered: Vec<&DesktopCard> = local.cards.iter().collect();
        ordered.sort_by_key(|card| (card.expanded, card.stacking_order));
        let live: Vec<String> = ordered
            .iter()
            .filter(|card| streamable(card))
            .take(MAX_REMOTE_VIEWERS)
            .map(|card| card.card_id.clone())
            .collect();
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
    /// the host really has; the owner is asked for a fresh snapshot as well,
    /// because a close changes the set of cards rather than one card's state.
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
        let request = peer_client::request(&peer, &epoch, command);
        let id = card_id.to_string();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let reply =
                gtk4::gio::spawn_blocking(move || peer_client::command(&peer, &request)).await;
            let Some(view) = weak.upgrade() else {
                return;
            };
            match reply {
                Ok(Ok(reply)) => {
                    view.adopt(&id, &reply);
                    if let Some(changed) = view.on_changed.borrow().clone() {
                        changed();
                    }
                }
                _ => {
                    view.pending_moves.borrow_mut().remove(&id);
                    view.gesturing.borrow_mut().remove(&id);
                    view.relayout();
                }
            }
        });
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
    fn adopt(self: &Rc<Self>, card_id: &str, reply: &CommandReply) {
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
        self.relayout();
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
        let (scale, ..) = self.fit();
        self.canvas.set_size_request(
            (f64::from(local.canvas.width) * scale).round() as i32,
            (f64::from(local.canvas.height) * scale).round() as i32,
        );
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
            card.adopt_host_geometry(width, height);
            // The font follows the host's grid, not this machine's theme: the
            // emulator has to line up with the host's own columns and rows.
            let header = if card.header_visible() {
                ((HEADER_HEIGHT * scale).round() as i32).clamp(16, (height / 2).max(16))
            } else {
                0
            };
            card.set_fit(width as f64, f64::from((height - header).max(1)), scale);
            self.canvas.move_(&card.container, x, y);
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

    /// The fit scale for the current snapshot: the rule the plan states, which
    /// never enlarges a host card.
    fn fit(&self) -> (f64, f64, f64) {
        let Some(snapshot) = self.snapshot.borrow().clone() else {
            return (1.0, 0.0, 0.0);
        };
        remote_workspace::fit(
            snapshot.local.canvas.width,
            snapshot.local.canvas.height,
            self.area.width() as f64,
            self.area.height() as f64,
        )
    }

    fn scale(&self) -> f64 {
        self.fit().0
    }

    /// The whole host canvas in this view's pixels. A card's own gestures are
    /// bounded by it, exactly as a local card is bounded by the screen.
    fn fitted_size(&self) -> (i32, i32) {
        let Some(snapshot) = self.snapshot.borrow().clone() else {
            return (0, 0);
        };
        let (scale, ..) = self.fit();
        (
            (f64::from(snapshot.local.canvas.width) * scale).round() as i32,
            (f64::from(snapshot.local.canvas.height) * scale).round() as i32,
        )
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
