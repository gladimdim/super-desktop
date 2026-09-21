//! Authenticated desktop snapshot routes. Called only after bridge admission,
//! origin checks and paired-device authorization. Never reads state.json.
use super::*;
use crate::desktop_protocol::{LocalWorkspaceSnapshot, WorkspaceEvent, WorkspaceSnapshot};

#[derive(Deserialize)]
struct DesktopReply {
    ok: bool,
    workspace: Option<LocalWorkspaceSnapshot>,
    error: Option<String>,
}

struct WorkspaceError {
    code: u16,
    reason: &'static str,
    error: &'static str,
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
