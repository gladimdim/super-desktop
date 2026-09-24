//! Native lifecycle observations scoped to one launch, never guessed from output.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::Command,
};

const OPTION: &str = "@super_desktop_metadata";
const LIMIT: u64 = 65536;

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
}

fn root() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/state/super-desktop/harness"))
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn clean(value: &str) -> String {
    crate::tmux::strip_terminal_escapes(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect()
}
fn start_time(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(
        stat.rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)?
            .to_owned(),
    )
}

/// A surviving old launch must not describe a respawned/replaced tmux pane.
pub fn owns_pane(metadata: &Metadata, pane_pid: u32) -> bool {
    let mut pid = metadata.pid;
    for _ in 0..32 {
        if pid == 0 { return false; }
        if pid == pane_pid { return true; }
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else { return false; };
        let Some(parent) = stat.rsplit_once(')').and_then(|(_, fields)| fields.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u32>().ok()) else { return false; };
        if parent == pid { return false; }
        pid = parent;
    }
    false
}
fn read(path: &Path) -> Option<Metadata> {
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    serde_json::from_reader(file.take(LIMIT)).ok()
}
fn atomic_write(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    atomic_bytes(path, &serde_json::to_vec(value)?)
}
fn atomic_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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

pub struct Launch {
    pub command: String,
    path: Option<PathBuf>,
}
impl Launch {
    pub fn register(&self, session: &str) {
        if let Some(path) = &self.path {
            let _ = Command::new(crate::tmux::tmux_bin())
                .args(["set-option", "-t", session, OPTION, &path.to_string_lossy()])
                .output();
        }
    }
}

/// Only decorate an attributable direct CLI invocation. Shell scripts and custom
/// wrappers keep working unchanged; they must opt in to a native adapter.
pub fn prepare(session: &str, agent: &str, command: &str) -> Launch {
    prepare_with_root(session, agent, command, root())
}

