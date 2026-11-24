//! Launch-scoped session tracking, status, completion, history, and resume helpers for Grok CLI.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
};

const TAIL_BYTES: u64 = 128 * 1024;
const MAX_LINE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrokSummary {
    pub title: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokCompletion {
    pub dir: PathBuf,
    pub events_path: PathBuf,
    pub state: &'static str,
    pub completion_id: Option<String>,
    pub attention: Option<&'static str>,
    pub attention_id: Option<String>,
    pub title: Option<String>,
    pub prompt: Option<String>,
}

fn hex_id(input: &str) -> String {
    format!("{:x}", Sha256::digest(input.as_bytes()))
}

pub fn sessions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".grok/sessions"))
}

#[cfg(test)]
fn sessions_dir_in(home: &Path) -> PathBuf {
    home.join(".grok/sessions")
}

/// URL-encode a working directory path the way Grok's session store names its per-workspace directory.
pub fn url_encode_cwd(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len() * 3);
    for &b in cwd.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && id.chars().any(|c| c.is_ascii_alphanumeric())
}

fn generate_uuid_v4() -> Option<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom").ok()?.read_exact(&mut bytes).ok()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    ))
}

/// Recognize a direct interactive Grok CLI launch without altering shell pipelines or wrappers.
pub fn interactive_args(command: &str) -> Option<Vec<String>> {
    if command.contains(['$', '`', '\n', ';', '|', '&', '<', '>', '#', '\\']) {
        return None;
    }
    let args = shlex::split(command)?;
    let executable = Path::new(args.first()?).file_name()?.to_str()?;
    if executable != "grok"
        || args.get(1).is_some_and(|arg| !arg.starts_with('-'))
        || args.iter().skip(1).any(|arg| {
            matches!(
                arg.split('=').next().unwrap_or_default(),
                "-p" | "--print" | "--prompt" | "--version" | "-v" | "--help" | "-h" | "--"
            )
        })
    {
        return None;
    }
    Some(args)
}

/// Attach a fresh launch-scoped `--session-id <uuid>` to interactive `grok` launches when supported.
pub fn launch_command(agent: &str, command: &str) -> String {
    with_session_id(agent, command, supports_session_id, generate_uuid_v4)
}

fn with_session_id(
    agent: &str,
    command: &str,
    supports_flag: impl FnOnce(&str) -> bool,
    make_uuid: impl FnOnce() -> Option<String>,
) -> String {
    if agent != "grok" && !agent.starts_with("custom-") {
        return command.into();
    }
    let Some(args) = interactive_args(command) else {
        return command.into();
    };
    let already_has_session = args.iter().skip(1).any(|arg| {
        let flag = arg.split('=').next().unwrap_or_default();
        matches!(flag, "--session-id" | "-s" | "--resume" | "-r" | "--continue" | "-c")
    });
    if already_has_session || !supports_flag(&args[0]) {
        return command.into();
    }
    let Some(id) = make_uuid() else {
        return command.into();
    };
    format!("{command} --session-id {id}")
}

fn supports_session_id(executable: &str) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = PathBuf::from(executable);
    if let Some(&supported) = cache.lock().unwrap().get(&key) {
        return supported;
    }
    let mut probe = Command::new(executable);
    probe.arg("--help").stdin(Stdio::null());
    let supported = crate::workspace_model::bounded_output(probe)
        .map(|out| {
            let help = String::from_utf8_lossy(&out);
            help.contains("--session-id") && help.contains("--resume")
        })
        .unwrap_or(false);
    cache.lock().unwrap().insert(key, supported);
    supported
}

