//! Authenticated desktop snapshot routes and the live terminal attach stream.
//! Called only after bridge admission, origin checks and paired-device
//! authorization. Never reads state.json.
use super::*;
use crate::desktop_protocol::{
    AttachCommand, AttachEvent, LocalWorkspaceSnapshot, TerminalSize, WorkspaceEvent,
    WorkspaceSnapshot, ATTACH_MAX_SECS, MAX_REMOTE_VIEWERS,
};
use crate::terminal_transport::PtyAttachment;
use std::io;
use std::os::fd::RawFd;
use std::time::Instant;

/// Concurrent live terminal attachments across the whole bridge. One device may
/// hold at most `MAX_REMOTE_VIEWERS` of them, matching the viewer's own budget.
const MAX_ATTACHMENTS: usize = 16;
/// One entry per live attachment, holding the owning credential hash.
static ATTACHMENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Reserve an attachment slot for the authenticated device, on behalf of one
/// card. The slot is released when the stream ends, for any reason.
struct AttachmentLease(String);
impl AttachmentLease {
    fn acquire(device: &str) -> Option<Self> {
        let device = device.to_string();
        let mut live = ATTACHMENTS.lock().ok()?;
        if live.len() >= MAX_ATTACHMENTS
            || live.iter().filter(|hash| **hash == device).count() >= MAX_REMOTE_VIEWERS
        {
            return None;
        }
        live.push(device.clone());
        Some(Self(device))
    }
}
impl Drop for AttachmentLease {
    fn drop(&mut self) {
        if let Ok(mut live) = ATTACHMENTS.lock() {
            if let Some(index) = live.iter().position(|hash| *hash == self.0) {
                live.remove(index);
            }
        }
    }
}

#[derive(Deserialize)]
struct DesktopReply {
    ok: bool,
    workspace: Option<LocalWorkspaceSnapshot>,
    error: Option<String>,
}

pub(super) struct WorkspaceError {
    pub(super) code: u16,
    pub(super) reason: &'static str,
    pub(super) error: &'static str,
}

fn workspace() -> Result<WorkspaceSnapshot, WorkspaceError> {
    let reply = match crate::ipc_request("desktop-workspace") {
        crate::Ipc::Reply(reply) => reply,
        crate::Ipc::NoDaemon => return Err(unavailable("desktop_unavailable")),
        crate::Ipc::Stalled => {
            return Err(WorkspaceError {
                code: 504,
                reason: "Gateway Timeout",
                error: "desktop_timeout",
            })
        }
    };
    let local = decode_reply(&reply)?;
    let machine_id = pair_state().lock().unwrap().cfg.bridge_id.clone();
    Ok(WorkspaceSnapshot { machine_id, local })
}

fn unavailable(error: &'static str) -> WorkspaceError {
    WorkspaceError {
        code: 503,
        reason: "Service Unavailable",
        error,
    }
}

fn decode_reply(reply: &str) -> Result<LocalWorkspaceSnapshot, WorkspaceError> {
    let invalid = || WorkspaceError {
        code: 502,
        reason: "Bad Gateway",
        error: "invalid_desktop_response",
    };
    if reply.len() > 1024 * 1024 {
        return Err(invalid());
    }
    let reply: DesktopReply = serde_json::from_str(reply).map_err(|_| invalid())?;
    if !reply.ok {
        return Err(match reply.error.as_deref() {
            Some("desktop_not_ready") => unavailable("desktop_not_ready"),
            Some("too_many_desktop_cards") => unavailable("too_many_desktop_cards"),
            _ => invalid(),
        });
    }
    let snapshot = reply.workspace.ok_or_else(invalid)?;
    if snapshot.epoch.is_empty()
        || snapshot.revision == 0
        || snapshot.cards.len() > crate::workspace_model::MAX_DESKTOP_CARDS
        || snapshot.canvas.width == 0
        || snapshot.canvas.height == 0
        || !snapshot.canvas.scale.is_finite()
        || snapshot.canvas.scale <= 0.0
    {
        return Err(invalid());
    }
    Ok(snapshot)
}

