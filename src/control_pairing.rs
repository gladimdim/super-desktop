//! Pairing jobs reuse the certificate-pinned PC client and its existing handshake.
use crate::control::{Command, Reply, Request};
use crate::peer_client::{Endpoint, Invitation, Pairing};
use crate::peer_store::PeerStore;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

struct Job {
    state: &'static str,
    code: Option<String>,
    peer: Option<Value>,
    error: Option<&'static str>,
}
type Jobs = BTreeMap<String, Arc<Mutex<Job>>>;
static JOBS: OnceLock<Mutex<Jobs>> = OnceLock::new();
fn jobs() -> &'static Mutex<Jobs> {
    JOBS.get_or_init(Mutex::default)
}
fn describe(id: &str, job: &Job) -> Value {
    json!({"jobId":id,"state":job.state,"code":job.code,"peer":job.peer,"error":job.error,"credentialsIncluded":false,"durable":false,"scope":"daemon-lifetime","automaticRetries":false})
}
pub fn execute(root: &Path, request: &Request) -> Reply {
    let fail = |code, message| Reply::failure(&request.request_id, code, message);
    if let Command::PeerPairing { id } = &request.command {
        return match jobs().lock().unwrap().get(id) {
            Some(job) => Reply::success(&request.request_id, describe(id, &job.lock().unwrap())),
            None => fail("not_found", "No pairing job has this ID in this daemon. Inspect its durable request and the host before starting another pairing."),
        };
    }
    crate::control_journal::execute(root, request, |_| {
        let Command::PeerAdd {
            invitation,
            host,
            port,
            name,
        } = &request.command
        else {
            return fail("invalid_arguments", "Expected peer add.");
        };
        let mut invitation = match Invitation::parse(invitation) {
            Ok(v) => v,
            Err(_) => {
                return fail(
                    "invalid_arguments",
                    "Use a complete trusted pairing invitation JSON document or link.",
                )
            }
        };
        invitation.endpoint = match Endpoint::new(
            host.as_deref().unwrap_or(&invitation.endpoint.host),
            port.unwrap_or(invitation.endpoint.port),
        ) {
            Ok(v) => v,
            Err(_) => return fail("invalid_arguments", "Invalid peer address or port."),
        };
        if name
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return fail(
                "invalid_arguments",
                "Peer name must be 1-256 bytes without control characters.",
            );
        }
        if PeerStore::default_store().and_then(|s| s.peers()).is_err() {
            return fail(
                "unavailable",
                "Cannot safely access the owner peer registry; no invitation was sent.",
            );
        }
        let mut all = jobs().lock().unwrap();
        if all.len() >= 32 {
            return fail(
                "limit_reached",
                "The pairing job inventory is full for this daemon lifetime.",
            );
        }
        if all
            .values()
            .any(|j| matches!(j.lock().unwrap().state, "connecting" | "waiting"))
        {
            return fail("busy", "A CLI pairing job is already waiting for the host.");
        }
        let job = Arc::new(Mutex::new(Job {
            state: "connecting",
            code: None,
            peer: None,
            error: None,
        }));
        all.insert(request.request_id.clone(), job.clone());
        drop(all);
        let worker = job.clone();
        let name = name.clone();
        let spawned = std::thread::Builder::new()
            .name("sd-cli-pairing".into())
            .spawn(move || {
                let run = || -> crate::peer_client::Result<Value> {
                    let own = crate::bridge::own_bridge_id();
                    let pairing = Pairing::begin(
                        invitation,
                        own.as_deref(),
                        &crate::bridge::hostname(),
                        name.as_deref(),
                    )?;
                    {
                        let mut j = worker.lock().unwrap();
                        j.state = "waiting";
                        j.code = Some(pairing.code.clone());
                    }
                    loop {
                        if let Some(peer) = pairing.poll()? {
                            let summary = serde_json::to_value(peer.summary())
                                .expect("peer summary serializes");
                            PeerStore::default_store()?.upsert(peer)?;
                            return Ok(summary);
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                };
                let result = run();
                let mut j = worker.lock().unwrap();
                match result {
                    Ok(peer) => {
                        j.state = "paired";
                        j.peer = Some(peer);
                    }
                    Err(error) => {
                        j.state = "failed";
                        j.error = Some(error.0);
                    }
                }
            });
        if spawned.is_err() {
            let mut j = job.lock().unwrap();
            j.state = "failed";
            j.error = Some("cannot_start_pairing_worker");
            return fail(
                "operation_failed",
                "Could not start the pairing worker; no invitation was sent.",
            );
        }
        Reply::success(
            &request.request_id,
            json!({"jobId":request.request_id,"outcome":"queued","jobStateDurable":false,"credentialsIncluded":false,"next":"peer pairing"}),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::process::{Child, Command as Process, Stdio};
    use std::time::Instant;
    const TEST: &str = "control_pairing::tests::cli_pairing_against_isolated_tls_bridge";
    fn env(command: &mut Process, root: &Path) {
        command
            .env("HOME", root)
            .env("XDG_STATE_HOME", root)
            .env("XDG_RUNTIME_DIR", root)
            .env("SUPER_DESKTOP_BRIDGE_STATE_DIR", root.join("bridge"))
            .env("SUPER_DESKTOP_PEERS_STATE_DIR", root.join("peers"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .env_remove("SD_GTK_TESTS_ON_DESKTOP");
    }
    struct Children(Vec<Child>);
    impl Drop for Children {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    fn wait(mut condition: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(15);
        while !condition() {
            assert!(Instant::now() < until, "fixture timed out");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    #[test]
    fn cli_pairing_against_isolated_tls_bridge() {
        if let Some(root) = std::env::var_os("SD_PAIR_CLI_FIXTURE").map(std::path::PathBuf::from) {
            let server = crate::control::Server::bind(&crate::control::runtime_dir()).unwrap();
            server.run(move |request, deadline| match &request.command {
                Command::Capabilities {} => {
                    Reply::success(&request.request_id, crate::control::capabilities())
                }
                Command::PeerAdd { .. } | Command::PeerPairing { .. } => {
                    execute(&root.join("receipts"), &request)
                }
                Command::PeerRead { .. } | Command::PeerForget { .. } => {
                    crate::control_peer::execute(&root.join("receipts"), &request, deadline)
                }
                Command::InspectRequest { id } => {
                    crate::control_journal::inspect(&root.join("receipts"), &request.request_id, id)
                }
                _ => crate::control_connection::execute(&root.join("receipts"), &request, deadline),
            });
            return;
        }
        let current = std::env::current_exe().unwrap();
        let target = current.parent().unwrap().parent().unwrap();
        let binary = target.join("super-desktop");
        let client = target.join("super-desktop-client");
        let root = Path::new("/tmp").join(format!("sd-pair-cli-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let root = root.canonicalize().unwrap();
        struct Remove(std::path::PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _remove = Remove(root.clone());
        let host = root.join("host");
        let viewer = root.join("viewer");
        for dir in [&host, &viewer] {
            std::fs::DirBuilder::new().mode(0o700).create(dir).unwrap();
        }
        let stub = std::os::unix::net::UnixListener::bind(host.join("super-desktop.sock")).unwrap();
        std::thread::spawn(move || {
            for mut socket in stub.incoming().flatten() {
                let mut buf = [0; 256];
                let _ = socket.read(&mut buf);
                let _ = socket.write_all(b"{\"ok\":true,\"presented\":true}\n");
            }
        });
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let mut children = Children(vec![]);
        let mut bridge = Process::new(&binary);
        env(&mut bridge, &host);
        children.0.push(
            bridge
                .args(["harness-bridge", &port.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        wait(|| host.join("bridge/control.sock").exists());
        for dir in [&host, &viewer] {
            let mut fixture = Process::new(&current);
            env(&mut fixture, dir);
            children.0.push(
                fixture
                    .args(["--exact", TEST, "--nocapture"])
                    .env("SD_PAIR_CLI_FIXTURE", dir)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            wait(|| dir.join("super-desktop/control-v1.sock").exists());
        }
        let cli = |exe: &Path, dir: &Path, args: &[&str], expected| -> Value {
            let mut process = Process::new(exe);
            env(&mut process, dir);
            let result = process
                .args(args)
                .args(["--format", "json"])
                .output()
                .unwrap();
            assert_eq!(
                result.status.code(),
                Some(expected),
                "{args:?}: {} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            serde_json::from_slice(&result.stdout).unwrap()
        };
        for (round, approve) in [("allow", true), ("deny", false)] {
            let file = host.join(format!("{round}.json"));
            let filename = file.to_str().unwrap();
            let invite_id = format!("invite-{round}");
            let invitation = cli(
                &binary,
                &host,
                &[
                    "connection",
                    "invite",
                    "--output",
                    filename,
                    "--request-id",
                    &invite_id,
                ],
                0,
            );
            let secret: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            assert!(!invitation
                .to_string()
                .contains(secret["secret"].as_str().unwrap()));
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                cli(
                    &client,
                    &host,
                    &[
                        "connection",
                        "invite",
                        "--output",
                        filename,
                        "--request-id",
                        &invite_id
                    ],
                    0
                ),
                invitation
            );
            cli(
                &client,
                &host,
                &[
                    "connection",
                    "invite",
                    "--output",
                    filename,
                    "--request-id",
                    &format!("overwrite-{round}"),
                ],
                2,
            );
            let args = [
                "peer",
                "add",
                "--file",
                filename,
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--request-id",
                round,
            ];
            let queued = cli(&client, &viewer, &args, 0);
            assert_eq!(cli(&binary, &viewer, &args, 0), queued);
            let mut job = Value::Null;
            wait(|| {
                job = cli(&client, &viewer, &["peer", "pairing", round], 0);
                job["data"]["state"] == "waiting" || job["data"]["state"] == "failed"
            });
            assert_eq!(job["data"]["state"], "waiting", "{job}");
            let pending = cli(&binary, &host, &["connection", "pending"], 0);
            let row = &pending["data"]["requests"][0];
            assert_eq!(row["code"], job["data"]["code"]);
            let id = row["requestId"].as_str().unwrap();
            let code = row["code"].as_str().unwrap();
            cli(
                &client,
                &host,
                &[
                    "connection",
                    "approve",
                    id,
                    "--code",
                    code,
                    "--request-id",
                    &format!("no-access-{round}"),
                ],
                4,
            );
            let wrong = if code == "000000" { "111111" } else { "000000" };
            cli(
                &client,
                &host,
                &[
                    "connection",
                    "approve",
                    id,
                    "--code",
                    wrong,
                    "--allow-access",
                    "--request-id",
                    &format!("wrong-{round}"),
                ],
                5,
            );
            let decision_id = format!("decide-{round}");
            let mut decision = vec![
                "connection",
                if approve { "approve" } else { "reject" },
                id,
                "--code",
                code,
                "--request-id",
                &decision_id,
            ];
            if approve {
                decision.push("--allow-access");
            }
            let decided = cli(&client, &host, &decision, 0);
            assert_eq!(cli(&binary, &host, &decision, 0), decided);
            wait(|| {
                job = cli(&client, &viewer, &["peer", "pairing", round], 0);
                matches!(job["data"]["state"].as_str(), Some("paired" | "failed"))
            });
            if approve {
                assert_eq!(job["data"]["state"], "paired", "{job}");
                let peers = cli(&client, &viewer, &["peer", "list"], 0);
                assert_eq!(peers["data"]["peers"].as_array().unwrap().len(), 1);
                let devices = cli(&client, &host, &["connection", "list"], 0);
                let device = devices["data"]["devices"][0]["id"].as_str().unwrap();
                let revoked = cli(
                    &client,
                    &host,
                    &["connection", "revoke", device, "--request-id", "revoke-1"],
                    0,
                );
                assert_eq!(
                    cli(
                        &binary,
                        &host,
                        &["connection", "revoke", device, "--request-id", "revoke-1"],
                        0
                    ),
                    revoked
                );
                cli(
                    &client,
                    &host,
                    &[
                        "connection",
                        "revoke",
                        device,
                        "--request-id",
                        "revoke-missing",
                    ],
                    3,
                );
                assert!(
                    cli(&client, &host, &["connection", "list"], 0)["data"]["devices"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
            } else {
                assert_eq!(job["data"]["error"], "pairing_denied");
            }
            for dir in [&host, &viewer] {
                for entry in std::fs::read_dir(dir.join("receipts")).unwrap().flatten() {
                    let bytes = std::fs::read_to_string(entry.path()).unwrap();
                    assert!(!bytes.contains(secret["secret"].as_str().unwrap()));
                    assert!(!bytes.contains("\"token\""));
                }
            }
        }
        // Durable replay survives a daemon restart; the in-memory job does not.
        // A missing job must never silently resubmit its consumed invitation.
        children.0[2].kill().unwrap();
        children.0[2].wait().unwrap();
        let mut fixture = Process::new(&current);
        env(&mut fixture, &viewer);
        children.0[2] = fixture
            .args(["--exact", TEST, "--nocapture"])
            .env("SD_PAIR_CLI_FIXTURE", &viewer)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait(|| {
            let probe = crate::control::new_request(Command::Capabilities {}).unwrap();
            crate::control::request_at(&viewer, &probe).ok
        });
        cli(&client, &viewer, &["peer", "pairing", "allow"], 3);
        cli(
            &client,
            &viewer,
            &[
                "peer",
                "add",
                "--file",
                host.join("allow.json").to_str().unwrap(),
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--request-id",
                "allow",
            ],
            0,
        );
        assert!(
            cli(&client, &host, &["connection", "pending"], 0)["data"]["requests"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let stopped = children.0.first_mut().unwrap();
        stopped.kill().unwrap();
        stopped.wait().unwrap();
        cli(&client, &host, &["connection", "list"], 6);
    }
}
