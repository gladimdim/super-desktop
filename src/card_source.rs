//! Who runs a card's session, and where its terminal is.
//!
//! A card's chrome, buttons, gestures and geometry are the same code in both
//! workspaces. The only thing that differs is this module: a local card's
//! emulator attaches a tmux session on this machine, while a remote card's
//! emulator is a pure emulator fed by the host over the pinned WebSocket, with
//! this PC owning no process and no session.
use crate::desktop_protocol::TerminalSize;
use crate::peer_client::Peer;
use crate::peer_terminal::{Event as StreamEvent, TerminalStream};
use futures_util::StreamExt;
use gtk4::prelude::*;
use vte4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A failed or ended attachment waits this long before the next attempt, so
/// eight cards cannot hammer an offline or refusing host.
pub const RETRY_AFTER: Duration = Duration::from_secs(5);
/// Font bounds for a fitted emulator. A card too small for the host's grid
/// clips, exactly like a small local card does.
pub const MIN_FONT: f64 = 3.0;
pub const MAX_FONT: f64 = 40.0;
/// How deep a remote emulator's scrollback is. The host owns the real
/// scrollback; this is only what the viewer can reach locally.
pub const REMOTE_SCROLLBACK: i64 = 2000;

/// Who runs this card's session.
#[derive(Clone)]
pub enum CardSource {
    /// A tmux session on this machine, attached by the emulator itself.
    Local,
    /// A host's own session, streamed here.
    Remote {
        peer: Peer,
        card_id: String,
        /// The fit scale of the view this card is drawn into.
        scale: f64,
    },
}

impl CardSource {
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    /// The smallest card in this workspace's pixel space. A local workspace is
    /// one-to-one with this machine's screen; a remote card is drawn fitted
    /// into the viewer's canvas, so the same host card is smaller there.
    pub fn scale(&self) -> f64 {
        match self {
            Self::Local => 1.0,
            Self::Remote { scale, .. } => *scale,
        }
    }
}

/// What a view is told when the host's grid changes, so it can refit the font.
pub type GridCallback = Rc<dyn Fn(TerminalSize)>;
/// What a card is told about its stream: something to say, or `None` to clear.
pub type MessageCallback = Rc<dyn Fn(Option<&str>)>;

/// The part of a card a remote session drives: its emulator, and the two places
/// a message about that session can appear.
#[derive(Clone)]
pub struct RemoteView {
    pub peer: Peer,
    pub card_id: String,
    /// The card's emulator, created by the card when its stream attaches.
    pub vte: Rc<RefCell<Option<vte4::Terminal>>>,
    /// The host's grid changed: the card refits its font to it.
    pub on_grid: GridCallback,
    /// Something to say about this session, or `None` to clear it. The card
    /// decides where it goes: the body when there is nothing to show, the
    /// footer when the last frame stays on screen.
    pub on_message: MessageCallback,
}

/// One host session, streamed to this machine.
pub struct RemoteSession {
    view: RemoteView,
    stream: RefCell<Option<TerminalStream>>,
    grid: Cell<Option<TerminalSize>>,
    /// Earliest time a new attachment may be attempted.
    next_attempt: Cell<Instant>,
    /// The stream was stopped on purpose (hide, or a card that is not
    /// streamable), so its end must not look like a failure.
    suspended: Cell<bool>,
    /// The current stream has received the host's `attached` frame. Commits
    /// before that, and after suspend, are dropped rather than queued across
    /// a reconnect.
    input_ready: Cell<bool>,
    /// Whether any host bytes ever reached the emulator.
    fed: Cell<bool>,
    /// Why the last stream ended, so a view can repeat it after a chrome
    /// change of its own.
    message: RefCell<Option<&'static str>>,
}

impl RemoteSession {
    pub fn new(view: RemoteView) -> Rc<Self> {
        Rc::new(Self {
            view,
            stream: RefCell::new(None),
            grid: Cell::new(None),
            next_attempt: Cell::new(Instant::now()),
            suspended: Cell::new(false),
            input_ready: Cell::new(false),
            fed: Cell::new(false),
            message: RefCell::new(None),
        })
    }

    /// The host-owned grid the emulator must match, once the host has said so.
    pub fn grid(&self) -> Option<TerminalSize> {
        self.grid.get()
    }