pub(super) fn get_workspace(stream: &mut Connection) {
    match workspace() {
        Ok(snapshot) => respond(stream, 200, "OK", &serde_json::to_value(snapshot).unwrap()),
        Err(error) => respond(
            stream,
            error.code,
            error.reason,
            &serde_json::json!({"error":error.error}),
        ),
    }
}

/// Cards are addressed by the desktop snapshot's own id, never by a tmux target
/// the caller supplied.
fn valid_card_id(card_id: &str) -> bool {
    !card_id.is_empty()
        && card_id.len() <= 128
        && card_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Resolve an owned card to its session. The snapshot is the daemon's own
/// workspace, so this cannot reach an unmanaged tmux session.
fn resolve_session(card_id: &str) -> Result<String, &'static str> {
    let snapshot = workspace().map_err(|error| error.error)?;
    let card = snapshot
        .local
        .cards
        .iter()
        .find(|card| card.card_id == card_id)
        .ok_or("unknown_card")?;
    if card.session_alive == Some(false) {
        return Err("terminal_exited");
    }
    if !card.session_name.starts_with("sd_term_") {
        return Err("unknown_card");
    }
    Ok(card.session_name.clone())
}

/// Everything the attach route needs before it upgrades: the resolved owned
/// session, the host's own grid, and this stream's share of the attach budget.
///
/// Resolving before the upgrade keeps every failure an HTTP status with a stable
/// code instead of a WebSocket that opens only to close immediately.
pub(super) struct AttachTarget {
    pub card_id: String,
    session: String,
    grid: TerminalSize,
    _lease: AttachmentLease,
}

/// The host-owned grid, read before the upgrade so a viewer never receives a
/// stream it could not be sized for.
fn attach_error(code: u16, reason: &'static str, error: &'static str) -> WorkspaceError {
    WorkspaceError { code, reason, error }
}

/// Resolve a card id to an attach target, or a typed error for the caller.
pub(super) fn resolve_attach(
    card_id: &str,
    device: Option<&str>,
) -> std::result::Result<AttachTarget, WorkspaceError> {
    if !valid_card_id(card_id) {
        return Err(attach_error(404, "Not Found", "unknown_card"));
    }
    // Authorization happens first in the route; a request without a credential
    // identity must not consume the attachment budget either.
    let Some(device) = device else {
        return Err(attach_error(401, "Unauthorized", "unknown_card"));
    };
    let lease = AttachmentLease::acquire(device)
        .ok_or_else(|| attach_error(429, "Too Many Requests", "attachment_limit"))?;
    let session = match resolve_session(card_id) {
        Ok(session) => session,
        Err(error) => {
            return Err(match error {
                "terminal_exited" => attach_error(409, "Conflict", "terminal_exited"),
                "desktop_unavailable" | "desktop_not_ready" => {
                    attach_error(503, "Service Unavailable", error)
                }
                _ => attach_error(404, "Not Found", "unknown_card"),
            })
        }
    };
    let grid = crate::terminal_transport::grid(&session)
        .map_err(|_| attach_error(503, "Service Unavailable", "terminal_grid_unknown"))?;
    Ok(AttachTarget {
        card_id: card_id.to_string(),
        session,
        grid,
        _lease: lease,
    })
}

/// Live read-only terminal output for one card, over an upgraded WSS stream.
///
/// The host attaches its own tmux client at the grid it already owns and
/// forwards raw PTY bytes in binary frames. This is one-way by construction:
/// binary frames from the viewer are ignored, and no frame type can type into
/// the host session.
pub(super) fn attach_terminal(mut stream: Connection, target: AttachTarget) {
    match PtyAttachment::open_at(&target.session, target.grid) {
        Ok((pty, grid)) => pump(stream, pty, grid, &target.card_id),
        Err(error) => {
            // Reaching here means the session changed between resolution and
            // attach, so the same stable codes still describe it.
            let reason = match error.to_string().as_str() {
                "invalid_owned_session_name" => "unknown_card",
                "invalid_terminal_size" => "invalid_terminal_size",
                "host_grid_mismatch" | "host_grid_unavailable" => "terminal_grid_unknown",
                _ => "terminal_unavailable",
            };
            let _ = crate::ws::write_close(&mut stream, 1011, reason);
        }
    }
}