pub fn extract_session_id_from_args(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if let Some(val) = arg
            .strip_prefix("--session-id=")
            .or_else(|| arg.strip_prefix("--resume="))
        {
            if is_valid_session_id(val) {
                return Some(val.to_string());
            }
        } else if matches!(arg.as_str(), "--session-id" | "-s" | "--resume" | "-r") {
            if let Some(val) = iter.next() {
                if is_valid_session_id(val) {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

fn read_cmdline_args(pid: u32) -> Option<Vec<String>> {
    let bytes = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<String> = bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    (!args.is_empty()).then_some(args)
}

/// Find `--session-id` / `--resume` on `pid` or its descendant processes in the pane.
pub fn find_session_id_for_pid(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([(pid, 0usize)]);
    while let Some((cur, depth)) = queue.pop_front() {
        if !seen.insert(cur) {
            continue;
        }
        if let Some(args) = read_cmdline_args(cur) {
            if let Some(id) = extract_session_id_from_args(&args) {
                return Some(id);
            }
        }
        if depth < 2 {
            for child in crate::tmux::get_direct_children(cur) {
                queue.push_back((child, depth + 1));
            }
        }
    }
    None
}

fn has_session_files(dir: &Path) -> bool {
    dir.is_dir()
        && (dir.join("events.jsonl").is_file()
            || dir.join("chat_history.jsonl").is_file()
            || dir.join("summary.json").is_file())
}

pub fn find_session_dir_in(base: &Path, id: &str, cwd: Option<&str>) -> Option<PathBuf> {
    if !is_valid_session_id(id) || !base.is_dir() {
        return None;
    }
    if let Some(cwd) = cwd.map(str::trim).filter(|c| !c.is_empty()) {
        let candidate = base.join(url_encode_cwd(cwd)).join(id);
        if has_session_files(&candidate) {
            return Some(candidate);
        }
    }
    let entries = fs::read_dir(base).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join(id);
        if has_session_files(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub fn find_session_dir(id: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let base = sessions_dir()?;
    find_session_dir_in(&base, id, cwd)
}

pub fn find_session_dir_for_pid(pid: u32, cwd: Option<&str>) -> Option<PathBuf> {
    let id = find_session_id_for_pid(pid)?;
    find_session_dir(&id, cwd)
}

pub fn conversation_exists(id: &str) -> bool {
    find_session_dir(id, None).is_some()
}

pub fn summary_in(dir: &Path) -> Option<GrokSummary> {
    let raw = fs::read_to_string(dir.join("summary.json")).ok()?;
    let val: Value = serde_json::from_str(&raw).ok()?;
    let non_empty = |key: &str| {
        val.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Some(GrokSummary {
        title: non_empty("generated_title").or_else(|| non_empty("title")),
        model: non_empty("current_model_id").or_else(|| non_empty("model")),
        effort: non_empty("reasoning_effort").or_else(|| non_empty("effort")),
    })
}

pub fn summary_for_pid(pid: u32, cwd: Option<&str>) -> Option<GrokSummary> {
    summary_in(&find_session_dir_for_pid(pid, cwd)?)
}

pub fn title_for_pid(pid: u32, cwd: Option<&str>) -> Option<String> {
    summary_for_pid(pid, cwd)?.title
}

fn read_tail_lines(path: &Path) -> Option<Vec<String>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    let len = meta.len();
    let offset = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::new();
    file.take(TAIL_BYTES + 1024).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<String> = text
        .lines()
        .skip(usize::from(offset > 0))
        .filter(|line| line.len() <= MAX_LINE_BYTES)
        .map(str::to_owned)
        .collect();
    Some(lines)
}

fn parse_event_line(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    (!trimmed.is_empty()).then(|| serde_json::from_str(trimmed).ok()).flatten()
}

pub fn inspect_status_in(dir: &Path) -> Option<(&'static str, &'static str)> {
    let lines = read_tail_lines(&dir.join("events.jsonl"))?;
    for line in lines.iter().rev() {
        let Some(event) = parse_event_line(line) else {
            continue;
        };
        let kind = event
            .get("type")
            .or_else(|| event.get("event"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "turn_ended" => return Some(("idle", "Idle")),
            "permission_requested" => return Some(("waiting", "Waiting")),
            "permission_resolved" | "turn_started" | "tool_completed" => {
                return Some(("running", "Running"))
            }
            "phase_changed" => {
                let phase = event
                    .get("phase")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match phase {
                    "permission" | "approval" | "waiting" | "waiting_approval" => {
                        return Some(("waiting", "Waiting"))
                    }
                    "idle" => return Some(("idle", "Idle")),
                    "thinking" | "executing" | "tool" | "streaming" | "running" => {
                        return Some(("running", "Running"))
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    None
}

pub fn inspect_status(pid: u32, cwd: Option<&str>) -> Option<(&'static str, &'static str)> {
    inspect_status_in(&find_session_dir_for_pid(pid, cwd)?)
}

pub fn inspect_completion_in(dir: &Path, session_id: &str) -> Option<GrokCompletion> {
    let events_path = dir.join("events.jsonl");
    let lines = read_tail_lines(&events_path)?;
    let status = inspect_status_in(dir)?;
    let summary = summary_in(dir);
    let title = summary.and_then(|s| s.title);
    let prompt = prompts_in(dir, 1).into_iter().next().map(|(p, _)| p);

    match status.0 {
        "running" => Some(GrokCompletion {
            dir: dir.to_path_buf(),
            events_path,
            state: "working",
            completion_id: None,
            attention: None,
            attention_id: None,
            title,
            prompt,
        }),
        "waiting" => {
            let wait_key = lines
                .iter()
                .enumerate()
                .rev()
                .find_map(|(idx, line)| {
                    let ev = parse_event_line(line)?;
                    let kind = ev
                        .get("type")
                        .or_else(|| ev.get("event"))
                        .and_then(Value::as_str)?;
                    if kind == "permission_requested" || kind == "phase_changed" {
                        let ts = ev.get("timestamp").and_then(Value::as_str).unwrap_or("");
                        let turn = ev
                            .get("turn_number")
                            .and_then(Value::as_u64)
                            .map(|n| n.to_string())
                            .unwrap_or_default();
                        return Some(format!("{kind}:{turn}:{ts}:{idx}"));
                    }
                    None
                })
                .unwrap_or_else(|| "waiting".to_string());
            Some(GrokCompletion {
                dir: dir.to_path_buf(),
                events_path,
                state: "working",
                completion_id: None,
                attention: Some("waiting"),
                attention_id: Some(hex_id(&format!("grok:{session_id}:waiting:{wait_key}"))),
                title,
                prompt,
            })
        }
        "idle" => {
            let ended = lines.iter().enumerate().rev().find_map(|(idx, line)| {
                let ev = parse_event_line(line)?;
                let kind = ev
                    .get("type")
                    .or_else(|| ev.get("event"))
                    .and_then(Value::as_str)?;
                (kind == "turn_ended").then_some((idx, ev))
            });
            let (idx, ev) = ended?;
            let outcome = ev
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let turn_key = ev
                .get("turn_number")
                .and_then(Value::as_u64)
                .map(|n| format!("turn-{n}"))
                .or_else(|| ev.get("timestamp").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| format!("line-{idx}"));
            if matches!(outcome, "error" | "failed") {
                return Some(GrokCompletion {
                    dir: dir.to_path_buf(),
                    events_path,
                    state: "error",
                    completion_id: None,
                    attention: Some("error"),
                    attention_id: Some(hex_id(&format!("grok:{session_id}:error:{turn_key}"))),
                    title,
                    prompt,
                });
            }
            if matches!(outcome, "aborted" | "cancelled") {
                return Some(GrokCompletion {
                    dir: dir.to_path_buf(),
                    events_path,
                    state: "error",
                    completion_id: None,
                    attention: None,
                    attention_id: None,
                    title,
                    prompt,
                });
            }
            Some(GrokCompletion {
                dir: dir.to_path_buf(),
                events_path,
                state: "completed",
                completion_id: Some(hex_id(&format!("grok:{session_id}:{turn_key}"))),
                attention: None,
                attention_id: None,
                title,
                prompt,
            })
        }
        _ => None,
    }
}

pub fn inspect_completion(pid: u32, cwd: Option<&str>) -> Option<GrokCompletion> {
    let id = find_session_id_for_pid(pid)?;
    let dir = find_session_dir(&id, cwd)?;
    inspect_completion_in(&dir, &id)
}

/// Extract plain user text from a Grok `chat_history.jsonl` user record, stripping
/// `<system-reminder>` / `<user_info>` wrapper framing when `<user_query>` tags are present.
pub fn extract_user_query(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(start) = trimmed.find("<user_query>") {
        let after = &trimmed[start + "<user_query>".len()..];
        let body = after
            .find("</user_query>")
            .map(|end| &after[..end])
            .unwrap_or(after);
        let cleaned = body.trim();
        return (!cleaned.is_empty()).then(|| cleaned.to_string());
    }
    // Wrapper-only user turns (system-reminder / user_info / context preamble without user_query)
    if trimmed.contains("<user_info>")
        || trimmed.contains("<system-reminder>")
        || trimmed.contains("<environment_context>")
    {
        return None;
    }
    Some(trimmed.to_string())
}

fn extract_content_text(val: &Value) -> Option<String> {
    let content = val
        .get("content")
        .or_else(|| val.get("text"))
        .or_else(|| val.get("message").and_then(|m| m.get("content")))?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(t) = item.as_str() {
                parts.push(t.to_string());
            } else if item.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = item.get("text").and_then(Value::as_str) {
                    parts.push(t.to_string());
                }
            }
        }
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    None
}

fn parse_timestamp_ms(val: &Value) -> Option<u64> {
    let ts = val.get("timestamp").or_else(|| val.get("created_at"))?;
    if let Some(ms) = ts.as_u64() {
        return Some(ms);
    }
    ts.as_str()
        .and_then(crate::prompt_log::parse_iso)
        .and_then(|ms| u64::try_from(ms).ok())
}

/// Chronological user prompts from `chat_history.jsonl`, each vetted by `is_user_prompt`.
pub fn prompts_in(dir: &Path, limit: usize) -> Vec<(String, Option<u64>)> {
    let Some(lines) = read_tail_lines(&dir.join("chat_history.jsonl")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in lines {
        let Some(val) = parse_event_line(&line) else {
            continue;
        };
        let is_user = val.get("type").and_then(Value::as_str) == Some("user")
            || val.get("role").and_then(Value::as_str) == Some("user");
        if !is_user {
            continue;
        }
        let Some(raw) = extract_content_text(&val) else {
            continue;
        };
        let Some(text) = extract_user_query(&raw) else {
            continue;
        };
        if !crate::harness_record::is_user_prompt(&text) {
            continue;
        }
        out.push((text, parse_timestamp_ms(&val)));
    }
    if limit > 0 && out.len() > limit {
        out.drain(..out.len() - limit);
    }
    out
}

pub fn latest_prompt_for_pid(pid: u32, cwd: Option<&str>) -> Option<String> {
    let dir = find_session_dir_for_pid(pid, cwd)?;
    prompts_in(&dir, 1).pop().map(|(p, _)| p)
}

pub fn prompt_history_for_id(id: &str, limit: usize) -> Vec<(String, Option<u64>)> {
    let Some(dir) = find_session_dir(id, None) else {
        return Vec::new();
    };
    prompts_in(&dir, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "grok-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn grok_launch_attaches_session_id_when_supported_and_clean() {
        let cmd = with_session_id(
            "grok",
            "grok --minimal --dangerously-skip-permissions",
            |exe| {
                assert_eq!(exe, "grok");
                true
            },
            || Some("11111111-2222-4333-8444-555555555555".into()),
        );
        assert_eq!(
            cmd,
            "grok --minimal --dangerously-skip-permissions --session-id 11111111-2222-4333-8444-555555555555"
        );
        // Existing resume / session flags or unsupported binary are left untouched.
        assert_eq!(
            with_session_id(
                "grok",
                "grok --resume abc",
                |_| panic!("should not probe when --resume already set"),
                || panic!("should not generate uuid"),
            ),
            "grok --resume abc"
        );
        assert_eq!(
            with_session_id(
                "grok",
                "grok --minimal",
                |_| false,
                || Some("unused".into()),
            ),
            "grok --minimal"
        );
        assert_eq!(
            with_session_id(
                "claude",
                "grok",
                |_| panic!("wrong agent should not probe"),
                || panic!("wrong agent should not generate"),
            ),
            "grok"
        );
    }

    #[test]
    fn grok_extracts_session_id_from_args() {
        assert_eq!(
            extract_session_id_from_args(&[
                "grok".into(),
                "--minimal".into(),
                "--session-id".into(),
                "abc-123".into(),
            ])
            .as_deref(),
            Some("abc-123")
        );
        assert_eq!(
            extract_session_id_from_args(&["grok".into(), "-r".into(), "deadbeef".into()])
                .as_deref(),
            Some("deadbeef")
        );
        assert_eq!(
            extract_session_id_from_args(&["grok".into(), "--session-id=xyz_9".into()])
                .as_deref(),
            Some("xyz_9")
        );
        assert_eq!(
            extract_session_id_from_args(&["grok".into(), "--session-id".into(), "../etc/passwd".into()]),
            None
        );
    }

    #[test]
    fn grok_status_and_completion_follow_events_jsonl() {
        let dir = temp_dir("events");
        fs::write(
            dir.join("summary.json"),
            r#"{"generated_title":"Fix cache bug","current_model_id":"grok-4","reasoning_effort":"high"}"#,
        )
        .unwrap();
        fs::write(
            dir.join("chat_history.jsonl"),
            r#"{"type":"user","content":"<system-reminder>ignore</system-reminder><user_query>Fix the login redirect</user_query>","timestamp":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        fs::write(
            dir.join("events.jsonl"),
            "{\"type\":\"turn_started\",\"turn_number\":1}\n{\"type\":\"phase_changed\",\"phase\":\"thinking\"}\n",
        )
        .unwrap();
        assert_eq!(inspect_status_in(&dir), Some(("running", "Running")));
        let c1 = inspect_completion_in(&dir, "sess-1").unwrap();
        assert_eq!(c1.state, "working");
        assert_eq!(c1.completion_id, None);

        fs::write(
            dir.join("events.jsonl"),
            "{\"type\":\"turn_started\",\"turn_number\":1}\n{\"type\":\"permission_requested\"}\n",
        )
        .unwrap();
        assert_eq!(inspect_status_in(&dir), Some(("waiting", "Waiting")));
        let c_wait = inspect_completion_in(&dir, "sess-1").unwrap();
        assert_eq!(c_wait.state, "working");
        assert_eq!(c_wait.attention, Some("waiting"));
        assert!(c_wait.attention_id.as_deref().is_some_and(|id| id.len() == 64));

        fs::write(
            dir.join("events.jsonl"),
            "{\"type\":\"turn_started\",\"turn_number\":2}\n{\"type\":\"tool_completed\",\"outcome\":\"error\"}\n",
        )
        .unwrap();
        assert_eq!(inspect_status_in(&dir), Some(("running", "Running")));
        let c_run = inspect_completion_in(&dir, "sess-1").unwrap();
        assert_eq!(c_run.attention, None);
        assert_eq!(c_run.attention_id, None);

        fs::write(
            dir.join("events.jsonl"),
            "{\"type\":\"turn_started\",\"turn_number\":2}\n{\"type\":\"turn_ended\",\"turn_number\":2,\"outcome\":\"failed\"}\n",
        )
        .unwrap();
        let c_err = inspect_completion_in(&dir, "sess-1").unwrap();
        assert_eq!(c_err.state, "error");
        assert_eq!(c_err.attention, Some("error"));
        assert!(c_err.attention_id.as_deref().is_some_and(|id| id.len() == 64));

        fs::write(
            dir.join("events.jsonl"),
            "{\"type\":\"turn_started\",\"turn_number\":2}\n{\"type\":\"turn_ended\",\"turn_number\":2,\"outcome\":\"completed\"}\n",
        )
        .unwrap();
        assert_eq!(inspect_status_in(&dir), Some(("idle", "Idle")));
        let c2 = inspect_completion_in(&dir, "sess-1").unwrap();
        assert_eq!(c2.state, "completed");
        assert_eq!(
            c2.completion_id.as_deref(),
            Some(hex_id("grok:sess-1:turn-2").as_str())
        );
        assert_eq!(c2.attention, None);
        assert_eq!(c2.attention_id, None);
        assert_eq!(c2.title.as_deref(), Some("Fix cache bug"));
        assert_eq!(c2.prompt.as_deref(), Some("Fix the login redirect"));
        assert_eq!(
            summary_in(&dir),
            Some(GrokSummary {
                title: Some("Fix cache bug".into()),
                model: Some("grok-4".into()),
                effort: Some("high".into()),
            })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_title_grok_user_query_filtering() {
        let dir = temp_dir("prompts");
        let lines = [
            r#"{"type":"user","content":"<user_info>OS Linux</user_info>","timestamp":"2026-01-01T00:00:00Z"}"#,
            r#"{"type":"user","content":"<user_query><task-notification>done</task-notification></user_query>","timestamp":"2026-01-01T00:01:00Z"}"#,
            r#"{"type":"user","content":"<system-reminder>sys</system-reminder>\n<user_query>Refactor the parser</user_query>","timestamp":"2026-01-01T00:02:00Z"}"#,
            r#"{"type":"assistant","content":"Sure thing"}"#,
        ]
        .join("\n");
        fs::write(dir.join("chat_history.jsonl"), lines).unwrap();
        let prompts = prompts_in(&dir, 10);
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].0, "Refactor the parser");
        assert!(prompts[0].1.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn grok_finds_session_dir_by_encoded_cwd_or_scan() {
        let base = sessions_dir_in(&temp_dir("lookup"));
        let cwd = "/home/user/My App";
        let id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let target = base.join(url_encode_cwd(cwd)).join(id);
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("events.jsonl"), "").unwrap();

        assert_eq!(find_session_dir_in(&base, id, Some(cwd)), Some(target.clone()));
        assert_eq!(find_session_dir_in(&base, id, None), Some(target));
        assert_eq!(find_session_dir_in(&base, "missing", None), None);
        let _ = fs::remove_dir_all(base.parent().unwrap().parent().unwrap());
    }
}
