//! Read-only local inventory built from a copy of the daemon's live state.
use crate::{
    control::{self, Command, Reply, Request},
    state::AppState,
};
use serde_json::{json, Value};
use std::time::Instant;

pub struct Snapshot {
    pub state: AppState,
    pub visible: bool,
    pub ready: bool,
}

pub struct Adoption {
    pub data: crate::state::TerminalData,
    pub responder: std::sync::mpsc::SyncSender<Result<(), ()>>,
    pub deadline: Instant,
}

pub struct Query {
    pub responder: std::sync::mpsc::SyncSender<Snapshot>,
    pub deadline: Instant,
}

/// Called on a control worker, never on the GTK thread. Harness discovery may
/// touch executable paths but never runs, downloads or installs a launcher.
pub fn answer(request: Request, snapshot: Snapshot) -> Reply {
    let id = &request.request_id;
    let data = match request.command {
        Command::PeerRead {..} | Command::PeerCommand {..} | Command::PeerForget {..}
        | Command::UpdatesCheck {} | Command::UpdatesInstall {..} | Command::UpdatesStatus {..}
        | Command::Forget {..} | Command::Relaunch {..}
        | Command::Shortcut {..}
        | Command::Audit { .. }
        | Command::Attach { .. }
        | Command::Viewport { .. }
        | Command::Viewports { .. }
        | Command::CardAction { .. }
        | Command::Files { .. }
        | Command::FilesEdit { .. }
        | Command::Preferences { .. }
        | Command::PreferencesEdit { .. }
        | Command::Workspace { .. }
        | Command::WorkspaceEdit { .. }
        | Command::Input { .. }
        | Command::Mode { .. }
        | Command::Close { .. }
        | Command::Geometry { .. }
        | Command::Move { .. }
        | Command::Resize { .. }
        | Command::Launch { .. }
        | Command::InspectRequest { .. }
        | Command::Lifecycle { .. }
        | Command::Composer { .. } | Command::Runtime { .. }
        | Command::Capture { .. } => {
            return Reply::failure(
                id,
                "invalid_request",
                "This request requires a dedicated dispatcher.",
            )
        }
        Command::Access {} => json!({"mode":"owner","uid":unsafe {libc::geteuid()},"delegationSupported":false,"sandbox":false,"credentials":[],"scope":"local-control","socketPermissions":"0600","directoryPermissions":"0700"}),
        Command::Capabilities {} => control::capabilities(),
        Command::Status {} => {
            json!({"serverVersion":env!("CARGO_PKG_VERSION"), "controlVersion":control::VERSION,
            "visible":snapshot.visible,"ready":snapshot.ready,
            "notesCount":snapshot.state.notes.len(),"terminalsCount":snapshot.state.terminals.len()})
        }
        Command::Terminals {} => {
            json!({"terminals":snapshot.state.terminals.iter().map(terminal).collect::<Vec<_>>(),
            "inventory":"saved-cards","runtimeObserved":false})
        }
        Command::Terminal { id: ref card_id } => {
            let Some(card) = snapshot
                .state
                .terminals
                .iter()
                .find(|card| card.id == *card_id)
            else {
                return Reply::failure(id, "not_found", "No local terminal card has that ID.");
            };
            terminal(card)
        }
        Command::Harnesses { all } => json!({"harnesses":harnesses(&snapshot.state).into_iter()
            .filter(|h| all || h["available"] == true).collect::<Vec<_>>() }),
        Command::Harness { id: ref key } => {
            let Some(harness) = harnesses(&snapshot.state)
                .into_iter()
                .find(|h| h["id"] == *key)
            else {
                return Reply::failure(id, "not_found", "No local harness type has that ID.");
            };
            harness
        }
    };
    Reply::success(id, data)
}

fn terminal(card: &crate::state::TerminalData) -> Value {
    json!({"id":card.id,"sessionName":card.session_name,"harnessId":card.agent_type,
        "launchDirectory":card.workspace_dir,"runtimeStatus":"not_observed",
        "geometry":{"x":card.x,"y":card.y,"width":card.width,"height":card.height,
            "restoredWidth":card.restored_width,"restoredHeight":card.restored_height,
            "iconified":card.iconified,"iconX":card.icon_x,"iconY":card.icon_y,
            "units":"logical-pixels","source":"saved-card"},"tag":card.tag})
}