/// What a client frame means for the pump loop.
enum Peer {
    Idle,
    Closed,
}

fn pump(mut stream: Connection, mut pty: PtyAttachment, grid: TerminalSize, card_id: &str) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_nodelay(true);
    let attached = AttachEvent::Attached {
        card_id: card_id.to_string(),
        columns: grid.columns,
        rows: grid.rows,
    };
    if crate::ws::write_text(&mut stream, &serde_json::to_string(&attached).unwrap()).is_err() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(ATTACH_MAX_SECS);
    let mut heartbeat = Instant::now();
    let mut grid_checked = Instant::now();
    let mut last_grid = grid;
    let mut draining = true;
    while Instant::now() < deadline {
        // Frames the peer already sent: never block on them, but never leave
        // them unread behind buffered TLS either.
        if draining {
            loop {
                let readable = match peer_readable(&mut stream) {
                    Some(readable) => readable,
                    None => return,
                };
                if !readable {
                    break;
                }
                if matches!(client_frame(&mut stream, &mut pty), Peer::Closed) {
                    return;
                }
            }
            draining = false;
        }
        // Wake at least twice a second so the heartbeat and the attach deadline
        // stay accurate while neither side has data.
        let timeout = if heartbeat.elapsed() >= Duration::from_secs(15) {
            Duration::ZERO
        } else {
            Duration::from_millis(500)
        };
        let (peer_ready, pty_ready) = match wait_for_input(&stream, &pty, timeout) {
            Some(ready) => ready,
            None => return,
        };
        if peer_ready {
            draining = true;
        }
        if pty_ready {
            match pty.read_output(Duration::ZERO) {
                Ok(crate::terminal_transport::Output::Bytes(bytes)) if !bytes.is_empty() => {
                    if crate::ws::write_binary(&mut stream, &bytes).is_err() {
                        return;
                    }
                    // A resize is followed by a full redraw, so this is where
                    // a changed host grid is most cheaply noticed.
                    if grid_checked.elapsed() >= Duration::from_secs(2) {
                        grid_checked = Instant::now();
                        if let Ok(live) = pty.host_grid() {
                            if live != last_grid {
                                last_grid = live;
                                if send_grid(&mut stream, live).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
                Ok(crate::terminal_transport::Output::Bytes(_)) => {}
                Ok(crate::terminal_transport::Output::Pending) => {}
                Ok(crate::terminal_transport::Output::Closed) => {
                    let _ = crate::ws::write_close(&mut stream, 1000, "terminal_exited");
                    return;
                }
                Err(_) => return,
            }
        }
        if heartbeat.elapsed() >= Duration::from_secs(15) {
            heartbeat = Instant::now();
            if crate::ws::write_ping(&mut stream, &[]).is_err() {
                return;
            }
        }
    }
    let _ = crate::ws::write_close(&mut stream, 1000, "reconnect");
}

fn send_grid(stream: &mut Connection, grid: TerminalSize) -> io::Result<()> {
    let event = AttachEvent::Grid {
        columns: grid.columns,
        rows: grid.rows,
    };
    crate::ws::write_text(stream, &serde_json::to_string(&event).unwrap())
}

/// Poll both directions so neither one can starve the other.
///
/// `None` means the connection is gone (revoked, closed or broken). `Some`
/// reports which descriptors have data within `timeout`.
fn wait_for_input(stream: &Connection, pty: &PtyAttachment, timeout: Duration) -> Option<(bool, bool)> {
    let socket: RawFd = stream.raw_fd()?;
    let mut fds = [
        libc::pollfd {
            fd: socket,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: pty.raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout.as_millis() as i32) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Some((false, false));
        }
        return None;
    }
    if fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        return None;
    }
    if fds[1].revents & libc::POLLNVAL != 0 {
        return None;
    }
    Some((
        fds[0].revents & libc::POLLIN != 0,
        fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0,
    ))
}

/// Whether a complete client frame is queued. `None` means the peer is gone.
///
/// TLS may hold already-decrypted bytes that `poll` cannot see, so this probe
/// is what keeps a second frame in the same record from stalling.
fn peer_readable(stream: &mut Connection) -> Option<bool> {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1)));
    match stream.peek(&mut [0u8; 1]) {
        Ok(0) => None,
        Ok(_) => Some(true),
        Err(error) => match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Some(false),
            _ => None,
        },
    }
}

