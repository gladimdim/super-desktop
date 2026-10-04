//! Native harness lifecycle recording in the shared, GTK-free library.
//! Hooks (`harness-event`) run once per agent tool call, so this module and
//! its platform services must stay std/serde/sha2/libc only.
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
    crate::platform::process::start_time(pid)
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

#[cfg(test)]
mod tests {
    use super::*;

    const TASK_NOTIFICATION: &str = "<task-notification>\n<task-id>b1bqtqonl</task-id>\n<tool-use-id>toolu_01</tool-use-id>\n<status>completed</status>\n</task-notification>";
    /// A message from another Claude Code session, as the UserPromptSubmit hook
    /// received it (seen in the wild replacing a card's title).
    const PEER_MESSAGE: &str = "<cross-session-message from=\"uds:/run/user/1000/cc-socks/26806.sock\" from-name=\"super-desktop-73\" from-mode=\"bypass\">\nAck. I verified f6141dc on origin/main.\n</cross-session-message>";
    /// The same message as Claude Code's transcript records it.
    const PEER_TRANSCRIPT: &str = "Another Claude session sent a message:\n<cross-session-message from=\"uds:/run/user/1000/cc-socks/26806.sock\" from-name=\"super-desktop-73\" from-mode=\"bypass\">\nAck. I verified f6141dc on origin/main.\n</cross-session-message>";

    #[test]
    fn card_title_ignores_injected_prompt_submissions() {
        let mut metadata = Metadata::default();
        let mut event = |name: &str, extra: Value| {
            let mut value = json!({"hook_event_name":name,"session_id":"own"});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            apply_claude(&mut metadata, &value);
            metadata.clone()
        };
        event("SessionStart", json!({}));
        assert_eq!(event("UserPromptSubmit", json!({"prompt":"Fix the login bug"})).prompt, "Fix the login bug");
        let turn = metadata_turn(&event("Stop", json!({})));
        for injected in [
            TASK_NOTIFICATION,
            "  <task-notification> <task-id>x</task-id>",
            "<system-reminder>Background task finished</system-reminder>",
            "<local-command-stdout>ok</local-command-stdout>",
            "<command-name>/model</command-name>",
            "<bash-input>seq 1 3</bash-input>",
            "<bash-stdout>1 2 3</bash-stdout>",
            "Caveat: The messages below were generated by the user while running local commands.",
            PEER_MESSAGE,
            PEER_TRANSCRIPT,
            "[Cross-session idle notice] super-desktop-73 is idle",
            "[Cross-session delivery notice] held for approval",
            "   ",
        ] {
            let state = event("UserPromptSubmit", json!({"prompt":injected}));
            assert_eq!(state.prompt, "Fix the login bug", "{injected:?} replaced the prompt");
            // The injected turn is still a real turn for status and completion.
            assert_eq!(state.status, "working");
        }
        assert!(metadata_turn(&metadata) > turn);
        // Ordinary prompts that merely contain or start with markup still count.
        for typed in ["<div> is not centered", "Explain <task-notification> tags"] {
            let mut data = Metadata::default();
            apply_claude(&mut data, &json!({"hook_event_name":"UserPromptSubmit","session_id":"own","prompt":typed}));
            assert_eq!(data.prompt, typed);
        }
    }

    fn metadata_turn(metadata: &Metadata) -> u64 {
        metadata.claude_turn
    }

    #[test]
    fn card_title_generic_adapters_drop_injected_prompts() {
        let mut data = Metadata::default();
        apply(&mut data, &json!({"session":"s","prompt":"Real request"}));
        apply(&mut data, &json!({"session":"s","prompt":TASK_NOTIFICATION}));
        assert_eq!(data.prompt, "Real request");
    }

