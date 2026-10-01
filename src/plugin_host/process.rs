//! A plugin's process component: spawn it, speak JSON-RPC over its stdio, and
//! stop it — always the whole process group, so nothing it started survives
//! deactivation.
//!
//! GTK-free. A reader thread turns stdout lines into `Event`s; stderr goes to
//! the plugin's log. Host → plugin requests wait on their own channel with a
//! timeout, so no caller can be stuck behind a plugin that does not answer.
use super::rpc::{self, Message, RpcError};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the reader thread reports.
#[derive(Debug)]
pub enum Event {
    /// A request from the plugin; answer with `Process::respond`.
    Request { id: Value, method: String, params: Value },
    Notification { method: String, params: Value },
    /// The plugin wrote something that is not valid protocol.
    ProtocolError(String),
    /// stdout closed: the process exited or closed it.
    Exited,
}

type Pending = Arc<Mutex<HashMap<u64, Sender<Result<Value, RpcError>>>>>;

/// A running plugin process. Cheap to clone; all clones talk to one process.
#[derive(Clone)]
pub struct Process {
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
    pub pid: u32,
}

pub struct Spawn<'a> {
    pub command: &'a [String],
    pub dir: &'a Path,
    pub env: Vec<(String, String)>,
    pub log: PathBuf,
}

impl Process {
    /// Start the process in its own process group. Events arrive on the
    /// returned receiver until `Event::Exited`.
    pub fn spawn(spec: Spawn) -> std::io::Result<(Process, Receiver<Event>)> {
        use std::os::unix::process::CommandExt;
        let (program, args) = spec.command.split_first().ok_or_else(|| std::io::Error::other("empty command"))?;
        // A relative program is inside the plugin directory, never on PATH.
        let program = if program.contains('/') { spec.dir.join(program.trim_start_matches("./")).into_os_string() } else { program.into() };
        if let Some(parent) = spec.log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log = std::fs::OpenOptions::new().create(true).append(true).open(&spec.log)?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(spec.dir)
            .envs(spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn()?;
        let pid = child.id();
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdin = child.stdin.take();
        let process = Process {
            stdin: Arc::new(Mutex::new(stdin)),
            child: Arc::new(Mutex::new(Some(child))),
            pending: Arc::default(),
            next_id: Arc::new(AtomicU64::new(1)),
            pid,
        };
        let (events, receiver) = mpsc::channel();
        let pending = Arc::clone(&process.pending);
        let reply = process.clone();
        std::thread::Builder::new()
            .name(format!("plugin-out-{pid}"))
            .spawn(move || read_stdout(stdout, events, pending, reply))?;
        std::thread::Builder::new().name(format!("plugin-err-{pid}")).spawn(move || copy_stderr(stderr, log))?;
        Ok((process, receiver))
    }

    fn write_line(&self, line: &str) -> bool {
        let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        let Some(pipe) = stdin.as_mut() else { return false };
        if pipe.write_all(line.as_bytes()).and_then(|_| pipe.write_all(b"\n")).and_then(|_| pipe.flush()).is_err() {
            *stdin = None;
            return false;
        }
        true
    }

    pub fn notify(&self, method: &str, params: Value) -> bool {
        self.write_line(&rpc::notification(method, params))
    }

    pub fn respond(&self, id: &Value, result: Result<Value, RpcError>) {
        let mut line = rpc::response(id, result);
        if line.len() > rpc::MAX_MESSAGE {
            line = rpc::response(
                id,
                Err(RpcError::new(rpc::LIMIT_EXCEEDED, "the answer is larger than 1 MiB", "Ask for less (fewer lines, a smaller range).", "references/host-api.md#transport")),
            );
        }
        self.write_line(&line);
    }

    /// Send a request and wait at most `timeout` for the answer. Blocks: never
    /// call it on the GTK thread.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, sender);
        if !self.write_line(&rpc::request(id, method, params)) {
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(RpcError::new(rpc::UNAVAILABLE, "the plugin process is not running", "See `super-desktop plugin logs <id>`.", "references/host-api.md#lifecycle"));
        }
        let answer = receiver.recv_timeout(timeout);
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        answer.unwrap_or_else(|_| {
            Err(RpcError::new(rpc::TIMEOUT, format!("{method} got no answer within {} ms", timeout.as_millis()), "Answer host requests promptly; do slow work on a thread.", "references/host-api.md#lifecycle"))
        })
    }

