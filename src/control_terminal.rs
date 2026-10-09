//! Bounded read-only observations of exact cards in the local live inventory.
use crate::control::{Command, Reply, Request};
use crate::state::{AppState, TerminalData};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::process::{Child, Command as Process, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_CAPTURE: usize = 65_536;
const MAX_INVENTORY: usize = 262_144;
const FORMAT: &str = "#{session_name}\t#{session_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{pane_width}\t#{pane_height}\t#{history_size}\t#{alternate_on}\t#{pid}";
pub(crate) type Failure = (&'static str, &'static str);
const UNAVAILABLE: Failure = (
    "unavailable",
    "Terminal observation is unavailable; no empty capture was substituted.",
);
const CONFLICT: Failure = (
    "conflict",
    "The card, pane identity or grid changed during observation; captured text was withheld.",
);
const TIMEOUT: Failure = ("timeout", "Terminal observation exceeded its deadline.");

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(crate) struct Output {
    pub bytes: Vec<u8>,
    pub limited: bool,
}

/// Drain a nonblocking pipe with a hard allocation limit and absolute deadline.
/// Never wait on a child while its stdout can fill, and never leave it behind.
pub(crate) fn read_process(
    command: Process,
    limit: usize,
    deadline: Instant,
) -> Result<Output, Failure> {
    read_process_input(command, None, limit, deadline)
}

pub(crate) fn read_process_input(
    mut command: Process,
    input: Option<&[u8]>,
    limit: usize,
    deadline: Instant,
) -> Result<Output, Failure> {
    if Instant::now() >= deadline {
        return Err(TIMEOUT);
    }
    let mut child = Reap(
        command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .spawn()
            .map_err(|_| UNAVAILABLE)?,
    );
    let mut stdin = child.0.stdin.take();
    if let Some(pipe) = &stdin {
        crate::terminal_transport::set_nonblocking(pipe.as_raw_fd()).map_err(|_| UNAVAILABLE)?;
    }
    let mut remaining_input = input.unwrap_or_default();
    let mut pipe = child.0.stdout.take().ok_or(UNAVAILABLE)?;
    crate::terminal_transport::set_nonblocking(pipe.as_raw_fd()).map_err(|_| UNAVAILABLE)?;
    let mut bytes = Vec::new();
    let mut eof = false;
    loop {
        if Instant::now() >= deadline {
            return Err(TIMEOUT);
        }
        if let Some(pipe) = &mut stdin {
            if !remaining_input.is_empty() {
                match pipe.write(remaining_input) {
                    Ok(0) => return Err(UNAVAILABLE),
                    Ok(n) => remaining_input = &remaining_input[n..],
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return Err(UNAVAILABLE),
                }
            }
            if remaining_input.is_empty() {
                stdin.take();
            }
        }
        for _ in 0..8 {
            let mut buffer = [0; 8192];
            match pipe.read(&mut buffer) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(count) => {
                    let remaining = limit.saturating_sub(bytes.len());
                    bytes.extend_from_slice(&buffer[..count.min(remaining)]);
                    if count > remaining {
                        return Ok(Output {
                            bytes,
                            limited: true,
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(UNAVAILABLE),
            }
        }
        if let Some(status) = child.0.try_wait().map_err(|_| UNAVAILABLE)? {
            if eof {
                return if status.success() {
                    Ok(Output {
                        bytes,
                        limited: false,
                    })
                } else {
                    Err(UNAVAILABLE)
                };
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn tmux(args: &[&str], limit: usize, deadline: Instant) -> Result<Output, Failure> {
    let mut command = Process::new(crate::tmux::tmux_bin());
    command.args(args);
    read_process(command, limit, deadline)
}

#[derive(Debug, PartialEq)]
pub(crate) struct Pane {
    pub session_id: String,
    pub pane_id: String,
    pub pid: u32,
    pub dead: bool,
    columns: u32,
    rows: u32,
    history: u32,
    alternate: bool,
    pub server_pid: u32,
    pub identity: String,
    observed_at_ms: u128,
}

fn numeric_id(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
}

fn process_start(pid: u32) -> Result<String, Failure> {
    let path = format!("/proc/{pid}/stat");
    if std::fs::metadata(&path).map_err(|_| UNAVAILABLE)?.uid() != unsafe { libc::geteuid() } {
        return Err(UNAVAILABLE);
    }
    let text = std::fs::read_to_string(path).map_err(|_| UNAVAILABLE)?;
    let start = text
        .rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .ok_or(UNAVAILABLE)?;
    if start.is_empty() || !start.bytes().all(|b| b.is_ascii_digit()) {
        return Err(UNAVAILABLE);
    }
    Ok(start.into())
}

pub(crate) fn probe(session: &str, deadline: Instant) -> Result<Pane, Failure> {
    // Filter exact names ourselves. A tmux target prefix, active window, or
    // user-created second pane must never silently choose a different target.
    let output = tmux(&["list-panes", "-a", "-F", FORMAT], MAX_INVENTORY, deadline)?;
    if output.limited {
        return Err((
            "output_limit",
            "Tmux pane inventory exceeds the observation limit.",
        ));
    }
    let text = std::str::from_utf8(&output.bytes).map_err(|_| UNAVAILABLE)?;
    let mut matched = text
        .lines()
        .filter(|line| line.split('\t').next() == Some(session));
    let row = matched
        .next()
        .ok_or(("terminal_not_running", "This saved card has no tmux pane."))?;
    if matched.next().is_some() {
        return Err((
            "unsupported_terminal",
            "Observation requires exactly one pane in the saved card's session.",
        ));
    }
    let fields: Vec<_> = row.split('\t').collect();
    if fields.len() != 10 || !numeric_id(fields[1], '$') || !numeric_id(fields[2], '%') {
        return Err(UNAVAILABLE);
    }
    let number = |at: usize| fields[at].parse::<u32>().map_err(|_| UNAVAILABLE);
    let boolean = |at: usize| match fields[at] {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(UNAVAILABLE),
    };
    let mut pane = Pane {
        session_id: fields[1].into(),
        pane_id: fields[2].into(),
        pid: number(3)?,
        dead: boolean(4)?,
        columns: number(5)?,
        rows: number(6)?,
        history: number(7)?,
        alternate: boolean(8)?,
        server_pid: number(9)?,
        identity: String::new(),
        observed_at_ms: now_ms(),
    };
    if pane.columns == 0 || pane.rows == 0 || pane.columns > 10000 || pane.rows > 10000 {
        return Err(UNAVAILABLE);
    }
    let server_start = process_start(pane.server_pid)?;
    let pane_start = if pane.dead {
        "exited".into()
    } else {
        process_start(pane.pid)?
    };
    let boot =
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(|_| UNAVAILABLE)?;
    pane.identity = format!(
        "{:x}",
        Sha256::digest(format!(
            "{boot}:{}:{server_start}:{}:{}:{}:{pane_start}",
            pane.server_pid, pane.session_id, pane.pane_id, pane.pid
        ))
    );
    Ok(pane)
}

fn same_instance(before: &Pane, after: &Pane) -> bool {
    before.identity == after.identity
        && before.columns == after.columns
        && before.rows == after.rows
        && before.alternate == after.alternate
        && before.dead == after.dead
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn runtime(card: &TerminalData, pane: &Pane) -> Value {
    json!({"id":card.id,"sessionName":card.session_name,"paneId":pane.pane_id,
        "paneIdentity":pane.identity,"panePid":pane.pid,"status":if pane.dead {"exited"} else {"running"},
        "columns":pane.columns,"rows":pane.rows,"units":"terminal-cells","alternateScreen":pane.alternate,
        "retainedHistoryLines":pane.history,"observedAtUnixMs":pane.observed_at_ms,"readiness":"not_observed"})
}

fn plain_text(bytes: &[u8]) -> (String, bool) {
    let lossy = String::from_utf8_lossy(bytes);
    let text = crate::terminal_text::strip_terminal_escapes(&lossy)
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    (text, matches!(lossy, std::borrow::Cow::Owned(_)))
}

pub fn execute(
    request: &Request,
    state: &AppState,
    deadline: Instant,
    recheck: impl FnOnce(&TerminalData) -> Result<bool, ()>,
) -> Reply {
    let result = observe(request, state, deadline, recheck);
    match result {
        Ok(data) => Reply::success(&request.request_id, data),
        Err((code, message)) => Reply::failure(&request.request_id, code, message),
    }
}

fn observe(
    request: &Request,
    state: &AppState,
    deadline: Instant,
    recheck: impl FnOnce(&TerminalData) -> Result<bool, ()>,
) -> Result<Value, Failure> {
    let (id, capture) = match &request.command {
        Command::Runtime { id } | Command::Lifecycle { id } | Command::Composer { id } => (id, None),
        Command::Capture { id, history, lines } => {
            if (!history && lines.is_some()) || lines.is_some_and(|n| !(1..=2000).contains(&n)) {
                return Err((
                    "invalid_arguments",
                    "History accepts 1-2000 lines; screen capture does not accept a line count.",
                ));
            }
            (id, Some(if *history { lines.unwrap_or(200) } else { 0 }))
        }
        _ => {
            return Err((
                "invalid_arguments",
                "Expected a terminal observation command.",
            ))
        }
    };
    let card = state
        .terminals
        .iter()
        .find(|card| card.id == *id)
        .ok_or(("not_found", "No saved local terminal card has that ID."))?;
    let session = &card.session_name;
    if !crate::tmux::is_owned_session(session) {
        return Err((
            "unsupported_terminal",
            "The saved card does not identify a supported local session.",
        ));
    }
    let before = probe(session, deadline)?;
    let lifecycle = if matches!(request.command, Command::Lifecycle { .. }) {
        Some(lifecycle(card, &before, deadline)?)
    } else {
        None
    };
    let composer = if matches!(request.command, Command::Composer { .. }) {
        Some(crate::control_input::composer(&card.agent_type, &before, deadline)?)
    } else { None };
    let captured = match capture {
        Some(lines) => Some(tmux(
            &[
                "capture-pane",
                "-p",
                "-t",
                &before.pane_id,
                "-S",
                &if lines == 0 {
                    "0".into()
                } else {
                    format!("-{lines}")
                },
                "-E",
                "-",
            ],
            MAX_CAPTURE,
            deadline,
        )?),
        None => None,
    };
    if !recheck(card).map_err(|_| TIMEOUT)? {
        return Err(CONFLICT);
    }
    let after = probe(session, deadline)?;
    if !same_instance(&before, &after) {
        return Err(CONFLICT);
    }
    if let Some((ready, reason)) = composer {
        let mut data=runtime(card, &after);
        data["ready"]=json!(ready);
        data["reason"]=json!(reason);
        data["readiness"]=json!("recognized-empty-composer");
        return Ok(data);
    }
    if let Some(data) = lifecycle {
        return Ok(data);
    }
    let Some(output) = captured else {
        return Ok(runtime(card, &after));
    };
    let (text, encoding_lossy) = plain_text(&output.bytes);
    let requested = capture.unwrap();
    let history_limited = requested > 0 && before.history.max(after.history) > requested;
    Ok(
        json!({"id":card.id,"sessionName":session,"text":text,"format":"plain-text",
        "mode":if requested == 0 {"screen"} else {"history"},"runtime":runtime(card, &before),
        "observedAtUnixMs":now_ms(),"consistency":"checked-before-and-after",
        "requestedHistoryLines":requested,"returnedLines":text.lines().count(),
        "maxCaptureBytes":MAX_CAPTURE,"truncated":output.limited || history_limited,
        "truncation":{"bytes":output.limited,"history":history_limited,"retained":"oldest-prefix"},
        "encodingLossy":encoding_lossy,
        "historyScope":if requested == 0 {"current-screen"} else {"tmux-retained-history-plus-current-screen"}}),
    )
}

fn lifecycle(card: &TerminalData, pane: &Pane, deadline: Instant) -> Result<Value, Failure> {
    let mut data = runtime(card, pane);
    let mut lifecycle = if pane.dead {
        "exited".to_string()
    } else {
        "unknown".to_string()
    };
    let mut completion = crate::completion::Completion {
        id: card.id.clone(),
        supported: false,
        state: "unknown".into(),
        completion_id: None,
    };
    let mut native = false;
    if !pane.dead {
        if card.agent_type == "codex" {
            completion = crate::completion::inspect(&card.id, pane.pid);
            lifecycle = completion.state.clone();
            native = completion.supported;
        } else {
            let option = tmux(
                &[
                    "-N",
                    "display-message",
                    "-p",
                    "-t",
                    &pane.pane_id,
                    "#{@super_desktop_metadata}",
                ],
                8192,
                deadline,
            )?;
            if option.limited {
                return Err(("output_limit", "Metadata reference exceeds the limit."));
            }
            let option = std::str::from_utf8(&option.bytes).map_err(|_| UNAVAILABLE)?;
            if let Some(metadata) =
                crate::harness_metadata::inspect_option(&card.agent_type, option).filter(
                    |metadata| {
                        crate::harness_metadata::owns_pane(metadata, pane.pid)
                            && metadata.adapter_reported()
                    },
                )
            {
                native = true;
                lifecycle = metadata.status.clone();
                completion = crate::completion::native_completion(&card.id, &metadata);
            }
        }
    }
    data["lifecycle"] = json!(lifecycle);
    data["nativeMetadataObserved"] = json!(native);
    data["completion"] = serde_json::to_value(completion).unwrap();
    data["turnCorrelation"] = json!("not_observed");
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(command: Command) -> Request {
        Request {
            control_version: 1,
            request_id: "observe-test".into(),
            command,
        }
    }
    fn capture(history: bool, lines: Option<u32>) -> Request {
        request(Command::Capture {
            id: "card-1".into(),
            history,
            lines,
        })
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(3)
    }

    #[test]
    fn cli_terminal_validates_owned_card_and_history_before_process_access() {
        let state = AppState::default();
        assert_eq!(
            execute(&capture(false, None), &state, deadline(), |_| panic!(
                "no card"
            ))
            .exit_code(),
            3
        );
        for req in [
            capture(false, Some(10)),
            capture(true, Some(0)),
            capture(true, Some(2001)),
        ] {
            assert_eq!(
                execute(&req, &state, deadline(), |_| panic!("invalid capture")).exit_code(),
                2
            );
        }
    }

    #[test]
    fn cli_terminal_process_output_is_bounded_and_deadlines_reap_children() {
        let mut flood = Process::new("/bin/sh");
        flood.args(["-c", "exec head -c 200000 /dev/zero"]);
        let output = read_process(flood, 512, deadline()).unwrap();
        assert!(output.limited);
        assert_eq!(output.bytes.len(), 512);
        let mut empty = Process::new("true");
        empty.arg("unused");
        let output = read_process(empty, 512, deadline()).unwrap();
        assert!(output.bytes.is_empty() && !output.limited);
        let mut stalled = Process::new("/bin/sh");
        stalled.args(["-c", "exec sleep 30"]);
        let start = Instant::now();
        assert_eq!(
            read_process(stalled, 512, start + Duration::from_millis(50))
                .err()
                .unwrap()
                .0,
            "timeout"
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(read_process(Process::new("false"), 512, deadline()).is_err());
    }

    #[test]
    fn cli_terminal_plain_capture_removes_controls_and_marks_encoding_loss() {
        let (text, lossy) = plain_text(
            "\u{1b}[31mred\u{1b}[0m\u{1b}]52;clipboard\u{7}\u{009b}\u{7f}\n✓\t".as_bytes(),
        );
        assert_eq!(text, "red\n✓\t");
        assert!(!lossy);
        let (text, lossy) = plain_text(&[b'x', 0xff, 0xe2]);
        assert!(lossy);
        assert!(text.starts_with('x'));
    }

    #[test]
    fn cli_terminal_tmux_observation_integration() {
        crate::test_isolation::rerun_in_private_root(
            "control_terminal::tests::cli_terminal_tmux_inner",
            "SD_CLI_OBSERVATION_ROOT",
            |root, child| {
                child.env("XDG_STATE_HOME", root.join("state"));
            },
        );
    }

    #[test]
    fn cli_terminal_tmux_inner() {
        let Some((root, _tmux)) = crate::test_isolation::private_root("SD_CLI_OBSERVATION_ROOT")
        else {
            return;
        };
        let run = crate::test_isolation::tmux;
        let script = root.join("screen.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf '\\033[31mCOLOR_MARK\\033[0m\\n'\ni=0; while [ $i -lt 60 ]; do printf 'HISTORY_%s\\n' \"$i\"; i=$((i+1)); done\nprintf 'UNICODE_✓_日本語\\n'; exec sleep 30\n").unwrap();
        let session = "sd_term_observation";
        let command = format!(
            "/bin/sh {}",
            crate::launch_args::quote(script.to_str().unwrap())
        );
        run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            "80",
            "-y",
            "12",
            &command,
        ]);
        let mut state = AppState::default();
        let card: TerminalData = serde_json::from_value(json!({
            "id":"card-1","session_name":session,"agent_type":"shell","command":"/bin/sh",
            "x":0,"y":0,"width":640,"height":480,"created_at":1.0,"tag":0
        }))
        .unwrap();
        state.terminals.push(card);
        let wait = deadline();
        while !run(&["capture-pane", "-p", "-t", session]).contains("UNICODE_") {
            assert!(Instant::now() < wait);
            std::thread::sleep(Duration::from_millis(10));
        }
        let get = |req: &Request| execute(req, &state, deadline(), |_| Ok(true));
        let observed = get(&request(Command::Runtime {
            id: "card-1".into(),
        }));
        assert!(observed.ok, "{observed:?}");
        let runtime = observed.data.unwrap();
        assert_eq!(runtime["columns"], 80);
        assert_eq!(runtime["rows"], 12);
        assert_eq!(runtime["status"], "running");
        assert!(runtime["retainedHistoryLines"].as_u64().unwrap() > 30);
        let native = get(&request(Command::Lifecycle {
            id: "card-1".into(),
        }))
        .data
        .unwrap();
        assert_eq!(native["lifecycle"], "unknown");
        assert_eq!(native["completion"]["supported"], false);
        assert_eq!(native["paneIdentity"], runtime["paneIdentity"]);
        assert!(native.get("text").is_none() && native.get("prompt").is_none());
        let screen = get(&capture(false, None)).data.unwrap();
        assert!(screen["text"]
            .as_str()
            .unwrap()
            .contains("UNICODE_✓_日本語"));
        assert!(!screen["text"].as_str().unwrap().contains("HISTORY_0\n"));
        assert_eq!(screen["truncated"], false);
        let history = get(&capture(true, Some(200))).data.unwrap();
        let text = history["text"].as_str().unwrap();
        assert!(text.contains("HISTORY_0\n") && text.contains("COLOR_MARK"));
        assert!(!text.contains('\u{1b}'));
        assert_eq!(history["truncated"], false);
        let bounded = get(&capture(true, Some(2))).data.unwrap();
        assert_eq!(bounded["truncation"]["history"], true);
        assert!(!bounded["text"].as_str().unwrap().contains("HISTORY_0\n"));
        let again = get(&request(Command::Runtime {
            id: "card-1".into(),
        }))
        .data
        .unwrap();
        assert_eq!(again["paneIdentity"], runtime["paneIdentity"]);
        assert_eq!(again["panePid"], runtime["panePid"]);
        assert_eq!(again["columns"], 80);
        assert_eq!(again["rows"], 12);
        assert_eq!(
            run(&["list-clients"]).trim(),
            "",
            "observations must not attach a client"
        );
        let closed = execute(&capture(false, None), &state, deadline(), |_| Ok(false));
        assert_eq!(closed.exit_code(), 5);
        assert!(closed.data.is_none());
        let resized = execute(&capture(false, None), &state, deadline(), |_| {
            run(&["resize-window", "-t", session, "-x", "90", "-y", "14"]);
            Ok(true)
        });
        assert_eq!(resized.exit_code(), 5);
        assert!(resized.data.is_none());
        let replaced = execute(&capture(false, None), &state, deadline(), |_| {
            run(&["respawn-pane", "-k", "-t", session, "sleep 30"]);
            Ok(true)
        });
        assert_eq!(replaced.exit_code(), 5);
        let blank = get(&capture(false, None));
        assert!(blank.ok, "{blank:?}");
        assert!(blank.data.unwrap()["text"]
            .as_str()
            .unwrap()
            .trim()
            .is_empty());
        let alternate_script = root.join("alternate.sh");
        std::fs::write(
            &alternate_script,
            "#!/bin/sh\nprintf '\\033[?1049h\\033[2J\\033[HALTERNATE_MARK'; exec sleep 30\n",
        )
        .unwrap();
        let alternate_command = format!(
            "/bin/sh {}",
            crate::launch_args::quote(alternate_script.to_str().unwrap())
        );
        run(&["respawn-pane", "-k", "-t", session, &alternate_command]);
        let until = deadline();
        while run(&["display-message", "-p", "-t", session, "#{alternate_on}"]).trim() != "1" {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let alternate = get(&capture(false, None)).data.unwrap();
        assert_eq!(alternate["runtime"]["alternateScreen"], true);
        assert!(alternate["text"]
            .as_str()
            .unwrap()
            .contains("ALTERNATE_MARK"));
        run(&["set-option", "-w", "-t", session, "remain-on-exit", "on"]);
        run(&["respawn-pane", "-k", "-t", session, "true"]);
        let until = deadline();
        while run(&["display-message", "-p", "-t", session, "#{pane_dead}"]).trim() != "1" {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let exited = get(&capture(false, None)).data.unwrap();
        assert_eq!(exited["runtime"]["status"], "exited");
        assert!(exited["text"].as_str().unwrap().contains("Pane is dead"));
        // Enough retained rows to exceed the byte budget, without any GUI client.
        let large_script = root.join("large.sh");
        std::fs::write(&large_script, "#!/bin/sh\ni=0; while [ $i -lt 1200 ]; do printf '%070d\\n' \"$i\"; i=$((i+1)); done; printf 'LARGE_DONE\\n'; exec sleep 30\n").unwrap();
        let large_command = format!(
            "/bin/sh {}",
            crate::launch_args::quote(large_script.to_str().unwrap())
        );
        run(&["respawn-pane", "-k", "-t", session, &large_command]);
        let until = deadline();
        while !run(&["capture-pane", "-p", "-t", session]).contains("LARGE_DONE") {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let large = get(&capture(true, Some(2000))).data.unwrap();
        assert_eq!(large["truncation"]["bytes"], true);
        assert!(large["text"].as_str().unwrap().len() <= MAX_CAPTURE);
        assert!(!large["text"].as_str().unwrap().contains("LARGE_DONE"));
        assert!(crate::tmux::session_exists(session));
        run(&["split-window", "-d", "-t", session, "sleep 30"]);
        let ambiguous = get(&capture(false, None));
        assert_eq!(ambiguous.error.unwrap().code, "unsupported_terminal");
        // A similarly named foreign session must never substitute for the exact card.
        run(&[
            "new-session",
            "-d",
            "-s",
            "sd_term_observation_foreign",
            "sleep 30",
        ]);
        run(&["kill-session", "-t", session]);
        assert_eq!(get(&capture(false, None)).exit_code(), 3);
        assert_eq!(
            get(&request(Command::Runtime {
                id: "sd_term_observation_foreign".into()
            }))
            .exit_code(),
            3
        );
        // Server replacement can recycle tmux's numeric pane/session IDs.
        run(&["new-session", "-d", "-s", session, "sleep 30"]);
        let restarted = execute(&capture(false, None), &state, deadline(), |_| {
            let old_server = probe(session, deadline()).unwrap().server_pid;
            run(&["kill-server"]);
            // tmux leaves a stale pathname socket after exit. Wait for its
            // process, not for the socket file to disappear.
            let until = deadline();
            while std::fs::read_to_string(format!("/proc/{old_server}/stat"))
                .ok()
                .and_then(|stat| {
                    stat.rsplit_once(')')
                        .map(|(_, fields)| !fields.trim_start().starts_with('Z'))
                })
                .unwrap_or(false)
            {
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(5));
            }
            run(&["new-session", "-d", "-s", session, "sleep 30"]);
            Ok(true)
        });
        assert_eq!(restarted.exit_code(), 5);
        assert!(restarted.data.is_none());
    }
}
