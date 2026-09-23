//! Explicit agent completion metadata; never infer completion from terminal silence.
//! Codex rollouts are an internal format: unknown versions fail closed.
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    pub id: String,
    pub supported: bool,
    pub state: String,
    pub completion_id: Option<String>,
}

fn unknown(id: &str) -> Completion {
    Completion {
        id: id.into(),
        state: "unknown".into(),
        ..Default::default()
    }
}

fn parse_tail(bytes: &[u8], identity: &str) -> (String, Option<String>) {
    // A partially written newer event could be a new turn or cancellation.
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return ("unknown".into(), None);
    }
    let mut state = "unknown";
    let mut completed = None;
    // Ignore an unfinished final JSON line. Never scan text inside response bodies.
    for line in bytes
        .split_inclusive(|b| *b == b'\n')
        .filter(|l| l.ends_with(b"\n"))
    {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if record["type"] != "event_msg" {
            continue;
        }
        let event = &record["payload"];
        match event["type"].as_str() {
            Some("task_started" | "turn_started" | "user_message") => {
                state = "working";
                completed = None;
            }
            Some("turn_aborted" | "task_failed" | "error") => {
                state = "unknown";
                completed = None;
            }
            Some("task_complete") => {
                state = "unknown";
                completed = None;
                if let (Some(turn), Some(message)) = (
                    event["turn_id"].as_str(),
                    event["last_agent_message"].as_str(),
                ) {
                    if !turn.is_empty() && turn.len() <= 256 && !message.trim().is_empty() {
                        state = "completed";
                        completed = Some(format!(
                            "{:x}",
                            Sha256::digest(format!("{identity}\0{turn}"))
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    (state.into(), completed)
}

/// Find only the nearest CLI's own open rollout, never "latest file in cwd".
/// Don't descend into a Codex process: its tool children may run other agents.
fn rollout(pane_pid: u32) -> Option<(PathBuf, String)> {
    let mut queue = VecDeque::from([(pane_pid, 0)]);
    let mut visited = HashSet::new();
    while let Some((pid, depth)) = queue.pop_front() {
        if !visited.insert(pid) || visited.len() > 32 {
            return None;
        }
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        if comm.trim() == "codex" {
            let mut found = HashMap::new();
            for fd in std::fs::read_dir(format!("/proc/{pid}/fd"))
                .ok()?
                .take(512)
                .flatten()
            {
                let Ok(path) = std::fs::read_link(fd.path()) else {
                    continue;
                };
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                    found.insert(path.to_string_lossy().into_owned(), fd.path());
                }
            }
            if found.len() != 1 {
                return None;
            }
            return found
                .into_iter()
                .next()
                .map(|(identity, fd)| (fd, identity));
        }
        if depth < 4 {
            let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
                .unwrap_or_default();
            for child in children
                .split_whitespace()
                .take(32)
                .filter_map(|s| s.parse().ok())
            {
                queue.push_back((child, depth + 1));
            }
        }
    }
    None
}

/// Codex's generated/renamed conversation name, attributed through the live
/// CLI's open rollout. Never guess by cwd, timestamps, or the latest session.
pub(crate) fn session_title(pane_pid: u32) -> Option<String> {
    let (fd, identity) = rollout(pane_pid)?;
    title_for_rollout(&fd, Path::new(&identity))
}

fn title_for_rollout(fd: &Path, identity: &Path) -> Option<String> {
    let file = File::open(fd).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    let mut header = Vec::new();
    BufReader::new(file.take(64 * 1024)).read_until(b'\n', &mut header).ok()?;
    if !header.ends_with(b"\n") { return None; }
    let record: Value = serde_json::from_slice(&header).ok()?;
    if record["type"] != "session_meta" || record["payload"]["source"] != "cli" {
        return None;
    }
    let id = record["payload"]["id"].as_str().filter(|id| !id.is_empty())?;
    // Derive CODEX_HOME from this rollout, including nondefault installations.
    let home = identity.ancestors().find(|path| {
        path.file_name().is_some_and(|name| name == "sessions" || name == "archived_sessions")
    })?.parent()?;
    let mut index = File::open(home.join("session_index.jsonl")).ok()?;
    let meta = index.metadata().ok()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } { return None; }
    // The append-only index records names separately from raw first prompts.
    // Bound work per refresh, and never interpret an incomplete JSON record.
    let start = meta.len().saturating_sub(4 * 1024 * 1024);
    index.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    index.take(meta.len() - start).read_to_end(&mut bytes).ok()?;
    let tail = if start > 0 {
        &bytes[bytes.iter().position(|b| *b == b'\n')? + 1..]
    } else { &bytes };
    title_from_index(tail, id)
}

fn title_from_index(bytes: &[u8], id: &str) -> Option<String> {
    for line in bytes.split_inclusive(|b| *b == b'\n').rev() {
        if !line.ends_with(b"\n") { continue; }
        let Ok(record) = serde_json::from_slice::<Value>(line) else { continue; };
        if record["id"].as_str() != Some(id) { continue; }
        let name = record["thread_name"].as_str()?;
        let clean = crate::tmux::strip_terminal_escapes(name)
            .split_whitespace().collect::<Vec<_>>().join(" ");
        return (!clean.is_empty()).then(|| clean.chars().take(240).collect());
    }
    None
}

/// Read explicit user-message records from the rollout owned by this pane.
/// This is a fallback for sessions opened before input tracking was attached.
pub fn last_user_prompt(session: &str) -> Option<String> {
    let out = std::process::Command::new(crate::tmux::tmux_bin())
        .args(["display-message", "-p", "-t", session, "#{pane_pid}"])
        .output().ok()?;
    if !out.status.success() { return None; }
    let pid = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
    let (fd, _) = rollout(pid)?;
    let mut file = File::open(fd).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } { return None; }
    // Bounded tail read; incomplete records and all response/tool records are ignored.
    let start = meta.len().saturating_sub(1024 * 1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(meta.len() - start).read_to_end(&mut bytes).ok()?;
    let tail = if start > 0 {
        &bytes[bytes.iter().position(|b| *b == b'\n')? + 1..]
    } else { &bytes };
    prompt_from_records(tail)
}

fn prompt_from_records(bytes: &[u8]) -> Option<String> {
    bytes.split_inclusive(|b| *b == b'\n').rev().find_map(|line| {
        if !line.ends_with(b"\n") { return None; }
        let record: Value = serde_json::from_slice(line).ok()?;
        if record["type"] != "event_msg" || record["payload"]["type"] != "user_message" {
            return None;
        }
        let text = record["payload"]["message"].as_str()?;
        (!text.trim().is_empty()).then(|| crate::tmux::truncate_prompt_title(text))
    })
}

type Stamp = (u64, u64, u64, i64, i64);
type Cached = (Stamp, (String, Option<String>));
static CACHE: OnceLock<Mutex<HashMap<String, Cached>>> = OnceLock::new();

pub(crate) fn inspect(id: &str, pid: u32) -> Completion {
    let mut result = unknown(id);
    let Some((fd, identity)) = rollout(pid) else {
        return result;
    };
    let Ok(mut file) = File::open(fd) else {
        return result;
    };
    let Ok(meta) = file.metadata() else {
        return result;
    };
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return result;
    }
    let stamp = (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
    );
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some((_, (state, completion_id))) = cache
        .lock()
        .unwrap()
        .get(&identity)
        .filter(|(s, _)| *s == stamp)
    {
        result.supported = true;
        result.state = state.clone();
        result.completion_id = completion_id.clone();
        return result;
    }
    let mut header = String::new();
    if BufReader::new((&mut file).take(64 * 1024))
        .read_line(&mut header)
        .is_err()
    {
        return result;
    }
    let Ok(header) = serde_json::from_str::<Value>(&header) else {
        return result;
    };
    if header["type"] != "session_meta" || header["payload"]["source"] != "cli" {
        return result;
    }
    let start = meta.len().saturating_sub(512 * 1024);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return result;
    }
    let mut bytes = Vec::new();
    if (&mut file)
        .take(meta.len() - start)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return result;
    }
    let Ok(after) = file.metadata() else {
        return result;
    };
    if (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
    ) != stamp
    {
        return result;
    }
    let tail = if start > 0 {
        let Some(i) = bytes.iter().position(|b| *b == b'\n') else {
            return result;
        };
        &bytes[i + 1..]
    } else {
        &bytes
    };
    let parsed = parse_tail(tail, &identity);
    let mut cache = cache.lock().unwrap();
    if cache.len() >= 64 {
        cache.clear();
    }
    cache.insert(identity, (stamp, parsed.clone()));
    result.supported = true;
    result.state = parsed.0;
    result.completion_id = parsed.1;
    result
}

pub fn collect(ids: &[String]) -> Vec<Completion> {
    let state = crate::state::load_state();
    // One inventory call, no capture-pane, prompt inspection, or terminal stream changes.
    let output = std::process::Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{session_name} #{pane_pid}"])
        .output();
    let panes = output
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    ids.iter()
        .map(|id| {
            if !state
                .terminals
                .iter()
                .any(|t| &t.session_name == id && t.agent_type == "codex")
            {
                return unknown(id);
            }
            let pids: Vec<u32> = panes
                .lines()
                .filter_map(|line| {
                    let (session, pid) = line.split_once(' ')?;
                    (session == id).then(|| pid.parse().ok()).flatten()
                })
                .collect();
            if pids.len() == 1 {
                inspect(id, pids[0])
            } else {
                unknown(id)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(kind: &str) -> String {
        format!("{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"{kind}\"}}}}\n")
    }
    const DONE: &str = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn-1\",\"last_agent_message\":\"Done\"}}\n";
    #[test]
    fn only_complete_responses_notify() {
        for kind in [
            "task_started",
            "item_completed",
            "agent_message",
            "turn_aborted",
            "approval_requested",
            "token_count",
        ] {
            assert!(parse_tail(event(kind).as_bytes(), "a").1.is_none());
        }
        assert_eq!(parse_tail(DONE.as_bytes(), "a").0, "completed");
        assert!(parse_tail(DONE.trim_end().as_bytes(), "a").1.is_none());
        assert!(parse_tail(DONE.replace("Done", "").as_bytes(), "a")
            .1
            .is_none());
    }
    #[test]
    fn new_turn_or_abort_clears_completion_and_ids_are_stable() {
        let id = parse_tail(DONE.as_bytes(), "a").1.unwrap();
        assert_eq!(
            parse_tail(DONE.as_bytes(), "a").1.as_deref(),
            Some(id.as_str())
        );
        assert_ne!(
            parse_tail(DONE.as_bytes(), "b").1.as_deref(),
            Some(id.as_str())
        );
        for kind in ["task_started", "user_message", "turn_aborted", "error"] {
            assert!(parse_tail(format!("{DONE}{}", event(kind)).as_bytes(), "a")
                .1
                .is_none());
        }
        assert!(parse_tail(format!("{DONE}{{\"type\":").as_bytes(), "a")
            .1
            .is_none());
    }

    #[test]
    fn live_rollout_child() {
        let Some(path) = std::env::var_os("SD_COMPLETION_TEST_ROLLOUT") else {
            return;
        };
        let _file = File::open(path).unwrap();
        // Rust's test runs on a worker thread; name the process leader, not that thread.
        std::fs::write("/proc/self/comm", "codex").unwrap();
        println!("ROLLOUT_READY");
        let mut byte = [0];
        let _ = std::io::stdin().read(&mut byte);
    }

    #[test]
    fn attributes_real_open_fd_and_refreshes_cached_rollout() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let directory = std::env::temp_dir().join(format!(
            "sd-completion-{}",
            crate::tmux::unique_session_name()
        ));
        let sessions = directory.join("sessions/2026/09/23");
        std::fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("rollout-fixture.jsonl");
        let header = "{\"type\":\"session_meta\",\"payload\":{\"source\":\"cli\",\"id\":\"own-thread\"}}\n";
        std::fs::write(&path, format!("{header}{DONE}")).unwrap();
        let index = directory.join("session_index.jsonl");
        std::fs::write(&index, concat!(
            "{\"id\":\"own-thread\",\"thread_name\":\"Fix toolbar sizing\"}\n",
            "{\"id\":\"another-thread\",\"thread_name\":\"Wrong conversation\"}\n",
        )).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "completion::tests::live_rollout_child",
                "--nocapture",
            ])
            .env("SD_COMPLETION_TEST_ROLLOUT", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            assert!(stdout.read_line(&mut line).unwrap() > 0);
            if line.contains("ROLLOUT_READY") {
                break;
            }
            line.clear();
        }
        let first = inspect("test", child.id());
        assert!(first.supported);
        assert_eq!(first.state, "completed");
        assert_eq!(session_title(child.id()).as_deref(), Some("Fix toolbar sizing"));
        std::fs::OpenOptions::new().append(true).open(&index).unwrap()
            .write_all(b"{\"id\":\"own-thread\",\"thread_name\":\"Renamed conversation\"}\n").unwrap();
        assert_eq!(session_title(child.id()).as_deref(), Some("Renamed conversation"));
        assert_eq!(
            inspect("test", child.id()).completion_id,
            first.completion_id
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(event("task_started").as_bytes())
            .unwrap();
        assert_eq!(inspect("test", child.id()).state, "working");
        std::fs::write(
            &path,
            format!("{}{}", header.replace("cli", "subagent"), DONE),
        )
        .unwrap();
        assert!(!inspect("test", child.id()).supported);
        assert!(session_title(child.id()).is_none(), "subagent metadata must not name the parent terminal");
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn session_titles_require_exact_identity_and_complete_name_records() {
        let records = concat!(
            "{\"id\":\"own\",\"thread_name\":\" First name \"}\n",
            "{\"id\":\"other\",\"thread_name\":\"Do not use this\"}\n",
            "invalid json\n",
            "{\"id\":\"own\",\"thread_name\":\"  Fix\\n toolbar 🛠  \"}\n",
            "{\"id\":\"own\",\"thread_name\":\"Incomplete",
        );
        assert_eq!(title_from_index(records.as_bytes(), "own").as_deref(), Some("Fix toolbar 🛠"));
        assert!(title_from_index(records.as_bytes(), "missing").is_none());
        for newest in [
            "{\"id\":\"own\",\"thread_name\":\" \"}\n",
            "{\"id\":\"own\",\"thread_name\":null}\n",
        ] {
            assert!(title_from_index(format!("{records}\n{newest}").as_bytes(), "own").is_none());
        }
        assert!(title_from_index(b"{\"id\":\"own\",\"title\":\"Raw first prompt\"}\n", "own").is_none());
        let long = serde_json::json!({"id": "own", "thread_name": "🙂".repeat(300)}).to_string() + "\n";
        assert_eq!(title_from_index(long.as_bytes(), "own").unwrap().chars().count(), 240);
    }
}

#[cfg(test)]
mod prompt_tests {
    #[test]
    fn user_messages_win_over_response_text_and_partial_records() {
        let records = concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Fix the layout\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"# Here is the final response\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"partial",
        );
        assert_eq!(super::prompt_from_records(records.as_bytes()).as_deref(), Some("Fix the layout"));
        assert_eq!(super::prompt_from_records(b"# response\n$ tool output\n"), None);
    }
}