    pub fn is_running(&self) -> bool {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    /// SIGTERM the process group, SIGKILL it after `grace`, and reap the child.
    /// Everything the plugin started in its group goes with it.
    pub fn kill(&self, grace: Duration) {
        *self.stdin.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let group = -(self.pid as i32);
        // SAFETY: signalling our own child's process group.
        unsafe { libc::kill(group, libc::SIGTERM) };
        let deadline = std::time::Instant::now() + grace;
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = child.as_mut() {
            while std::time::Instant::now() < deadline {
                if matches!(c.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        // The leader may be gone while its children still run: always finish
        // with SIGKILL to the whole group.
        unsafe { libc::kill(group, libc::SIGKILL) };
        if let Some(mut c) = child.take() {
            let _ = c.wait();
        }
    }
}

fn read_stdout(stdout: impl Read, events: Sender<Event>, pending: Pending, reply: Process) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        // Read at most one byte past the limit, so a flood cannot exhaust memory.
        let read = (&mut reader).take(rpc::MAX_MESSAGE as u64 + 1).read_until(b'\n', &mut line);
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.len() > rpc::MAX_MESSAGE && line.last() != Some(&b'\n') {
            // Skip the rest of the oversized line.
            let _ = reader.skip_until(b'\n');
            let _ = events.send(Event::ProtocolError("a message larger than 1 MiB was dropped".into()));
            continue;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let event = match rpc::parse(text) {
            Ok(Message::Request { id, method, params }) => Event::Request { id, method, params },
            Ok(Message::Notification { method, params }) => Event::Notification { method, params },
            Ok(Message::Response { id, result }) => {
                if let Some(sender) = id.as_u64().and_then(|id| pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id)) {
                    let _ = sender.send(result);
                }
                continue;
            }
            Err((id, error)) => {
                if let Some(id) = id {
                    reply.respond(&id, Err(error.clone()));
                }
                Event::ProtocolError(error.reason())
            }
        };
        if events.send(event).is_err() {
            break;
        }
    }
    // Nobody will answer the requests still waiting: fail them now rather than
    // at their timeout.
    let waiting: Vec<_> = pending.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, s)| s).collect();
    for sender in waiting {
        let _ = sender.send(Err(RpcError::new(rpc::UNAVAILABLE, "the plugin process exited before answering", "Read its log (super-desktop plugin logs <id>), or run its command by hand to see the error.", "references/host-api.md#lifecycle")));
    }
    let _ = events.send(Event::Exited);
}

/// stderr into the plugin log, capped so a chatty plugin cannot fill the disk.
fn copy_stderr(stderr: impl Read, mut log: std::fs::File) {
    const MAX_LOG: u64 = 4 * 1024 * 1024;
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    while matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
        if log.metadata().map(|m| m.len()).unwrap_or(0) < MAX_LOG {
            let _ = write!(log, "{} stderr {}", chrono::Local::now().format("%H:%M:%S"), line);
        }
        line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn script(name: &str, body: &str) -> (PathBuf, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("sd-proc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.py"), body).unwrap();
        (dir, vec!["python3".into(), "main.py".into()])
    }

    fn start(dir: &Path, command: &[String]) -> (Process, Receiver<Event>) {
        Process::spawn(Spawn { command, dir, env: vec![], log: dir.join("plugin.log") }).unwrap()
    }

    #[test]
    fn plugin_process_round_trip_and_requests() {
        let (dir, command) = script(
            "echo",
            r#"
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "activate":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
        print(json.dumps({"jsonrpc": "2.0", "id": 7, "method": "host.describe", "params": {}}), flush=True)
        print("not json", flush=True)
        print("to the log", file=sys.stderr, flush=True)
    elif "result" in msg and msg.get("id") == 7:
        print(json.dumps({"jsonrpc": "2.0", "method": "log", "params": {"level": "info", "message": "got " + json.dumps(msg["result"])}}), flush=True)
"#,
        );
        let (process, events) = start(&dir, &command);
        assert_eq!(process.request("activate", json!({}), Duration::from_secs(10)).unwrap(), json!({}));
        let Event::Request { id, method, .. } = events.recv_timeout(Duration::from_secs(5)).unwrap() else { panic!() };
        assert_eq!(method, "host.describe");
        assert!(matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::ProtocolError(_)));
        process.respond(&id, Ok(json!({"apiVersion": 1})));
        let Event::Notification { params, .. } = events.recv_timeout(Duration::from_secs(5)).unwrap() else { panic!() };
        assert_eq!(params["message"], "got {\"apiVersion\": 1}");
        process.kill(Duration::from_millis(500));
        assert!(matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Exited));
        std::thread::sleep(Duration::from_millis(100));
        assert!(std::fs::read_to_string(dir.join("plugin.log")).unwrap().contains("to the log"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_process_exit_fails_waiting_requests_at_once() {
        let (dir, command) = script("dies", "import sys\nprint('SyntaxError: nope', file=sys.stderr)\nsys.exit(1)\n");
        let (process, _events) = start(&dir, &command);
        let started = std::time::Instant::now();
        let error = process.request("activate", json!({}), Duration::from_secs(10)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3), "not the 10 s timeout");
        assert_eq!(error.code, rpc::UNAVAILABLE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_process_timeout_and_group_kill() {
        // Never answers, and starts a grandchild that ignores SIGTERM.
        let (dir, command) = script(
            "hang",
            r#"
import subprocess, sys, time
subprocess.Popen(["sh", "-c", "trap '' TERM; sleep 60"])
time.sleep(60)
"#,
        );
        let (process, _events) = start(&dir, &command);
        std::thread::sleep(Duration::from_millis(300));
        let error = process.request("activate", json!({}), Duration::from_millis(300)).unwrap_err();
        assert_eq!(error.code, rpc::TIMEOUT);
        let group = process.pid as i32;
        process.kill(Duration::from_millis(200));
        std::thread::sleep(Duration::from_millis(200));
        // SAFETY: probing whether any process of the group still exists.
        let alive = unsafe { libc::kill(-group, 0) } == 0;
        assert!(!alive, "the plugin's process group survived");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
