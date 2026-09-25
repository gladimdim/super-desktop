//! `POST /api/v1/completions`, with an optional long poll.
//!
//! Every response carries `etag`, a digest of the canonical `terminals`
//! result. A request that sends back the current etag with `waitMs > 0` is
//! held until the result changes, `waitMs` passes, the client hangs up or its
//! device is revoked. While held, it checks once a second whether anything the
//! last result was read from changed (file stamps, pane and agent process
//! start times, the sessions' metadata file names; no subprocess) and
//! re-collects (`tmux list-panes -a`) only then, or when that result is
//! `FULL_RECOLLECT` old, for what none of these shows (see
//! `completion::collect_watched`).
//! Held requests do not take the four asset/job slots; at most `MAX_WAITERS`
//! are held at once and any excess is answered immediately, exactly like a
//! request without `waitMs`.
use super::*;
use crate::completion::{Completion, Watched};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

pub(super) const MAX_WAIT_MS: u64 = 25_000;
pub(super) const MAX_WAITERS: usize = 8;
/// A held request looks for a change this often, and a result younger than
/// this is reused as is.
const RECOLLECT: Duration = Duration::from_secs(1);
/// A held request re-collects at least this often even when nothing it
/// watches changed. The phone holds each request for 25 s.
const FULL_RECOLLECT: Duration = Duration::from_secs(15);
/// Longest nap while readable bytes that are not a hang-up (the next
/// pipelined request) keep the socket ready, so the wait cannot spin.
const HANGUP_CHECK: Duration = Duration::from_millis(250);
/// Distinct id lists whose last result is kept (one per watching phone).
const MAX_SNAPSHOTS: usize = 16;

static WAITERS: AtomicUsize = AtomicUsize::new(0);