fn prepare_with_root(session: &str, agent: &str, command: &str, root: Option<PathBuf>) -> Launch {
    let unchanged = || Launch {
        command: command.into(),
        path: None,
    };
    if !native_agent(agent) && !agent.starts_with("custom-") {
        return unchanged();
    }
    let Some(mut args) = shlex::split(command) else {
        return unchanged();
    };
    let Some(exe_name) = args
        .first()
        .and_then(|s| Path::new(s).file_name())
        .and_then(|s| s.to_str())
    else {
        return unchanged();
    };
    let exe_name = exe_name.to_owned();
    let launcher = agent;
    let agent = if launcher.starts_with("custom-") && native_agent(&exe_name) {
        exe_name.as_str()
    } else {
        agent
    };
    if !native_agent(agent)
        || exe_name != agent
        || command.contains(['$', '`', '\n'])
        || args.iter().any(|s| {
            s.starts_with("--settings=")
                || matches!(
                    s.as_str(),
                    "&&" | "||"
                        | ";"
                        | "|"
                        | "&"
                        | ">"
                        | ">>"
                        | "<"
                        | "--"
                        | "--settings"
                        | "--pure"
                        | "--bare"
                        | "-p"
                        | "--print"
                        | "run"
                        | "serve"
                        | "attach"
                )
        })
    {
        return unchanged();
    }
    let Some(root) = root else {
        return unchanged();
    };
    if install(&root).is_err() {
        return unchanged();
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = root.join(format!("{session}-{stamp}.json"));
    let initial = Metadata {
        version: 1,
        agent: agent.into(),
        launcher: launcher.into(),
        status: "unknown".into(),
        ..Default::default()
    };
    if atomic_write(&path, &initial).is_err() {
        return unchanged();
    }
    let Some(exe) = std::env::current_exe().ok() else {
        return unchanged();
    };
    let mut envs = vec![
        format!("SD_HARNESS_FILE={}", path.display()),
        format!("SD_HARNESS_EXE={}", exe.display()),
        format!("SD_HARNESS_AGENT={agent}"),
    ];
    match agent {
        "claude" => {
            let reporter = format!("{} harness-event claude", quote(&exe.to_string_lossy()));
            let mut hooks = serde_json::Map::new();
            for event in [
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PermissionRequest",
                "PostToolUse",
                "PostToolUseFailure",
                "Notification",
                "Stop",
                "StopFailure",
                "SessionEnd",
                "PreCompact",
                "PostCompact",
                "PostModelSwitch",
            ] {
                hooks.insert(
                    event.into(),
                    json!([{ "hooks": [{"type":"command", "command":reporter, "timeout":3}]}]),
                );
            }
            args.extend(["--settings".into(), json!({"hooks":hooks}).to_string()]);
        }
        "pi" => args.extend([
            "--extension".into(),
            root.join("pi.mjs").to_string_lossy().into_owned(),
        ]),
        "opencode" => {
            let mut config: Value = match std::env::var("OPENCODE_CONFIG_CONTENT") {
                Ok(raw) => match serde_json::from_str(&raw) {
                    Ok(v) => v,
                    Err(_) => return unchanged(),
                },
                Err(_) => json!({}),
            };
            if !config.is_object() {
                return unchanged();
            }
            let plugins = config
                .as_object_mut()
                .unwrap()
                .entry("plugin")
                .or_insert_with(|| json!([]));
            let Some(plugins) = plugins.as_array_mut() else {
                return unchanged();
            };
            let url = reqwest::Url::from_file_path(root.join("opencode.mjs")).unwrap();
            plugins.push(json!(url.as_str()));
            envs.push(format!("OPENCODE_CONFIG_CONTENT={config}"));
        }
        "openclaw" => {
            // A dedicated gateway session avoids mixing multiple TUI cards.
            if args
                .iter()
                .any(|a| a == "--session" || a.starts_with("--session="))
            {
                return unchanged();
            }
            args.extend(["--session".into(), session.into()]);
            let _ = atomic_write(
                &root.join(format!("{session}.link.json")),
                &json!({"path":path,"exe":exe}),
            );
        }
        _ => unreachable!(),
    }
    // Record the actual exec'd harness PID, not the short-lived hook child.
    let script = "export SD_HARNESS_PID=$$; printf '{}' | \"$SD_HARNESS_EXE\" harness-event init; exec \"$@\"";
    let command = std::iter::once("env".to_string())
        .chain(envs.iter().map(|s| quote(s)))
        .chain([
            "sh".into(),
            "-c".into(),
            quote(script),
            "super-desktop-harness".into(),
        ])
        .chain(args.iter().map(|s| quote(s)))
        .collect::<Vec<_>>()
        .join(" ");
    Launch {
        command,
        path: Some(path),
    }
}

fn install(root: &Path) -> std::io::Result<()> {
    fs::create_dir_all(root)?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    for (name, content) in [
        ("report.mjs", include_str!("../assets/harness/report.mjs")),
        ("pi.mjs", include_str!("../assets/harness/pi.mjs")),
        (
            "opencode.mjs",
            include_str!("../assets/harness/opencode.mjs"),
        ),
        (
            "openclaw.mjs",
            include_str!("../assets/harness/openclaw.mjs"),
        ),
        (
            "openclaw.plugin.json",
            include_str!("../assets/harness/openclaw.plugin.json"),
        ),
    ] {
        if fs::read_to_string(root.join(name)).ok().as_deref() != Some(content) {
            atomic_bytes(&root.join(name), content.as_bytes())?;
        }
    }
    let plugin = root.join("openclaw");
    fs::create_dir_all(&plugin)?;
    atomic_bytes(
        &plugin.join("index.mjs"),
        include_bytes!("../assets/harness/openclaw.mjs"),
    )?;
    atomic_bytes(
        &plugin.join("report.mjs"),
        include_bytes!("../assets/harness/report.mjs"),
    )?;
    atomic_bytes(
        &plugin.join("openclaw.plugin.json"),
        include_bytes!("../assets/harness/openclaw.plugin.json"),
    )?;
    atomic_bytes(
        &plugin.join("package.json"),
        br#"{"name":"super-desktop-metadata","version":"1.0.0","type":"module","openclaw":{"extensions":["./index.mjs"]}}"#,
    )?;
    Ok(())
}

/// Explicit setup for the independently running OpenClaw gateway.
pub fn install_openclaw() -> Result<(), String> {
    let root = root().ok_or("HOME unavailable")?;
    install(&root).map_err(|e| e.to_string())?;
    let status = Command::new("openclaw")
        .args([
            "plugins",
            "install",
            "--link",
            "--force",
            "--accept-capabilities",
        ])
        .arg(root.join("openclaw"))
        .status()
        .map_err(|e| format!("Install OpenClaw first: {e}"))?;
    if !status.success() {
        return Err("OpenClaw plugin registration failed".into());
    }
    // Recent gateways require explicit conversation-hook access for local
    // plugins. Only grant this bundled observer its required hook access.
    for args in [
        vec!["plugins", "enable", "super-desktop-metadata"],
        vec![
            "config",
            "set",
            "plugins.entries.super-desktop-metadata.hooks.allowConversationAccess",
            "true",
            "--strict-json",
        ],
    ] {
        if !Command::new("openclaw")
            .args(args)
            .status()
            .map_err(|e| e.to_string())?
            .success()
        {
            return Err(
                "OpenClaw plugin installed but activation failed; inspect its plugin configuration"
                    .into(),
            );
        }
    }
    println!("Gateway plugin registered. Restart your OpenClaw gateway, then launch a new OpenClaw card.");
    Ok(())
}

pub fn native_agent(agent: &str) -> bool {
    matches!(agent, "claude" | "opencode" | "pi" | "openclaw")
}

pub fn inspect(session: &str, agent: &str) -> Option<Metadata> {
    if !native_agent(agent) && !agent.starts_with("custom-") {
        return None;
    }
    type Cache =
        std::collections::HashMap<(String, String), (std::time::Instant, Option<Metadata>)>;
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (session.to_owned(), agent.to_owned());
    if let Some((at, value)) = cache.lock().unwrap().get(&key) {
        if at.elapsed() < std::time::Duration::from_millis(250) {
            return value.clone();
        }
    }
    let value = inspect_uncached(session, agent);
    let mut cache = cache.lock().unwrap();
    if cache.len() >= 256 {
        cache.clear();
    }
    cache.insert(key, (std::time::Instant::now(), value.clone()));
    value
}

fn inspect_uncached(session: &str, agent: &str) -> Option<Metadata> {
    if !native_agent(agent) && !agent.starts_with("custom-") {
        return None;
    }
    let out = Command::new(crate::tmux::tmux_bin())
        .args(["show-options", "-qv", "-t", &format!("={session}"), OPTION])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let path = Path::new(text.trim());
    if path.parent()? != root()? {
        return None;
    }
    let mut value = read(path)?;
    if value.version != 1
        || !native_agent(&value.agent)
        || (value.agent != agent && value.launcher != agent)
        || start_time(value.pid).as_deref() != Some(&value.process_start)
    {
        return None;
    }
    refresh_liveness(&mut value, now_ms());
    Some(value)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn refresh_liveness(value: &mut Metadata, now: u64) {
    if value.agent == "openclaw"
        && (value.observed_at_ms == 0
            || now < value.observed_at_ms
            || now - value.observed_at_ms > 15_000)
    {
        value.status = "unknown".into();
    }
}

pub fn title(session: &str, agent: &str) -> Option<String> {
    let title = inspect(session, agent)
        .map(|value| value.title)
        .filter(|title| !title.is_empty());
    if title.is_some() {
        return title;
    }
    if agent == "opencode" {
        return crate::tmux::get_opencode_title_by_id(&crate::tmux::resolve_own_opencode_id(
            session, None,
        )?);
    }
    None
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
fn record_inner(kind: &str) -> Result<(), Box<dyn std::error::Error>> {
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

fn apply(data: &mut Metadata, event: &Value) {
    if let Some(session) = event["session"].as_str() {
        if !session.is_empty() && data.native_session != session {
            data.native_session = session.into();
            data.title.clear();
            data.prompt.clear();
            data.model.clear();
            data.status = "unknown".into();
            data.completion_id = None;
            data.completion_supported = false;
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
            if data.agent == "pi" && data.completion_supported && !data.native_session.is_empty() {
                if let Some(turn) = event["completionTurn"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 128)
                {
                    data.completion_id = Some(format!(
                        "{:x}",
                        Sha256::digest(format!(
                            "pi\0{}\0{}\0{}\0{turn}",
                            data.pid, data.process_start, data.native_session
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
    if data.agent == "pi" && event["completionSupported"] == true {
        data.completion_supported = true;
    }
}

fn apply_claude(data: &mut Metadata, input: &Value) {
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
    apply(data, &patch);
}

fn claude_transcript(path: &Path, session: &str) -> Option<(Option<String>, Option<String>)> {
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

fn claude_transcript_records(bytes: &[u8], session: &str) -> (Option<String>, Option<String>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_lifecycle_does_not_confuse_permissions_failures_or_subagents() {
        let mut metadata = Metadata::default();
        let mut event = |name: &str, session: &str, extra: Value| {
            let mut value = json!({"hook_event_name":name,"session_id":session});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            apply_claude(&mut metadata, &value);
            metadata.clone()
        };
        assert_eq!(event("SessionStart", "one", json!({})).status, "idle");
        let state = event("UserPromptSubmit", "one", json!({"prompt":"Fix\nthis bug"}));
        assert_eq!(state.status, "working");
        assert_eq!(state.prompt, "Fix this bug");
        assert_eq!(
            event("PermissionRequest", "one", json!({})).status,
            "waiting"
        );
        assert_eq!(event("Stop", "subagent", json!({})).status, "waiting");
        assert_eq!(event("PostToolUse", "one", json!({})).status, "working");
        assert_eq!(event("StopFailure", "one", json!({})).status, "error");
        assert_eq!(
            event("UserPromptSubmit", "one", json!({"prompt":"Retry"})).status,
            "working"
        );
        assert_eq!(event("Stop", "one", json!({})).status, "idle");
        let next = event("SessionStart", "two", json!({}));
        assert!(next.prompt.is_empty());
        assert_eq!(next.native_session, "two");
    }

    #[test]
    fn session_switch_clears_old_metadata_and_invalid_states_are_ignored() {
        let mut data = Metadata::default();
        apply(
            &mut data,
            &json!({"session":"one","title":"\u{1b}[31mOwn title\u{1b}[0m","prompt":"Task","model":"model-a","status":"working"}),
        );
        assert_eq!(data.title, "Own title");
        apply(
            &mut data,
            &json!({"status":"garbage","assistant":"Do not use as title"}),
        );
        assert_eq!(data.status, "working");
        assert_eq!(data.prompt, "Task");
        apply(&mut data, &json!({"session":"two","status":"idle"}));
        assert!(data.title.is_empty() && data.prompt.is_empty() && data.model.is_empty());
        assert_eq!(status("waiting"), Some(("WAITING", "◌ WAITING")));
        assert_eq!(status("unknown"), Some(("UNKNOWN", "? UNKNOWN")));
    }

    #[test]
    fn claude_child_hooks_compaction_and_model_switch_preserve_parent_state() {
        let mut data = Metadata::default();
        apply_claude(
            &mut data,
            &json!({"hook_event_name":"SessionStart","session_id":"own","model":"old"}),
        );
        apply_claude(
            &mut data,
            &json!({"hook_event_name":"PermissionRequest","session_id":"own"}),
        );
        for event in ["SessionStart", "Stop", "PreToolUse"] {
            apply_claude(
                &mut data,
                &json!({"hook_event_name":event,"session_id":"own","agent_id":"child"}),
            );
            assert_eq!(data.status, "waiting");
        }
        apply_claude(
            &mut data,
            &json!({"hook_event_name":"PostModelSwitch","session_id":"own","to_model":"new"}),
        );
        assert_eq!(data.model, "new");
        assert_eq!(data.status, "waiting");
        apply_claude(
            &mut data,
            &json!({"hook_event_name":"PostCompact","session_id":"own"}),
        );
        assert_eq!(data.status, "working");
        apply_claude(
            &mut data,
            &json!({"hook_event_name":"StopFailure","session_id":"own"}),
        );
        apply_claude(
            &mut data,
            &json!({"hook_event_name":"Stop","session_id":"own"}),
        );
        assert_eq!(data.status, "error");
        apply_claude(&mut data, &json!({"hook_event_name":"SessionStart"}));
        assert_eq!(data.native_session, "own");
    }

    #[test]
    fn claude_transcript_uses_only_complete_own_user_and_title_records() {
        let records = [
            json!({"type":"custom-title","sessionId":"own","customTitle":"Named task"}),
            json!({"type":"user","sessionId":"own","message":{"content":[{"type":"text","text":"My request"},{"type":"tool_result","content":"noise"}]}}),
            json!({"type":"assistant","message":{"content":"assistant noise"}}),
            json!({"type":"user","sessionId":"other","message":{"content":"other task"}}),
            json!({"type":"user","isSidechain":true,"message":{"content":"child task"}}),
            json!({"type":"user","isMeta":true,"message":{"content":"internal task"}}),
        ];
        let mut bytes = records.iter().map(|r| format!("{r}\n")).collect::<String>();
        bytes.push_str(r#"{"type":"custom-title","customTitle":"partial"}"#);
        let (title, prompt) = claude_transcript_records(bytes.as_bytes(), "own");
        assert_eq!(title.as_deref(), Some("Named task"));
        assert_eq!(prompt.as_deref(), Some("My request"));
    }

    #[test]
    fn process_identity_is_not_just_a_reused_pid() {
        assert!(start_time(std::process::id()).is_some());
        assert_eq!(start_time(u32::MAX), None);
        let own = Metadata { pid: std::process::id(), ..Default::default() };
        let parent = unsafe { libc::getppid() } as u32;
        assert!(owns_pane(&own, own.pid));
        assert!(owns_pane(&own, parent));
        assert!(!owns_pane(&own, u32::MAX));
        assert!(!owns_pane(&Metadata { pid: parent, ..Default::default() }, own.pid));
    }

    #[test]
    fn openclaw_gateway_liveness_expires_without_changing_other_native_adapters() {
        let mut data = Metadata {
            agent: "openclaw".into(),
            status: "working".into(),
            observed_at_ms: 1000,
            ..Default::default()
        };
        refresh_liveness(&mut data, 16000);
        assert_eq!(data.status, "working");
        refresh_liveness(&mut data, 16001);
        assert_eq!(data.status, "unknown");
        data.agent = "pi".into();
        data.status = "idle".into();
        refresh_liveness(&mut data, 60000);
        assert_eq!(data.status, "idle");
    }

    #[test]
    fn pi_completion_is_scoped_durable_and_cleared_by_activity_or_session_switch() {
        let mut data = Metadata {
            agent: "pi".into(),
            pid: 42,
            process_start: "123".into(),
            ..Default::default()
        };
        apply(
            &mut data,
            &json!({"session":"one","status":"idle","completionSupported":true}),
        );
        let completed = json!({"session":"one","status":"completed","completionTurn":"turn-one"});
        apply(&mut data, &completed);
        let id = data.completion_id.clone().unwrap();
        assert_eq!(id.len(), 64);
        let mut restored: Metadata =
            serde_json::from_slice(&serde_json::to_vec(&data).unwrap()).unwrap();
        apply(&mut restored, &completed);
        assert_eq!(restored.completion_id.as_deref(), Some(id.as_str()));
        apply(&mut data, &json!({"status":"waiting"}));
        assert!(data.completion_id.is_none());
        apply(
            &mut data,
            &json!({"session":"two","status":"completed","completionTurn":"turn-one"}),
        );
        assert_eq!(data.status, "unknown");
        assert!(!data.completion_supported);
        data.agent = "claude".into();
        data.completion_supported = true;
        apply(&mut data, &completed);
        assert!(data.completion_id.is_none());
        assert_eq!(data.status, "unknown");
    }

    #[test]
    fn custom_shell_commands_are_left_untouched() {
        for command in [
            "claude --settings custom.json",
            "claude && echo done",
            "claude --settings=custom.json",
            "wrapper claude",
            "claude '$HOME'",
            "pi --print 'hello'",
        ] {
            let agent = if command.starts_with("pi") {
                "pi"
            } else {
                "claude"
            };
            let launch = prepare("sd_term_test", agent, command);
            assert_eq!(launch.command, command);
            assert!(launch.path.is_none());
        }
    }

    #[test]
    fn direct_custom_launchers_receive_scoped_adapters_without_rewriting_wrappers() {
        let root = std::env::temp_dir().join(format!("sd-adapter-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for agent in ["claude", "pi", "opencode", "openclaw"] {
            let command = format!(
                "'/opt/tools/{agent}' {}",
                if agent == "openclaw" { "tui" } else { "" }
            );
            let launch =
                prepare_with_root("sd_term_test", "custom-test", &command, Some(root.clone()));
            let metadata = read(
                launch
                    .path
                    .as_ref()
                    .expect("custom direct launch has adapter"),
            )
            .unwrap();
            assert_eq!(metadata.agent, agent);
            assert_eq!(metadata.launcher, "custom-test");
            assert!(launch.command.contains("SD_HARNESS_PID"));
        }
        for command in [
            "'/opt/tools/wrapper' claude",
            "claude --settings custom.json",
            "custom-test",
            "bash -c claude",
        ] {
            let launch =
                prepare_with_root("sd_term_test", "custom-test", command, Some(root.clone()));
            assert_eq!(launch.command, command);
            assert!(launch.path.is_none());
        }
        fs::remove_dir_all(root).unwrap();
    }
}
