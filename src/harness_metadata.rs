//! Native lifecycle observations scoped to one launch, never guessed from output.
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

// Recording (`harness-event`) lives in a GTK-free module shared with the small
// client binary; re-export it so existing callers keep their paths.
pub use crate::harness_record::{status, Metadata};
#[cfg_attr(not(test), allow(unused_imports))]
use crate::harness_record::{apply, apply_claude, claude_transcript_records};
use crate::harness_record::{atomic_bytes, atomic_write, now_ms, read, root, start_time};

const OPTION: &str = "@super_desktop_metadata";

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
    let Some(exe) = std::env::current_exe().ok().map(event_exe) else {
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

/// Hooks run `<exe> harness-event` once per agent tool call. Prefer the small
/// GTK-free client installed beside this binary, which records natively.
fn event_exe(exe: PathBuf) -> PathBuf {
    let client = exe.with_file_name("super-desktop-client");
    if client.is_file() {
        client
    } else {
        exe
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
    inspect_option(agent, &String::from_utf8(out.stdout).ok()?)
}

/// `inspect` for a value of the session's `@super_desktop_metadata` option
/// that the caller already read (e.g. in one batched `list-panes -a`).
pub fn inspect_option(agent: &str, option: &str) -> Option<Metadata> {
    if !native_agent(agent) && !agent.starts_with("custom-") {
        return None;
    }
    let path = Path::new(option.trim());
    if path.as_os_str().is_empty() {
        return None;
    }
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



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_completion_is_prompt_scoped_durable_and_fail_closed() {
        let mut data = Metadata { agent: "claude".into(), pid: 42, process_start: "launch".into(), ..Default::default() };
        let start = json!({"hook_event_name":"SessionStart","session_id":"own"});
        let prompt = json!({"hook_event_name":"UserPromptSubmit","session_id":"own","prompt":"Same text"});
        let stop = json!({"hook_event_name":"Stop","session_id":"own","stop_hook_active":false,"last_assistant_message":"Done"});
        apply_claude(&mut data, &start);
        assert!(data.completion_supported);
        apply_claude(&mut data, &stop);
        assert!(data.completion_id.is_none(), "Resumed history must not alert");
        apply_claude(&mut data, &prompt);
        apply_claude(&mut data, &stop);
        let first = data.completion_id.clone().unwrap();
        let mut data: Metadata = serde_json::from_slice(&serde_json::to_vec(&data).unwrap()).unwrap();
        apply_claude(&mut data, &stop);
        assert_eq!(data.completion_id.as_ref(), Some(&first));
        apply_claude(&mut data, &prompt);
        assert!(data.completion_id.is_none());
        for extra in [json!({"agent_id":"child"}), json!({"session_id":"other"})] {
            let mut event = stop.clone();
            event.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            apply_claude(&mut data, &event);
            assert!(data.completion_id.is_none());
            assert_eq!(data.status, "working");
        }
        apply_claude(&mut data, &stop);
        assert_ne!(data.completion_id.as_ref(), Some(&first));
        for extra in [json!({"stop_hook_active":true}), json!({"stop_hook_active":null}), json!({"last_assistant_message":" "}), json!({"last_assistant_message":null})] {
            apply_claude(&mut data, &prompt);
            let mut event = stop.clone();
            event.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            apply_claude(&mut data, &event);
            assert!(data.completion_id.is_none());
        }
        for event in ["StopFailure", "PermissionRequest", "SessionEnd"] {
            apply_claude(&mut data, &prompt);
            apply_claude(&mut data, &json!({"hook_event_name":event,"session_id":"own"}));
            apply_claude(&mut data, &stop);
            assert!(data.completion_id.is_none());
        }
        apply_claude(&mut data, &prompt);
        apply_claude(&mut data, &json!({"hook_event_name":"SessionStart","session_id":"new"}));
        apply_claude(&mut data, &stop);
        assert!(!data.claude_turn_active && data.completion_id.is_none());
        apply_claude(&mut data, &start);
        apply_claude(&mut data, &prompt);
        apply_claude(&mut data, &stop);
        assert!(data.completion_id.is_some());
        assert_ne!(data.completion_id.as_ref(), Some(&first), "Returning to a session must not reuse turn IDs");
    }

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
    fn opencode_completion_requires_capability_and_is_agent_scoped() {
        let mut data = Metadata {
            agent: "opencode".into(),
            ..Default::default()
        };
        apply(
            &mut data,
            &json!({"session":"root", "status":"completed", "completionTurn":"prompt"}),
        );
        assert_eq!(data.status, "unknown");
        apply(&mut data, &json!({"completionSupported":true}));
        let completed = json!({"status":"completed", "completionTurn":"prompt"});
        apply(&mut data, &completed);
        let id = data.completion_id.clone().unwrap();
        apply(&mut data, &completed);
        assert_eq!(data.completion_id.as_ref(), Some(&id));
        data.agent = "pi".into();
        apply(&mut data, &completed);
        assert_ne!(data.completion_id.as_ref(), Some(&id));
        apply(
            &mut data,
            &json!({"session":"other", "status":"completed", "completionTurn":"prompt"}),
        );
        assert!(data.completion_id.is_none());
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
        data.agent = "openclaw".into();
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
