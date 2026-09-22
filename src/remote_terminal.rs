//! Live remote consoles, drawn at the host's own card geometry.
//!
//! The host owns the terminal grid and the layout; this view renders what the
//! host reports and does not write card geometry back. Cards keep the host's
//! positions, sizes, stacking order and iconified state. Each streamed card is
//! a VTE widget fed by the network stream. Keystrokes, paste and IME are the
//! bytes VTE commits for that widget: they are forwarded only after the host's
//! `attached` handshake, and a hide or machine switch drops anything still
//! queued. Creating, closing and dragging cards stay local to the host.
use crate::desktop_protocol::{DesktopCard, TerminalSize, WorkspaceSnapshot, MAX_REMOTE_VIEWERS};
use crate::peer_client::{self, Peer};
use crate::peer_terminal::{Event as StreamEvent, TerminalStream};
use crate::remote_workspace::{self, Rect};
use futures_util::StreamExt;
use gtk4::{glib, prelude::*};
use vte4::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A failed or ended attachment waits this long before the next attempt, so
/// eight cards cannot hammer an offline or refusing host.
const RETRY_AFTER: Duration = Duration::from_secs(5);
/// Header height at 100% scale. Every other card dimension comes from the
/// host's logical rectangle, multiplied by the viewer's fit scale.
const HEADER_HEIGHT: f64 = 30.0;
/// Font bounds for the fitted terminal. A card too small for the host grid
/// clips, exactly like a small local card does.
const MIN_FONT: f64 = 3.0;
const MAX_FONT: f64 = 40.0;

/// The host's workspace, rendered live inside the viewer's canvas.
pub struct RemoteCanvas {
    /// `message` (status/errors) or `canvas` (live cards).
    pub area: gtk4::Stack,
    canvas: gtk4::Fixed,
    cards: RefCell<HashMap<String, Rc<RemoteCard>>>,
    snapshot: RefCell<Option<WorkspaceSnapshot>>,
    peer: RefCell<Option<Peer>>,
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
            snapshot: RefCell::new(None),
            peer: RefCell::new(None),
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

    /// Number of rendered cards. Used by regression tests, which cannot open a
    /// real remote connection.
    #[cfg(test)]
    pub fn card_count(&self) -> usize {
        self.cards.borrow().len()
    }

