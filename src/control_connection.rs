//! Owner CLI adapters for the unchanged bridge pairing operations.
use crate::control::{Command, Reply, Request};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::Instant;

pub fn execute(root: &Path, request: &Request, deadline: Instant) -> Reply {
    execute_with(
        root,
        request,
        deadline,
        crate::bridge::owner_pairing_request,
    )
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn bridge_error(request: &Request, error: &str, mutation: bool) -> Reply {
    // No raw response, invitation, token or arbitrary server text in receipts.
    if mutation && matches!(error, "bridge_not_responding" | "invalid_bridge_response") {
        return Reply::unknown(&request.request_id);
    }
    let (code, message) = match error {
        "cli_deadline" => ("timeout", "The local deadline expired before the next bridge operation."),
        "bridge_offline" => ("unavailable", "The bridge is stopped; enable it in Settings → Connections first."),
        "request_expired_or_already_decided" => ("not_found", "The pairing request expired or was already decided."),
        "revoke_old_devices_first" => ("limit_reached", "The bridge device limit was reached."),
        _ => ("unavailable", "The bridge could not complete this operation; inspect connection pending/list before another mutation."),
    };
    Reply::failure(&request.request_id, code, message)
}
fn private_output(path: &str) -> Result<(File, File), ()> {
    let path = Path::new(path);
    let parent = path.parent().ok_or(())?;
    if !path.is_absolute()
        || path.file_name().is_none()
        || parent.canonicalize().map_err(|_| ())? != parent
    {
        return Err(());
    }
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)
        .map_err(|_| ())?;
    let meta = dir.metadata().map_err(|_| ())?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(());
    }
    // Pin the checked directory even if its pathname is replaced concurrently.
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.file_name().unwrap().as_bytes()).map_err(|_| ())?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(());
    }
    Ok((unsafe { File::from_raw_fd(fd) }, dir))
}
fn execute_with(
    root: &Path,
    request: &Request,
    deadline: Instant,
    mut bridge: impl FnMut(&str, &str, Value) -> Result<Value, String>,
) -> Reply {
    let mut run = || {
        let fail = |code, message| Reply::failure(&request.request_id, code, message);
        let mut call = |method, path, body| {
            if Instant::now() >= deadline {
                return Err("cli_deadline".into());
            }
            bridge(method, path, body)
        };
        if Instant::now() >= deadline {
            return fail("timeout", "Connection operation expired before dispatch.");
        }
        match &request.command {
            Command::ConnectionRead { pending } => {
                let (route, key) = if *pending {
                    ("/api/v1/pair/state", "requests")
                } else {
                    ("/api/v1/pair/devices", "devices")
                };
                match call("GET", route, json!({})) {
                    Ok(v) if v[key].is_array() => Reply::success(
                        &request.request_id,
                        json!({key:v[key],"source":"live-bridge","credentialsIncluded":false}),
                    ),
                    Ok(_) => fail(
                        "invalid_response",
                        "The bridge omitted its device/request inventory.",
                    ),
                    Err(e) => bridge_error(request, &e, false),
                }
            }
            Command::ConnectionInvite { output } => {
                // Reserve the file before consuming/replacing an invitation. An
                // empty file may remain on failure and is never overwritten.
                let (mut file, dir) = match private_output(output) {
                    Ok(v) => v,
                    Err(_) => return fail("invalid_arguments", "Use a new absolute output filename inside an existing owner-only directory, without symlink components."),
                };
                let invite = match call("POST", "/api/v1/pair/invitation", json!({})) {
                    Ok(v) => v,
                    Err(e) => return bridge_error(request, &e, true),
                };
                if invite["v"] != 3
                    || !invite["secret"].as_str().is_some_and(|s| hex(s, 48))
                    || invite["expiresIn"] != 300
                {
                    return Reply::unknown(&request.request_id);
                }
                let bytes = format!("{}\n", invite);
                if file
                    .write_all(bytes.as_bytes())
                    .and_then(|_| file.sync_all())
                    .and_then(|_| dir.sync_all())
                    .is_err()
                {
                    return Reply::unknown(&request.request_id);
                }
                Reply::success(
                    &request.request_id,
                    json!({"outcome":"created","output":output,"expiresIn":300,"singleUse":true,"secretIncluded":false}),
                )
            }
            Command::ConnectionDecide {
                id,
                code,
                approve,
                allow_access,
            } => {
                if !hex(id, 48) || code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
                    return fail("invalid_arguments", "Copy the exact request ID and six-digit comparison code from connection pending.");
                }
                if *approve && !allow_access {
                    return fail("denied", "Approval requires --allow-access after comparing the code on the requesting device.");
                }
                let pending = match call("GET", "/api/v1/pair/state", json!({})) {
                    Ok(v) => v,
                    Err(e) => return bridge_error(request, &e, false),
                };
                let Some(rows) = pending["requests"].as_array() else {
                    return fail("invalid_response", "The bridge omitted pending requests.");
                };
                let Some(selected) = rows.iter().find(|v| v["requestId"] == *id) else {
                    return fail("not_found", "No pending request has this exact ID.");
                };
                if selected["code"] != *code {
                    return fail(
                        "conflict",
                        "The comparison code does not match; no decision was sent.",
                    );
                }
                let route = if *approve {
                    "/api/v1/pair/approve"
                } else {
                    "/api/v1/pair/deny"
                };
                match call("POST", route, json!({"requestId":id})) {
                    Ok(v) if v["status"] == if *approve { "approved" } else { "denied" } => {
                        Reply::success(
                            &request.request_id,
                            json!({"pairingRequestId":id,"outcome":if *approve {"approved"} else {"rejected"},"remembered":v["remembered"]!=false}),
                        )
                    }
                    Ok(_) => Reply::unknown(&request.request_id),
                    Err(e) => bridge_error(request, &e, true),
                }
            }
            Command::ConnectionRevoke { id } => {
                if !hex(id, 32) {
                    return fail(
                        "invalid_arguments",
                        "Copy an exact device ID from connection list.",
                    );
                }
                let listed = match call("GET", "/api/v1/pair/devices", json!({})) {
                    Ok(v) => v,
                    Err(e) => return bridge_error(request, &e, false),
                };
                let Some(rows) = listed["devices"].as_array() else {
                    return fail(
                        "invalid_response",
                        "The bridge omitted its device inventory.",
                    );
                };
                if !rows.iter().any(|v| v["id"] == *id) {
                    return fail("not_found", "No registered device has this exact ID.");
                }
                match call("POST", "/api/v1/pair/revoke", json!({"deviceId":id})) {
                    Ok(v) if v["status"] == "revoked" => Reply::success(
                        &request.request_id,
                        json!({"deviceId":id,"outcome":"revoked"}),
                    ),
                    Ok(_) => Reply::unknown(&request.request_id),
                    Err(e) => bridge_error(request, &e, true),
                }
            }
            _ => fail("invalid_arguments", "Expected a connection operation."),
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
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    #[test]
    fn connection_invites_refuse_unsafe_files_and_preserve_unknown_outcomes() {
        let root = Path::new("/tmp").join(format!("sd-conn-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let root = root.canonicalize().unwrap();
        let public = root.join("public");
        std::fs::create_dir(&public).unwrap();
        std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
        let existing = root.join("existing");
        std::fs::write(&existing, b"KEEP").unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&existing, &link).unwrap();
        for (n, path) in [
            public.join("invite"),
            existing.clone(),
            link,
            Path::new("relative.json").to_owned(),
        ]
        .into_iter()
        .enumerate()
        {
            let request = Request {
                control_version: 1,
                request_id: format!("unsafe-{n}"),
                command: Command::ConnectionInvite {
                    output: path.to_string_lossy().into(),
                },
            };
            let reply = execute_with(
                &root.join("journal"),
                &request,
                Instant::now() + std::time::Duration::from_secs(2),
                |_, _, _| panic!("invalid output cannot consume invitation"),
            );
            assert_eq!(reply.exit_code(), 2);
        }
        assert_eq!(std::fs::read(&existing).unwrap(), b"KEEP");
        let file = root.join("uncertain");
        let request = Request {
            control_version: 1,
            request_id: "uncertain".into(),
            command: Command::ConnectionInvite {
                output: file.to_string_lossy().into(),
            },
        };
        let reply = execute_with(
            &root.join("journal"),
            &request,
            Instant::now() + std::time::Duration::from_secs(2),
            |method, path, _| {
                assert_eq!((method, path), ("POST", "/api/v1/pair/invitation"));
                Err("bridge_not_responding".into())
            },
        );
        assert_eq!(reply.error.as_ref().unwrap().outcome, "unknown");
        let replay = execute_with(
            &root.join("journal"),
            &request,
            Instant::now() + std::time::Duration::from_secs(2),
            |_, _, _| panic!("unknown invitation must not replay"),
        );
        assert_eq!(replay.error.unwrap().outcome, "unknown");
        assert!(std::fs::read(&file).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
