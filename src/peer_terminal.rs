//! Viewer-side live terminal stream for one remote card.
//!
//! A worker thread owns the pinned WSS stream. Host output arrives as binary
//! frames and is forwarded to the GTK side through a bounded channel. Viewer
//! keystrokes go back as binary frames, but only after the host's `attached`
//! handshake: until then they sit in a bounded queue, and a stopped stream
//! throws them away instead of writing them into a later session. The one text
//! frame this side sends asks the host to re-apply the host's own grid.
use crate::desktop_protocol::{
    AttachCommand, AttachEvent, TerminalSize, ATTACH_MAX_BACKLOG, ATTACH_MAX_CHUNK,
};
use crate::peer_client::{self, Peer, PeerError, Result};
use futures_channel::mpsc::{self, Receiver, Sender};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tungstenite::{error::Error as SocketError, Message};

/// What the viewer learns from one attachment.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// The host attached and reported the grid its pane is rendered at. This is
    /// the only grid the viewer may use.
    Attached {
        columns: u16,
        rows: u16,
    },
    /// The host grid changed after a local resize on the host.
    Grid {
        columns: u16,
        rows: u16,
    },
    /// Raw terminal bytes, in order. Feed them to the emulator unchanged.
    Bytes(Vec<u8>),
    /// The stream ended. Retrying is the caller's decision; `reason` never
    /// contains credentials or response bodies.
    Closed(&'static str),
}

/// One card's live stream. Dropping it stops the worker and detaches only this
/// viewer's tmux client on the host — never the session or its processes.
///
/// The event receiver is handed to the caller, which owns the rendering side
/// (a GTK task or a CLI loop); this handle only controls the stream.
pub struct TerminalStream {
    stop: Arc<AtomicBool>,
    commands: std::sync::mpsc::Sender<AttachCommand>,
    input: std::sync::mpsc::SyncSender<Vec<u8>>,
    wake: Arc<Wake>,
}

impl TerminalStream {
    pub fn open(peer: Peer, card_id: &str) -> (Self, Receiver<Event>) {
        let (events, receiver) = mpsc::channel(ATTACH_MAX_BACKLOG / ATTACH_MAX_CHUNK);
        let (commands, queue) = std::sync::mpsc::channel();
        // Bounded keystroke queue: human typing cannot fill 64 chunks, and a
        // network that slow drops pastes rather than ballooning memory.
        let (input, inbox) = std::sync::mpsc::sync_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Wake::new();
        let worker = Worker {
            peer,
            card_id: card_id.to_string(),
            stop: Arc::clone(&stop),
            events,
            commands: queue,
            input: inbox,
            wake: Arc::clone(&wake),
            ready: false,
        };
        // A live stream must never run on the GTK thread.
        std::thread::Builder::new()
            .name("peer-terminal".into())
            .spawn(move || worker.run())
            .expect("terminal stream worker");
        (
            Self {
                stop,
                commands,
                input,
                wake,
            },
            receiver,
        )
    }

    /// Report the grid the viewer observed. The host applies it only when it is
    /// already the host's own grid, and answers with the authoritative one.
    pub fn observe_grid(&self, grid: TerminalSize) {
        let _ = self.commands.send(AttachCommand::Grid {
            columns: grid.columns,
            rows: grid.rows,
        });
    }

    /// Queue keystrokes for the host session, split into bounded frames. A
    /// full queue or a dead worker drops the input: keys are never replayed
    /// after a disconnect, and Ctrl+C is just another byte (`0x03`).
    pub fn send_input(&self, bytes: &[u8]) {
        self.sender().send_input(bytes);
    }