    /// Stop every stream while the overlay is hidden.
    ///
    /// The host keeps its sessions and every card keeps its last frame, so the
    /// hide animation still has something to move and the next show reconnects
    /// without a failure backoff.
    pub fn suspend(&self) {
        for card in self.cards.borrow().values() {
            card.suspend();
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
        self.cards
            .borrow()
            .values()
            .filter_map(|card| {
                let (x, y, width) = card.rest()?;
                Some((
                    card.root.clone().upcast::<gtk4::Widget>(),
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
            card.detach();
            self.canvas.remove(&card.root);
        }
        *self.snapshot.borrow_mut() = None;
        *self.peer.borrow_mut() = None;
    }

    /// Render a fresh host snapshot. Cards that did not change keep their
    /// widget, their scrollback and their live stream.
    pub fn apply(self: &Rc<Self>, peer: &Peer, snapshot: &WorkspaceSnapshot) {
        *self.peer.borrow_mut() = Some(peer.clone());
        *self.snapshot.borrow_mut() = Some(snapshot.clone());
        let stale: Vec<String> = self
            .cards
            .borrow()
            .keys()
            .filter(|id| {
                !snapshot
                    .local
                    .cards
                    .iter()
                    .any(|card| &card.card_id == *id)
            })
            .cloned()
            .collect();
        for id in stale {
            if let Some(card) = self.cards.borrow_mut().remove(&id) {
                card.detach();
                self.canvas.remove(&card.root);
            }
        }
        // Topmost first: when the host shows more consoles than the attach
        // budget allows, the ones in front are the ones with live output.
        let mut ordered: Vec<&DesktopCard> = snapshot.local.cards.iter().collect();
        ordered.sort_by_key(|card| (card.expanded, card.stacking_order));
        let live: Vec<String> = ordered
            .iter()
            .filter(|card| streamable(card))
            .take(MAX_REMOTE_VIEWERS)
            .map(|card| card.card_id.clone())
            .collect();
        for card in &ordered {
            let existing = self.cards.borrow().get(&card.card_id).cloned();
            let widget = match existing {
                Some(widget) => widget,
                None => {
                    let widget = RemoteCard::new(card);
                    self.canvas.put(&widget.root, 0.0, 0.0);
                    self.cards
                        .borrow_mut()
                        .insert(card.card_id.clone(), Rc::clone(&widget));
                    widget
                }
            };
            widget.update(card, live.contains(&card.card_id), peer);
        }
        self.relayout();
    }

    fn relayout(self: &Rc<Self>) {
        let Some(snapshot) = self.snapshot.borrow().clone() else {
            return;
        };
        let (scale, ..) = remote_workspace::fit(
            snapshot.local.canvas.width,
            snapshot.local.canvas.height,
            self.area.width() as f64,
            self.area.height() as f64,
        );
        self.canvas.set_size_request(
            (snapshot.local.canvas.width as f64 * scale).round() as i32,
            (snapshot.local.canvas.height as f64 * scale).round() as i32,
        );
        let cards = self.cards.borrow().clone();
        for (id, card) in cards {
            let Some(host) = snapshot
                .local
                .cards
                .iter()
                .find(|candidate| candidate.card_id == id)
            else {
                continue;
            };
            let rect = remote_workspace::card_rect(
                host,
                snapshot.local.canvas.width,
                snapshot.local.canvas.height,
            );
            self.canvas.move_(&card.root, rect.x * scale, rect.y * scale);
            card.set_rest(
                rect.x * scale,
                rect.y * scale,
                (rect.width * scale).round(),
            );
            card.fit(&rect, scale);
        }
        self.area.set_visible_child_name("canvas");
    }
}

/// A card holds a live stream when the host session exists and the card shows a
/// console rather than an icon. Iconified cards keep the host's compact tile.
fn streamable(card: &DesktopCard) -> bool {
    !card.layout.iconified && card.session_alive != Some(false)
}

struct RemoteCard {
    card_id: String,
    root: gtk4::Box,
    header: gtk4::Box,
    title: gtk4::Label,
    status: gtk4::Label,
    body: gtk4::Overlay,
    placeholder: gtk4::Label,
    terminal: RefCell<Option<vte4::Terminal>>,
    stream: RefCell<Option<TerminalStream>>,
    grid: Cell<Option<TerminalSize>>,
    /// Earliest time a new attachment may be attempted.
    next_attempt: Cell<Instant>,
    /// Last applied geometry, so an unchanged card does not restyle its font.
    fitted: Cell<Option<(i32, i32, i32, u64)>>,
    /// Rest pose inside the fitted canvas: where the slide starts and ends.
    rest: Cell<Option<(f64, f64, f64)>>,
    /// Last measured body size and fit scale, so a grid that arrives after
    /// layout can still size the font for it.
    body_size: Cell<Option<(f64, f64, f64)>>,
    /// The stream was stopped on purpose (hide, or a card that is not
    /// streamable), so its end must not look like a failure.
    suspended: Cell<bool>,
    /// Whether any host bytes ever reached the emulator. A stream that ends
    /// before the first byte leaves an empty terminal overlay hiding the
    /// placeholder; that widget must step aside so the reason is visible.
    fed: Cell<bool>,
    /// The current stream has received the host's `attached` frame. Commits
    /// before that, and after suspend, are dropped rather than queued across
    /// a reconnect.
    input_ready: Cell<bool>,
}

impl RemoteCard {
    fn new(card: &DesktopCard) -> Rc<Self> {
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        root.add_css_class("mini-terminal");
        root.add_css_class("term-remote");
        root.add_css_class(&format!(
            "agent-card-{}",
            peer_client::label(&card.agent_type)
        ));

        let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        header.add_css_class("term-header");
        let title = gtk4::Label::new(None);
        title.add_css_class("term-title");
        title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        title.set_single_line_mode(true);
        title.set_hexpand(true);
        title.set_halign(gtk4::Align::Start);
        header.append(&title);
        let status = gtk4::Label::new(None);
        status.add_css_class("term-status-badge");
        status.add_css_class("status-idle");
        status.set_halign(gtk4::Align::End);
        header.append(&status);
        root.append(&header);

        // `Overlay` gives its main child the whole body, so a terminal grid can
        // never grow a card beyond the host's rectangle.
        let body = gtk4::Overlay::new();
        // Without these the body collapses to its minimum inside the card's
        // vertical box while the chrome keeps full size: the emulator paints
        // into a sliver and the card looks black despite a live byte stream.
        body.set_hexpand(true);
        body.set_vexpand(true);
        let placeholder = gtk4::Label::new(None);
        placeholder.set_wrap(true);
        placeholder.set_justify(gtk4::Justification::Center);
        placeholder.set_valign(gtk4::Align::Center);
        placeholder.add_css_class("term-preview-text");
        body.set_child(Some(&placeholder));
        root.append(&body);

        Rc::new(Self {
            card_id: card.card_id.clone(),
            root,
            header,
            title,
            status,
            body,
            placeholder,
            terminal: RefCell::new(None),
            stream: RefCell::new(None),
            grid: Cell::new(None),
            next_attempt: Cell::new(Instant::now()),
            fitted: Cell::new(None),
            rest: Cell::new(None),
            body_size: Cell::new(None),
            suspended: Cell::new(false),
            fed: Cell::new(false),
            input_ready: Cell::new(false),
        })
    }

    /// Apply host content and stream state. Called on every snapshot refresh,
    /// so it must be safe to repeat with unchanged data.
    fn update(self: &Rc<Self>, card: &DesktopCard, live: bool, peer: &Peer) {
        self.title.set_text(&peer_client::label(&card.title));
        self.status.set_text(match card.session_alive {
            Some(false) => "○ EXITED",
            _ => "● REMOTE",
        });
        for class in ["status-idle", "status-busy", "status-exited"] {
            self.status.remove_css_class(class);
        }
        self.status.add_css_class(match card.session_alive {
            Some(false) => "status-exited",
            _ => "status-idle",
        });
        if card.layout.iconified {
            self.root.add_css_class("term-compact");
            self.header.set_visible(false);
            self.placeholder.add_css_class("term-agent-icon");
            self.placeholder.set_text(&format!(
                "{}\n{}",
                agent_icon(&card.agent_type),
                peer_client::label(&card.title)
            ));
            self.detach();
            return;
        }
        self.root.remove_css_class("term-compact");
        self.header.set_visible(true);
        self.placeholder.remove_css_class("term-agent-icon");
        if live {
            self.attach(peer);
            return;
        }
        self.detach();
        self.placeholder.set_visible(true);
        self.placeholder.set_text(if card.session_alive == Some(false) {
            "Host session exited"
        } else {
            "Preview only · at most 8 live consoles"
        });
    }

    /// Size the card to the host's fitted rectangle and match the terminal grid
    /// to the card body, so cells line up with the host's own rendering.
    fn fit(&self, rect: &Rect, scale: f64) {
        let width = (rect.width * scale).round().max(24.0) as i32;
        let height = (rect.height * scale).round().max(24.0) as i32;
        let header = if self.header.is_visible() {
            ((HEADER_HEIGHT * scale).round() as i32).clamp(16, (height / 2).max(16))
        } else {
            0
        };
        let signature = (width, height, header, scale.to_bits());
        if self.fitted.get() == Some(signature) {
            return;
        }
        self.fitted.set(Some(signature));
        self.root.set_size_request(width, height);
        self.header.set_size_request(-1, header);
        let body_width = (width as f64).max(1.0);
        let body_height = f64::from((height - header).max(1));
        self.body_size.set(Some((body_width, body_height, scale)));
        if let Some(terminal) = self.terminal.borrow().as_ref() {
            fit_font(terminal, body_width, body_height, self.grid.get(), scale);
        }
    }

    /// Attach a stream when none is running and the backoff has expired.
    fn attach(self: &Rc<Self>, peer: &Peer) {
        if self.stream.borrow().is_some() || Instant::now() < self.next_attempt.get() {
            return;
        }
        // A new attachment has not completed its handshake. Keys typed at the
        // previous session must not ride along.
        self.input_ready.set(false);
        // The emulator exists before the first byte arrives, so the host's
        // initial redraw is never dropped.
        let _ = self.terminal();
        let (stream, mut events) = TerminalStream::open(peer.clone(), &self.card_id);
        *self.stream.borrow_mut() = Some(stream);
        // A new stream starts clean: the next end is a real end unless this
        // card is deliberately suspended again.
        self.suspended.set(false);
        self.next_attempt.set(Instant::now() + RETRY_AFTER);
        self.placeholder.set_text("Connecting…");
        self.placeholder.set_visible(true);
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            while let Some(event) = events.next().await {
                let Some(card) = weak.upgrade() else {
                    return;
                };
                match event {
                    StreamEvent::Attached { columns, rows } => {
                        // Typing is armed only for this attachment. A later
                        // reconnect clears the flag before its own handshake.
                        card.input_ready.set(true);
                        card.note_grid(columns, rows);
                    }
                    StreamEvent::Grid { columns, rows } => card.note_grid(columns, rows),
                    StreamEvent::Bytes(bytes) => card.feed(&bytes),
                    StreamEvent::Closed(reason) => {
                        card.ended(reason);
                        return;
                    }
                }
            }
            if let Some(card) = weak.upgrade() {
                card.ended("closed");
            }
        });
    }

    /// The card's emulator, created on first use.
    ///
    /// There is no local PTY. VTE still translates keys, paste and IME against
    /// the terminal state it has parsed from the host (application cursor
    /// keys, bracketed paste) and reports the resulting bytes on `commit`.
    fn terminal(self: &Rc<Self>) -> Option<vte4::Terminal> {
        if let Some(terminal) = self.terminal.borrow().as_ref() {
            return Some(terminal.clone());
        }
        let terminal = vte4::Terminal::new();
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);
        terminal.set_input_enabled(true);
        terminal.set_can_focus(true);
        terminal.set_focusable(true);
        terminal.set_scroll_on_keystroke(true);
        terminal.set_scroll_on_output(true);
        terminal.set_scrollback_lines(2000);
        terminal.add_css_class("term-vte");
        let weak_commit = Rc::downgrade(self);
        connect_host_input(&terminal, move |bytes| {
            if let Some(card) = weak_commit.upgrade() {
                card.type_on_host(bytes);
            }
        });
        // Return never arrives as a commit on this PTY-less emulator: the
        // input method and the window's activate-default binding consume it,
        // while letters still come through `commit`. Catch it on the way in
        // and send the carriage return the host shell treats as "run this".
        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let weak_key = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, keyval, _, state| {
            if !is_submit_key(keyval, state) {
                return glib::Propagation::Proceed;
            }
            if let Some(card) = weak_key.upgrade() {
                card.type_on_host(b"\r");
            }
            glib::Propagation::Stop
        });
        terminal.add_controller(keys);
        let weak_focus = Rc::downgrade(self);
        let click = gtk4::GestureClick::new();
        click.connect_pressed(move |_, _, _, _| {
            if let Some(card) = weak_focus.upgrade() {
                if let Some(term) = card.terminal.borrow().as_ref() {
                    term.grab_focus();
                }
            }
        });
        terminal.add_controller(click);
        // The title is outside the emulator. Focusing from there is what lets
        // a click on the header reach the host session.
        let weak_header = Rc::downgrade(self);
        let header_click = gtk4::GestureClick::new();
        header_click.connect_pressed(move |_, _, _, _| {
            if let Some(card) = weak_header.upgrade() {
                if let Some(term) = card.terminal.borrow().as_ref() {
                    term.grab_focus();
                }
            }
        });
        self.header.add_controller(header_click);
        crate::mini_terminal::apply_vte_theme(&terminal, 10.0);
        self.body.add_overlay(&terminal);
        if let Some(grid) = self.grid.get() {
            terminal.set_size(grid.columns as i64, grid.rows as i64);
        }
        *self.terminal.borrow_mut() = Some(terminal.clone());
        Some(terminal)
    }

