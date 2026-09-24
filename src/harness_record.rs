//! Native harness lifecycle recording, shared by the GTK application and the
//! small GTK-free `super-desktop-client` binary. Hooks (`harness-event`) run
//! once per agent tool call, so this module must stay std/serde/sha2/libc only:
//! it is compiled into both binaries (see `src/bin/super-desktop-client.rs`).
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    path::{Path, PathBuf},
};

pub const LIMIT: u64 = 65536;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Metadata {
    pub version: u32,
    pub agent: String,
    pub launcher: String,
    pub native_session: String,
    pub title: String,
    pub prompt: String,
    pub status: String,
    pub model: String,
    pub pid: u32,
    pub process_start: String,
    pub emitter: u32,
    pub observed_at_ms: u64,
    pub completion_supported: bool,
    pub completion_id: Option<String>,
    pub claude_turn: u64,
    pub claude_turn_active: bool,
}

pub fn root() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/state/super-desktop/harness"))
}

pub fn clean(value: &str) -> String {
    crate::terminal_text::strip_terminal_escapes(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect()
}

pub fn start_time(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(
        stat.rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)?
            .to_owned(),
    )
}

pub fn read(path: &Path) -> Option<Metadata> {
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    serde_json::from_reader(file.take(LIMIT)).ok()
}
pub fn atomic_write(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    atomic_bytes(path, &serde_json::to_vec(value)?)
}
pub fn atomic_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_extension(format!("{}-{serial}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(bytes)?;
    fs::rename(temp, path)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn status(value: &str) -> Option<(&'static str, &'static str)> {
    Some(match value {
        "working" => ("WORKING", "● WORKING"),
        "completed" => ("FINISHED", "✓ FINISHED"),
        "idle" => ("IDLE", "● IDLE"),
        "waiting" => ("WAITING", "◌ WAITING"),
        "error" => ("ERROR", "⚠ ERROR"),
        "unknown" => ("UNKNOWN", "? UNKNOWN"),
        _ => return None,
    })
}

/// Hooks only observe. Never return blocking decisions or print prompt data.
pub fn record(kind: &str) {
    if let Err(error) = record_inner(kind) {
        eprintln!("Harness metadata: {error}");
    }
}
pub fn record_inner(kind: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::var("SD_HARNESS_FILE")?);
    if Some(path.parent().ok_or("missing parent")?) != root().as_deref() {
        return Err("invalid metadata path".into());
    }
    let input: Value = serde_json::from_reader(std::io::stdin().take(LIMIT))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path.with_extension("lock"))?;
    // flock is released when this descriptor closes, including on early return.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut data = read(&path).ok_or("metadata unavailable")?;
    let pid = std::env::var("SD_HARNESS_PID")?.parse::<u32>()?;
    let start = start_time(pid).ok_or("harness exited")?;
    if data.pid != 0 && (data.pid != pid || data.process_start != start) {
        return Err("wrong launch".into());
    }
    data.pid = pid;
    data.process_start = start;
    data.observed_at_ms = now_ms();
    if kind == "init" {
        return Ok(atomic_write(&path, &data)?);
    }
    if kind == "claude" {
        if data.agent != "claude" {
            return Err("wrong adapter".into());
        }
        apply_claude(&mut data, &input);
    } else {
        if kind != data.agent {
            return Err("wrong adapter".into());
        }
        let emitter = input["emitter"].as_u64().unwrap_or(0) as u32;
        if emitter == 0 || (kind != "openclaw" && data.emitter != 0 && data.emitter != emitter) {
            return Ok(());
        }
        data.emitter = emitter;
        apply(&mut data, &input);
    }
    atomic_write(&path, &data)?;
    Ok(())
}

pub fn apply(data: &mut Metadata, event: &Value) {
    if let Some(session) = event["session"].as_str() {
        if !session.is_empty() && data.native_session != session {
            data.native_session = session.into();
            data.title.clear();
            data.prompt.clear();
            data.model.clear();
            data.status = "unknown".into();
            data.completion_id = None;
            data.completion_supported = false;
            data.claude_turn_active = false;
        }
    }
    if let Some(title) = event["title"].as_str() {
        data.title = clean(title);
    }
    if let Some(prompt) = event["prompt"].as_str() {
        data.prompt = clean(prompt);
    }
    if let Some(model) = event["model"].as_str() {
        data.model = clean(model);
    }
    if let Some(state) = event["status"].as_str().filter(|s| status(s).is_some()) {
        data.completion_id = None;
        if state == "completed" {
            use sha2::{Digest, Sha256};
            if matches!(data.agent.as_str(), "pi" | "opencode" | "claude")
                && data.completion_supported
                && !data.native_session.is_empty()
            {
                if let Some(turn) = event["completionTurn"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 128)
                {
                    data.completion_id = Some(format!(
                        "{:x}",
                        Sha256::digest(format!(
                            "{}\0{}\0{}\0{}\0{turn}",
                            data.agent, data.pid, data.process_start, data.native_session
                        ))
                    ));
                }
            }
            data.status = if data.completion_id.is_some() {
                "completed"
            } else {
                "unknown"
            }
            .into();
        } else {
            data.status = state.into();
        }
    }
    if matches!(data.agent.as_str(), "pi" | "opencode" | "claude") && event["completionSupported"] == true {
        data.completion_supported = true;
    }
}

