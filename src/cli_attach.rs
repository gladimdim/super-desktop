//! A bounded attachment client. Raw rendering and keyboard input are opt-in.
use crate::cli_extended::{seconds, send, valid_id, Options};
use crate::control::{self, Command};
use base64::Engine;
use serde_json::{json, Value};
use std::io::{self, IsTerminal, Write};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};
static STOP: AtomicI32 = AtomicI32::new(0);
extern "C" fn stopped(signal: libc::c_int) {
    STOP.store(signal, Ordering::SeqCst);
}
struct TerminalGuard {
    attributes: Option<libc::termios>,
    signals: Vec<(i32, libc::sigaction)>,
}
impl TerminalGuard {
    fn install(interactive: bool) -> io::Result<Self> {
        STOP.store(0, Ordering::SeqCst);
        let mut guard = Self {
            attributes: None,
            signals: vec![],
        };
        for signal in [
            libc::SIGINT,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGQUIT,
            libc::SIGTSTP,
        ] {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut old = unsafe { std::mem::zeroed() };
            action.sa_sigaction = stopped as *const () as usize;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            if unsafe { libc::sigaction(signal, &action, &mut old) } < 0 {
                return Err(io::Error::last_os_error());
            }
            guard.signals.push((signal, old));
        }
        if interactive {
            let mut state = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(0, &mut state) } < 0 {
                return Err(io::Error::last_os_error());
            }
            guard.attributes = Some(state);
            unsafe { libc::cfmakeraw(&mut state) };
            if unsafe { libc::tcsetattr(0, libc::TCSANOW, &state) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some(state) = self.attributes {
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &state) };
        }
        for (signal, action) in self.signals.drain(..).rev() {
            unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) };
        }
    }
}
fn emit(value: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "{value}")?;
    out.flush()
}