    /// Forward one VTE commit to the host, after this attachment's handshake.
    fn type_on_host(&self, bytes: &[u8]) {
        if bytes.is_empty() || !self.input_ready.get() {
            return;
        }
        let stream = self.stream.borrow();
        let Some(stream) = stream.as_ref() else {
            return;
        };
        stream.send_input(bytes);
    }

    fn note_grid(&self, columns: u16, rows: u16) {
        self.set_grid(TerminalSize { columns, rows });
        // Confirm the grid back to the host, which verifies it against its
        // own live grid before applying anything.
        if let Some(stream) = self.stream.borrow().as_ref() {
            stream.observe_grid(TerminalSize { columns, rows });
        }
    }

    /// Rest position and width inside the fitted canvas, once laid out.
    fn rest(&self) -> Option<(f64, f64, f64)> {
        self.rest.get()
    }

    fn set_rest(&self, x: f64, y: f64, width: f64) {
        self.rest.set(Some((x, y, width)));
    }

    /// Stop the stream on purpose: the last frame stays on screen and the next
    /// snapshot restarts the stream immediately.
    fn suspend(&self) {
        // Drop the handshake before the stream, so a commit that lands while
        // the worker is still exiting cannot queue into a dying attachment.
        self.input_ready.set(false);
        self.suspended.set(true);
        if let Some(stream) = self.stream.borrow_mut().take() {
            stream.stop();
        }
    }