fn client_frame(stream: &mut Connection, pty: &mut PtyAttachment) -> Peer {
    // A complete frame is queued; a peer that stalls mid-frame is dropped rather
    // than allowed to hold terminal output hostage.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    match crate::ws::read_frame(stream) {
        Ok(None) | Ok(Some(crate::ws::Frame::Close)) => Peer::Closed,
        Ok(Some(crate::ws::Frame::Ping(payload))) => {
            if crate::ws::write_pong(stream, &payload).is_err() {
                Peer::Closed
            } else {
                Peer::Idle
            }
        }
        Ok(Some(crate::ws::Frame::Text(text))) => apply_control(stream, pty, &text),
        // Terminal input is not part of this increment. Binary frames are
        // consumed and dropped, never replayed into the host session.
        Ok(Some(_)) => Peer::Idle,
        Err(_) => Peer::Closed,
    }
}

fn apply_control(stream: &mut Connection, pty: &mut PtyAttachment, text: &str) -> Peer {
    let Some(AttachCommand::Grid { columns, rows }) = AttachCommand::parse(text) else {
        // Unknown or malformed controls are ignored; the stream stays usable.
        return Peer::Idle;
    };
    let requested = TerminalSize { columns, rows };
    match pty.host_grid() {
        // The viewer only ever asks for the host's own grid, so applying it
        // changes nothing that the host did not already decide.
        Ok(live) if live == requested => {
            if pty.set_view_size(requested).is_err() {
                return Peer::Closed;
            }
        }
        Ok(live) => {
            if send_grid(stream, live).is_err() {
                return Peer::Closed;
            }
        }
        Err(_) => {}
    }
    Peer::Idle
}

pub(super) fn stream_workspace(mut stream: Connection) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let deadline = std::time::Instant::now() + Duration::from_secs(STREAM_MAX_SECS);
    let mut previous = String::new();
    let mut heartbeat = std::time::Instant::now();
    while std::time::Instant::now() < deadline {
        if !ws_client_alive(&mut stream) {
            return;
        }
        // Every message is a complete, independently usable snapshot. Polling
        // the owning model avoids a snapshot/subscription gap without an event
        // log; intermediate edits may coalesce, but the final state cannot be
        // lost. Runtime collection is cached/off-thread in the daemon.
        let event = match workspace() {
            Ok(workspace) => WorkspaceEvent::Snapshot { workspace },
            Err(error) => WorkspaceEvent::Unavailable {
                error: error.error.into(),
            },
        };
        let document = serde_json::to_string(&event).unwrap();
        if document != previous || heartbeat.elapsed() >= Duration::from_secs(5) {
            if crate::ws::write_text(&mut stream, &document).is_err() {
                return;
            }
            previous = document;
            heartbeat = std::time::Instant::now();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = crate::ws::write_close(&mut stream, 1000, "reconnect");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_and_unavailable_daemon_responses_are_not_empty_workspaces() {
        for invalid in [
            "{}",
            "{\"ok\":true}",
            "{\"ok\":true,\"workspace\":{}}",
            "not json",
        ] {
            assert!(decode_reply(invalid).is_err());
        }
        let error = decode_reply(r#"{"ok":false,"error":"desktop_not_ready"}"#)
            .err()
            .unwrap();
        assert_eq!(error.code, 503);
        assert_eq!(error.error, "desktop_not_ready");
    }
}
