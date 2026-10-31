//! Explicit owner-client lifecycle orchestration over the existing local IPC.
//! No startup on reads; a connected mutation is never automatically resent.
use crate::cli::{render_reply, Output};
use crate::cli_extended::{respond, valid_id, Options};
use crate::{control, control_journal};
use serde_json::{json, Value};
use std::io::{self, Write};
use std::os::unix::{
    fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    net::UnixStream,
    process::CommandExt,
};
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
fn pid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of_val(&cred) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.pid as u32)
}
#[cfg(target_os = "macos")]
fn pid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut pid: libc::pid_t = 0;
    let mut size = std::mem::size_of_val(&pid) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(pid as u32)
}
fn connect(path: &Path, deadline: Instant) -> io::Result<Option<UnixStream>> {
    control::private_dir(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(m) if m.uid() != unsafe { libc::geteuid() } || !m.file_type().is_socket() => {
            return Err(io::ErrorKind::PermissionDenied.into())
        }
        _ => {}
    }
    match control::connect_bounded(path, deadline) {
        Ok(stream) => {
            control::check_peer(&stream)?;
            Ok(Some(stream))
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}
fn exchange(mut stream: UnixStream, action: &str, deadline: Instant) -> io::Result<Value> {
    let timeout = deadline
        .checked_duration_since(Instant::now())
        .ok_or(io::ErrorKind::TimedOut)?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(format!("{action}\n").as_bytes())?;
    let mut bytes = vec![];
    loop {
        let mut buffer = [0; 4096];
        let count = match control::read_chunk(&mut stream, &mut buffer, deadline) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        if bytes.len() + count > 65536 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value["ok"].is_boolean() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(value)
}
fn wait_stopped(pid: u32, start: &str, deadline: Instant) -> bool {
    while crate::platform::process::start_time(pid).as_deref() == Some(start) {
        if unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) } == pid as i32 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    true
}
fn spawn(exe: &Path, log: &Path) -> io::Result<std::process::Child> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(log)?;
    let mut process = std::process::Command::new(exe);
    process
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    #[cfg(target_os = "linux")]
    {
        let layer = "/usr/lib/libgtk4-layer-shell.so";
        if Path::new(layer).exists() {
            let previous = std::env::var("LD_PRELOAD").unwrap_or_default();
            if !previous.split([':', ' ']).any(|p| p == layer) {
                process.env(
                    "LD_PRELOAD",
                    if previous.is_empty() {
                        layer.into()
                    } else {
                        format!("{layer}:{previous}")
                    },
                );
            }
        }
    }
    unsafe {
        process.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    process.spawn()
}
pub(crate) fn execute(root: &Path, path: &Path, exe: &Path, action: &str, id: &str) -> control::Reply {
    control_journal::execute_operation(
        root,
        id,
        &json!({"method":"client.app.lifecycle","action":action}),
        |_| "application".into(),
        |_| {
            let fail = |code, message| control::Reply::failure(id, code, message);
            let deadline = Instant::now() + Duration::from_secs(10);
            if action == "restart" {
                #[cfg(target_os = "linux")]
                if std::env::var_os("WAYLAND_DISPLAY").is_none()
                    && std::env::var_os("DISPLAY").is_none()
                {
                    return fail(
                        "display_unavailable",
                        "Restart requires a desktop display environment; nothing was stopped.",
                    );
                }
                if !exe.is_file() {
                    return fail(
                        "unavailable",
                        "Desktop executable is missing; nothing was stopped.",
                    );
                }
            }
            let mut stopped = false;
            let stream =
                match connect(path, deadline) {
                    Ok(s) => s,
                    Err(_) => return fail(
                        "unavailable",
                        "Cannot safely connect to the owner application socket; nothing was sent.",
                    ),
                };
            if let Some(stream) = stream {
                if action == "start" {
                    return match exchange(stream, "status", deadline) {
                        Ok(status) if status["ok"] == true => control::Reply::success(
                            id,
                            json!({"outcome":"already_running","status":status,"sessionsPreserved":true}),
                        ),
                        _ => fail(
                            "unavailable",
                            "Application status is unavailable; no second daemon was started.",
                        ),
                    };
                }
                let stopping = matches!(action, "stop" | "restart");
                let identity =
                    if stopping {
                        match pid(&stream).ok().and_then(|pid| {
                            crate::platform::process::start_time(pid).map(|start| (pid, start))
                        }) {
                            Some(pair) => Some(pair),
                            None => return fail(
                                "unavailable",
                                "Cannot establish daemon process identity; nothing was stopped.",
                            ),
                        }
                    } else {
                        None
                    };
                match exchange(stream, if stopping { "kill" } else { action }, deadline) {
                    Ok(v) if v["ok"] == true => {
                        if let Some((pid, start)) = identity {
                            if !wait_stopped(pid, &start, deadline) {
                                return control::Reply::unknown(id);
                            }
                            stopped = true;
                        }
                        if action != "restart" {
                            return control::Reply::success(
                                id,
                                json!({"outcome":if stopping{"stopped"}else{"applied"},"status":v,"sessionsPreserved":true}),
                            );
                        }
                    }
                    Ok(v) => {
                        let mut reply =
                            fail("operation_failed", "Application refused the operation.");
                        reply.data = Some(v);
                        return reply;
                    }
                    Err(_) => return control::Reply::unknown(id),
                }
            } else if action == "stop" {
                return control::Reply::success(
                    id,
                    json!({"outcome":"already_stopped","sessionsPreserved":true}),
                );
            } else if !matches!(action, "start" | "restart") {
                return fail(
                    "unavailable",
                    "No daemon is running; use app start explicitly.",
                );
            }
            #[cfg(target_os = "linux")]
            if std::env::var_os("WAYLAND_DISPLAY").is_none()
                && std::env::var_os("DISPLAY").is_none()
            {
                return fail(
                    "display_unavailable",
                    "Starting the application requires a desktop display environment.",
                );
            }
            let log = root.parent().unwrap().join(format!("cli-start-{id}.log"));
            let mut child = match spawn(exe, &log) {
                Ok(c) => c,
                Err(_) => {
                    return if stopped {
                        control::Reply::unknown(id)
                    } else {
                        fail("operation_failed","Cannot spawn the desktop executable; inspect its path and private startup log.")
                    }
                }
            };
            loop {
                if let Ok(Some(stream)) = connect(path, deadline) {
                    if let Ok(status) = exchange(stream, "status", deadline) {
                        if status["ok"] == true {
                            return control::Reply::success(
                                id,
                                json!({"outcome":"started","status":status,"spawnedPid":child.id(),"log":log,"sessionsPreserved":true,"readyObserved":false}),
                            );
                        }
                    }
                }
                if child.try_wait().ok().flatten().is_some() {
                    return control::Reply::unknown(id);
                }
                if Instant::now() >= deadline {
                    return control::Reply::unknown(id);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        },
    )
}
pub(crate) fn run(args: &[String]) -> Option<Output> {
    if args.first()?.as_str() != "app"
        || !args.get(1).is_some_and(|v| {
            matches!(
                v.as_str(),
                "start" | "stop" | "restart" | "show" | "hide" | "toggle"
            )
        })
    {
        return None;
    }
    let build = || -> Result<Output, &'static str> {
        let options = Options::parse(args, &[], &[])?;
        if options.words.len() != 2 {
            return Err("Unexpected application arguments.");
        }
        if options.values.get("--target").is_some_and(|v| v != "local") {
            return Err("Application lifecycle only targets local; no fallback.");
        }
        let id = options.required("--request-id")?;
        if !valid_id(&id, 64) {
            return Err("Use a unique request ID, up to64 ASCII letters/digits/_/-.");
        }
        let exe = std::env::current_exe()
            .map_err(|_| "Cannot locate desktop executable.")?
            .with_file_name("super-desktop");
        Ok(render_reply(
            execute(
                &control_journal::root(),
                &crate::platform::runtime::socket_path(),
                &exe,
                &options.words[1],
                &id,
            ),
            options.json,
        ))
    };
    respond(args, build)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    #[test]
    fn lifecycle_keeps_complete_reply_from_a_peer_that_closes_immediately() {
        for _ in 0..32 {
            let (client, mut server) = UnixStream::pair().unwrap();
            let worker = std::thread::spawn(move || {
                let mut action = [0; 7];
                server.read_exact(&mut action).unwrap();
                assert_eq!(&action, b"status\n");
                server.write_all(b"{\"ok\":true}").unwrap();
            });
            let reply = exchange(client, "status", Instant::now() + Duration::from_secs(2));
            worker.join().unwrap();
            assert_eq!(reply.unwrap()["ok"], true);
        }
    }
    #[test]
    fn lifecycle_reply_deadline_is_absolute_even_when_bytes_keep_arriving() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut action = [0; 7];
            server.read_exact(&mut action).unwrap();
            for _ in 0..50 {
                if server.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = server.write_all(b"{\"ok\":true}");
        });
        let result = exchange(
            client,
            "status",
            Instant::now() + Duration::from_millis(200),
        );
        assert!(matches!(
            result.unwrap_err().kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        worker.join().unwrap();
    }
    #[test]
    fn application_daemon_fixture() {
        let Some(root) = std::env::var_os("SD_APP_DAEMON_FIXTURE") else {
            return;
        };
        let path = Path::new(&root).join("app.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let mut visible = false;
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut line = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(&stream), &mut line).unwrap();
            let action = line.trim();
            match action {
                "show" => visible = true,
                "hide" => visible = false,
                "toggle" => visible = !visible,
                _ => {}
            }
            stream
                .write_all(json!({"ok":true,"visible":visible}).to_string().as_bytes())
                .unwrap();
            drop(stream);
            if action == "kill" {
                break;
            }
        }
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn application_lifecycle_is_explicit_journaled_and_preserves_sessions() {
        let Some(root) = std::env::var_os("SD_APP_TEST_ROOT").map(std::path::PathBuf::from) else {
            let root = std::env::temp_dir()
                .canonicalize()
                .expect("resolve system temporary directory")
                .join(format!("sd-cli-app-{}", std::process::id()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            let output=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","cli_application::tests::application_lifecycle_is_explicit_journaled_and_preserves_sessions","--nocapture"]).env("SD_APP_TEST_ROOT",&root).env("HOME",&root).env("XDG_STATE_HOME",&root).env("DISPLAY","sd-cli-stub-only").env_remove("WAYLAND_DISPLAY").env_remove("HYPRLAND_INSTANCE_SIGNATURE").output().unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let exe = root.join("desktop-stub");
        let test = std::env::current_exe().unwrap();
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        std::fs::write(&exe,format!("#!/bin/sh\nexport SD_APP_DAEMON_FIXTURE={}\nexec {} --exact cli_application::tests::application_daemon_fixture --nocapture\n",quote(root.to_str().unwrap()),quote(test.to_str().unwrap()))).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let journal = root.join("receipts");
        let path = root.join("app.sock");
        let run = |action, id| execute(&journal, &path, &exe, action, id);
        assert!(!run("show", "absent").ok);
        assert!(!path.exists());
        let started = run("start", "start");
        assert!(started.ok, "{started:?}");
        let already = run("start", "already");
        assert!(already.ok, "{already:?}\n{}", std::fs::read_to_string(root.join("cli-start-start.log")).unwrap_or_default());
        assert_eq!(
            already.data.unwrap()["outcome"],
            "already_running"
        );
        let shown = run("show", "show");
        assert!(shown.ok, "{shown:?}\n{}", std::fs::read_to_string(root.join("cli-start-start.log")).unwrap_or_default());
        assert_eq!(shown.data.unwrap()["status"]["visible"], true);
        assert_eq!(
            run("toggle", "toggle").data.unwrap()["status"]["visible"],
            false
        );
        assert_eq!(
            run("toggle", "toggle").data.unwrap()["status"]["visible"],
            false,
            "replay must not toggle twice"
        );
        assert_eq!(run("show", "toggle").exit_code(), 5);
        let restarted = run("restart", "restart");
        assert!(restarted.ok, "{restarted:?}");
        assert_ne!(
            started.data.unwrap()["spawnedPid"],
            restarted.data.unwrap()["spawnedPid"]
        );
        assert!(run("stop", "stop").ok);
        assert!(!path.exists());
        assert_eq!(
            run("restart", "restart").data.unwrap()["outcome"],
            "started",
            "historical receipt does not restart again"
        );
        assert!(!path.exists());
        assert_eq!(
            run("stop", "stopped").data.unwrap()["outcome"],
            "already_stopped"
        );
    }
}