    fn set_grid(&self, grid: TerminalSize) {
        if self.grid.get() == Some(grid) {
            return;
        }
        self.grid.set(Some(grid));
        let Some(terminal) = self.terminal.borrow().as_ref().cloned() else {
            return;
        };
        terminal.set_size(grid.columns as i64, grid.rows as i64);
        // The grid usually arrives after the first layout, so the font is sized
        // for it here as well as on every later resize.
        if let Some((width, height, scale)) = self.body_size.get() {
            fit_font(&terminal, width, height, Some(grid), scale);
        }
    }

    fn feed(&self, bytes: &[u8]) {
        if let Some(terminal) = self.terminal.borrow().as_ref() {
            terminal.feed(bytes);
        }
        self.fed.set(true);
        self.placeholder.set_visible(false);
    }

    /// The stream ended. Stay on screen with an explanation and let the next
    /// snapshot refresh retry after the backoff.
    fn ended(self: &Rc<Self>, reason: &'static str) {
        // A deliberate stop is not a failure: keep the last frame and let the
        // next snapshot attach again at once.
        if self.suspended.replace(false) {
            self.next_attempt.set(Instant::now());
            return;
        }
        self.detach();
        // An emulator that never received a byte is just a black overlay
        // hiding the explanation: step aside so the reason can be read. A
        // card with content keeps its last frame behind the message.
        if !self.fed.get() {
            if let Some(terminal) = self.terminal.borrow_mut().take() {
                self.body.remove_overlay(&terminal);
            }
        }
        self.placeholder.set_visible(true);
        self.placeholder.set_text(match reason {
            "unknown_card" | "terminal_exited" => "Host session ended",
            "update_remote_super_desktop" => "Update SUPER DESKTOP on the host",
            "peer_revoked_or_expired" | "invalid_peer_response" => {
                "Pairing required · add this PC again"
            }
            "attachment_limit" => "Too many live consoles",
            "connection_failed_or_pin_mismatch" => "Cannot reach this PC",
            _ => "Reconnecting…",
        });
    }

