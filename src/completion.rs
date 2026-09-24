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
            // An interrupted turn says nothing about the next state.
            Some("turn_aborted") => {
                state = "unknown";
                completed = None;
            }
            // Explicit turn failures (Codex >= 0.1xx reports a usage limit as
            // `task_complete` with an `error` object and no final message).
            Some("task_failed" | "error") => {
                state = "error";
                completed = None;
            }
            Some("task_complete") if event["error"].is_object() => {
                state = "error";
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

/// Cached `rollout` answers: `(found_at, pane start time, answer)`.
type RolloutEntry = (std::time::Instant, Option<String>, Option<(PathBuf, String)>);
static ROLLOUTS: OnceLock<Mutex<HashMap<u32, RolloutEntry>>> = OnceLock::new();
/// A validated rollout is re-discovered at most this often.
const ROLLOUT_REWALK: std::time::Duration = std::time::Duration::from_secs(5);
/// A pane without a Codex rollout is re-checked at most this often (one
/// card refresh asks up to three times: status, title and prompt).
const ROLLOUT_NEGATIVE: std::time::Duration = std::time::Duration::from_secs(1);

fn process_start(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.to_owned())
}

/// Find only the nearest CLI's own open rollout, never "latest file in cwd".
///
/// Cached per pane process: a hit is only reused while the pane process is the
/// same (start time) and the Codex descriptor still links to that rollout,
/// and it is re-walked every few seconds regardless.
fn rollout(pane_pid: u32) -> Option<(PathBuf, String)> {
    let cache = ROLLOUTS.get_or_init(Mutex::default);
    let start = process_start(pane_pid);
    if let Some((at, cached_start, answer)) = cache.lock().unwrap().get(&pane_pid) {
        if *cached_start == start {
            match answer {
                Some((fd, identity))
                    if at.elapsed() < ROLLOUT_REWALK
                        && std::fs::read_link(fd)
                            .is_ok_and(|link| link.as_os_str() == identity.as_str()) =>
                {
                    return answer.clone();
                }
                None if at.elapsed() < ROLLOUT_NEGATIVE => return None,
                _ => {}
            }
        }
    }
    let answer = rollout_walk(pane_pid);
    let mut cache = cache.lock().unwrap();
    if cache.len() >= 128 {
        cache.clear();
    }
    cache.insert(pane_pid, (std::time::Instant::now(), start, answer.clone()));
    answer
}

#[cfg(test)]
thread_local! {
    /// `/proc` walks made by this test thread.
    static ROLLOUT_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Don't descend into a Codex process: its tool children may run other agents.
fn rollout_walk(pane_pid: u32) -> Option<(PathBuf, String)> {
    #[cfg(test)]
    ROLLOUT_WALKS.with(|walks| walks.set(walks.get() + 1));
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

/// `(dev, inode, length, mtime s, mtime ns)`: changes whenever a file does.
type Stamp = (u64, u64, u64, i64, i64);

fn stamp(meta: &std::fs::Metadata) -> Stamp {
    (meta.dev(), meta.ino(), meta.len(), meta.mtime(), meta.mtime_nsec())
}

/// Memo of a value derived from a file's content, reused while the file's
/// stamp is unchanged. Refreshes re-ask every second; files change rarely.
fn memo<T: Clone>(
    cache: &'static OnceLock<Mutex<HashMap<String, (Stamp, T)>>>,
    key: String,
    stamp: Stamp,
    compute: impl FnOnce() -> T,
) -> T {
    let cache = cache.get_or_init(Mutex::default);
    if let Some((_, value)) = cache.lock().unwrap().get(&key).filter(|(s, _)| *s == stamp) {
        return value.clone();
    }
    let value = compute();
    let mut cache = cache.lock().unwrap();
    if cache.len() >= 64 {
        cache.clear();
    }
    cache.insert(key, (stamp, value.clone()));
    value
}

static SESSION_IDS: OnceLock<Mutex<HashMap<String, (Stamp, Option<String>)>>> = OnceLock::new();
static TITLES: OnceLock<Mutex<HashMap<String, (Stamp, Option<String>)>>> = OnceLock::new();
static PROMPTS: OnceLock<Mutex<HashMap<String, (Stamp, Option<String>)>>> = OnceLock::new();

fn title_for_rollout(fd: &Path, identity: &Path) -> Option<String> {
    let file = File::open(fd).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    // Keyed on the whole stamp: a rewritten file may carry another header.
    let id = memo(&SESSION_IDS, identity.to_string_lossy().into_owned(), stamp(&meta), || {
        let mut header = Vec::new();
        BufReader::new(file.take(64 * 1024)).read_until(b'\n', &mut header).ok()?;
        if !header.ends_with(b"\n") { return None; }
        let record: Value = serde_json::from_slice(&header).ok()?;
        if record["type"] != "session_meta" || record["payload"]["source"] != "cli" {
            return None;
        }
        record["payload"]["id"].as_str().filter(|id| !id.is_empty()).map(str::to_owned)
    })?;
    // Derive CODEX_HOME from this rollout, including nondefault installations.
    let home = identity.ancestors().find(|path| {
        path.file_name().is_some_and(|name| name == "sessions" || name == "archived_sessions")
    })?.parent()?;
    let index_path = home.join("session_index.jsonl");
    let mut index = File::open(&index_path).ok()?;
    let meta = index.metadata().ok()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } { return None; }
    let key = format!("{}\0{id}", index_path.display());
    memo(&TITLES, key, stamp(&meta), move || title_from_index_file(&mut index, &meta, &id))
}

fn title_from_index_file(index: &mut File, meta: &std::fs::Metadata, id: &str) -> Option<String> {
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
    last_user_prompt_for_pid(String::from_utf8(out.stdout).ok()?.trim().parse().ok()?)
}

/// `last_user_prompt` for a pane process the caller already knows.
pub fn last_user_prompt_for_pid(pid: u32) -> Option<String> {
    let (fd, identity) = rollout(pid)?;
    let mut file = File::open(fd).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } { return None; }
    memo(&PROMPTS, identity, stamp(&meta), move || prompt_from_rollout(&mut file, &meta))
}