fn harnesses(state: &AppState) -> Vec<Value> {
    let mut result = Vec::new();
    for key in crate::tmux::HARNESS_KEYS {
        let config = crate::tmux::get_agent_config(key);
        let executable = crate::tmux::harness_binary(key);
        let arguments = state
            .harness_args
            .get(*key)
            .cloned()
            .unwrap_or_else(|| crate::launch_args::builtin(key));
        let resolved = crate::tmux::harness_command_with(key, &arguments);
        let available = resolved.is_some();
        let download = executable.is_none() && available && config.npx_package.is_some();
        let reason = if download {
            "package-runner"
        } else if executable.is_some() {
            "installed"
        } else if available {
            "shell-environment-fallback"
        } else {
            "executable-not-found"
        };
        result.push(json!({"id":key,"name":config.name,"source":"builtin", "available":available,
            "availabilityReason":reason,"executable":executable,"mayDownload":download,
            "argumentsConfigured":state.harness_args.contains_key(*key),"argumentsRedacted":true,
            "permissionBypassDetected":permission_bypass(&arguments),
            "permissionPolicyVerified":false,
            "visible":state.visible_harnesses.as_ref().is_none_or(|keys| keys.iter().any(|k| k == key))}));
    }
    for custom in &state.custom_harnesses {
        let available = custom.validate().is_ok();
        result.push(json!({"id":custom.id,"name":custom.name,"source":"custom","available":available,
            "availabilityReason":if available {"configured-executable"} else {"invalid-or-unavailable-launcher"},
            "executable":custom.executable,"mayDownload":Value::Null,"argumentsRedacted":true,
            "argumentsConfigured":true,"permissionBypassDetected":permission_bypass(&custom.arguments),
            "permissionPolicyVerified":false,
            "visible":state.visible_harnesses.as_ref().is_none_or(|keys| keys.contains(&custom.id))}));
    }
    result
}

pub(crate) fn permission_bypass(arguments: &[String]) -> bool {
    arguments.iter().any(|arg| {
        matches!(
            arg.split('=').next().unwrap_or(""),
            "--dangerously-skip-permissions"
                | "--dangerously-bypass-approvals-and-sandbox"
                | "--yolo"
                | "--yes-always"
                | "--auto"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Snapshot {
        Snapshot {
            state: AppState::default(),
            visible: false,
            ready: true,
        }
    }
    fn request(command: Command) -> Request {
        Request {
            control_version: 1,
            request_id: "test".into(),
            command,
        }
    }

    #[test]
    fn inventories_distinguish_types_from_instances_and_do_not_expose_prompts() {
        let reply = answer(request(Command::Terminals {}), snapshot());
        assert_eq!(reply.data.unwrap()["terminals"], json!([]));
        let mut view = snapshot();
        view.state
            .harness_args
            .insert("claude".into(), vec!["--api-key=private-secret".into()]);
        let reply = answer(request(Command::Harnesses { all: true }), view);
        let encoded = serde_json::to_string(&reply).unwrap();
        assert!(!encoded.contains("private-secret"));
        let values = reply.data.unwrap()["harnesses"].as_array().unwrap().clone();
        assert!(values.iter().any(|h| h["id"] == "shell"));
        assert!(values.iter().any(|h| h["id"] == "claude"));
        assert!(values
            .iter()
            .all(|h| h["permissionPolicyVerified"] == false));
    }

    #[test]
    fn inspect_uses_exact_card_identity_and_reports_unobserved_runtime() {
        let mut view = snapshot();
        let card: crate::state::TerminalData = serde_json::from_value(json!({
            "id":"card-123","session_name":"sd_term_123","agent_type":"shell",
            "command":"secret launch command","x":80,"y":100,"created_at":1.0
        }))
        .unwrap();
        view.state.terminals.push(card);
        let reply = answer(
            request(Command::Terminal {
                id: "sd_term_123".into(),
            }),
            snapshot_with_state(&view.state),
        );
        assert_eq!(reply.exit_code(), 3, "session name is not a card ID");
        let reply = answer(
            request(Command::Terminal {
                id: "card-123".into(),
            }),
            view,
        );
        assert!(reply.ok);
        let encoded = serde_json::to_string(&reply).unwrap();
        assert!(!encoded.contains("secret launch command"));
        let data = reply.data.unwrap();
        assert_eq!(data["runtimeStatus"], "not_observed");
        assert_eq!(data["geometry"]["units"], "logical-pixels");
    }

    fn snapshot_with_state(state: &AppState) -> Snapshot {
        Snapshot {
            state: state.clone(),
            visible: false,
            ready: true,
        }
    }
}