    /// Stop the stream on purpose without changing what the card says. Dropping
    /// the stream closes only this viewer's tmux client on the host.
    fn detach(&self) {
        self.suspend();
        self.grid.set(None);
    }
}

/// Size the terminal's font so the host's whole grid fits the card body.
///
/// VTE cell metrics scale with the font size, so two or three passes converge;
/// the result matches the host's own layout scale, and the host's grid is never
/// changed by it.
fn fit_font(
    terminal: &vte4::Terminal,
    body_width: f64,
    body_height: f64,
    grid: Option<TerminalSize>,
    scale: f64,
) {
    let Some(grid) = grid else {
        return;
    };
    let target_width = body_width / f64::from(grid.columns.max(1));
    let target_height = body_height / f64::from(grid.rows.max(1));
    let mut size = (10.0 * scale).clamp(MIN_FONT, MAX_FONT);
    for _ in 0..3 {
        crate::mini_terminal::apply_vte_theme(terminal, size);
        let cell_width = terminal.char_width() as f64;
        let cell_height = terminal.char_height() as f64;
        if cell_width <= 0.0 || cell_height <= 0.0 {
            return;
        }
        let factor = (target_width / cell_width).min(target_height / cell_height);
        if !factor.is_finite() || (factor - 1.0).abs() < 0.02 {
            return;
        }
        size = (size * factor).clamp(MIN_FONT, MAX_FONT);
    }
}