fn prompt_from_rollout(file: &mut File, meta: &std::fs::Metadata) -> Option<String> {
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
    // The metadata option comes from the same listing: a per-session
    // `show-options -t =<session>` names a pane target, which tmux rejects
    // ("no such session"), so native adapters were never reported.
    let output = std::process::Command::new("tmux")
        .args(["list-panes", "-a", "-F", PANE_LISTING])
        .output();
    let panes = output
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    ids.iter()
        .map(|id| {
            let Some(terminal) = state.terminals.iter().find(|t| &t.session_name == id) else {
                return unknown(id);
            };
            if let [(pid, option)] = session_panes(&panes, id)[..] {
                if terminal.agent_type == "codex" {
                    api_state(inspect(id, pid))
                } else if let Some(metadata) =
                    crate::harness_metadata::inspect_option(&terminal.agent_type, option)
                {
                    if crate::harness_metadata::owns_pane(&metadata, pid) {
                        native_completion(id, &metadata)
                    } else {
                        unknown(id)
                    }
                } else {
                    unknown(id)
                }
            } else {
                unknown(id)
            }
        })
        .collect()
}

/// The completions API only reports `working`, `completed` or `unknown`; a
/// failed turn (shown as ERROR on cards) is not a completion.
fn api_state(mut completion: Completion) -> Completion {
    if completion.state == "error" {
        completion.state = "unknown".into();
    }
    completion
}

/// `list-panes -a` format: session, pane PID and the launch's metadata option,
/// separated by US (absent from session names, PIDs and our metadata paths).
const PANE_LISTING: &str = "#{session_name}\u{1f}#{pane_pid}\u{1f}#{@super_desktop_metadata}";

/// Every pane of `id` in a `PANE_LISTING` listing: (pane PID, metadata option).
/// More than one pane means the session is ambiguous; callers require exactly one.
fn session_panes<'a>(listing: &'a str, id: &str) -> Vec<(u32, &'a str)> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\u{1f}');
            if fields.next()? != id {
                return None;
            }
            Some((fields.next()?.trim().parse().ok()?, fields.next().unwrap_or("").trim()))
        })
        .collect()
}