    /// A detached handle for threads that type without holding the stream.
    pub fn sender(&self) -> InputSender {
        InputSender {
            input: self.input.clone(),
            stop: Arc::clone(&self.stop),
            wake: Arc::clone(&self.wake),
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // The worker may be parked in poll(); the wake is what makes hide
        // and machine-switch drop queued keys immediately.
        self.wake.notify();
    }
}

/// Cloneable keystroke handle: stdin forwarding and VTE commit callbacks
/// share it without owning the stream or its event receiver.
#[derive(Clone)]
pub struct InputSender {
    input: std::sync::mpsc::SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    wake: Arc<Wake>,
}

impl InputSender {
    pub fn send_input(&self, bytes: &[u8]) {
        if self.stop.load(Ordering::Relaxed) {
            return;
        }
        let mut queued = false;
        for chunk in bytes.chunks(ATTACH_MAX_CHUNK) {
            if chunk.is_empty() || self.input.try_send(chunk.to_vec()).is_err() {
                break;
            }
            queued = true;
        }
        if queued {
            self.wake.notify();
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

impl Drop for TerminalStream {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Worker {
    peer: Peer,
    card_id: String,
    stop: Arc<AtomicBool>,
    events: Sender<Event>,
    commands: std::sync::mpsc::Receiver<AttachCommand>,
    input: std::sync::mpsc::Receiver<Vec<u8>>,
    wake: Arc<Wake>,
    /// Set once the host's `attached` frame has been accepted. Keystrokes
    /// queued before that stay in `input` and are never written early.
    ready: bool,
}

impl Worker {
    fn run(mut self) {
        let reason = match self.session() {
            Ok(()) => "closed",
            Err(error) => error.0,
        };
        // A full queue means the UI is already behind and stopping anyway.
        let _ = self.events.try_send(Event::Closed(reason));
    }

    fn session(&mut self) -> Result<()> {
        let mut socket = peer_client::desktop_socket(
            &self.peer,
            &self.attach_path()?,
            crate::desktop_protocol::TERMINAL_PTY,
        )?;
        let socket_fd = socket.get_ref().sock.as_raw_fd();
        // poll() is what waits. A long read would hide keystrokes that arrive
        // while this thread is blocked inside the socket.
        let _ = socket
            .get_ref()
            .sock
            .set_read_timeout(Some(Duration::from_millis(1)));
        let mut heartbeat = Instant::now();
        loop {
            if self.stop.load(Ordering::Relaxed) {
                self.discard_input();
                let _ = socket.close(None);
                return Ok(());
            }
            self.send_controls(&mut socket)?;
            self.send_keystrokes(&mut socket)?;
            if heartbeat.elapsed() >= Duration::from_secs(20) {
                heartbeat = Instant::now();
                // Keeps a half-open connection from looking idle forever.
                socket.flush().map_err(socket_failure)?;
                socket
                    .send(Message::Ping(Vec::new().into()))
                    .map_err(socket_failure)?;
            }
            match socket.read() {
                Ok(Message::Binary(bytes)) => self.deliver(bytes.to_vec())?,
                Ok(Message::Text(text)) => self.apply_event(&text)?,
                // tungstenite answers a Ping itself when the next frame is
                // written; a Pong is only evidence of liveness. A Close frame
                // ends this attachment deliberately, and its reason is only
                // trusted when it is one of the protocol's own codes.
                Ok(Message::Close(frame)) => {
                    return Err(PeerError(
                        frame
                            .and_then(|frame| {
                                crate::desktop_protocol::known_reason(&frame.reason)
                            })
                            .unwrap_or("closed"),
                    ))
                }
                Ok(_) => {}
                Err(SocketError::Io(error)) if is_timeout(&error) => {
                    let wait = Duration::from_secs(20)
                        .saturating_sub(heartbeat.elapsed())
                        .min(Duration::from_millis(250));
                    if wait_stream(socket_fd, self.wake.raw(), wait).is_err() {
                        return Err(PeerError("connection_failed_or_pin_mismatch"));
                    }
                    self.wake.drain();
                }
                Err(_) => return Err(PeerError("connection_failed_or_pin_mismatch")),
            }
        }
    }

    fn send_controls(
        &mut self,
        socket: &mut tungstenite::WebSocket<peer_client::PinnedStream>,
    ) -> Result<()> {
        while let Ok(command) = self.commands.try_recv() {
            socket
                .send(Message::text(
                    serde_json::to_string(&command).map_err(|_| PeerError("invalid_peer_response"))?,
                ))
                .map_err(socket_failure)?;
        }
        Ok(())
    }

    /// Write queued keystrokes once the host has finished the attach handshake.
    ///
    /// Before that they stay in the channel. After `stop` they are discarded:
    /// a key pressed on one PC must not be delivered to the next session this
    /// worker happens to attach.
    fn send_keystrokes(
        &mut self,
        socket: &mut tungstenite::WebSocket<peer_client::PinnedStream>,
    ) -> Result<()> {
        match input_disposition(self.ready, self.stop.load(Ordering::Relaxed)) {
            Disposition::Hold => Ok(()),
            Disposition::Drop => {
                self.discard_input();
                Ok(())
            }
            Disposition::Send => {
                while let Ok(chunk) = self.input.try_recv() {
                    if self.stop.load(Ordering::Relaxed) {
                        self.discard_input();
                        return Ok(());
                    }
                    socket
                        .send(Message::Binary(chunk.into()))
                        .map_err(socket_failure)?;
                }
                Ok(())
            }
        }
    }

    fn discard_input(&mut self) {
        while self.input.try_recv().is_ok() {}
    }

    /// Card ids come from the host's own snapshot and are validated again here,
    /// so nothing but a card id can reach the request path.
    fn attach_path(&self) -> Result<String> {
        let card_id = &self.card_id;
        if card_id.is_empty()
            || card_id.len() > 128
            || !card_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(PeerError("invalid_peer_response"));
        }
        Ok(format!("/api/v1/desktop/terminals/{card_id}/attach"))
    }

    fn apply_event(&mut self, text: &str) -> Result<()> {
        let event: AttachEvent =
            serde_json::from_str(text).map_err(|_| PeerError("invalid_peer_response"))?;
        match event {
            AttachEvent::Attached {
                card_id,
                columns,
                rows,
            } => {
                // A stream that reports another card is not the one we asked
                // for: the emulator must never show a different console.
                if card_id != self.card_id {
                    return Err(PeerError("invalid_peer_response"));
                }
                // A grid from the host is validated before it reaches an emulator.
                TerminalSize { columns, rows }.validate().map_err(|_| {
                    PeerError("invalid_peer_response")
                })?;
                // Keystrokes may already be queued (a paste that raced the
                // handshake). They are written on the next loop turn, and
                // only because this frame was accepted.
                self.ready = true;
                self.send(Event::Attached { columns, rows })
            }
            AttachEvent::Grid { columns, rows } => {
                TerminalSize { columns, rows }.validate().map_err(|_| {
                    PeerError("invalid_peer_response")
                })?;
                self.send(Event::Grid { columns, rows })
            }
        }
    }

    /// Bounded delivery. A viewer that cannot keep up is disconnected rather
    /// than silently skipping terminal output and showing a corrupted screen.
    fn deliver(&mut self, mut bytes: Vec<u8>) -> Result<()> {
        if bytes.len() > ATTACH_MAX_CHUNK {
            return Err(PeerError("peer_response_too_large"));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.events.try_send(Event::Bytes(bytes)) {
                Ok(()) => return Ok(()),
                // `try_send` hands the message back when the queue is full, so
                // the same bytes are retried in order, never dropped.
                Err(error) if error.is_full() => {
                    if Instant::now() >= deadline || self.stop.load(Ordering::Relaxed) {
                        return Err(PeerError("remote_desktop_unavailable"));
                    }
                    bytes = match error.into_inner() {
                        Event::Bytes(bytes) => bytes,
                        _ => return Ok(()),
                    };
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return Err(PeerError("remote_desktop_unavailable")),
            }
        }
    }

    fn send(&mut self, event: Event) -> Result<()> {
        match self.events.try_send(event) {
            // Only the final Closed event may be lost, and the UI stops anyway
            // when the stream ends.
            Ok(()) => Ok(()),
            Err(error) if error.is_full() => Ok(()),
            Err(_) => Err(PeerError("remote_desktop_unavailable")),
        }
    }
}

fn socket_failure(_: SocketError) -> PeerError {
    PeerError("connection_failed_or_pin_mismatch")
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// What to do with keystrokes sitting in the queue.
#[derive(Debug, PartialEq, Eq)]
enum Disposition {
    /// The host has not finished `attached` yet. Leave the bytes where they are.
    Hold,
    /// Handshake is done and this stream is still the selected one.
    Send,
    /// Hide, switch or disconnect. The bytes must not be written later.
    Drop,
}

fn input_disposition(ready: bool, stopped: bool) -> Disposition {
    if stopped {
        Disposition::Drop
    } else if ready {
        Disposition::Send
    } else {
        Disposition::Hold
    }
}

/// `eventfd` the worker polls beside the socket, so a keystroke is written
/// without waiting out the idle read.
struct Wake(RawFd);

impl Wake {
    fn new() -> Arc<Self> {
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(fd >= 0, "eventfd: {}", std::io::Error::last_os_error());
        Arc::new(Self(fd))
    }

    fn notify(&self) {
        let one = 1u64.to_ne_bytes();
        let _ = unsafe { libc::write(self.0, one.as_ptr().cast(), one.len()) };
    }

    fn drain(&self) {
        let mut buf = [0u8; 8];
        loop {
            let n = unsafe { libc::read(self.0, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                break;
            }
        }
    }

    fn raw(&self) -> RawFd {
        self.0
    }

    #[cfg(test)]
    fn pending(&self) -> bool {
        let mut fd = libc::pollfd {
            fd: self.0,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut fd, 1, 0) > 0 && fd.revents & libc::POLLIN != 0 }
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

/// Park until the socket has bytes, a keystroke is queued, or `timeout` elapses.
fn wait_stream(socket_fd: RawFd, wake_fd: RawFd, timeout: Duration) -> std::io::Result<()> {
    let mut fds = [
        libc::pollfd {
            fd: socket_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    loop {
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(card_id: &str, capacity: usize) -> (Worker, Receiver<Event>) {
        let (events, receiver) = mpsc::channel(capacity);
        (
            Worker {
                peer: peer_client::test_peer('a'),
                card_id: card_id.to_string(),
                stop: Arc::new(AtomicBool::new(false)),
                events,
                commands: std::sync::mpsc::channel().1,
                input: std::sync::mpsc::sync_channel(64).1,
                wake: Wake::new(),
                ready: false,
            },
            receiver,
        )
    }

    #[test]
    fn only_a_plain_card_id_can_reach_the_request_path() {
        for bad in ["", "a/b", "..", "a b", "sd_term_x?y", &"x".repeat(129)] {
            assert!(worker(bad, 1).0.attach_path().is_err(), "{bad:?}");
        }
        assert!(worker("card/../other", 1).0.attach_path().is_err());
        assert!(worker("card\r\nHost: evil", 1).0.attach_path().is_err());
        assert_eq!(
            worker("card-one", 1).0.attach_path().unwrap(),
            "/api/v1/desktop/terminals/card-one/attach"
        );
    }

    #[test]
    fn host_events_are_typed_and_a_misrouted_card_is_refused() {
        let (mut stream, mut events) = worker("card-one", 4);
        assert!(stream
            .apply_event(r#"{"type":"attached","cardId":"card-one","columns":120,"rows":40}"#)
            .is_ok());
        assert_eq!(
            events.try_recv(),
            Ok(Event::Attached {
                columns: 120,
                rows: 40
            })
        );
        assert!(stream
            .apply_event(r#"{"type":"grid","columns":80,"rows":24}"#)
            .is_ok());
        assert_eq!(
            events.try_recv(),
            Ok(Event::Grid {
                columns: 80,
                rows: 24
            })
        );
        // Another card's console, an input frame, a malformed control and
        // invalid JSON are all refused rather than rendered.
        for bad in [
            r#"{"type":"attached","cardId":"card-two","columns":120,"rows":40}"#,
            r#"{"type":"grid","columns":0,"rows":24}"#,
            r#"{"type":"resize","columns":80,"rows":24}"#,
            r#"{"type":"grid","columns":80,"rows":24,"data":"typed"}"#,
            "not json",
        ] {
            assert!(stream.apply_event(bad).is_err(), "{bad}");
        }
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn delivered_bytes_stay_ordered_and_bounded() {
        let (mut stream, mut events) = worker("card-one", 4);
        assert!(stream.deliver(vec![0x1b, b'[', b'3', b'1', b'm']).is_ok());
        assert!(stream.deliver(vec![b'h', b'i']).is_ok());
        assert_eq!(
            events.try_recv(),
            Ok(Event::Bytes(vec![0x1b, b'[', b'3', b'1', b'm']))
        );
        assert_eq!(events.try_recv(), Ok(Event::Bytes(vec![b'h', b'i'])));
        // An oversized chunk is a protocol violation, not a partial render.
        assert!(stream.deliver(vec![0; ATTACH_MAX_CHUNK + 1]).is_err());
    }

    #[test]
    fn keystrokes_chunk_and_never_panic() {
        let (input, inbox) = std::sync::mpsc::sync_channel(64);
        let wake = Wake::new();
        let sender = InputSender {
            input,
            stop: Arc::new(AtomicBool::new(false)),
            wake: Arc::clone(&wake),
        };
        sender.send_input(b"hi");
        assert_eq!(inbox.try_recv().unwrap(), b"hi");
        assert!(wake.pending(), "a queued key must wake the worker");
        wake.drain();
        // Oversized pastes split into bounded frames in order.
        sender.send_input(&vec![b'x'; ATTACH_MAX_CHUNK + 1]);
        assert_eq!(inbox.try_recv().unwrap().len(), ATTACH_MAX_CHUNK);
        assert_eq!(inbox.try_recv().unwrap(), vec![b'x'; 1]);
        // A stopped stream or a dead worker drops keys instead of blocking.
        sender.stop.store(true, Ordering::Relaxed);
        sender.send_input(b"dropped");
        assert!(inbox.try_recv().is_err());
        drop(inbox);
        sender.stop.store(false, Ordering::Relaxed);
        sender.send_input(b"also dropped");
    }

    #[test]
    fn keystrokes_wait_for_the_attach_handshake_and_die_with_the_stream() {
        assert_eq!(input_disposition(false, false), Disposition::Hold);
        assert_eq!(input_disposition(true, false), Disposition::Send);
        // Stop wins even if the handshake already completed: a switch must
        // not flush the previous machine's queue into the next attach.
        assert_eq!(input_disposition(true, true), Disposition::Drop);
        assert_eq!(input_disposition(false, true), Disposition::Drop);
    }

    #[test]
    fn a_viewer_can_only_tell_the_host_the_grid_the_host_reported() {
        let (commands, queue) = std::sync::mpsc::channel();
        let stream = TerminalStream {
            stop: Arc::new(AtomicBool::new(false)),
            commands,
            input: std::sync::mpsc::sync_channel(64).0,
            wake: Wake::new(),
        };
        stream.observe_grid(TerminalSize {
            columns: 80,
            rows: 24,
        });
        assert_eq!(
            serde_json::to_string(&queue.try_recv().unwrap()).unwrap(),
            r#"{"type":"grid","columns":80,"rows":24}"#
        );
        // Nothing else is sendable: the type has no other variant.
        stream.stop();
        assert!(stream.stop.load(Ordering::Relaxed));
    }
}