/// Enter, keypad Enter and the ISO Enter key submit the line.
///
/// Ctrl and Alt stay with VTE (Ctrl+Enter is a different sequence). Shift and
/// Lock do not: a shell still treats Shift+Enter as submit.
fn is_submit_key(keyval: gtk4::gdk::Key, state: gtk4::gdk::ModifierType) -> bool {
    if state.contains(gtk4::gdk::ModifierType::CONTROL_MASK)
        || state.contains(gtk4::gdk::ModifierType::ALT_MASK)
    {
        return false;
    }
    matches!(
        keyval,
        gtk4::gdk::Key::Return | gtk4::gdk::Key::KP_Enter | gtk4::gdk::Key::ISO_Enter
    )
}

/// Subscribe to VTE's `commit` signal and forward the bytes the host should see.
///
/// VTE emits the payload with its real length, then GObject delivers `text` as
/// a C string. Reading `size` bytes past the first NUL walks off that string.
/// A commit that is only a NUL (Ctrl+Space) arrives as an empty C string with
/// `size == 1`; anything after an interior NUL is already gone. The closure
/// is freed with the widget.
fn connect_host_input<F>(terminal: &vte4::Terminal, forward: F)
where
    F: Fn(&[u8]) + 'static,
{
    unsafe extern "C" fn trampoline<F: Fn(&[u8]) + 'static>(
        _terminal: *mut vte4::ffi::VteTerminal,
        text: *mut std::ffi::c_char,
        size: std::ffi::c_uint,
        data: gtk4::glib::ffi::gpointer,
    ) {
        if data.is_null() || text.is_null() || size == 0 {
            return;
        }
        let forward = unsafe { &*(data as *const F) };
        let available = unsafe { libc::strlen(text) };
        if available == 0 {
            forward(&[0]);
            return;
        }
        let n = (size as usize).min(available);
        let bytes = unsafe { std::slice::from_raw_parts(text as *const u8, n) };
        forward(bytes);
    }
    let boxed = Box::new(forward);
    unsafe {
        gtk4::glib::signal::connect_raw(
            terminal.as_ptr() as *mut gtk4::glib::gobject_ffi::GObject,
            c"commit".as_ptr(),
            Some(std::mem::transmute::<*const (), unsafe extern "C" fn()>(
                trampoline::<F> as *const (),
            )),
            Box::into_raw(boxed),
        );
    }
}

