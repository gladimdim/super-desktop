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

impl Metadata {
    /// Whether this launch's native adapter has reported anything yet.
    ///
    /// `prepare` writes the file and `harness-event init` only stamps the
    /// process identity. Every adapter event after that names a native session
    /// (Claude hooks, the OpenClaw gateway plugin, Pi) or carries the JS
    /// reporter's emitter (OpenCode's load-time idle). A silent adapter — for
    /// example an OpenClaw gateway without the SUPER DESKTOP plugin, or hooks
    /// that never ran — describes nothing, so readers fall back to the typed
    /// prompt and the screen status instead of a blank title and UNKNOWN. Once
    /// the adapter reports, it is authoritative again.
    pub fn adapter_reported(&self) -> bool {
        !self.native_session.is_empty()
            || self.emitter != 0
            || !self.title.is_empty()
            || !self.prompt.is_empty()
    }
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

/// Messages a harness injects into the conversation as if the user typed them.
/// Claude Code writes background-task results (`<task-notification>`), slash
/// command echoes, `!` shell input/output, reminders and messages from another
/// Claude Code session (`<cross-session-message>`, and its delivery and idle
/// notices) as "user" turns, and also passes them to the `UserPromptSubmit`
/// hook. None of them is a prompt.
const SYNTHETIC_PROMPT_PREFIXES: &[&str] = &[
    "<task-notification",
    "<system-reminder",
    "<local-command-",
    "<command-name",
    "<command-message",
    "<command-args",
    "<bash-input",
    "<bash-stdout",
    "<bash-stderr",
    "<user-prompt-submit-hook",
    "caveat: the messages below were generated",
    // The hook gets the bare message; the transcript prefaces it.
    "<cross-session-message",
    "another claude session sent a message",
    "[cross-session ",
];

/// True only for text a person submitted as a prompt.
///
/// CARD TITLE INVARIANT (regressed three times): every prompt stored in or
/// read from harness metadata goes through this check, so the card and phone
/// title only ever shows what the user typed. See the "Card titles" section of
/// AGENTS.md before changing it or adding a new prompt source.
pub fn is_user_prompt(text: &str) -> bool {
    let text = text.trim_start();
    if text.is_empty() {
        return false;
    }
    let head: String = text.chars().take(64).collect::<String>().to_lowercase();
    !SYNTHETIC_PROMPT_PREFIXES.iter().any(|prefix| head.starts_with(prefix))
}

/// OpenCode names a session "New session - <ISO timestamp>" ("Child session -
/// …" for subagents) until it generates a real title, and keeps that
/// placeholder when title generation is unavailable. It names nothing the user
/// did, so it is treated as no title: the card falls back to the submitted
/// prompt (see the "Card titles" rules in AGENTS.md).
pub fn is_placeholder_title(agent: &str, title: &str) -> bool {
    if agent != "opencode" {
        return false;
    }
    let title = title.trim();
    let Some(stamp) = ["New session - ", "Child session - "]
        .iter()
        .find_map(|prefix| title.strip_prefix(prefix))
    else {
        return false;
    };
    // Exactly `YYYY-MM-DDTHH:MM:SS.mmmZ`, as OpenCode's own `isDefaultTitle`.
    let shape = "dddd-dd-ddTdd:dd:dd.dddZ";
    stamp.len() == shape.len()
        && stamp.bytes().zip(shape.bytes()).all(|(c, s)| match s {
            b'd' => c.is_ascii_digit(),
            _ => c == s,
        })
}

/// A native title worth showing: never an auto-generated placeholder.
pub fn native_title(agent: &str, title: &str) -> String {
    let title = clean(title);
    if is_placeholder_title(agent, &title) {
        String::new()
    } else {
        title
    }
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
        data.title = native_title(&data.agent, title);
    }
    if let Some(prompt) = event["prompt"].as_str().filter(|p| is_user_prompt(p)) {
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
    // A task notification or other injected turn still starts a turn (status and
    // completion tracking below), but it must never replace the user's prompt.
    // The hook input has no origin field, so the transcript (below) also vetoes
    // text it records as injected, e.g. the usage-limit auto-continuation.
    let submitted = input["prompt"].as_str().filter(|p| event == "UserPromptSubmit" && is_user_prompt(p));
    if let Some(model) = input["model"].as_str() {
        patch["model"] = json!(model);
    }
    if let Some(title) = input["session_name"].as_str() {
        patch["title"] = json!(title);
    }
    if matches!(event, "SessionStart" | "Stop" | "UserPromptSubmit") {
        let scan = input["transcript_path"]
            .as_str()
            .and_then(|path| claude_transcript(Path::new(path), session));
        if let Some(scan) = &scan {
            if let Some(title) = &scan.title {
                patch["title"] = json!(title);
            }
        }
        let injected = |text: &str| scan.as_ref().is_some_and(|scan| scan.injected.contains(&clean(text)));
        if let Some(prompt) = submitted.filter(|p| !injected(p)) {
            patch["prompt"] = json!(prompt);
        } else if event == "SessionStart" || !is_user_prompt(&data.prompt) || injected(&data.prompt) {
            // Resume, or repair a stored prompt that was injected (the transcript
            // record can land after the hook ran, or predates the filter).
            if let Some(prompt) = scan.as_ref().and_then(|scan| scan.prompt.clone()) {
                patch["prompt"] = json!(prompt);
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

/// What the tail of a Claude transcript says about the card's own session.
#[derive(Debug, Default)]
pub struct TranscriptScan {
    pub title: Option<String>,
    /// Last prompt a person submitted.
    pub prompt: Option<String>,
    /// Cleaned text of user turns Claude Code injected itself.
    pub injected: Vec<String>,
}

pub fn claude_transcript(path: &Path, session: &str) -> Option<TranscriptScan> {
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
    Some(claude_transcript_scan(bytes, session))
}

#[cfg(test)]
pub fn claude_transcript_records(bytes: &[u8], session: &str) -> (Option<String>, Option<String>) {
    let scan = claude_transcript_scan(bytes, session);
    (scan.title, scan.prompt)
}

pub fn claude_transcript_scan(bytes: &[u8], session: &str) -> TranscriptScan {
    let mut scan = TranscriptScan::default();
    for line in bytes
        .split_inclusive(|b| *b == b'\n')
        .filter(|l| l.ends_with(b"\n"))
    {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if record["isSidechain"] == true
            || record["sessionId"].as_str().is_some_and(|id| id != session)
        {
            continue;
        }
        let text = || {
            let content = &record["message"]["content"];
            content.as_str().map(str::to_owned).or_else(|| {
                content.as_array().map(|parts| {
                    parts
                        .iter()
                        .filter(|p| p["type"] == "text")
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
            })
        };
        // Newer transcripts label turns Claude Code produced itself.
        let injected = record["isMeta"] == true
            || record["promptSource"] == "system"
            || matches!(
                record["origin"]["kind"].as_str(),
                Some("task-notification" | "auto-continuation" | "peer")
            );
        if injected {
            if record["type"] == "user" {
                if let Some(value) = text().filter(|v| !v.trim().is_empty()) {
                    scan.injected.push(clean(&value));
                }
            }
            continue;
        }
        match record["type"].as_str() {
            Some("custom-title") => {
                if let Some(value) = record["customTitle"].as_str() {
                    scan.title = Some(clean(value));
                }
            }
            Some("user") => {
                if let Some(value) = text().filter(|v| is_user_prompt(v)) {
                    scan.prompt = Some(clean(&value));
                }
            }
            _ => {}
        }
    }
    scan
}