    /// What the viewer should say about this session right now.
    pub fn message(&self) -> Option<&'static str> {
        *self.message.borrow()
    }

    /// Start a stream when none is running and the backoff has expired.
    pub fn attach(self: &Rc<Self>) {
        if self.stream.borrow().is_some() || Instant::now() < self.next_attempt.get() {
            return;
        }
        // A new attachment has not completed its handshake. Keys typed at the
        // previous session must not ride along.
        self.input_ready.set(false);
        let (stream, mut events) = TerminalStream::open(self.view.peer.clone(), &self.view.card_id);
        *self.stream.borrow_mut() = Some(stream);
        // A new stream starts clean: the next end is a real end unless this
        // card is deliberately suspended again.
        self.suspended.set(false);
        self.next_attempt.set(Instant::now() + RETRY_AFTER);
        (self.view.on_message)(Some("Connecting…"));
        let weak = Rc::downgrade(self);
        gtk4::glib::MainContext::default().spawn_local(async move {
            while let Some(event) = events.next().await {
                let Some(session) = weak.upgrade() else {
                    return;
                };
                match event {
                    StreamEvent::Attached { columns, rows } => {
                        // Typing is armed only for this attachment. A later
                        // reconnect clears the flag before its own handshake.
                        session.input_ready.set(true);
                        session.note_grid(columns, rows);
                        (session.view.on_message)(None);
                    }
                    StreamEvent::Grid { columns, rows } => session.note_grid(columns, rows),
                    StreamEvent::Bytes(bytes) => session.feed(&bytes),
                    StreamEvent::Closed(reason) => {
                        session.ended(reason);
                        return;
                    }
                }
            }
            if let Some(session) = weak.upgrade() {
                session.ended("closed");
            }
        });
    }

    /// Viewer keystrokes for the host session. Never queued for a later
    /// attachment: a commit that lands before the handshake, or after a
    /// suspend, is dropped.
    pub fn input(&self, bytes: &[u8]) {
        if !self.input_ready.get() {
            return;
        }
        if let Some(stream) = self.stream.borrow().as_ref() {
            stream.send_input(bytes);
        }
    }

    /// Stop the stream on purpose: the last frame stays on screen and the next
    /// snapshot restarts the stream immediately.
    pub fn suspend(&self) {
        // Drop the handshake before the stream, so a commit that lands while
        // the worker is still exiting cannot queue into a dying attachment.
        self.input_ready.set(false);
        self.suspended.set(true);
        if let Some(stream) = self.stream.borrow_mut().take() {
            stream.stop();
        }
    }

    /// Stop the stream as a card that is going away: no retry, no message.
    pub fn stop(&self) {
        self.input_ready.set(false);
        self.suspended.set(true);
        if let Some(stream) = self.stream.borrow_mut().take() {
            stream.stop();
        }
    }

    /// Stop the stream on purpose while keeping the card's own state.
    pub fn detach(&self) {
        self.suspend();
        self.grid.set(None);
    }

    fn note_grid(self: &Rc<Self>, columns: u16, rows: u16) {
        let grid = TerminalSize { columns, rows };
        if self.grid.get() != Some(grid) {
            self.grid.set(Some(grid));
            if let Some(terminal) = self.view.vte.borrow().as_ref() {
                terminal.set_size(grid.columns as i64, grid.rows as i64);
            }
        }
        // The grid usually arrives after the first layout, so the font is sized
        // for it here as well as on every later resize.
        (self.view.on_grid)(grid);
        // Confirm the grid back to the host, which verifies it against its own
        // live grid before applying anything.
        if let Some(stream) = self.stream.borrow().as_ref() {
            stream.observe_grid(grid);
        }
    }

    fn feed(&self, bytes: &[u8]) {
        if let Some(terminal) = self.view.vte.borrow().as_ref() {
            terminal.feed(bytes);
        }
        self.fed.set(true);
        *self.message.borrow_mut() = None;
        (self.view.on_message)(None);
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
        let message = match reason {
            "unknown_card" | "terminal_exited" => "Host session ended",
            "update_remote_super_desktop" => "Update SUPER DESKTOP on the host",
            "peer_revoked_or_expired" | "invalid_peer_response" => {
                "Pairing required · add this PC again"
            }
            "attachment_limit" => "Too many live consoles",
            "connection_failed_or_pin_mismatch" => "Cannot reach this PC",
            _ => "Reconnecting…",
        };
        // An emulator that never received a byte is just a black overlay hiding
        // the explanation: the card steps aside for it (an empty emulator is
        // dropped, so the body shows the message). A card with content keeps its
        // last frame, and the message goes to its footer instead.
        *self.message.borrow_mut() = Some(message);
        (self.view.on_message)(Some(message));
    }
}

/// Size the emulator's font so the host's whole grid fits the card body.
///
/// VTE cell metrics scale with the font size, so two or three passes converge;
/// the result matches the host's own layout scale, and the host's grid is never
/// changed by it.
pub fn fit_font(
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
pub fn is_submit_key(keyval: gtk4::gdk::Key, state: gtk4::gdk::ModifierType) -> bool {
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
/// VTE emits `commit` with a length that can exceed the C string it points at,
/// so the string length is the bound actually used: a card must never read past
/// the buffer its terminal handed it.
pub fn connect_host_input<F>(terminal: &vte4::Terminal, forward: F)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_submits_but_modified_enter_does_not() {
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
    fn only_a_remote_source_claims_a_stream() {
        assert!(!CardSource::Local.is_remote());
        assert!(CardSource::Remote {
            peer: crate::peer_client::test_peer('a'),
            card_id: "card-one".into(),
            scale: 0.5,
        }
        .is_remote());
        // A remote card's minima follow the view it is fitted into.
        assert_eq!(CardSource::Local.scale(), 1.0);
    }
}
