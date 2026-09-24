//! Viewer side of a host's live workspace events (`workspace-events-v1`).
//!
//! A worker thread owns one pinned WSS subscription to
//! `GET /api/v1/desktop/events`, reconnects it with bounded, jittered backoff,
//! and hands typed events to the GTK side through a small bounded channel. It
//! never sends anything to the host: events only describe state, so there is
//! nothing to replay after a reconnect. [`EventCursor`] is the pure decision
//! the GTK side applies to each event (sequence, epoch and revision checks);
//! any doubt becomes one snapshot fetch, never a guess.
use crate::desktop_protocol::{
    WorkspaceEvent, WorkspaceSnapshot, EVENT_HEARTBEAT_SECS, EVENT_MAX_MESSAGE,
    WORKSPACE_EVENTS,
};
use crate::peer_client::{self, Peer, PeerError, Result};
use futures_channel::mpsc::{self, Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tungstenite::{error::Error as SocketError, Message};

const EVENTS_PATH: &str = "/api/v1/desktop/events";
/// Events waiting for the GTK side. A full queue drops the event; the gap in
/// `sequence` then makes the cursor ask for a snapshot.
const QUEUE: usize = 8;
/// Three missed heartbeats mean the subscription is gone even if TCP has not
/// noticed yet.
const SILENCE: Duration = Duration::from_secs(EVENT_HEARTBEAT_SECS * 3);

/// What the GTK side hears from the worker.
#[derive(Debug)]
pub enum Update {
    /// A new connection is open; its sequence numbers start again at 1.
    Connected,
    Event(WorkspaceEvent),
    /// The subscription dropped and the worker is backing off to reconnect.
    Down,
    /// The worker stopped for good (revoked, identity changed, host too old).
    Ended(&'static str),
}

/// What to do with one event.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Draw this snapshot; it is the newest this viewer has seen.
    Apply(Box<WorkspaceSnapshot>),
    /// Nothing new (a heartbeat, a copy or an older read).
    Ignore,
    /// Something was missed or the host restarted: fetch a snapshot.
    Resync,
    /// The host's desktop is not answering.
    Unavailable(String),
}

/// Sequence, epoch and revision bookkeeping for one selected host.
///
/// Shared by the event stream and the snapshot fetch, so an older fetch that
/// lands after a newer event cannot take the view back in time.
#[derive(Debug, Default)]
pub struct EventCursor {
    sequence: Option<u64>,
    epoch: Option<String>,
    revision: u64,
}

impl EventCursor {
    /// A new connection restarts the host's numbering at 1.
    pub fn connected(&mut self) {
        self.sequence = None;
    }

    /// Whether a complete snapshot (from an event or a fetch) is at least as
    /// new as what this view holds, and if so remember it.
    ///
    /// A different epoch is always taken: the host restarted and its revisions
    /// start again.
    pub fn admit(&mut self, snapshot: &WorkspaceSnapshot) -> bool {
        let local = &snapshot.local;
        if self.epoch.as_deref() == Some(local.epoch.as_str()) && local.revision < self.revision {
            return false;
        }
        self.epoch = Some(local.epoch.clone());
        self.revision = local.revision;
        true
    }

    pub fn accept(&mut self, event: WorkspaceEvent) -> Action {
        let sequence = event.sequence();
        let expected = self.sequence.map_or(1, |last| last + 1);
        self.sequence = Some(sequence);
        if sequence != expected {
            // A message was lost (dropped by a full queue here, or by a host
            // that could not keep up): nothing after it can be trusted alone.
            return Action::Resync;
        }
        match event {
            WorkspaceEvent::Snapshot { workspace, .. } => {
                if self
                    .epoch
                    .as_deref()
                    .is_some_and(|epoch| epoch != workspace.local.epoch)
                {
                    // A restarted host: re-verify it with a fetch (capabilities
                    // and identity included) instead of trusting the stream.
                    self.epoch = None;
                    self.revision = 0;
                    return Action::Resync;
                }
                if self.admit(&workspace) {
                    Action::Apply(Box::new(workspace))
                } else {
                    Action::Ignore
                }
            }
            WorkspaceEvent::Heartbeat {
                epoch: Some(epoch),
                revision: Some(revision),
                ..
            } => {
                let other_epoch = self.epoch.as_deref().is_some_and(|known| known != epoch);
                if other_epoch || revision > self.revision {
                    Action::Resync
                } else {
                    Action::Ignore
                }
            }
            WorkspaceEvent::Heartbeat { .. } => Action::Ignore,
            WorkspaceEvent::Resync { .. } => Action::Resync,
            WorkspaceEvent::Unavailable { error, .. } => Action::Unavailable(error),
        }
    }
}

/// Reconnect delays: 1 s doubling to 30 s, each with up to ±20% jitter so a
/// host that restarts is not reconnected to by every viewer at once.
#[derive(Debug, Default)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// `jitter` in [0, 1): 0.5 is the nominal delay.
    pub fn next(&mut self, jitter: f64) -> Duration {
        let base = 1u64 << self.attempt.min(5);
        self.attempt = self.attempt.saturating_add(1);
        let nominal = Duration::from_secs(base.min(30));
        nominal.mul_f64(0.8 + 0.4 * jitter.clamp(0.0, 1.0))
    }
}

fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos % 1000) / 1000.0
}

/// Failures a reconnect cannot fix. The poll path reports them to the user.
fn is_final(reason: &str) -> bool {
    matches!(
        reason,
        "peer_revoked_or_expired"
            | "peer_identity_changed"
            | "update_remote_super_desktop"
            | "peer_endpoint_unavailable"
            | "invalid_peer_record"
            | "invalid_peer_response"
    )
}

/// One host's subscription. Dropping it stops the worker; the host then
/// releases the budget slot when the socket closes.
pub struct WorkspaceEvents {
    stop: Arc<AtomicBool>,
}

impl WorkspaceEvents {
    pub fn open(peer: Peer) -> (Self, Receiver<Update>) {
        let (updates, receiver) = mpsc::channel(QUEUE);
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            peer,
            stop: Arc::clone(&stop),
            updates,
        };
        // A live stream must never run on the GTK thread.
        std::thread::Builder::new()
            .name("peer-events".into())
            .spawn(move || worker.run())
            .expect("workspace events worker");
        (Self { stop }, receiver)
    }
}

impl Drop for WorkspaceEvents {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Worker {
    peer: Peer,
    stop: Arc<AtomicBool>,
    updates: Sender<Update>,
}

impl Worker {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.updates.is_closed()
    }