pub(crate) fn run(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some("terminal")
        || args.get(1).map(String::as_str) != Some("attach")
    {
        return None;
    }
    let raw = args.iter().any(|a| a == "--raw");
    let fail = |code, message: &str| {
        let reply = control::Reply::failure("", code, message);
        if raw {
            eprintln!("{}", message);
        } else {
            let _ = emit(&serde_json::to_value(&reply).unwrap());
        }
        reply.exit_code()
    };
    let build = || -> Result<(Command, Options), &'static str> {
        let mut parse = args.to_vec();
        for i in 0..parse.len() {
            if parse[i] == "--format=jsonl" {
                parse[i] = "--format=json".into();
            } else if i > 0 && parse[i - 1] == "--format" && parse[i] == "jsonl" {
                parse[i] = "json".into();
            }
        }
        if args
            .iter()
            .any(|v| v.starts_with("--format=") && v != "--format=jsonl")
            || args
                .windows(2)
                .any(|w| w[0] == "--format" && w[1] != "jsonl")
        {
            return Err("Attach emits JSONL; --raw explicitly renders terminal bytes.");
        }
        let options = Options::parse(
            &parse,
            &[
                "--seconds",
                "--expect-epoch",
                "--expect-revision",
                "--expect-pane-identity",
            ],
            &["--raw", "--interactive"],
        )?;
        if options.words.len() != 3 || !valid_id(&options.words[2], 128) {
            return Err("Use an exact terminal card ID.");
        }
        if options.flags.contains("--raw") && options.values.contains_key("--format") {
            return Err("Choose either --raw or JSONL formatting.");
        }
        let interactive = options.flags.contains("--interactive");
        if interactive && (!raw || !io::stdin().is_terminal() || !io::stdout().is_terminal()) {
            return Err("Interactive attach requires --raw and terminal stdin/stdout. Use guarded send/keys for piped input.");
        }
        let duration = options
            .values
            .get("--seconds")
            .map_or(Ok(Duration::from_secs(30)), |v| seconds(v))?
            .as_secs();
        if duration > 300 {
            return Err("Attachment duration is 1..300 seconds.");
        }
        let (epoch, revision, pane) = options.guard()?;
        Ok((
            Command::Attach {
                id: options.words[2].clone(),
                interactive,
                seconds: duration as u16,
                expect_epoch: epoch,
                expect_revision: revision,
                expect_pane_identity: pane,
            },
            options,
        ))
    };
    let (command, options) = match build() {
        Ok(v) => v,
        Err(m) => return Some(fail("invalid_arguments", m)),
    };
    let Command::Attach {
        interactive,
        seconds,
        ref expect_pane_identity,
        ..
    } = command
    else {
        unreachable!()
    };
    let expected = expect_pane_identity.clone();
    let reply = send(command, &options, "terminal.attach");
    if !reply.ok {
        if raw {
            eprintln!("{}", reply.error.as_ref().unwrap().message);
        } else {
            let _ = emit(&serde_json::to_value(&reply).unwrap());
        }
        return Some(reply.exit_code());
    }
    let Some(data) = reply.data else {
        return Some(fail("invalid_response", "Missing attachment endpoint."));
    };
    let Some(name) = data["socket"].as_str().filter(|n| {
        n.starts_with("attach-")
            && n.ends_with(".sock")
            && n.len() <= 64
            && n.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
    }) else {
        return Some(fail("invalid_response", "Invalid attachment endpoint."));
    };
    let Some(token) = data["token"].as_str().filter(|t| valid_id(t, 64)) else {
        return Some(fail("invalid_response", "Invalid attachment token."));
    };
    let directory = control::runtime_dir().join("super-desktop");
    let path = directory.join(name);
    if control::private_dir(&directory).is_err() || control::private_file(&path, true).is_err() {
        return Some(fail("unsafe_socket", "Attachment socket is not private."));
    }
    let mut socket = match control::connect_bounded(&path, Instant::now() + Duration::from_secs(2))
    {
        Ok(s) => s,
        Err(_) => {
            return Some(fail(
                "unavailable",
                "Attachment expired or cannot connect; no automatic replay.",
            ))
        }
    };
    if control::check_peer(&socket).is_err() {
        return Some(fail("permission_denied", "Attachment peer UID mismatch."));
    }
    if control::write_frame(
        &mut socket,
        &serde_json::to_vec(&json!({"token":token})).unwrap(),
        256,
        Instant::now() + Duration::from_secs(1),
    )
    .is_err()
    {
        return Some(fail("unavailable", "Attachment handshake failed."));
    }
    let _terminal = match TerminalGuard::install(interactive) {
        Ok(g) => g,
        Err(_) => {
            return Some(fail(
                "unavailable",
                "Cannot configure local terminal safely.",
            ))
        }
    };
    let deadline = Instant::now() + Duration::from_secs(seconds as u64 + 3);
    let mut sequence = None;
    let mut bytes_total = 0usize;
    let mut result = 0;
    loop {
        let signal = STOP.load(Ordering::SeqCst);
        if signal != 0 {
            result = 128 + signal;
            break;
        }
        if Instant::now() >= deadline {
            result = 7;
            break;
        }
        let mut fds = [
            libc::pollfd {
                fd: socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if interactive { 0 } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, 100) } < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            result = 6;
            break;
        }
        if fds[1].revents != 0 {
            let mut input = [0u8; 4096];
            let n = unsafe { libc::read(0, input.as_mut_ptr().cast(), input.len()) };
            if n <= 0 {
                break;
            }
            let input = &input[..n as usize];
            if input.contains(&0x1d) {
                break;
            }
            let frame = json!({"type":"input","bytes":base64::engine::general_purpose::STANDARD.encode(input)});
            if control::write_frame(
                &mut socket,
                &serde_json::to_vec(&frame).unwrap(),
                8192,
                Instant::now() + Duration::from_millis(500),
            )
            .is_err()
            {
                result = 7;
                break;
            }
        }
        if fds[0].revents == 0 {
            continue;
        }
        let event = match control::read_frame(
            &mut socket,
            65536,
            Instant::now() + Duration::from_millis(500),
        )
        .ok()
        .and_then(|v| serde_json::from_slice::<Value>(&v).ok())
        {
            Some(v) => v,
            None => {
                result = 6;
                break;
            }
        };
        if event["ok"] != true {
            if !raw {
                let _ = emit(&event);
            }
            result = 6;
            break;
        }
        let Some(next) = event["sequence"].as_u64() else {
            result = 8;
            break;
        };
        if sequence.is_none() && event["type"] != "attached"
            || sequence.is_some() && event["type"] == "attached"
        {
            result = 8;
            break;
        }
        if event["streamId"] != token || sequence.map_or(next != 0, |n| next != n + 1) {
            result = 8;
            break;
        }
        sequence = Some(next);
        match event["type"].as_str() {
            Some("attached") => {
                if event["paneIdentity"] != expected {
                    result = 5;
                    break;
                }
                if raw {
                    eprintln!(
                        "Attached to host grid {}×{}; Ctrl-] detaches interactive input.",
                        event["grid"]["columns"], event["grid"]["rows"]
                    );
                }
            }
            Some("output") => {
                let chunk = match event["bytes"]
                    .as_str()
                    .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
                {
                    Some(v) if v.len() <= 32768 => v,
                    _ => {
                        result = 8;
                        break;
                    }
                };
                bytes_total += chunk.len();
                if bytes_total > 17 * 1024 * 1024 {
                    result = 8;
                    break;
                }
                if raw {
                    let mut out = io::stdout().lock();
                    if out.write_all(&chunk).and_then(|_| out.flush()).is_err() {
                        result = 8;
                        break;
                    }
                }
            }
            Some("grid") => {}
            Some("end") => {
                if !raw && emit(&event).is_err() {
                    result = 8;
                }
                break;
            }
            _ => {
                result = 8;
                break;
            }
        }
        if !raw && emit(&event).is_err() {
            result = 8;
            break;
        }
    }
    let _ = control::write_frame(
        &mut socket,
        br#"{"type":"detach"}"#,
        256,
        Instant::now() + Duration::from_millis(200),
    );
    Some(result)
}
