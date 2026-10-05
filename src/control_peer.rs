//! Local wrappers around existing pinned peer clients; no new peer wire behavior.
use crate::control::{Command, Reply, Request};
use crate::peer_store::PeerStore;
use serde_json::json;
use std::time::Instant;
fn id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
pub fn execute(root: &std::path::Path, request: &Request, deadline: Instant) -> Reply {
    let run = || {
        let fail = |code, message| Reply::failure(&request.request_id, code, message);
        if Instant::now() >= deadline {
            return fail("timeout", "Peer request expired before dispatch.");
        }
        let selected = match &request.command {
            Command::PeerRead { id: None, query } if query == "list" => {
                return match PeerStore::default_store().and_then(|s| s.peers()) {
                    Ok(peers) => Reply::success(
                        &request.request_id,
                        json!({"peers":peers.iter().map(|p|p.summary()).collect::<Vec<_>>(),"credentialsIncluded":false}),
                    ),
                    Err(e) => fail("unavailable", e.0),
                };
            }
            Command::PeerRead { id: Some(id), .. }
            | Command::PeerCommand { id, .. }
            | Command::PeerForget { id } => id,
            _ => return fail("invalid_arguments", "Choose an exact peer operation."),
        };
        if !id(selected) {
            return fail("invalid_arguments", "Use an exact32hex peer machine ID.");
        }
        let peer = match PeerStore::default_store().and_then(|s| s.get(selected)) {
            Ok(p) => p,
            Err(e) => {
                return fail(
                    if e.0 == "peer_not_found" {
                        "not_found"
                    } else {
                        "unavailable"
                    },
                    e.0,
                )
            }
        };
        match &request.command {
            Command::PeerRead { query, .. } => {
                let data = match query.as_str() {
                    "inspect" => {
                        json!({"peer":peer.summary(),"credentialsIncluded":false,"runtimeObserved":false})
                    }
                    "workspace" => match crate::peer_client::verified_workspace(&peer) {
                        Ok((capabilities, workspace)) => {
                            json!({"peerId":selected,"capabilities":capabilities,"workspace":workspace,"source":"pinned-peer"})
                        }
                        Err(e) => return fail("peer_unavailable", e.0),
                    },
                    _ => return fail("invalid_arguments", "Unknown peer read operation."),
                };
                if Instant::now() >= deadline {
                    return fail("timeout", "Peer observation exceeded the local deadline.");
                }
                Reply::success(&request.request_id, data)
            }
            Command::PeerForget { .. } => {
                match PeerStore::default_store().and_then(|s| s.forget(selected)) {
                    Ok(()) => Reply::success(
                        &request.request_id,
                        json!({"peerId":selected,"outcome":"forgotten","remoteCredentialRevoked":false}),
                    ),
                    Err(_) => Reply::unknown(&request.request_id),
                }
            }
            Command::PeerCommand {
                document,
                allow_mutation,
                ..
            } => {
                if !allow_mutation {
                    return fail("denied", "Peer commands require --allow-peer-mutation.");
                }
                let command: crate::desktop_protocol::CommandRequest =
                    match serde_json::from_value(document.clone()) {
                        Ok(c) => c,
                        Err(_) => {
                            return fail(
                                "invalid_arguments",
                                "Input must be an existing typed peer CommandRequest envelope.",
                            )
                        }
                    };
                if command.machine_id != *selected
                    || command.request_id != request.request_id
                    || !crate::desktop_protocol::valid_request_id(&command.request_id)
                    || command.expected_epoch.is_empty()
                    || command.expected_epoch.len() > 128
                    || command.command.shape_error().is_some()
                {
                    return fail("invalid_arguments","Peer envelope must match the exact machine ID and CLI request ID, with valid epoch and command fields.");
                }
                if Instant::now() >= deadline {
                    return fail("timeout", "Peer mutation expired before sending.");
                }
                let response = match crate::peer_client::command(&peer, &command) {
                    Ok(r) => r,
                    Err(_) => return Reply::unknown(&request.request_id),
                };
                use crate::desktop_protocol::CommandResult;
                let mut reply = match &response.result {
                    CommandResult::Applied { .. } => Reply::success(&request.request_id, json!({})),
                    CommandResult::Conflict { .. } => fail(
                        "conflict",
                        "The remote card changed; inspect its current workspace.",
                    ),
                    CommandResult::Rejected { .. } => {
                        fail("peer_rejected", "The peer rejected this command.")
                    }
                };
                reply.data =
                    Some(json!({"peerId":selected,"reply":response,"localFallback":false}));
                reply
            }
            _ => unreachable!(),
        }
    };
    if request.command.is_mutation() {
        crate::control_journal::execute(root, request, |_| run())
    } else {
        run()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_peer_wrappers_redact_credentials_and_never_fallback() {
        if std::env::var_os("SD_PEER_CLI_TEST").is_none() {
            let root = std::env::temp_dir().join(format!("sd-cli-peer-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let out=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","control_peer::tests::local_peer_wrappers_redact_credentials_and_never_fallback","--nocapture"]).env("SD_PEER_CLI_TEST","1").env("HOME",&root).env("SUPER_DESKTOP_PEERS_STATE_DIR",root.join("peers")).output().unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("journal");
        let peer = crate::peer_client::test_peer('a');
        let machine = peer.machine_id.clone();
        PeerStore::default_store().unwrap().upsert(peer).unwrap();
        let request = |id: &str, command| Request {
            control_version: 1,
            request_id: id.into(),
            command,
        };
        let run =
            |r: &Request| execute(&root, r, Instant::now() + std::time::Duration::from_secs(2));
        let listed = run(&request(
            "read",
            Command::PeerRead {
                id: None,
                query: "list".into(),
            },
        ));
        assert!(listed.ok);
        let text = listed.data.unwrap().to_string();
        assert!(!text.contains("token"));
        assert!(!text.contains("fingerprint"));
        assert_eq!(
            run(&request(
                "missing",
                Command::PeerRead {
                    id: Some("b".repeat(32)),
                    query: "inspect".into()
                }
            ))
            .exit_code(),
            3
        );
        let rejected = run(&request(
            "wrong",
            Command::PeerCommand {
                id: machine.clone(),
                document: json!({}),
                allow_mutation: true,
            },
        ));
        assert!(!rejected.ok);
        let forget = request(
            "forget",
            Command::PeerForget {
                id: machine.clone(),
            },
        );
        assert!(run(&forget).ok);
        assert!(run(&forget).ok);
        assert!(PeerStore::default_store().unwrap().get(&machine).is_err());
    }
}