    fn run(mut self) {
        let mut backoff = Backoff::default();
        while !self.stopped() {
            match self.session(&mut backoff) {
                // A clean end (the stream's lifetime) reconnects at once.
                Ok(()) => {}
                Err(error) if is_final(error.0) => {
                    let _ = self.updates.try_send(Update::Ended(error.0));
                    return;
                }
                Err(_) => {
                    let _ = self.updates.try_send(Update::Down);
                    let wake = Instant::now() + backoff.next(jitter());
                    while Instant::now() < wake {
                        if self.stopped() {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
    }

    fn session(&mut self, backoff: &mut Backoff) -> Result<()> {
        let mut socket = peer_client::desktop_socket_bounded(
            &self.peer,
            EVENTS_PATH,
            WORKSPACE_EVENTS,
            EVENT_MAX_MESSAGE,
            EVENT_MAX_MESSAGE,
        )?;
        if self.updates.try_send(Update::Connected).is_err() {
            return Ok(());
        }
        let mut heard = Instant::now();
        loop {
            if self.stopped() {
                let _ = socket.close(None);
                return Ok(());
            }
            match socket.read() {
                Ok(Message::Text(text)) => {
                    heard = Instant::now();
                    let event = parse(&text, &self.peer.machine_id)?;
                    backoff.reset();
                    // A full queue drops this event; the sequence gap it
                    // leaves makes the cursor fetch a snapshot instead.
                    if let Err(error) = self.updates.try_send(Update::Event(event)) {
                        if error.is_disconnected() {
                            return Ok(());
                        }
                    }
                }
                Ok(Message::Close(frame)) => {
                    let reason = frame
                        .and_then(|frame| crate::desktop_protocol::known_reason(&frame.reason))
                        .unwrap_or("closed");
                    return if reason == "reconnect" {
                        Ok(())
                    } else {
                        Err(PeerError("remote_desktop_unavailable"))
                    };
                }
                Ok(_) => heard = Instant::now(),
                Err(SocketError::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if heard.elapsed() >= SILENCE {
                        return Err(PeerError("remote_desktop_unavailable"));
                    }
                }
                Err(SocketError::Capacity(_)) => {
                    return Err(PeerError("peer_response_too_large"))
                }
                Err(_) => return Err(PeerError("connection_failed_or_pin_mismatch")),
            }
        }
    }
}

/// One host message, typed and checked before it can reach the view: a
/// snapshot must name the host we subscribed to and pass the same validation
/// as a fetched one.
pub fn parse(text: &str, machine_id: &str) -> Result<WorkspaceEvent> {
    let event: WorkspaceEvent =
        serde_json::from_str(text).map_err(|_| PeerError("invalid_peer_response"))?;
    if event.sequence() == 0 {
        return Err(PeerError("invalid_peer_response"));
    }
    if let WorkspaceEvent::Snapshot { workspace, .. } = &event {
        if workspace.machine_id != machine_id {
            return Err(PeerError("peer_identity_changed"));
        }
        crate::remote_workspace::validate(workspace).map_err(PeerError)?;
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(epoch: &str, revision: u64) -> WorkspaceSnapshot {
        let mut workspace = crate::remote_workspace::fixture();
        workspace.local.epoch = epoch.into();
        workspace.local.revision = revision;
        workspace
    }

    fn event(sequence: u64, epoch: &str, revision: u64) -> WorkspaceEvent {
        WorkspaceEvent::Snapshot {
            sequence,
            workspace: snapshot(epoch, revision),
        }
    }

    fn heartbeat(sequence: u64, epoch: &str, revision: u64) -> WorkspaceEvent {
        WorkspaceEvent::Heartbeat {
            sequence,
            epoch: Some(epoch.into()),
            revision: Some(revision),
        }
    }

    #[test]
    fn events_apply_in_sequence_and_a_gap_asks_for_a_snapshot() {
        let mut cursor = EventCursor::default();
        assert!(matches!(cursor.accept(event(1, "e", 1)), Action::Apply(_)));
        assert!(matches!(cursor.accept(event(2, "e", 2)), Action::Apply(_)));
        assert_eq!(cursor.accept(heartbeat(3, "e", 2)), Action::Ignore);
        // Message 4 never arrived.
        assert_eq!(cursor.accept(event(5, "e", 4)), Action::Resync);
        // Numbering continues from what was seen; the fetch brings the state.
        assert_eq!(cursor.accept(heartbeat(6, "e", 2)), Action::Ignore);
        // A stream that does not start at 1 has already lost something.
        let mut fresh = EventCursor::default();
        assert_eq!(fresh.accept(event(2, "e", 1)), Action::Resync);
    }

    #[test]
    fn a_reconnect_restarts_the_numbering() {
        let mut cursor = EventCursor::default();
        assert!(matches!(cursor.accept(event(1, "e", 1)), Action::Apply(_)));
        assert!(matches!(cursor.accept(event(2, "e", 2)), Action::Apply(_)));
        cursor.connected();
        assert!(matches!(cursor.accept(event(1, "e", 3)), Action::Apply(_)));
    }

    #[test]
    fn an_epoch_change_or_a_missed_revision_resyncs() {
        let mut cursor = EventCursor::default();
        assert!(cursor.admit(&snapshot("e", 5)));
        // The host restarted: fetch and re-verify rather than apply blindly.
        assert_eq!(cursor.accept(event(1, "f", 1)), Action::Resync);
        // The fetch admits the new epoch even though its revision is lower.
        assert!(cursor.admit(&snapshot("f", 1)));
        assert!(matches!(cursor.accept(event(2, "f", 2)), Action::Apply(_)));
        // A heartbeat naming a revision we never received means we missed it.
        assert_eq!(cursor.accept(heartbeat(3, "f", 3)), Action::Resync);
        // A heartbeat from another epoch too.
        assert!(cursor.admit(&snapshot("f", 3)));
        assert_eq!(cursor.accept(heartbeat(4, "g", 1)), Action::Resync);
        // An explicit resync from the host is always honoured.
        assert_eq!(
            cursor.accept(WorkspaceEvent::Resync { sequence: 5, reason: "backpressure".into() }),
            Action::Resync
        );
    }

    #[test]
    fn an_older_read_cannot_take_the_view_back() {
        let mut cursor = EventCursor::default();
        assert!(matches!(cursor.accept(event(1, "e", 4)), Action::Apply(_)));
        // A fetch that started before the event and lands after it.
        assert!(!cursor.admit(&snapshot("e", 3)));
        assert!(cursor.admit(&snapshot("e", 4)));
        // An older snapshot on the stream (never sent by a correct host).
        assert_eq!(cursor.accept(event(2, "e", 2)), Action::Ignore);
        assert_eq!(
            cursor.accept(WorkspaceEvent::Unavailable { sequence: 3, error: "desktop_unavailable".into() }),
            Action::Unavailable("desktop_unavailable".into())
        );
    }

    #[test]
    fn events_are_typed_and_bound_to_the_host() {
        let workspace = crate::remote_workspace::fixture();
        let machine = workspace.machine_id.clone();
        let text = serde_json::to_string(&WorkspaceEvent::Snapshot { sequence: 1, workspace }).unwrap();
        assert!(parse(&text, &machine).is_ok());
        assert_eq!(parse(&text, &"b".repeat(32)).unwrap_err().0, "peer_identity_changed");
        // Sequence is mandatory on this stream; free-form or unknown messages fail.
        for bad in [
            r#"{"type":"heartbeat","sequence":0}"#,
            r#"{"type":"surprise","sequence":1}"#,
            "not json",
        ] {
            assert_eq!(parse(bad, &machine).unwrap_err().0, "invalid_peer_response", "{bad}");
        }
        let mut broken = crate::remote_workspace::fixture();
        broken.local.canvas.width = 0;
        let text = serde_json::to_string(&WorkspaceEvent::Snapshot { sequence: 1, workspace: broken }).unwrap();
        assert!(parse(&text, &machine).is_err());
    }

    #[test]
    fn backoff_grows_bounded_with_jitter_and_resets() {
        let mut backoff = Backoff::default();
        let nominal: Vec<_> = (0..8).map(|_| backoff.next(0.5)).collect();
        assert_eq!(nominal[0], Duration::from_secs(1));
        assert_eq!(nominal[1], Duration::from_secs(2));
        assert_eq!(nominal[4], Duration::from_secs(16));
        assert!(nominal.iter().all(|delay| *delay <= Duration::from_secs(30)));
        assert_eq!(nominal[7], Duration::from_secs(30));
        let low = Backoff::default().next(0.0);
        let high = Backoff::default().next(0.999);
        assert!(low >= Duration::from_millis(800) && high < Duration::from_millis(1200));
        backoff.reset();
        assert_eq!(backoff.next(0.5), Duration::from_secs(1));
    }

    #[test]
    fn only_unfixable_failures_stop_reconnecting() {
        assert!(is_final("peer_revoked_or_expired"));
        assert!(is_final("peer_identity_changed"));
        assert!(!is_final("connection_failed_or_pin_mismatch"));
        assert!(!is_final("subscription_limit"));
        assert!(!is_final("remote_desktop_unavailable"));
    }
}