fn agent_icon(agent_type: &str) -> &'static str {
    crate::tmux::get_agent_config(&peer_client::label(agent_type)).icon
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_submits_and_modified_enter_does_not() {
        use gtk4::gdk::{Key, ModifierType};
        let none = ModifierType::empty();
        assert!(is_submit_key(Key::Return, none));
        assert!(is_submit_key(Key::KP_Enter, none));
        assert!(is_submit_key(Key::ISO_Enter, none));
        assert!(is_submit_key(Key::Return, ModifierType::SHIFT_MASK));
        assert!(!is_submit_key(Key::Return, ModifierType::CONTROL_MASK));
        assert!(!is_submit_key(Key::Return, ModifierType::ALT_MASK));
        assert!(!is_submit_key(Key::a, none));
    }

    #[test]
    fn committed_bytes_keep_their_length_including_a_nul() {
        crate::gtk_test::run_in_child_process("remote_terminal::tests::commit_bytes_inner");
    }

    #[test]
    fn commit_bytes_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().expect("a graphical session is required for GTK checks");
        let terminal = vte4::Terminal::new();
        terminal.set_input_enabled(true);
        let got = Rc::new(RefCell::new(Vec::new()));
        let slot = Rc::clone(&got);
        connect_host_input(&terminal, move |bytes| {
            slot.borrow_mut().extend_from_slice(bytes);
        });
        // feed_child is the same path a keystroke takes on a PTY-less VTE.
        // Ctrl+C is a single control byte. Ctrl+Space is a NUL, which GObject
        // delivers as an empty C string; it must still be forwarded.
        terminal.feed_child(b"hi");
        terminal.feed_child(&[0x03]);
        terminal.feed_child(&[0]);
        assert_eq!(&*got.borrow(), b"hi\x03\x00");
    }

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
    fn card_geometry_is_the_host_rectangle_scaled_uniformly() {
        crate::gtk_test::run_in_child_process("remote_terminal::tests::geometry_inner");
    }

    #[test]
    fn geometry_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().expect("a graphical session is required for GTK checks");
        let card = crate::remote_workspace::fixture().local.cards.remove(0);
        let widget = RemoteCard::new(&card);
        widget.set_grid(TerminalSize {
            columns: 80,
            rows: 24,
        });
        let rect = Rect {
            x: 100.0,
            y: 200.0,
            width: 640.0,
            height: 480.0,
        };
        widget.fit(&rect, 0.5);
        assert_eq!(widget.root.width_request(), 320);
        assert_eq!(widget.root.height_request(), 240);
        assert_eq!(widget.header.height_request(), 16);
        // The fit is idempotent: a repeated refresh must not restyle the font.
        let fitted = widget.fitted.get();
        widget.fit(&rect, 0.5);
        assert_eq!(widget.fitted.get(), fitted);

        // The rest pose is what the overlay's slide-out animates between.
        widget.set_rest(100.0, 200.0, 320.0);
        assert_eq!(widget.rest(), Some((100.0, 200.0, 320.0)));

        // Hiding suspends the stream without ending the view: the card keeps
        // its last frame, and the next snapshot attaches again immediately
        // instead of waiting out a failure backoff.
        widget.suspend();
        assert!(widget.suspended.get());
        assert!(widget.stream.borrow().is_none());
        widget.ended("closed");
        assert!(!widget.suspended.get());
        assert_eq!(widget.placeholder.text(), "", "last frame stays untouched");

        // A real failure does explain itself to the user.
        widget.ended("terminal_exited");
        assert_eq!(widget.placeholder.text(), "Host session ended");
        assert!(widget.placeholder.is_visible());

        // The fit loop relies on VTE recomputing cell metrics synchronously
        // from the font: without that, a fitted grid would never line up.
        let terminal = vte4::Terminal::new();
        crate::mini_terminal::apply_vte_theme(&terminal, 10.0);
        let small = terminal.char_width();
        crate::mini_terminal::apply_vte_theme(&terminal, 20.0);
        assert!(
            terminal.char_width() > small,
            "font size must drive cell width ({} → {})",
            small,
            terminal.char_width()
        );
        // And the fit itself: the whole host grid has to land inside the body.
        fit_font(
            &terminal,
            640.0,
            400.0,
            Some(TerminalSize {
                columns: 80,
                rows: 24,
            }),
            1.0,
        );
        let grid_width = terminal.char_width() as f64 * 80.0;
        let grid_height = terminal.char_height() as f64 * 24.0;
        assert!(
            grid_width <= 640.0 * 1.05 && grid_height <= 400.0 * 1.05,
            "fitted grid {grid_width}x{grid_height} must fit 640x400"
        );
        assert!(
            grid_width > 640.0 * 0.6,
            "fitted grid {grid_width} must fill the card, not shrink into a corner"
        );
    }
}
