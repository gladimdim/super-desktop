//! `POST /api/v1/completions`, with an optional long poll.
//!
//! Every response carries `etag`, a digest of the canonical `terminals`
//! result. A request that sends back the current etag with `waitMs > 0` is
//! held (re-collecting about once a second, which the Codex/metadata caches
//! make cheap) until the result changes, `waitMs` passes, the client hangs up
//! or its device is revoked. Held requests do not take the four asset/job
//! slots; at most `MAX_WAITERS` are held at once and any excess is answered
//! immediately, exactly like a request without `waitMs`.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

pub(super) const MAX_WAIT_MS: u64 = 25_000;
pub(super) const MAX_WAITERS: usize = 8;
const RECOLLECT: Duration = Duration::from_secs(1);
const HANGUP_CHECK: Duration = Duration::from_millis(250);

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
pub(super) fn etag(terminals: &[crate::completion::Completion]) -> String {
    let canonical = serde_json::to_string(terminals).unwrap_or_default();
    security::digest(canonical.as_bytes())[..32].to_string()
}

/// Hold the request only when the client already has the current result and
/// asked to wait. A missing or different etag is answered immediately.
pub(super) fn should_wait(request_etag: Option<&str>, current: &str, wait_ms: u64) -> bool {
    wait_ms > 0 && request_etag == Some(current)
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

pub(super) fn handle(stream: &mut Connection, body: &serde_json::Value, ids: &[String]) {
    let mut terminals = crate::completion::collect(ids);
    let mut current = etag(&terminals);
    let request_etag = body["etag"].as_str();
    let wait = wait_ms(body);
    if should_wait(request_etag, &current, wait) {
        if let Some(_slot) = WaitSlot::acquire() {
            // The request is fully read; only hang-up detection reads now.
            stream.streaming();
            let end = Instant::now() + Duration::from_millis(wait);
            let mut collected = Instant::now();
            loop {
                let now = Instant::now();
                if now >= end {
                    break;
                }
                std::thread::sleep(HANGUP_CHECK.min(end - now));
                if peer_hung_up(stream) || !stream.still_authorized() {
                    stream.persist = false;
                    return;
                }
                if collected.elapsed() >= RECOLLECT || Instant::now() >= end {
                    terminals = crate::completion::collect(ids);
                    current = etag(&terminals);
                    collected = Instant::now();
                    if request_etag != Some(current.as_str()) {
                        break;
                    }
                }
            }
        }
    }
    respond(stream, 200, "OK", &serde_json::json!({"terminals": terminals, "etag": current}));
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

    #[test]
    fn held_requests_are_bounded() {
        let slots: Vec<_> = std::iter::from_fn(WaitSlot::acquire).take(MAX_WAITERS + 1).collect();
        assert_eq!(slots.len(), MAX_WAITERS);
        assert!(WaitSlot::acquire().is_none());
        drop(slots);
        assert!(WaitSlot::acquire().is_some());
    }
}
