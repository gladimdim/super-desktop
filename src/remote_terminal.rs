//! Live remote consoles, drawn at the host's own card geometry.
//!
//! The host owns the terminal grid and the layout; this view only renders what
//! the host reports and never writes back. Cards keep the host's positions,
//! sizes, stacking order and iconified state, and each streamed card shows the
//! host session's real pixels through a VTE widget fed by the network stream.
//! Nothing here can type into a remote session: the emulators are read-only and
//! the transport carries no input frames.
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
                    StreamEvent::Attached { columns, rows }
                    | StreamEvent::Grid { columns, rows } => {
                        card.set_grid(TerminalSize { columns, rows });
                        // Confirm the grid back to the host, which verifies it
                        // against its own live grid before applying anything.
                        if let Some(stream) = card.stream.borrow().as_ref() {
                            stream.observe_grid(TerminalSize { columns, rows });
                        }
                    }
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

    /// The card's emulator, created on first use. Read-only by construction:
    /// this widget never forwards a keystroke anywhere.
    fn terminal(self: &Rc<Self>) -> Option<vte4::Terminal> {
        if let Some(terminal) = self.terminal.borrow().as_ref() {
            return Some(terminal.clone());
        }
        let terminal = vte4::Terminal::new();
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);
        terminal.set_input_enabled(false);
        terminal.set_can_focus(false);
        terminal.set_scroll_on_output(true);
        terminal.set_scrollback_lines(2000);
        terminal.add_css_class("term-vte");
        // Placeholder text keeps showing until the first frame of real output.
        crate::mini_terminal::apply_vte_theme(&terminal, 10.0);
        self.body.add_overlay(&terminal);
        if let Some(grid) = self.grid.get() {
            terminal.set_size(grid.columns as i64, grid.rows as i64);
        }
        *self.terminal.borrow_mut() = Some(terminal.clone());
        Some(terminal)
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

fn agent_icon(agent_type: &str) -> &'static str {
    crate::tmux::get_agent_config(&peer_client::label(agent_type)).icon
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
