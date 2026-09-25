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
/// Below `MIN_FONT` text is unreadable, but a card drawn smaller than the
/// host's grid at `MIN_FONT` still has to keep that grid: otherwise VTE drops
/// rows and the host's output lands on the wrong lines (and stays scrambled
/// after the card grows again). Fitting may go down to this instead.
const GRID_FLOOR_FONT: f64 = 1.0;
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

    /// Whether the current stream has completed the host's handshake.
    #[cfg(test)]
    pub fn is_attached(&self) -> bool {
        self.input_ready.get() && self.stream.borrow().is_some()
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

/// Pack a remote card's emulator at its natural size (the host grid times the
/// cell size), centred in the card body.
///
/// VTE derives its grid from whatever it is allocated, so an emulator that
/// expanded to fill the body silently became e.g. 70×21 or 76×22 for a 76×21
/// host. The host's tmux then drew into a different screen than the one shown:
/// its scroll region and line feeds landed a row off, leaving the text a line
/// above the cursor, and long lines wrapped. A natural-size emulator keeps
/// exactly the grid `set_size` gives it; the body's remainder is a margin.
pub fn pack_remote_emulator(terminal: &vte4::Terminal) {
    terminal.set_hexpand(false);
    terminal.set_vexpand(false);
    terminal.set_halign(gtk4::Align::Center);
    terminal.set_valign(gtk4::Align::Center);
}

/// What a remote emulator's font was last fitted to, and what that fit left on
/// screen.
///
/// Every VTE font change re-measures the glyphs and redraws the whole
/// terminal, even a change to the font it already has, and a view draws its
/// cards again far more often than their size changes. A card keeps one of
/// these so that fitting it again for the same space leaves the emulator
/// alone.
#[derive(Clone, Debug, PartialEq)]
pub struct FontFit {
    inputs: FitInputs,
    shown: Shown,
}

/// Everything a fit depends on.
#[derive(Clone, Debug, PartialEq)]
struct FitInputs {
    /// The space the grid had: the body, bounded by the emulator's parent
    /// once that is allocated.
    width: f64,
    height: f64,
    /// The host's grid, as the emulator is sized to it.
    grid: (i64, i64),
    scale: f64,
    /// The theme's font family: another family measures differently.
    family: String,
}

/// What an emulator shows: its font's family and size (in Pango units) and
/// its grid.
#[derive(Clone, Debug, PartialEq)]
struct Shown {
    family: Option<String>,
    size: i32,
    grid: (i64, i64),
}

impl Shown {
    fn of(terminal: &vte4::Terminal) -> Self {
        let font = terminal.font();
        Self {
            family: font
                .as_ref()
                .and_then(|font| font.family())
                .map(|family| family.to_string()),
            size: font.map_or(0, |font| font.size()),
            grid: (terminal.column_count(), terminal.row_count()),
        }
    }
}

impl FontFit {
    /// Whether fitting for `inputs` would change nothing: they are the ones
    /// this fit was made for, and the emulator still shows what it left.
    /// Something else may have set a font since (restoring an icon sets the
    /// theme's), or VTE may have re-derived the grid from a short allocation;
    /// either needs a real fit.
    fn covers(&self, inputs: &FitInputs, shown: &Shown) -> bool {
        self.inputs == *inputs && self.shown == *shown
    }
}

/// Size the emulator's font so the host's whole grid fits the card body.
///
/// The emulator keeps the host's grid (see `pack_remote_emulator`); only the
/// font changes. VTE cells are whole pixels, so after estimating, the font
/// steps down until the whole grid (as VTE measures it, padding included)
/// fits the space the emulator really has: its parent's allocation once laid
/// out, never more than the body the view computed. The host's own grid is
/// never changed by it.
///
/// `last` is the card's own record of its previous fit: the same space, grid,
/// scale and theme font leave the emulator untouched.
pub fn fit_font(
    terminal: &vte4::Terminal,
    body_width: f64,
    body_height: f64,
    grid: Option<TerminalSize>,
    scale: f64,
    last: &RefCell<Option<FontFit>>,
) {
    let Some(grid) = grid else {
        return;
    };
    let (mut width, mut height) = (body_width, body_height);
    if let Some(parent) = terminal.parent() {
        if parent.width() > 1 && parent.height() > 1 {
            width = width.min(f64::from(parent.width()));
            height = height.min(f64::from(parent.height()));
        }
    }
    if width < 1.0 || height < 1.0 {
        return;
    }
    let inputs = FitInputs {
        width,
        height,
        grid: (i64::from(grid.columns.max(1)), i64::from(grid.rows.max(1))),
        scale,
        family: crate::theme::current_theme().font_family,
    };
    let shown = Shown::of(terminal);
    if last
        .borrow()
        .as_ref()
        .is_some_and(|fit| fit.covers(&inputs, &shown))
    {
        return;
    }
    // VTE may have re-derived its grid from an earlier allocation.
    if shown.grid != inputs.grid {
        terminal.set_size(inputs.grid.0, inputs.grid.1);
    }
    let natural = || {
        let (_, w, _, _) = terminal.measure(gtk4::Orientation::Horizontal, -1);
        let (_, h, _, _) = terminal.measure(gtk4::Orientation::Vertical, -1);
        (f64::from(w), f64::from(h))
    };
    // Only the font changes here: the colors are the theme's, set when the
    // emulator is built and when the theme changes. VTE re-measures its glyphs
    // even for the font it already has, so a size already on screen is
    // measured as it is.
    let family = crate::mini_terminal::vte_font(&inputs.family, 10.0).family();
    let mut on_screen = (shown.family.as_deref() == family.as_deref()).then_some(shown.size);
    // Cells grow with the font, so what is on screen already says which size
    // to try: a card that changed a little changes its font a little. An
    // emulator in another family starts from a card's size at this scale.
    let mut size = on_screen
        .filter(|units| *units > 0)
        .map_or(10.0 * scale, |units| {
            f64::from(units) / f64::from(gtk4::pango::SCALE)
        })
        .clamp(MIN_FONT, MAX_FONT);
    let mut show = |size: f64| {
        let font = crate::mini_terminal::vte_font(&inputs.family, size);
        if on_screen != Some(font.size()) {
            terminal.set_font(Some(&font));
            on_screen = Some(font.size());
        }
    };
    for _ in 0..3 {
        show(size);
        let (w, h) = natural();
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let factor = (width / w).min(height / h);
        if !factor.is_finite() || (factor - 1.0).abs() < 0.01 {
            break;
        }
        size = (size * factor).clamp(MIN_FONT, MAX_FONT);
    }
    // Whole-pixel cells: never let the grid overflow the space it has.
    for _ in 0..60 {
        show(size);
        let (w, h) = natural();
        if (w <= width && h <= height) || size <= GRID_FLOOR_FONT {
            break;
        }
        size = (size * 0.97).max(GRID_FLOOR_FONT);
    }
    *last.borrow_mut() = Some(FontFit {
        inputs,
        shown: Shown::of(terminal),
    });
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
    fn remote_emulator_keeps_the_host_grid_at_any_card_size() {
        crate::gtk_test::run_in_child_process(
            "card_source::tests::remote_emulator_grid_inner",
        );
    }

    /// A remote card body is a vertical box the card sizes; the emulator in it
    /// must show exactly the host's grid, whatever the body size. Before, it
    /// expanded and VTE re-derived e.g. 70×21 or 74×22 for a 76×21 host, which
    /// put the host's text a line above its cursor.
    #[test]
    fn remote_emulator_grid_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        use vte4::prelude::*;
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let pump = |ms: u64| {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            while std::time::Instant::now() < until {
                while gtk4::glib::MainContext::default().iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let mut wrong = Vec::new();
        for &(columns, rows) in &[(76u16, 21u16), (80, 24), (99, 38), (156, 40)] {
            for &(width, height) in &[(640, 450), (600, 300), (500, 330), (733, 402), (420, 250), (900, 600)] {
                let body = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
                body.set_size_request(width, height);
                let terminal = vte4::Terminal::new();
                // What `spawn_vte` sets before a remote card packs it.
                terminal.set_hexpand(true);
                terminal.set_vexpand(true);
                pack_remote_emulator(&terminal);
                body.append(&terminal);
                let window = gtk4::Window::new();
                window.set_default_size(width, height);
                window.set_resizable(false);
                window.set_child(Some(&body));
                window.present();
                pump(120);
                let grid = TerminalSize { columns, rows };
                terminal.set_size(i64::from(columns), i64::from(rows));
                fit_font(
                    &terminal,
                    f64::from(width),
                    f64::from(height),
                    Some(grid),
                    1.0,
                    &RefCell::new(None),
                );
                pump(150);
                let shown = (terminal.column_count(), terminal.row_count());
                let fits = terminal.width() <= body.width() && terminal.height() <= body.height();
                if shown != (i64::from(columns), i64::from(rows)) || !fits {
                    wrong.push(format!(
                        "host {columns}x{rows} in {width}x{height}: shows {}x{} at {}x{} in {}x{}",
                        shown.0, shown.1, terminal.width(), terminal.height(), body.width(), body.height()
                    ));
                }
                window.close();
                pump(50);
            }
        }
        assert!(wrong.is_empty(), "emulator grid differs from the host's:\n{}", wrong.join("\n"));
    }

    #[test]
    fn a_fit_is_only_made_again_when_its_inputs_or_the_emulator_changed() {
        let inputs = FitInputs {
            width: 640.0,
            height: 450.0,
            grid: (80, 24),
            scale: 1.0,
            family: "Mono".into(),
        };
        let shown = Shown {
            family: Some("Mono".into()),
            size: 9 * gtk4::pango::SCALE,
            grid: (80, 24),
        };
        let fit = FontFit {
            inputs: inputs.clone(),
            shown: shown.clone(),
        };
        assert!(fit.covers(&inputs, &shown));
        // Another space, grid, scale or theme font needs a new fit.
        for changed in [
            FitInputs { width: 639.0, ..inputs.clone() },
            FitInputs { height: 451.0, ..inputs.clone() },
            FitInputs { grid: (80, 25), ..inputs.clone() },
            FitInputs { scale: 0.5, ..inputs.clone() },
            FitInputs { family: "Other Mono".into(), ..inputs.clone() },
        ] {
            assert!(!fit.covers(&changed, &shown), "{changed:?}");
        }
        // So does an emulator that no longer shows what the fit left: a font
        // set since (a restored icon, a theme), or a grid VTE re-derived.
        for changed in [
            Shown { size: 10 * gtk4::pango::SCALE, ..shown.clone() },
            Shown { family: Some("Other Mono".into()), ..shown.clone() },
            Shown { family: None, ..shown.clone() },
            Shown { grid: (79, 24), ..shown.clone() },
        ] {
            assert!(!fit.covers(&inputs, &changed), "{changed:?}");
        }
    }

    #[test]
    fn a_fitted_emulator_is_left_alone_until_its_space_or_font_changes() {
        crate::gtk_test::run_in_child_process("card_source::tests::fit_memo_inner");
    }

    /// A view draws its cards again on every snapshot and every resize frame.
    /// Each VTE font change re-measures the glyphs and redraws the terminal,
    /// so a fit that has nothing to change must not set a font at all, and a
    /// fit sets nothing but the font.
    #[test]
    fn fit_memo_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        use vte4::prelude::*;
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let pump = |ms: u64| {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            while std::time::Instant::now() < until {
                while gtk4::glib::MainContext::default().iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let body = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        body.set_size_request(640, 450);
        let terminal = vte4::Terminal::new();
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);
        pack_remote_emulator(&terminal);
        body.append(&terminal);
        let window = gtk4::Window::new();
        window.set_default_size(640, 450);
        window.set_resizable(false);
        window.set_child(Some(&body));
        window.present();
        // What `spawn_vte` builds the emulator with.
        crate::mini_terminal::apply_vte_theme(&terminal, 10.0);
        pump(120);
        let grid = TerminalSize { columns: 99, rows: 38 };
        let last = RefCell::new(None);
        // The host's grid, whole inside the space as VTE measures it (padding
        // included), and still the grid once GTK has laid the new font out: a
        // short allocation would have made VTE re-derive a smaller one.
        let holds_grid_in = |width: i32, height: i32| {
            let clock = terminal.frame_clock().expect("a mapped emulator");
            let laid_out = clock.frame_counter() + 2;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while clock.frame_counter() < laid_out {
                assert!(std::time::Instant::now() < deadline, "no frame after the fit");
                pump(20);
            }
            let (_, natural_width, _, _) = terminal.measure(gtk4::Orientation::Horizontal, -1);
            let (_, natural_height, _, _) = terminal.measure(gtk4::Orientation::Vertical, -1);
            (terminal.column_count(), terminal.row_count()) == (99, 38)
                && natural_width <= width
                && natural_height <= height
        };
        fit_font(&terminal, 640.0, 450.0, Some(grid), 1.0, &last);
        assert!(holds_grid_in(640, 450));
        let fitted = terminal.font().unwrap().size();

        let changes = Rc::new(Cell::new(0));
        terminal.connect_notify_local(Some("font-desc"), {
            let changes = Rc::clone(&changes);
            move |_, _| changes.set(changes.get() + 1)
        });
        // Drawn again at the same size, as every snapshot and relayout does.
        for _ in 0..3 {
            fit_font(&terminal, 640.0, 450.0, Some(grid), 1.0, &last);
        }
        assert_eq!(changes.get(), 0, "an unchanged fit set a font");
        assert_eq!(terminal.font().unwrap().size(), fitted);

        // Something else set a font since (restoring an icon sets the theme's
        // own size): the next fit is made again, and changes only the font.
        let red = gtk4::gdk::RGBA::new(1.0, 0.0, 0.0, 1.0);
        terminal.set_color_background(&red);
        let family = crate::theme::current_theme().font_family;
        terminal.set_font(Some(&crate::mini_terminal::vte_font(&family, 10.0)));
        changes.set(0);
        fit_font(&terminal, 640.0, 450.0, Some(grid), 1.0, &last);
        assert!(changes.get() >= 1, "a replaced font was not fitted again");
        assert!(holds_grid_in(640, 450));
        assert_eq!(terminal.color_background_for_draw(), red);

        // A smaller body: a smaller font, the same host grid.
        fit_font(&terminal, 500.0, 350.0, Some(grid), 1.0, &last);
        assert!(holds_grid_in(500, 350));
        assert!(terminal.font().unwrap().size() < fitted);
        window.close();
        pump(50);
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