pub fn apply_claude(data: &mut Metadata, input: &Value) {
    let event = input["hook_event_name"].as_str().unwrap_or("");
    let session = input["session_id"].as_str().unwrap_or("");
    if session.is_empty() || input["agent_id"].as_str().is_some_and(|id| !id.is_empty()) {
        return;
    }
    if !data.native_session.is_empty() && data.native_session != session && event != "SessionStart"
    {
        return;
    }
    if event == "PostModelSwitch" {
        if let Some(model) = input["to_model"].as_str() {
            apply(data, &json!({"session":session,"model":model}));
        }
        return;
    }
    let state = match event {
        "SessionStart" => "idle",
        "Stop" => {
            if data.status == "error" {
                "error"
            } else {
                "idle"
            }
        }
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PreCompact"
        | "PostCompact" => "working",
        "PermissionRequest" => "waiting",
        "StopFailure" => "error",
        "SessionEnd" => "unknown",
        "Notification"
            if input["notification_type"] == "permission_prompt"
                || input["notification_type"] == "elicitation_dialog" =>
        {
            "waiting"
        }
        _ => return,
    };
    let mut patch = json!({"session":session,"status":state});
    if event == "UserPromptSubmit" {
        patch["prompt"] = input["prompt"].clone();
    }
    if let Some(model) = input["model"].as_str() {
        patch["model"] = json!(model);
    }
    if let Some(title) = input["session_name"].as_str() {
        patch["title"] = json!(title);
    }
    if matches!(event, "SessionStart" | "Stop" | "UserPromptSubmit") {
        if let Some(path) = input["transcript_path"].as_str() {
            if let Some((title, prompt)) = claude_transcript(Path::new(path), session) {
                if let Some(title) = title {
                    patch["title"] = json!(title);
                }
                if event == "SessionStart" {
                    if let Some(prompt) = prompt {
                        patch["prompt"] = json!(prompt);
                    }
                }
            }
        }
    }
    // Register capability before completion, but never turn resumed history into an alert.
    patch["completionSupported"] = json!(true);
    if event == "Stop" && matches!(data.status.as_str(), "working" | "completed") && data.claude_turn_active
        && input["stop_hook_active"] == false
        && input["last_assistant_message"].as_str().is_some_and(|text| !text.trim().is_empty())
    {
        patch["status"] = json!("completed");
        patch["completionTurn"] = json!(format!("prompt-{}", data.claude_turn));
    }
    apply(data, &patch);
    match event {
        "SessionStart" | "SessionEnd" | "StopFailure" => data.claude_turn_active = false,
        "UserPromptSubmit" => {
            data.claude_turn = data.claude_turn.saturating_add(1);
            data.claude_turn_active = data.claude_turn < u64::MAX;
        }
        _ => {}
    }
}

pub fn claude_transcript(path: &Path, session: &str) -> Option<(Option<String>, Option<String>)> {
    let mut file = fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let offset = metadata.len().saturating_sub(256 * 1024);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    file.take(metadata.len() - offset)
        .read_to_end(&mut bytes)
        .ok()?;
    let bytes = if offset > 0 {
        &bytes[bytes.iter().position(|b| *b == b'\n')? + 1..]
    } else {
        &bytes
    };
    Some(claude_transcript_records(bytes, session))
}

pub fn claude_transcript_records(bytes: &[u8], session: &str) -> (Option<String>, Option<String>) {
    let (mut title, mut prompt) = (None, None);
    for line in bytes
        .split_inclusive(|b| *b == b'\n')
        .filter(|l| l.ends_with(b"\n"))
    {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if record["isSidechain"] == true
            || record["isMeta"] == true
            || record["sessionId"].as_str().is_some_and(|id| id != session)
        {
            continue;
        }
        match record["type"].as_str() {
            Some("custom-title") => {
                if let Some(value) = record["customTitle"].as_str() {
                    title = Some(clean(value));
                }
            }
            Some("user") => {
                let content = &record["message"]["content"];
                let value = content.as_str().map(str::to_owned).or_else(|| {
                    content.as_array().map(|parts| {
                        parts
                            .iter()
                            .filter(|p| p["type"] == "text")
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                });
                if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
                    prompt = Some(clean(&value));
                }
            }
            _ => {}
        }
    }
    (title, prompt)
}