fn native_completion(id: &str, metadata: &crate::harness_metadata::Metadata) -> Completion {
    if !matches!(metadata.agent.as_str(), "pi" | "opencode" | "claude") || !metadata.completion_supported {
        return unknown(id);
    }
    let state = match metadata.status.as_str() {
        "working" => "working",
        "completed" if metadata.completion_id.is_some() => "completed",
        _ => "unknown",
    };
    Completion {
        id: id.into(),
        supported: true,
        state: state.into(),
        completion_id: if state == "completed" {
            metadata.completion_id.clone()
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pane_listing_carries_each_sessions_metadata_option() {
        let listing = "sd_term_a\u{1f}101\u{1f}/state/harness/sd_term_a-1.json\n\
                       sd_term_ab\u{1f}102\u{1f}/state/harness/sd_term_ab-1.json\n\
                       sd_term_split\u{1f}103\u{1f}\n\
                       sd_term_split\u{1f}104\u{1f}\n\
                       sd_term_plain\u{1f}105\u{1f}\n";
        assert_eq!(session_panes(listing, "sd_term_a"), vec![(101, "/state/harness/sd_term_a-1.json")]);
        // Exact names only, and a session without the option has an empty one.
        assert_eq!(session_panes(listing, "sd_term_plain"), vec![(105, "")]);
        // Split panes stay ambiguous for the caller, missing sessions are empty.
        assert_eq!(session_panes(listing, "sd_term_split").len(), 2);
        assert!(session_panes(listing, "sd_term_missing").is_empty());
        assert!(PANE_LISTING.contains("#{@super_desktop_metadata}"));
    }

    #[test]
    fn native_completion_never_promotes_idle_waits_or_errors_to_completed() {
        let mut metadata = crate::harness_metadata::Metadata {
            agent: "pi".into(),
            completion_supported: true,
            completion_id: Some("a".repeat(64)),
            ..Default::default()
        };
        for status in ["idle", "waiting", "error", "unknown"] {
            metadata.status = status.into();
            let event = native_completion("session", &metadata);
            assert!(event.supported);
            assert_eq!(event.state, "unknown");
            assert!(event.completion_id.is_none());
        }
        metadata.status = "completed".into();
        assert_eq!(native_completion("session", &metadata).state, "completed");
        metadata.agent = "opencode".into();
        assert_eq!(native_completion("session", &metadata).state, "completed");
        metadata.agent = "claude".into();
        assert_eq!(native_completion("session", &metadata).state, "completed");
        metadata.agent = "openclaw".into();
        assert!(!native_completion("session", &metadata).supported);
    }
    fn event(kind: &str) -> String {
        format!("{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"{kind}\"}}}}\n")
    }
    /// Codex 0.14x usage-limit turn, as written to the rollout (sanitized).
    const USAGE_LIMIT: &str = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn-2\",\"last_agent_message\":null,\"error\":{\"message\":\"You\\u2019ve hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at Sep 30th, 2026 11:36 AM.\",\"codex_error_info\":\"usage_limit_exceeded\"},\"started_at\":1790269332,\"completed_at\":1790269333,\"duration_ms\":358}}\n";

    #[test]
    fn codex_failed_turns_are_errors_and_never_completions() {
        let started = event("task_started");
        let (state, id) = parse_tail(format!("{DONE}{started}{USAGE_LIMIT}").as_bytes(), "a");
        assert_eq!((state.as_str(), id), ("error", None));
        for kind in ["error", "task_failed"] {
            assert_eq!(parse_tail(format!("{started}{}", event(kind)).as_bytes(), "a").0, "error");
        }
        // A new prompt after the failure is ordinary work again.
        assert_eq!(parse_tail(format!("{USAGE_LIMIT}{}", event("user_message")).as_bytes(), "a").0, "working");
        // Response text that merely mentions a limit is not an error.
        let talk = DONE.replace("\"Done\"", "\"You hit the usage limit of the rate limit API\"");
        assert_eq!(parse_tail(talk.as_bytes(), "a").0, "completed");
        let message = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"usage limit error\"}}\n";
        assert_eq!(parse_tail(format!("{started}{message}").as_bytes(), "a").0, "working");
        // An interrupt still reports UNKNOWN, and the phone API keeps its three states.
        assert_eq!(parse_tail(format!("{started}{}", event("turn_aborted")).as_bytes(), "a").0, "unknown");
        let failed = Completion { id: "t".into(), supported: true, state: "error".into(), completion_id: None };
        assert_eq!(api_state(failed).state, "unknown");
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
        let walks = || ROLLOUT_WALKS.with(|walks| walks.get());
        let before = walks();
        let first = inspect("test", child.id());
        assert!(first.supported);
        assert_eq!(first.state, "completed");
        assert_eq!(session_title(child.id()).as_deref(), Some("Fix toolbar sizing"));
        let _ = last_user_prompt_for_pid(child.id());
        // Status, title and prompt of one refresh share one /proc walk while
        // the descriptor still links to the same rollout.
        assert_eq!(walks() - before, 1);
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