pub(super) struct WaitSlot;
impl WaitSlot {
    pub(super) fn acquire() -> Option<Self> {
        WAITERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_WAITERS).then_some(n + 1))
            .ok()
            .map(|_| WaitSlot)
    }
}
impl Drop for WaitSlot {
    fn drop(&mut self) {
        WAITERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// `waitMs` clamped to 0..=25000; absent, negative or non-numeric is 0.
pub(super) fn wait_ms(body: &serde_json::Value) -> u64 {
    let value = &body["waitMs"];
    value
        .as_u64()
        .or_else(|| value.as_f64().filter(|v| v.is_finite() && *v > 0.0).map(|v| v as u64))
        .unwrap_or(0)
        .min(MAX_WAIT_MS)
}

/// Stable digest of the result the client sees. `Completion` has no volatile
/// fields; its serialization order is fixed by the struct.
pub(super) fn etag(terminals: &[Completion]) -> String {
    let canonical = serde_json::to_string(terminals).unwrap_or_default();
    security::digest(canonical.as_bytes())[..32].to_string()
}

/// Hold the request only when the client already has the current result and
/// asked to wait. A missing or different etag is answered immediately.
pub(super) fn should_wait(request_etag: Option<&str>, current: &str, wait_ms: u64) -> bool {
    wait_ms > 0 && request_etag == Some(current)
}

/// `(dev, inode, length, mtime s/ns, ctime s/ns)` of one watched path; `None`
/// when it is missing. A file replaced by rename gets a new inode, so a
/// rewrite shows even within one coarse timestamp tick.
type Stamp = Option<(u64, u64, u64, i64, i64, i64, i64)>;

/// Everything a `Watched` set can show without a subprocess.
#[derive(Clone, Debug, Default, PartialEq)]
struct Fingerprint {
    watch: Watched,
    files: Vec<Stamp>,
    /// Start time (`/proc/<pid>/stat` field 22) of each watched process;
    /// `None` once it has exited. A reused PID has another start time.
    processes: Vec<Option<String>>,
    /// The watched sessions' metadata file names.
    launches: Vec<String>,
}

fn fingerprint(watch: &Watched) -> Fingerprint {
    fingerprint_with(watch, crate::harness_record::start_time)
}

/// `fingerprint` with the process start-time reader passed in (tests).
fn fingerprint_with(watch: &Watched, start_time: impl Fn(u32) -> Option<String>) -> Fingerprint {
    let files = watch
        .files
        .iter()
        .map(|path| {
            fs::metadata(path).ok().map(|m| {
                (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec())
            })
        })
        .collect();
    let processes = watch.processes.iter().map(|&pid| start_time(pid)).collect();
    let mut launches = Vec::new();
    if let (false, Some(dir)) = (watch.sessions.is_empty(), &watch.launch_dir) {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let launch = |session: &String| {
                name.strip_prefix(session.as_str())
                    .and_then(|rest| rest.strip_prefix('-'))
                    .and_then(|rest| rest.strip_suffix(".json"))
                    .is_some_and(|stamp| !stamp.is_empty() && stamp.bytes().all(|b| b.is_ascii_digit()))
            };
            if watch.sessions.iter().any(launch) {
                launches.push(name.to_owned());
            }
        }
        launches.sort();
    }
    Fingerprint { watch: watch.clone(), files, processes, launches }
}

/// The last result for one list of ids, shared by every request for it.
#[derive(Clone)]
struct Snapshot {
    /// When the collection started.
    at: Instant,
    /// Fingerprint of the previous collection's `Watched`, taken just before
    /// this collection ran: a change during it still differs next time.
    seen: Fingerprint,
    /// What this result was read from.
    watch: Watched,
    terminals: Vec<Completion>,
}

static SNAPSHOTS: Mutex<Vec<(Vec<String>, Snapshot)>> = Mutex::new(Vec::new());

/// Why a result is needed.
#[derive(Clone, Copy)]
enum Moment {
    /// A request answered at once: always collected, as before long polling.
    Answer,
    /// The start of a request that may be held.
    Start,
    /// A held request's once-a-second look for a change.
    Tick,
}

#[derive(Debug, PartialEq)]
enum Plan {
    /// Use the last result as is.
    Reuse,
    /// Use it if no watched file changed, else collect.
    Check,
    Collect,
}

/// What to do with a last result `age` old (`None`: there is none). Any
/// held request may reuse a result from the last second; a tick may also keep
/// an older one while no file it was read from changed.
fn plan(age: Option<Duration>, moment: Moment) -> Plan {
    match (age, moment) {
        (None, _) | (_, Moment::Answer) => Plan::Collect,
        (Some(age), _) if age < RECOLLECT => Plan::Reuse,
        (Some(age), Moment::Tick) if age < FULL_RECOLLECT => Plan::Check,
        _ => Plan::Collect,
    }
}

/// The result for `ids`: collected now, or the last one when `plan` allows.
fn current(ids: &[String], moment: Moment) -> Vec<Completion> {
    current_with(ids, moment, crate::completion::collect_watched)
}

/// `current` with the collector passed in (tests count its calls).
fn current_with(
    ids: &[String],
    moment: Moment,
    collect: impl FnOnce(&[String]) -> (Vec<Completion>, Watched),
) -> Vec<Completion> {
    let last = SNAPSHOTS.lock().unwrap().iter().find(|(key, _)| key == ids).map(|(_, s)| s.clone());
    let age = last.as_ref().map(|s| s.at.elapsed());
    let previous = match (plan(age, moment), last) {
        (Plan::Reuse, Some(last)) => return last.terminals,
        (Plan::Check, Some(last)) if fingerprint(&last.watch) == last.seen => return last.terminals,
        (_, last) => last.map(|s| s.watch).unwrap_or_default(),
    };
    // Fingerprint before collecting; see `Snapshot::seen`. The first result
    // for a list of ids has nothing to compare with, so its first check
    // collects once more.
    let at = Instant::now();
    let seen = fingerprint(&previous);
    let (terminals, watch) = collect(ids);
    let snapshot = Snapshot { at, seen, watch, terminals: terminals.clone() };
    let mut snapshots = SNAPSHOTS.lock().unwrap();
    snapshots.retain(|(key, _)| key != ids);
    if snapshots.len() >= MAX_SNAPSHOTS {
        snapshots.remove(0);
    }
    snapshots.push((ids.to_vec(), snapshot));
    terminals
}

/// Whether the peer has closed its side. Bytes that are really the next
/// pipelined request stay buffered in the connection.
fn peer_hung_up(stream: &mut Connection) -> bool {
    let Some(fd) = stream.raw_fd() else { return true };
    let mut poll = libc::pollfd { fd, events: libc::POLLIN | libc::POLLRDHUP, revents: 0 };
    let ready = unsafe { libc::poll(&mut poll, 1, 0) };
    if ready < 0 {
        return std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted;
    }
    if poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL | libc::POLLRDHUP) != 0 {
        return true;
    }
    if poll.revents & libc::POLLIN == 0 {
        return false;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
    match stream.peek(&mut [0u8; 1]) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
    }
}

