//! Live workspace events for paired PCs: `GET /api/v1/desktop/events` (WSS).
//!
//! One hub per bridge watches the owning daemon's change feed (`desktop-watch`
//! over the owner-only Unix socket) and fetches one snapshot per coalesced
//! change, instead of every stream polling the daemon twice a second. Each
//! subscriber has a small bounded mailbox; one that falls behind loses its
//! queue and is told to `resync` (fetch a snapshot) rather than being buffered
//! without bound. Subscriptions are budgeted per credential and per bridge,
//! end after `STREAM_MAX_SECS`, and die with the socket when the credential is
//! revoked.
//!
//! An owner without the change feed (an older daemon) is polled once a second
//! by the hub — still one poll per bridge, not per viewer.
use super::desktop::WorkspaceError;
use super::*;
use crate::desktop_protocol::{
    WorkspaceEvent, WorkspaceSnapshot, EVENT_HEARTBEAT_SECS, EVENT_QUEUE,
    MAX_EVENT_SUBSCRIPTIONS, MAX_EVENT_SUBSCRIPTIONS_TOTAL,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

/// What the owner published, shared by every subscriber.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Published {
    Snapshot(WorkspaceSnapshot),
    Unavailable(&'static str),
}

impl Published {
    pub(super) fn of(result: Result<WorkspaceSnapshot, WorkspaceError>) -> Self {
        match result {
            Ok(workspace) => Self::Snapshot(workspace),
            Err(error) => Self::Unavailable(error.error),
        }
    }

    fn event(&self, sequence: u64) -> WorkspaceEvent {
        match self {
            Self::Snapshot(workspace) => WorkspaceEvent::Snapshot {
                sequence,
                workspace: workspace.clone(),
            },
            Self::Unavailable(error) => WorkspaceEvent::Unavailable {
                sequence,
                error: (*error).to_string(),
            },
        }
    }

    /// Whether a subscriber that last sent `self` must also send `next`.
    ///
    /// Inside one epoch the owner's revision advances whenever published
    /// content changes, so an equal or lower revision is a copy (or an older
    /// read) of what the viewer already has.
    fn superseded_by(&self, next: &Published) -> bool {
        match (self, next) {
            (Self::Snapshot(old), Self::Snapshot(new)) => {
                old.local.epoch != new.local.epoch || new.local.revision > old.local.revision
            }
            (Self::Unavailable(old), Self::Unavailable(new)) => old != new,
            _ => true,
        }
    }
}

/// Where the owner's changes come from. Production reads the daemon; tests
/// drive a fake so they never touch a real desktop.
pub(super) trait Source: Send + Sync + 'static {
    fn fetch(&self) -> Result<WorkspaceSnapshot, WorkspaceError>;
    /// Block until the owner reports a change, it reports that it is idle, or
    /// no change feed is available.
    fn wait(&self, timeout: Duration) -> Wake;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Wake {
    /// Something may have changed: fetch a snapshot.
    Changed,
    /// Nothing changed for a while. A snapshot is still fetched, because
    /// runtime facts (a session that exited) have no change notification.
    Idle,
    /// No change feed: poll after `Timing::fallback_poll`.
    Unsupported,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Timing {
    /// Silence on a stream after which a heartbeat is sent.
    pub heartbeat: Duration,
    /// How long one stream lives before it asks the viewer to reconnect.
    pub lifetime: Duration,
    /// Poll interval when the owner offers no change feed.
    pub fallback_poll: Duration,
    /// Longest wait between checks of the socket and the credential.
    pub tick: Duration,
}

/// Shortest gap between two owner reads by the hub.
const MIN_FETCH_INTERVAL: Duration = Duration::from_millis(100);

const TIMING: Timing = Timing {
    heartbeat: Duration::from_secs(EVENT_HEARTBEAT_SECS),
    lifetime: Duration::from_secs(STREAM_MAX_SECS),
    fallback_poll: Duration::from_secs(1),
    tick: Duration::from_millis(250),
};

/// One subscriber's bounded queue.
struct Mailbox {
    queue: Mutex<Queue>,
    ready: Condvar,
}

#[derive(Default)]
struct Queue {
    events: VecDeque<Arc<Published>>,
    /// Events were dropped: the subscriber must fetch a snapshot.
    overflowed: bool,
}

impl Mailbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Never blocks the hub and never grows past `EVENT_QUEUE`: a full queue
    /// is replaced by a single "resync required".
    fn push(&self, published: Arc<Published>) {
        let mut queue = self.lock();
        if queue.events.len() >= EVENT_QUEUE {
            queue.events.clear();
            queue.overflowed = true;
        } else if !queue.overflowed {
            queue.events.push_back(published);
        }
        self.ready.notify_all();
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Next {
    Event(Arc<Published>),
    Resync,
    Nothing,
}

struct Entry {
    id: u64,
    device: String,
    mailbox: Arc<Mailbox>,
}

#[derive(Default)]
struct HubState {
    subscribers: Vec<Entry>,
    running: bool,
    next_id: u64,
}

pub(super) struct Hub {
    source: Arc<dyn Source>,
    timing: Timing,
    state: Mutex<HubState>,
}

/// One live subscription. Dropping it releases the budget slot.
pub(super) struct Subscription {
    hub: Arc<Hub>,
    id: u64,
    mailbox: Arc<Mailbox>,
}

impl Subscription {
    /// The next thing to tell the viewer, waiting at most `timeout`.
    pub(super) fn next(&self, timeout: Duration) -> Next {
        let mut queue = self.mailbox.lock();
        if !queue.overflowed && queue.events.is_empty() {
            queue = self
                .mailbox
                .ready
                .wait_timeout_while(queue, timeout, |q| !q.overflowed && q.events.is_empty())
                .map(|(queue, _)| queue)
                .unwrap_or_else(|e| e.into_inner().0);
        }
        if queue.overflowed {
            queue.overflowed = false;
            queue.events.clear();
            return Next::Resync;
        }
        queue.events.pop_front().map_or(Next::Nothing, Next::Event)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut state = self.hub.lock();
        state.subscribers.retain(|entry| entry.id != self.id);
    }
}

impl Hub {
    pub(super) fn new(source: Arc<dyn Source>, timing: Timing) -> Arc<Self> {
        Arc::new(Self {
            source,
            timing,
            state: Mutex::new(HubState::default()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HubState> {
        // A panic in one stream must not make every later subscription fail.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reserve a subscription for one credential, or `None` when that
    /// credential or the bridge as a whole is at its budget.
    pub(super) fn subscribe(self: &Arc<Self>, device: &str) -> Option<Subscription> {
        let mut state = self.lock();
        if state.subscribers.len() >= MAX_EVENT_SUBSCRIPTIONS_TOTAL
            || state
                .subscribers
                .iter()
                .filter(|entry| entry.device == device)
                .count()
                >= MAX_EVENT_SUBSCRIPTIONS
        {
            return None;
        }
        state.next_id += 1;
        let id = state.next_id;
        let mailbox = Arc::new(Mailbox {
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
        });
        state.subscribers.push(Entry {
            id,
            device: device.to_string(),
            mailbox: Arc::clone(&mailbox),
        });
        if !state.running {
            state.running = true;
            let hub = Arc::clone(self);
            let started = std::thread::Builder::new()
                .name("desktop-events".into())
                .spawn(move || hub.run());
            if started.is_err() {
                state.running = false;
                state.subscribers.retain(|entry| entry.id != id);
                return None;
            }
        }
        Some(Subscription {
            hub: Arc::clone(self),
            id,
            mailbox,
        })
    }

    #[cfg(test)]
    pub(super) fn subscribers(&self) -> usize {
        self.lock().subscribers.len()
    }

    fn publish(&self, published: &Arc<Published>) {
        let mailboxes: Vec<_> = self
            .lock()
            .subscribers
            .iter()
            .map(|entry| Arc::clone(&entry.mailbox))
            .collect();
        for mailbox in mailboxes {
            mailbox.push(Arc::clone(published));
        }
    }

    /// Fetch after each change notification, publish only what differs, and
    /// stop once the last subscriber has gone.
    fn run(self: Arc<Self>) {
        let mut last: Option<Arc<Published>> = None;
        let mut fetched: Option<Instant> = None;
        loop {
            // A burst of saves (typing in a note, a settings slider) is read at
            // most ten times a second; the final state is never skipped.
            if let Some(wait) = fetched.and_then(|at| MIN_FETCH_INTERVAL.checked_sub(at.elapsed())) {
                std::thread::sleep(wait);
            }
            {
                let mut state = self.lock();
                if state.subscribers.is_empty() {
                    state.running = false;
                    return;
                }
            }
            fetched = Some(Instant::now());
            let current = Arc::new(Published::of(self.source.fetch()));
            if last.as_deref() != Some(&*current) {
                self.publish(&current);
                last = Some(current);
            }
            if self.source.wait(self.timing.heartbeat) == Wake::Unsupported {
                std::thread::sleep(self.timing.fallback_poll);
            }
        }
    }
}

/// The owning daemon, over its owner-only Unix socket.
struct Owner {
    watch: Mutex<Option<std::io::BufReader<std::os::unix::net::UnixStream>>>,
    /// An owner that refused the feed (an older build) is not asked again
    /// for a minute; the hub polls it meanwhile.
    refused_until: Mutex<Option<Instant>>,
}

impl Owner {
    fn connect(&self) -> Option<std::io::BufReader<std::os::unix::net::UnixStream>> {
        use std::io::BufRead;
        if self
            .refused_until
            .lock()
            .ok()
            .and_then(|until| *until)
            .is_some_and(|until| Instant::now() < until)
        {
            return None;
        }
        let mut socket = std::os::unix::net::UnixStream::connect(crate::get_socket_path()).ok()?;
        let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
        socket.write_all(b"desktop-watch\n").ok()?;
        let mut reader = std::io::BufReader::new(socket);
        let mut first = String::new();
        let accepted = reader.read_line(&mut first).is_ok()
            && serde_json::from_str::<serde_json::Value>(first.trim())
                .ok()
                .is_some_and(|reply| reply["ok"] == true && reply["watch"].is_u64());
        if !accepted {
            if let Ok(mut until) = self.refused_until.lock() {
                *until = Some(Instant::now() + Duration::from_secs(60));
            }
            return None;
        }
        Some(reader)
    }
}

impl Source for Owner {
    fn fetch(&self) -> Result<WorkspaceSnapshot, WorkspaceError> {
        super::desktop::workspace()
    }

    fn wait(&self, timeout: Duration) -> Wake {
        use std::io::BufRead;
        let mut watch = self.watch.lock().unwrap_or_else(|e| e.into_inner());
        if watch.is_none() {
            *watch = self.connect();
        }
        let Some(reader) = watch.as_mut() else {
            return Wake::Unsupported;
        };
        // The owner writes at least an `idle` line every heartbeat; a longer
        // silence is a stalled or vanished daemon.
        let _ = reader
            .get_ref()
            .set_read_timeout(Some(timeout + Duration::from_secs(2)));
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 && line.ends_with('\n') => {
                if line.starts_with("changed") {
                    Wake::Changed
                } else {
                    Wake::Idle
                }
            }
            _ => {
                *watch = None;
                Wake::Unsupported
            }
        }
    }
}

fn hub() -> &'static Arc<Hub> {
    static HUB: OnceLock<Arc<Hub>> = OnceLock::new();
    HUB.get_or_init(|| {
        Hub::new(
            Arc::new(Owner {
                watch: Mutex::new(None),
                refused_until: Mutex::new(None),
            }),
            TIMING,
        )
    })
}

/// Reserve this credential's subscription before the WebSocket upgrade, so a
/// refusal is an HTTP status (`subscription_limit`, 429).
pub(super) fn subscribe(device: &str) -> Option<Subscription> {
    hub().subscribe(device)
}

/// Serve one upgraded event stream until the viewer leaves, the credential is
/// revoked, the socket breaks or the stream's lifetime ends.
pub(super) fn serve(stream: &mut Connection, subscription: Subscription) {
    let hub = Arc::clone(&subscription.hub);
    run_stream(stream, subscription, || Published::of(hub.source.fetch()), TIMING);
}

/// The stream loop, with the snapshot source and timing injectable for tests.
///
/// The subscription is registered before the initial snapshot is read, so a
/// change between the two is queued rather than lost; a queued copy of what was
/// already sent is skipped by revision.
pub(super) fn run_stream(
    stream: &mut Connection,
    subscription: Subscription,
    fetch: impl Fn() -> Published,
    timing: Timing,
) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let deadline = Instant::now() + timing.lifetime;
    let mut sequence = 0u64;
    let send = |stream: &mut Connection, event: WorkspaceEvent| -> bool {
        let Ok(document) = serde_json::to_string(&event) else {
            return false;
        };
        crate::ws::write_text(stream, &document).is_ok()
    };
    let initial = fetch();
    sequence += 1;
    if !send(stream, initial.event(sequence)) {
        return;
    }
    let mut sent: Option<Published> = Some(initial);
    let mut last_write = Instant::now();
    while Instant::now() < deadline {
        // Revocation also shuts the socket down at once; this bounds the
        // window for input TLS had already buffered.
        if !stream.still_authorized() || !ws_client_alive(stream) {
            return;
        }
        let event = match subscription.next(timing.tick) {
            Next::Event(published) => {
                if sent
                    .as_ref()
                    .is_some_and(|sent| !sent.superseded_by(&published))
                {
                    None
                } else {
                    sequence += 1;
                    let event = published.event(sequence);
                    sent = Some((*published).clone());
                    Some(event)
                }
            }
            Next::Resync => {
                // Whatever the viewer fetches is newer than anything we held,
                // so the next published change is always sent.
                sent = None;
                sequence += 1;
                Some(WorkspaceEvent::Resync {
                    sequence,
                    reason: "backpressure".into(),
                })
            }
            Next::Nothing if last_write.elapsed() >= timing.heartbeat => {
                sequence += 1;
                let (epoch, revision) = match &sent {
                    Some(Published::Snapshot(workspace)) => (
                        Some(workspace.local.epoch.clone()),
                        Some(workspace.local.revision),
                    ),
                    _ => (None, None),
                };
                Some(WorkspaceEvent::Heartbeat {
                    sequence,
                    epoch,
                    revision,
                })
            }
            Next::Nothing => None,
        };
        if let Some(event) = event {
            if !send(stream, event) {
                return;
            }
            last_write = Instant::now();
        }
    }
    let _ = crate::ws::write_close(stream, 1000, "reconnect");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn snapshot(epoch: &str, revision: u64, x: i32) -> WorkspaceSnapshot {
        let mut workspace = crate::remote_workspace::fixture();
        workspace.local.epoch = epoch.into();
        workspace.local.revision = revision;
        workspace.local.cards[0].layout.x = x;
        workspace
    }

    /// An owner whose changes the test makes by hand. `wait` blocks until the
    /// test calls `change`, so any extra fetch would be a poll.
    struct Fake {
        current: Mutex<WorkspaceSnapshot>,
        changed: Condvar,
        pending: Mutex<bool>,
        fetches: AtomicUsize,
    }

    impl Fake {
        fn new(workspace: WorkspaceSnapshot) -> Arc<Self> {
            Arc::new(Self {
                current: Mutex::new(workspace),
                changed: Condvar::new(),
                pending: Mutex::new(false),
                fetches: AtomicUsize::new(0),
            })
        }
        fn change(&self, workspace: WorkspaceSnapshot) {
            *self.current.lock().unwrap() = workspace;
            *self.pending.lock().unwrap() = true;
            self.changed.notify_all();
        }
    }

    impl Source for Fake {
        fn fetch(&self) -> Result<WorkspaceSnapshot, WorkspaceError> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            Ok(self.current.lock().unwrap().clone())
        }
        fn wait(&self, timeout: Duration) -> Wake {
            let pending = self.pending.lock().unwrap();
            let (mut pending, _) = self
                .changed
                .wait_timeout_while(pending, timeout, |pending| !*pending)
                .unwrap();
            if std::mem::take(&mut *pending) {
                Wake::Changed
            } else {
                Wake::Idle
            }
        }
    }

    const QUIET: Timing = Timing {
        heartbeat: Duration::from_secs(60),
        lifetime: Duration::from_secs(60),
        fallback_poll: Duration::from_secs(60),
        tick: Duration::from_millis(20),
    };

    #[test]
    fn subscriptions_are_budgeted_per_credential_and_per_bridge() {
        let hub = Hub::new(Fake::new(snapshot("e", 1, 0)), QUIET);
        let mine: Vec<_> = (0..MAX_EVENT_SUBSCRIPTIONS)
            .map(|_| hub.subscribe("device-a").expect("within budget"))
            .collect();
        assert!(hub.subscribe("device-a").is_none(), "per-credential budget");
        let mut others = Vec::new();
        for index in 0.. {
            match hub.subscribe(&format!("device-{index}")) {
                Some(subscription) => others.push(subscription),
                None => break,
            }
        }
        assert_eq!(mine.len() + others.len(), MAX_EVENT_SUBSCRIPTIONS_TOTAL);
        assert!(hub.subscribe("device-new").is_none(), "bridge-wide budget");
        // Releasing a stream releases exactly its slot.
        drop(mine);
        assert_eq!(hub.subscribers(), others.len());
        assert!(hub.subscribe("device-a").is_some());
    }

    #[test]
    fn a_subscriber_that_falls_behind_is_told_to_resync_not_buffered() {
        let hub = Hub::new(Fake::new(snapshot("e", 1, 0)), QUIET);
        let subscription = hub.subscribe("device").unwrap();
        // Wait until the hub's own first publish has been delivered.
        assert!(matches!(subscription.next(Duration::from_secs(2)), Next::Event(_)));
        for revision in 2..(2 + EVENT_QUEUE as u64 * 3) {
            hub.publish(&Arc::new(Published::Snapshot(snapshot("e", revision, 0))));
        }
        let queued = subscription.mailbox.lock().events.len();
        assert!(queued <= EVENT_QUEUE, "queue stayed bounded: {queued}");
        assert_eq!(subscription.next(Duration::ZERO), Next::Resync);
        // After the resync marker the queue restarts with new changes only.
        assert_eq!(subscription.next(Duration::ZERO), Next::Nothing);
        let newer = Arc::new(Published::Snapshot(snapshot("e", 99, 0)));
        hub.publish(&newer);
        assert_eq!(subscription.next(Duration::ZERO), Next::Event(newer));
    }

    #[test]
    fn copies_and_older_reads_are_not_resent_but_restarts_are() {
        let one = Published::Snapshot(snapshot("e", 3, 0));
        assert!(!one.superseded_by(&Published::Snapshot(snapshot("e", 3, 0))));
        assert!(!one.superseded_by(&Published::Snapshot(snapshot("e", 2, 0))));
        assert!(one.superseded_by(&Published::Snapshot(snapshot("e", 4, 0))));
        assert!(one.superseded_by(&Published::Snapshot(snapshot("f", 1, 0))));
        assert!(one.superseded_by(&Published::Unavailable("desktop_unavailable")));
        let down = Published::Unavailable("desktop_unavailable");
        assert!(!down.superseded_by(&Published::Unavailable("desktop_unavailable")));
        assert!(down.superseded_by(&Published::Snapshot(snapshot("e", 3, 0))));
    }

    /// A viewer connection to an in-process stream handler, over plain TCP.
    struct Viewer {
        socket: tungstenite::WebSocket<std::net::TcpStream>,
    }

    impl Viewer {
        fn next(&mut self) -> Option<WorkspaceEvent> {
            loop {
                match self.socket.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        return Some(serde_json::from_str(&text).expect("typed event"))
                    }
                    Ok(tungstenite::Message::Close(_)) | Err(_) => return None,
                    Ok(_) => {}
                }
            }
        }
    }

    fn serve_one(
        hub: &Arc<Hub>,
        timing: Timing,
    ) -> (Viewer, std::net::TcpStream, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let hub = Arc::clone(hub);
        let (server_socket, handed) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            server_socket.send(socket.try_clone().unwrap()).unwrap();
            let mut connection = Connection::plain(socket);
            let request = read_request(&mut connection, None).expect("upgrade request");
            let subscription = hub.subscribe("device").expect("budget");
            assert!(ws_upgrade(&mut connection, &request));
            let source = Arc::clone(&hub.source);
            run_stream(&mut connection, subscription, || Published::of(source.fetch()), timing);
        });
        let client = std::net::TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (socket, _) =
            tungstenite::client(format!("ws://{address}/api/v1/desktop/events"), client)
                .expect("handshake");
        (Viewer { socket }, handed.recv().unwrap(), server)
    }

    #[test]
    fn a_layout_change_reaches_a_subscribed_viewer_without_polling() {
        let fake = Fake::new(snapshot("epoch-a", 1, 100));
        let hub = Hub::new(fake.clone(), QUIET);
        let (mut viewer, _server_socket, server) = serve_one(&hub, QUIET);
        let mut cursor = crate::peer_events::EventCursor::default();
        let first = viewer.next().unwrap();
        assert_eq!(first.sequence(), 1);
        let crate::peer_events::Action::Apply(applied) = cursor.accept(first) else {
            panic!("the initial snapshot is applied");
        };
        assert_eq!(applied.local.cards[0].layout.x, 100);
        // The hub fetched once at start and the stream once for its initial
        // snapshot; with no change, nothing else is read.
        std::thread::sleep(Duration::from_millis(200));
        let settled = fake.fetches.load(Ordering::SeqCst);
        assert!(settled <= 2, "no polling while idle: {settled} fetches");

        // The host moves the card: one change notification, one fetch, one
        // event carrying the new geometry.
        fake.change(snapshot("epoch-a", 2, 700));
        let moved = viewer.next().unwrap();
        assert_eq!(moved.sequence(), 2);
        let crate::peer_events::Action::Apply(applied) = cursor.accept(moved) else {
            panic!("the change is applied in order");
        };
        assert_eq!(applied.local.revision, 2);
        assert_eq!(applied.local.cards[0].layout.x, 700);
        assert_eq!(fake.fetches.load(Ordering::SeqCst), settled + 1);

        drop(viewer);
        server.join().unwrap();
        // The stream released its budget slot when it ended.
        assert_eq!(hub.subscribers(), 0);
    }

    #[test]
    fn heartbeats_name_the_revision_and_the_lifetime_ends_with_reconnect() {
        let fake = Fake::new(snapshot("epoch-a", 7, 0));
        let timing = Timing {
            heartbeat: Duration::from_millis(100),
            lifetime: Duration::from_millis(450),
            ..QUIET
        };
        let hub = Hub::new(fake, timing);
        let (mut viewer, _server_socket, server) = serve_one(&hub, timing);
        assert!(matches!(viewer.next(), Some(WorkspaceEvent::Snapshot { sequence: 1, .. })));
        match viewer.next() {
            Some(WorkspaceEvent::Heartbeat { sequence, epoch, revision }) => {
                assert_eq!(sequence, 2);
                assert_eq!(epoch.as_deref(), Some("epoch-a"));
                assert_eq!(revision, Some(7));
            }
            other => panic!("expected a heartbeat, got {other:?}"),
        }
        // Heartbeats keep counting; the stream then closes with `reconnect`.
        let mut last = 2;
        loop {
            match viewer.socket.read() {
                Ok(tungstenite::Message::Text(text)) => {
                    let event: WorkspaceEvent = serde_json::from_str(&text).unwrap();
                    assert_eq!(event.sequence(), last + 1);
                    last = event.sequence();
                }
                Ok(tungstenite::Message::Close(frame)) => {
                    assert_eq!(frame.unwrap().reason.as_str(), "reconnect");
                    break;
                }
                Ok(_) => {}
                Err(error) => panic!("stream broke instead of closing: {error}"),
            }
        }
        server.join().unwrap();
    }

    #[test]
    fn a_revoked_socket_ends_the_stream_within_a_second() {
        let hub = Hub::new(Fake::new(snapshot("epoch-a", 1, 0)), QUIET);
        let (mut viewer, server_socket, server) = serve_one(&hub, QUIET);
        assert!(viewer.next().is_some());
        assert_eq!(hub.subscribers(), 1);
        // What `security::disconnect` does to every socket of a revoked device.
        let started = Instant::now();
        server_socket.shutdown(std::net::Shutdown::Both).unwrap();
        server.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        assert_eq!(hub.subscribers(), 0);
        assert!(viewer.next().is_none());
    }

    #[test]
    fn the_hub_stops_when_the_last_subscriber_leaves() {
        let fake = Fake::new(snapshot("e", 1, 0));
        let hub = Hub::new(fake.clone(), QUIET);
        drop(hub.subscribe("device").unwrap());
        // Wake the hub so it sees the empty list.
        fake.change(snapshot("e", 2, 0));
        let deadline = Instant::now() + Duration::from_secs(2);
        while hub.lock().running && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!hub.lock().running);
    }
}
