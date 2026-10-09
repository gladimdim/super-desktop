//! Single-use owner-only local PTY streams, independent of the phone bridge.
use crate::control::{self, Command, Reply, Request};
use crate::control_close::{Target, UiResult};
use crate::control_terminal::probe;
use crate::terminal_transport::{Output, PtyAttachment};
use base64::Engine;
use serde_json::{json, Value};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
/// Local attachments in progress, at most four.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Socket {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl Drop for Socket {
    fn drop(&mut self) {
        if control::private_file(&self.path, true)
            .is_ok_and(|m| m.dev() == self.device && m.ino() == self.inode)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
fn send(socket: &mut UnixStream, event: Value) -> io::Result<()> {
    control::write_frame(
        socket,
        &serde_json::to_vec(&event)?,
        65536,
        Instant::now() + Duration::from_millis(500),
    )
}
fn readable(socket: &UnixStream, timeout: i32) -> io::Result<bool> {
    let mut fd = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let n = unsafe { libc::poll(&mut fd, 1, timeout) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n > 0)
    }
}

pub fn execute(
    root: &Path,
    request: &Request,
    deadline: Instant,
    mut target: impl FnMut() -> Result<UiResult, ()>,
) -> Reply {
    crate::control_journal::execute(root, request, |_| {
        let fail = |code, message| Reply::failure(&request.request_id, code, message);
        let Command::Attach {
            id,
            interactive,
            seconds,
            expect_pane_identity,
            ..
        } = &request.command
        else {
            return fail("invalid_request", "Expected attach.");
        };
        if !(1..=300).contains(seconds)
            || expect_pane_identity.len() != 64
            || !expect_pane_identity.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return fail(
                "invalid_arguments",
                "Use 1..300 seconds and paneIdentity from runtime.",
            );
        }
        let owner = match target() {
            Ok(Ok(Some(t))) => t,
            Ok(Err(r)) => return r,
            _ => return fail("timeout", "Attachment target lookup failed."),
        };
        if owner.data.id != *id
            || owner.task.is_closed()
            || !probe(&owner.data.session_name, deadline)
                .is_ok_and(|p| !p.dead && p.identity == *expect_pane_identity)
        {
            return fail("conflict", "Pane identity changed or exited.");
        }
        let Some(slot) = crate::platform::permit::Permit::try_acquire(&ACTIVE, 4) else {
            return fail("busy", "At most four local attachments may be active.");
        };
        let nonce = match control::new_request(Command::Status {}) {
            Ok(r) => r.request_id,
            Err(_) => return fail("unavailable", "OS randomness unavailable."),
        };
        let directory = control::runtime_dir().join("super-desktop");
        if control::private_dir(&directory).is_err() {
            return fail("unsafe_socket", "Attachment directory is not private.");
        }
        let name = format!("attach-{nonce}.sock");
        let path = directory.join(&name);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(_) => return fail("unavailable", "Cannot bind private attachment socket."),
        };
        if std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).is_err() {
            let _ = std::fs::remove_file(&path);
            return fail("unsafe_socket", "Cannot secure attachment socket.");
        }
        let metadata = match control::private_file(&path, true) {
            Ok(m) => m,
            Err(_) => return fail("unsafe_socket", "Attachment socket ownership mismatch."),
        };
        let cleanup = Socket {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if listener.set_nonblocking(true).is_err() {
            return fail("unavailable", "Cannot prepare attachment listener.");
        }
        let (identity, interactive, seconds) =
            (expect_pane_identity.clone(), *interactive, *seconds);
        let token = nonce.clone();
        let spawned = std::thread::Builder::new()
            .name("sd-cli-attach".into())
            .spawn(move || {
                let (_slot, _cleanup) = (slot, cleanup);
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            if control::check_peer(&stream).is_err() {
                                continue;
                            }
                            let handshake = control::read_frame(&mut stream, 256, deadline)
                                .ok()
                                .and_then(|v| serde_json::from_slice::<Value>(&v).ok());
                            if handshake.as_ref().and_then(|v| v["token"].as_str())
                                != Some(token.as_str())
                            {
                                break;
                            }
                            drop(listener);
                            let _ = serve(stream, &token, owner, &identity, interactive, seconds);
                            break;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(_) => break,
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        if spawned.is_err() {
            return fail("unavailable", "Cannot start attachment worker.");
        }
        Reply::success(
            &request.request_id,
            json!({"id":id,"socket":name,"token":nonce,"connectWithinMs":5000,"seconds":seconds,"interactive":interactive,"outcome":"stream_ready","singleUse":true,"gridOwnership":false}),
        )
    })
}
fn serve(
    mut socket: UnixStream,
    stream_id: &str,
    owner: Target,
    identity: &str,
    interactive: bool,
    seconds: u16,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(seconds.into());
    let alive = || {
        !owner.task.is_closed()
            && probe(
                &owner.data.session_name,
                Instant::now() + Duration::from_millis(500),
            )
            .is_ok_and(|p| !p.dead && p.identity == identity)
    };
    if !alive() {
        return send(
            &mut socket,
            json!({"type":"end","reason":"pane_changed","ok":false}),
        );
    }
    let (mut attachment, mut grid) = PtyAttachment::open_cli(
        &owner.data.session_name,
        Instant::now() + Duration::from_secs(2),
    )?;
    if !alive() {
        return send(
            &mut socket,
            json!({"type":"end","reason":"pane_changed","ok":false}),
        );
    }
    send(
        &mut socket,
        json!({"type":"attached","ok":true,"streamId":stream_id,"sequence":0,"paneIdentity":identity,"grid":grid,"interactive":interactive,"gridOwnership":false}),
    )?;
    let mut sequence = 0u64;
    let mut total = 0usize;
    let mut next_check = Instant::now();
    let mut reason = "duration";
    while Instant::now() < deadline {
        if Instant::now() >= next_check {
            if !alive() {
                reason = "pane_changed";
                break;
            }
            let latest = PtyAttachment::cli_grid(
                &owner.data.session_name,
                Instant::now() + Duration::from_millis(500),
            )?;
            if latest != grid {
                attachment.set_view_size(latest)?;
                grid = latest;
                sequence += 1;
                send(
                    &mut socket,
                    json!({"type":"grid","ok":true,"streamId":stream_id,"sequence":sequence,"grid":grid}),
                )?;
            }
            next_check = Instant::now() + Duration::from_millis(500);
        }
        if readable(&socket, 0)? {
            let frame = control::read_frame(
                &mut socket,
                8192,
                Instant::now() + Duration::from_millis(500),
            )?;
            let input: Value = serde_json::from_slice(&frame)?;
            if input["type"] == "detach" {
                reason = "detached";
                break;
            }
            if !interactive || input["type"] != "input" {
                reason = "input_refused";
                break;
            }
            let bytes = input["bytes"]
                .as_str()
                .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
                .filter(|v| !v.is_empty() && v.len() <= 4096)
                .ok_or_else(|| io::Error::other("invalid input frame"))?;
            if !alive() {
                reason = "pane_changed";
                break;
            }
            let input_deadline = Instant::now() + Duration::from_millis(500);
            owner
                .task
                .with_idle_until(input_deadline, || -> io::Result<()> {
                    if !alive() {
                        return Err(io::Error::other("pane changed before input"));
                    }
                    let mut pending = &bytes[..];
                    while !pending.is_empty() {
                        if Instant::now() >= input_deadline {
                            return Err(io::Error::other("input outcome unknown"));
                        }
                        match attachment.write_input(pending) {
                            Ok(0) => return Err(io::Error::other("input closed")),
                            Ok(n) => pending = &pending[n..],
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(2))
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    Ok(())
                })
                .ok_or_else(|| io::Error::other("input busy; nothing replayed"))??;
        }
        match attachment.read_output(Duration::from_millis(20))? {
            Output::Bytes(bytes) => {
                total += bytes.len();
                sequence += 1;
                send(
                    &mut socket,
                    json!({"type":"output","ok":true,"streamId":stream_id,"sequence":sequence,"encoding":"base64","bytes":base64::engine::general_purpose::STANDARD.encode(bytes)}),
                )?;
                if total >= 16 * 1024 * 1024 {
                    reason = "output_limit";
                    break;
                }
            }
            Output::Pending => {}
            Output::Closed => {
                reason = "client_closed";
                break;
            }
        }
    }
    // Dropping the PTY client never kills the harness. Input is never replayed.
    drop(attachment);
    send(
        &mut socket,
        json!({"type":"end","streamId":stream_id,"sequence":sequence+1,"reason":reason,"ok":matches!(reason,"duration"|"detached"|"output_limit"|"client_closed")}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command as Process, Stdio};
    use std::sync::Arc;
    #[test]
    fn local_attachment_is_guarded_bounded_and_never_kills_session() {
        if std::env::var_os("SD_ATTACH_TEST").is_none() {
            let root = PathBuf::from("/tmp").join(format!("sd-attach-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            let root = root.canonicalize().unwrap();
            let output=Process::new(std::env::current_exe().unwrap()).args(["--exact","control_attach::tests::local_attachment_is_guarded_bounded_and_never_kills_session","--nocapture"]).env("SD_ATTACH_TEST","1").env("HOME",&root).env("XDG_RUNTIME_DIR",&root).env("TMUX_TMPDIR",&root).env_remove("TMUX").env_remove("TMUX_PANE").env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY").output().unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(output.status.success(), "{output:?}");
            return;
        }
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Process::new("tmux")
                    .args(["kill-server"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _cleanup = Cleanup;
        let root = control::runtime_dir();
        let _server = control::Server::bind(&root).unwrap();
        let session = "sd_term_attach";
        let made = Process::new("tmux")
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                session,
                "-x",
                "80",
                "-y",
                "24",
                "cat",
            ])
            .output()
            .unwrap();
        assert!(made.status.success());
        let deadline = || Instant::now() + Duration::from_secs(3);
        let before = probe(session, deadline()).unwrap();
        let grid = PtyAttachment::cli_grid(session, deadline()).unwrap();
        let card:crate::state::TerminalData=serde_json::from_value(json!({"id":session,"session_name":session,"agent_type":"shell","command":"cat","x":10,"y":100,"width":500,"height":300,"iconified":false,"created_at":1.0})).unwrap();
        let task = Arc::new(crate::session_task::SessionTask::default());
        let target = || {
            Ok(Ok(Some(Target {
                data: card.clone(),
                task: task.clone(),
            })))
        };
        let request = |interactive, name: &str| Request {
            control_version: 1,
            request_id: name.into(),
            command: Command::Attach {
                id: session.into(),
                interactive,
                seconds: 2,
                expect_epoch: "epoch".into(),
                expect_revision: "a".repeat(64),
                expect_pane_identity: before.identity.clone(),
            },
        };
        let connect = |reply: Reply| {
            assert!(reply.ok, "{reply:?}");
            let data = reply.data.unwrap();
            let path = root
                .join("super-desktop")
                .join(data["socket"].as_str().unwrap());
            let mut socket = UnixStream::connect(&path).unwrap();
            control::write_frame(
                &mut socket,
                &serde_json::to_vec(&json!({"token":data["token"]})).unwrap(),
                256,
                deadline(),
            )
            .unwrap();
            (socket, path)
        };
        let event = |socket: &mut UnixStream| {
            serde_json::from_slice::<Value>(
                &control::read_frame(socket, 65536, deadline()).unwrap(),
            )
            .unwrap()
        };
        let readonly = request(false, "readonly");
        let reply = execute(&root.join("journal"), &readonly, deadline(), target);
        assert!(
            execute(&root.join("journal"), &readonly, deadline(), || panic!(
                "receipt must not spawn again"
            ))
            .ok
        );
        let (mut socket, path) = connect(reply);
        assert_eq!(event(&mut socket)["type"], "attached");
        control::write_frame(&mut socket,&serde_json::to_vec(&json!({"type":"input","bytes":base64::engine::general_purpose::STANDARD.encode(b"MUST_NOT_TYPE\n")})).unwrap(),8192,deadline()).unwrap();
        loop {
            let e = event(&mut socket);
            if e["type"] == "end" {
                assert_eq!(e["reason"], "input_refused");
                break;
            }
        }
        drop(socket);
        let until = deadline();
        while path.exists() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        let captured = Process::new("tmux")
            .args(["capture-pane", "-p", "-t", session])
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&captured.stdout).contains("MUST_NOT_TYPE"));
        let (mut socket, _) = connect(execute(
            &root.join("journal"),
            &request(true, "interactive"),
            deadline(),
            target,
        ));
        assert_eq!(event(&mut socket)["type"], "attached");
        control::write_frame(&mut socket,&serde_json::to_vec(&json!({"type":"input","bytes":base64::engine::general_purpose::STANDARD.encode(b"CLI_TYPED\n")})).unwrap(),8192,deadline()).unwrap();
        let mut captured = Vec::new();
        loop {
            let e = event(&mut socket);
            if e["type"] == "output" {
                captured.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(e["bytes"].as_str().unwrap())
                        .unwrap(),
                );
                if String::from_utf8_lossy(&captured).contains("CLI_TYPED") {
                    break;
                }
            }
            assert_ne!(e["type"], "end");
        }
        control::write_frame(&mut socket, br#"{"type":"detach"}"#, 256, deadline()).unwrap();
        loop {
            let e = event(&mut socket);
            if e["type"] == "end" {
                assert_eq!(e["reason"], "detached");
                break;
            }
        }
        drop(socket);
        let until = deadline();
        while ACTIVE.load(Ordering::SeqCst) > 0 {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            probe(session, deadline()).unwrap().identity,
            before.identity
        );
        assert_eq!(PtyAttachment::cli_grid(session, deadline()).unwrap(), grid);
        let (socket, _) = connect(execute(
            &root.join("journal"),
            &request(false, "disconnect"),
            deadline(),
            target,
        ));
        drop(socket);
        let until = deadline();
        while ACTIVE.load(Ordering::SeqCst) > 0 {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            probe(session, deadline()).unwrap().identity,
            before.identity
        );
    }
}