/// Sleep until `until` in `poll(2)` on the socket, returning early (true) if
/// the peer hangs up. Revocation and expiry shut the socket down, which wakes
/// the poll at once.
fn hung_up_before(stream: &mut Connection, until: Instant) -> bool {
    let Some(fd) = stream.raw_fd() else { return true };
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let mut poll = libc::pollfd { fd, events: libc::POLLIN | libc::POLLRDHUP, revents: 0 };
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        if unsafe { libc::poll(&mut poll, 1, ms) } == 0 {
            continue;
        }
        if peer_hung_up(stream) {
            return true;
        }
        // Readable, but not a hang-up (for example the next pipelined
        // request): nap instead of spinning on a socket that stays ready.
        std::thread::sleep(HANGUP_CHECK.min(until.saturating_duration_since(Instant::now())));
    }
}

pub(super) fn handle(stream: &mut Connection, body: &serde_json::Value, ids: &[String]) {
    let request_etag = body["etag"].as_str();
    let wait = wait_ms(body);
    let start = if wait > 0 && request_etag.is_some() { Moment::Start } else { Moment::Answer };
    let mut terminals = current(ids, start);
    let mut current_etag = etag(&terminals);
    if should_wait(request_etag, &current_etag, wait) {
        if let Some(_slot) = WaitSlot::acquire() {
            // The request is fully read; only hang-up detection reads now.
            stream.streaming();
            let end = Instant::now() + Duration::from_millis(wait);
            loop {
                let tick = (Instant::now() + RECOLLECT).min(end);
                if hung_up_before(stream, tick) || !stream.still_authorized() {
                    stream.persist = false;
                    return;
                }
                terminals = current(ids, Moment::Tick);
                current_etag = etag(&terminals);
                if request_etag != Some(current_etag.as_str()) || tick >= end {
                    break;
                }
            }
        }
    }
    respond(stream, 200, "OK", &serde_json::json!({"terminals": terminals, "etag": current_etag}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::Completion;

    fn completion(state: &str, id: Option<&str>) -> Completion {
        Completion {
            id: "sd_term_a".into(),
            supported: true,
            state: state.into(),
            completion_id: id.map(str::to_string),
        }
    }

    #[test]
    fn etag_tracks_the_result_only() {
        let working = etag(&[completion("working", None)]);
        assert_eq!(working, etag(&[completion("working", None)]), "stable across calls");
        assert_ne!(working, etag(&[completion("completed", Some("abc"))]));
        assert_ne!(etag(&[completion("completed", Some("abc"))]), etag(&[completion("completed", Some("abd"))]));
        assert_eq!(working.len(), 32);
    }

    #[test]
    fn long_poll_decision() {
        // Old clients (no etag / no waitMs) are answered at once.
        assert!(!should_wait(None, "e1", 0));
        assert!(!should_wait(None, "e1", 20_000));
        assert!(!should_wait(Some("e1"), "e1", 0));
        // A stale etag means the client missed a change: answer at once.
        assert!(!should_wait(Some("old"), "e1", 20_000));
        assert!(should_wait(Some("e1"), "e1", 1));
    }

    #[test]
    fn wait_is_clamped() {
        use serde_json::json;
        assert_eq!(wait_ms(&json!({})), 0);
        assert_eq!(wait_ms(&json!({"waitMs": -5})), 0);
        assert_eq!(wait_ms(&json!({"waitMs": "100"})), 0);
        assert_eq!(wait_ms(&json!({"waitMs": 1500})), 1500);
        assert_eq!(wait_ms(&json!({"waitMs": 1500.7})), 1500);
        assert_eq!(wait_ms(&json!({"waitMs": 60_000})), MAX_WAIT_MS);
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sd-completions-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Replace `path` the way harness hooks do: write a temp file, rename it.
    fn replace(path: &std::path::Path, text: &str, mtime: Option<std::time::SystemTime>) {
        let temp = path.with_extension("tmp");
        fs::write(&temp, text).unwrap();
        if let Some(mtime) = mtime {
            fs::File::options().write(true).open(&temp).unwrap().set_modified(mtime).unwrap();
        }
        fs::rename(temp, path).unwrap();
    }

    fn backdate(ids: &[String], by: Duration) {
        let mut snapshots = SNAPSHOTS.lock().unwrap();
        let (_, snapshot) = snapshots.iter_mut().find(|(key, _)| key == ids).unwrap();
        snapshot.at = Instant::now().checked_sub(by).unwrap();
    }

    #[test]
    fn completion_poll_plan() {
        let s = Duration::from_secs;
        // Nothing collected yet, or a request answered at once: collect.
        assert_eq!(plan(None, Moment::Start), Plan::Collect);
        assert_eq!(plan(None, Moment::Tick), Plan::Collect);
        assert_eq!(plan(Some(Duration::ZERO), Moment::Answer), Plan::Collect);
        // A result from the last second serves any request that may be held.
        assert_eq!(plan(Some(Duration::from_millis(900)), Moment::Start), Plan::Reuse);
        assert_eq!(plan(Some(Duration::from_millis(900)), Moment::Tick), Plan::Reuse);
        // An older one only serves a tick, and only while no file changed ...
        assert_eq!(plan(Some(s(1)), Moment::Start), Plan::Collect);
        assert_eq!(plan(Some(s(1)), Moment::Tick), Plan::Check);
        assert_eq!(plan(Some(s(14)), Moment::Tick), Plan::Check);
        // ... and never past the full re-collection interval (pane exits).
        assert_eq!(plan(Some(FULL_RECOLLECT), Moment::Tick), Plan::Collect);
        assert_eq!(plan(Some(s(60)), Moment::Tick), Plan::Collect);
    }

    fn files(paths: &[&std::path::Path]) -> Watched {
        Watched { files: paths.iter().map(|p| p.to_path_buf()).collect(), ..Default::default() }
    }

    #[test]
    fn completion_fingerprint_sees_rewrites_appends_and_closed_descriptors() {
        let dir = scratch("fingerprint");
        let metadata = dir.join("sd_term_a-1.json");
        let rollout = dir.join("rollout-1.jsonl");
        let later = dir.join("rollout-2.jsonl");
        let mtime = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        replace(&metadata, "{\"status\":\"working\"}", Some(mtime));
        fs::write(&rollout, "{}\n").unwrap();
        // The directory's own mtime moves when a file is created in it.
        fs::File::open(&dir).unwrap().set_modified(mtime).unwrap();
        let watch = files(&[&dir, &metadata, &rollout, &later]);
        let before = fingerprint(&watch);
        assert_eq!(before, fingerprint(&watch), "unchanged files, same fingerprint");
        assert!(before.files[3].is_none(), "a missing file still has an entry");

        // A hook rewrite with the same length and mtime is a new inode.
        replace(&metadata, "{\"status\":\"stopped\"}", Some(mtime));
        let rewritten = fingerprint(&watch);
        assert_ne!(before.files[1], rewritten.files[1]);

        // Codex appends to its rollout in place.
        fs::OpenOptions::new().append(true).open(&rollout).unwrap().write_all(b"{}\n").unwrap();
        let appended = fingerprint(&watch);
        assert_ne!(rewritten.files[2], appended.files[2]);

        // A new conversation's rollout appears in the same directory.
        fs::File::open(&dir).unwrap().set_modified(mtime).unwrap();
        let settled = fingerprint(&watch);
        fs::write(&later, "{}").unwrap();
        let created = fingerprint(&watch);
        assert_ne!(settled.files[0], created.files[0], "directory stamp moves on create");
        assert!(created.files[3].is_some());

        // The CLI's descriptor stops resolving once the rollout is closed.
        let open = fs::File::open(&rollout).unwrap();
        use std::os::fd::AsRawFd;
        let descriptor = PathBuf::from(format!("/proc/{}/fd/{}", std::process::id(), open.as_raw_fd()));
        let watch = files(&[&descriptor]);
        let held = fingerprint(&watch);
        assert_eq!(held, fingerprint(&watch));
        assert_eq!(held.files[0], created.files[2], "follows the descriptor to the rollout");
        drop(open);
        assert_ne!(held, fingerprint(&watch));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn completion_fingerprint_sees_exited_and_reused_processes() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let watch = Watched { processes: vec![std::process::id(), child.id()], ..Default::default() };
        // Live and unchanged: same fingerprint, every second.
        let live = fingerprint(&watch);
        assert!(live.processes.iter().all(Option::is_some));
        assert_eq!(live, fingerprint(&watch));
        // A reused PID has another start time.
        let reused = fingerprint_with(&watch, |pid| {
            let start = crate::harness_record::start_time(pid)?;
            Some(if pid == child.id() { format!("{start}1") } else { start })
        });
        assert_ne!(live, reused);
        // The pane or agent process exits.
        child.kill().unwrap();
        child.wait().unwrap();
        let dead = fingerprint(&watch);
        assert_eq!(dead.processes[1], None);
        assert_ne!(live, dead);
        assert_eq!(dead, fingerprint(&watch), "an exit is one change, not a new one each second");
    }

    #[test]
    fn completion_fingerprint_sees_a_relaunch_in_the_same_session() {
        let dir = scratch("launches");
        let watch = Watched {
            sessions: vec!["sd_term_a".into()],
            launch_dir: Some(dir.clone()),
            ..Default::default()
        };
        replace(&dir.join("sd_term_a-100.json"), "{}", None);
        let before = fingerprint(&watch);
        assert_eq!(before.launches, ["sd_term_a-100.json"]);
        // Other sessions' launches, hook rewrites and temp files are not one.
        replace(&dir.join("sd_term_ab-200.json"), "{}", None);
        replace(&dir.join("sd_term_a-100.json"), "{\"status\":\"working\"}", None);
        fs::write(dir.join("sd_term_a-100.4242-0.tmp"), "{}").unwrap();
        assert_eq!(before, fingerprint(&watch));
        // A new launch in the session writes a new metadata file.
        replace(&dir.join("sd_term_a-300.json"), "{}", None);
        assert_ne!(before, fingerprint(&watch));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn completion_held_request_collects_only_after_a_watched_file_changes() {
        let dir = scratch("schedule");
        let watched = dir.join("sd_term_sched-1.json");
        replace(&watched, "one", None);
        let ids = vec!["sd_term_schedule_test".to_string()];
        let calls = std::cell::Cell::new(0);
        let state = std::cell::RefCell::new("working");
        let get = |moment| {
            let terminals = current_with(&ids, moment, |_: &[String]| {
                calls.set(calls.get() + 1);
                (vec![completion(&state.borrow(), None)], files(&[&watched]))
            });
            terminals[0].state.clone()
        };

        assert_eq!(get(Moment::Start), "working");
        assert_eq!(calls.get(), 1, "nothing to reuse yet");
        // The next request may reuse that result for a second ...
        assert_eq!(get(Moment::Start), "working");
        assert_eq!(calls.get(), 1);
        // ... but one answered at once always collects.
        get(Moment::Answer);
        assert_eq!(calls.get(), 2);

        // Ticks with nothing changed reuse the result without collecting.
        for _ in 0..5 {
            backdate(&ids, Duration::from_secs(3));
            get(Moment::Tick);
        }
        assert_eq!(calls.get(), 2);

        // A hook rewrites the metadata file: the next tick collects it.
        *state.borrow_mut() = "completed";
        replace(&watched, "two", None);
        backdate(&ids, Duration::from_secs(3));
        assert_eq!(get(Moment::Tick), "completed");
        assert_eq!(calls.get(), 3);
        backdate(&ids, Duration::from_secs(3));
        get(Moment::Tick);
        assert_eq!(calls.get(), 3, "the change is not collected twice");

        // Pane exits show in no file: collected every FULL_RECOLLECT anyway.
        backdate(&ids, FULL_RECOLLECT);
        get(Moment::Tick);
        assert_eq!(calls.get(), 4);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn completion_held_request_collects_within_a_tick_of_a_process_exit() {
        let mut agent = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let ids = vec!["sd_term_exit_test".to_string()];
        let calls = std::cell::Cell::new(0);
        let watch = Watched { processes: vec![agent.id()], ..Default::default() };
        let tick = |moment| {
            current_with(&ids, moment, |_: &[String]| {
                calls.set(calls.get() + 1);
                (vec![completion("working", None)], watch.clone())
            });
        };
        tick(Moment::Answer);
        backdate(&ids, Duration::from_secs(3));
        tick(Moment::Tick); // the first check has nothing to compare with
        assert_eq!(calls.get(), 2);
        backdate(&ids, Duration::from_secs(3));
        tick(Moment::Tick);
        assert_eq!(calls.get(), 2, "agent still running");
        agent.kill().unwrap();
        agent.wait().unwrap();
        backdate(&ids, Duration::from_secs(3));
        tick(Moment::Tick);
        assert_eq!(calls.get(), 3, "exit seen at the next tick, not after FULL_RECOLLECT");
    }

    #[test]
    fn completion_write_during_a_collection_is_seen_next_tick() {
        let dir = scratch("race");
        let watched = dir.join("sd_term_race-1.json");
        replace(&watched, "one", None);
        let ids = vec!["sd_term_race_test".to_string()];
        let calls = std::cell::Cell::new(0);
        let collect = |rewrite: bool| {
            let watch = files(&[&watched]);
            let watched = watched.clone();
            let calls = &calls;
            move |_: &[String]| {
                calls.set(calls.get() + 1);
                // The hook replaces the file after this collection read it.
                if rewrite {
                    replace(&watched, "two", None);
                }
                (vec![completion("working", None)], watch)
            }
        };
        current_with(&ids, Moment::Answer, collect(false));
        current_with(&ids, Moment::Answer, collect(true));
        assert_eq!(calls.get(), 2);
        backdate(&ids, Duration::from_secs(3));
        current_with(&ids, Moment::Tick, collect(false));
        assert_eq!(calls.get(), 3, "stamps are taken before collecting");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn completion_wait_wakes_on_hang_up_or_shutdown() {
        let pair = || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (server, _) = listener.accept().unwrap();
            (client, server)
        };
        // Nothing happens: the wait lasts until the deadline.
        let (_client, server) = pair();
        let mut connection = Connection::plain(server);
        let start = Instant::now();
        assert!(!hung_up_before(&mut connection, start + Duration::from_millis(300)));
        assert!(start.elapsed() >= Duration::from_millis(300));

        // The next pipelined request is not a hang-up and stays readable.
        let (mut client, server) = pair();
        let mut connection = Connection::plain(server);
        client.write_all(b"GET").unwrap();
        assert!(!hung_up_before(&mut connection, Instant::now() + Duration::from_millis(300)));
        let mut first = [0u8; 1];
        connection.peek(&mut first).unwrap();
        assert_eq!(&first, b"G");

        // The phone closes its socket: noticed at once, not at the deadline.
        let (client, server) = pair();
        let mut connection = Connection::plain(server);
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(client);
        });
        let start = Instant::now();
        assert!(hung_up_before(&mut connection, start + Duration::from_secs(10)));
        assert!(start.elapsed() < Duration::from_secs(2));
        closer.join().unwrap();

        // Revocation/expiry shut the socket down from another thread.
        let (_client, server) = pair();
        let shutter = server.try_clone().unwrap();
        let mut connection = Connection::plain(server);
        let revoker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            shutter.shutdown(std::net::Shutdown::Both).unwrap();
        });
        let start = Instant::now();
        assert!(hung_up_before(&mut connection, start + Duration::from_secs(10)));
        assert!(start.elapsed() < Duration::from_secs(2));
        revoker.join().unwrap();
    }

    #[test]
    fn held_requests_are_bounded() {
        let slots: Vec<_> = std::iter::from_fn(WaitSlot::acquire).take(MAX_WAITERS + 1).collect();
        assert_eq!(slots.len(), MAX_WAITERS);
        assert!(WaitSlot::acquire().is_none());
        drop(slots);
        assert!(WaitSlot::acquire().is_some());
    }
}