    #[test]
    fn card_title_transcript_skips_injected_user_turns() {
        let records = [
            json!({"type":"user","sessionId":"own","origin":{"kind":"human"},"promptSource":"typed","message":{"content":"My request"}}),
            // Claude Code 2.1.281 shape of a background-task result.
            json!({"type":"user","sessionId":"own","origin":{"kind":"task-notification"},"promptSource":"system","turnOrigin":"task_notification","message":{"content":TASK_NOTIFICATION}}),
            json!({"type":"user","sessionId":"own","origin":{"kind":"auto-continuation"},"promptSource":"system","message":{"content":"Continue"}}),
            // A message from another session (Claude Code 2.1.3xx shape), and the
            // same text without the labels.
            json!({"type":"user","sessionId":"own","isMeta":true,"origin":{"kind":"peer","name":"super-desktop-73"},"promptSource":"system","message":{"content":PEER_TRANSCRIPT}}),
            json!({"type":"user","sessionId":"own","origin":{"kind":"peer"},"message":{"content":"Ack from a peer"}}),
            json!({"type":"user","sessionId":"own","message":{"content":PEER_TRANSCRIPT}}),
            // Older transcripts without origin fields.
            json!({"type":"user","sessionId":"own","message":{"content":TASK_NOTIFICATION}}),
            json!({"type":"user","sessionId":"own","message":{"content":"<local-command-stdout>done</local-command-stdout>"}}),
            json!({"type":"user","sessionId":"own","message":{"content":[{"type":"text","text":"<bash-input>ls</bash-input>"}]}}),
            json!({"type":"user","sessionId":"own","message":{"content":[{"type":"tool_result","content":"noise"}]}}),
        ];
        let bytes = records.iter().map(|r| format!("{r}\n")).collect::<String>();
        let (_, prompt) = claude_transcript_records(bytes.as_bytes(), "own");
        assert_eq!(prompt.as_deref(), Some("My request"));
    }

    #[test]
    fn card_title_stop_repairs_a_stored_injected_prompt() {
        let dir = std::env::temp_dir().join(format!("sd-title-repair-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("t.jsonl");
        let records = [
            json!({"type":"user","sessionId":"own","origin":{"kind":"human"},"promptSource":"typed","message":{"content":"Rewrite the quest texts"}}),
            json!({"type":"user","sessionId":"own","origin":{"kind":"task-notification"},"promptSource":"system","message":{"content":TASK_NOTIFICATION}}),
        ];
        fs::write(&transcript, records.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
        // Metadata written by an older build that stored the notification, or a
        // message from another session (the prompt a card showed in the wild).
        for stored in [TASK_NOTIFICATION, PEER_MESSAGE] {
            let mut data = Metadata { native_session: "own".into(), prompt: crate::harness_record::clean(stored), ..Default::default() };
            apply_claude(&mut data, &json!({"hook_event_name":"Stop","session_id":"own","transcript_path":transcript}));
            assert_eq!(data.prompt, "Rewrite the quest texts", "{stored:?} was not repaired");
        }
        let mut data = Metadata { native_session: "own".into(), prompt: crate::harness_record::clean(TASK_NOTIFICATION), ..Default::default() };
        apply_claude(&mut data, &json!({"hook_event_name":"Stop","session_id":"own","transcript_path":transcript}));
        // A valid stored prompt is not replaced by the transcript on Stop.
        data.prompt = "Newer typed prompt".into();
        apply_claude(&mut data, &json!({"hook_event_name":"Stop","session_id":"own","transcript_path":transcript}));
        assert_eq!(data.prompt, "Newer typed prompt");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn card_title_hook_defers_to_transcript_for_untagged_injected_text() {
        // Claude Code's auto-continuation after a usage-limit reset reaches the
        // UserPromptSubmit hook as plain text; only the transcript marks it.
        let continuation = "Your claude.ai usage limit has reset. Continue the task you were working on when the limit was reached; do not repeat work.";
        let dir = std::env::temp_dir().join(format!("sd-title-cont-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("t.jsonl");
        let write = |records: &[Value]| {
            fs::write(&transcript, records.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
        };
        let human = json!({"type":"user","sessionId":"own","origin":{"kind":"human"},"promptSource":"typed","message":{"content":"Push the PR"}});
        let auto = json!({"type":"user","sessionId":"own","isMeta":true,"origin":{"kind":"auto-continuation"},"promptSource":"system","message":{"content":continuation}});
        let submit = |data: &mut Metadata, prompt: &str| {
            apply_claude(data, &json!({"hook_event_name":"UserPromptSubmit","session_id":"own","prompt":prompt,"transcript_path":transcript}));
        };
        let mut data = Metadata::default();
        write(&[human.clone()]);
        submit(&mut data, "Push the PR");
        assert_eq!(data.prompt, "Push the PR");
        // Record already written when the hook runs: vetoed immediately.
        write(&[human.clone(), auto.clone()]);
        submit(&mut data, continuation);
        assert_eq!(data.prompt, "Push the PR");
        assert_eq!(data.status, "working");
        // Record written after the hook: Stop repairs it from the transcript.
        let mut late = Metadata::default();
        write(&[human.clone()]);
        submit(&mut late, "Push the PR");
        submit(&mut late, continuation);
        write(&[human, auto]);
        apply_claude(&mut late, &json!({"hook_event_name":"Stop","session_id":"own","transcript_path":transcript}));
        assert_eq!(late.prompt, "Push the PR");
        fs::remove_dir_all(dir).unwrap();
    }

}
