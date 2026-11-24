//! Launch options, hook registration, and conversation inspection for the Antigravity CLI.

use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const INLINE_SETTINGS: &str = r#"{"altScreenMode":"never","clearScrollbackOnResize":false}"#;

pub fn is_launcher(agent: &str) -> bool {
    matches!(agent, "agy" | "antigravity")
}

/// Recognize a direct interactive CLI invocation without changing shell wrappers.
pub fn interactive_args(command: &str) -> Option<Vec<String>> {
    if command.contains(['$', '`', '\n', ';', '|', '&', '<', '>', '#', '\\']) {
        return None;
    }
    let args = shlex::split(command)?;
    let executable = Path::new(args.first()?).file_name()?.to_str()?;
    if !is_launcher(executable)
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

/// Apply inline rendering to this launch, when the CLI supports session
/// overrides. A user's explicit settings and older CLIs keep their own mode.
pub fn launch_command(agent: &str, command: &str) -> String {
    with_inline_rendering(agent, command, supports_settings)
}

fn with_inline_rendering(
    agent: &str,
    command: &str,
    supports_settings: impl FnOnce(&str) -> bool,
) -> String {
    if !is_launcher(agent) && !agent.starts_with("custom-") {
        return command.into();
    }
    let Some(args) = interactive_args(command) else {
        return command.into();
    };
    if args
        .iter()
        .any(|arg| arg == "--settings" || arg.starts_with("--settings="))
        || !supports_settings(&args[0])
    {
        return command.into();
    }
    format!("{command} --settings '{INLINE_SETTINGS}'")
}

fn supports_settings(executable: &str) -> bool {
    // agy writes --help to stderr. Merging it this way lets the bounded
    // subprocess reader keep older or misidentified executables from blocking.
    let mut probe = Command::new("sh");
    probe
        .args([
            "-c",
            "exec \"$1\" --help 2>&1",
            "super-desktop-agy",
            executable,
        ])
        .stdin(Stdio::null());
    let Some(output) = crate::workspace_model::bounded_output(probe) else {
        return false;
    };
    let help = String::from_utf8_lossy(&output);
    let has_flag = |flag| {
        help.lines()
            .any(|line| line.split_whitespace().next() == Some(flag))
    };
    has_flag("--settings") && has_flag("--conversation")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookStatus {
    Connected,
    Missing(&'static str),
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".gemini/antigravity-cli"))
}

pub fn gemini_config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".gemini/config"))
}

pub fn transcript_for_conversation(id: &str) -> Option<PathBuf> {
    let home = home_dir()?;
    transcript_for_conversation_in(&home, id)
}

pub fn transcript_for_conversation_in(home: &Path, id: &str) -> Option<PathBuf> {
    if !crate::tmux::is_resumable_id(id) {
        return None;
    }
    let path = home
        .join("brain")
        .join(id)
        .join(".system_generated/logs/transcript.jsonl");
    path.is_file().then_some(path)
}

pub fn conversation_exists(id: &str) -> bool {
    home_dir().is_some_and(|h| conversation_exists_in(&h, id))
}

pub fn conversation_exists_in(home: &Path, id: &str) -> bool {
    if !crate::tmux::is_resumable_id(id) {
        return false;
    }
    if transcript_for_conversation_in(home, id).is_some()
        || home.join("conversations").join(format!("{id}.pb")).is_file()
    {
        return true;
    }
    let db = home.join("conversation_summaries.db");
    if !db.is_file() {
        return false;
    }
    let esc = crate::tmux::sql_escape(id);
    let sql = format!(
        "SELECT 1 FROM conversation_summaries WHERE conversation_id = '{esc}' LIMIT 1;"
    );
    crate::tmux::sqlite_query(&db, &sql).is_some_and(|out| out.trim() == "1")
}

pub fn summary_title(id: &str) -> Option<String> {
    let db = home_dir()?.join("conversation_summaries.db");
    summary_title_in(&db, id)
}

pub fn summary_title_in(db: &Path, id: &str) -> Option<String> {
    if !crate::tmux::is_resumable_id(id) || !db.is_file() {
        return None;
    }
    let esc = crate::tmux::sql_escape(id);
    let sql = format!(
        "SELECT COALESCE(title, '') FROM conversation_summaries WHERE conversation_id = '{esc}' LIMIT 1;"
    );
    let raw = crate::tmux::sqlite_query(db, &sql)?;
    let title = raw.trim();
    (!title.is_empty() && !crate::harness_record::is_placeholder_title("antigravity", title))
        .then(|| title.to_string())
}

pub fn prompt_history(id: &str, limit: usize) -> Vec<(String, Option<u64>)> {
    let Some(path) = transcript_for_conversation(id) else {
        return Vec::new();
    };
    let Some((records, _)) = crate::harness_record::antigravity_prompts(&path, 256 * 1024) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Option<u64>)> = records
        .into_iter()
        .map(|rec| {
            let ms = rec
                .at
                .as_deref()
                .and_then(crate::prompt_log::parse_iso)
                .and_then(|ms| u64::try_from(ms).ok());
            (rec.text, ms)
        })
        .collect();
    if limit > 0 && out.len() > limit {
        out.drain(..out.len() - limit);
    }
    out
}

pub fn latest_prompt(id: &str) -> Option<String> {
    prompt_history(id, 1).pop().map(|(p, _)| p)
}

pub fn hook_status() -> HookStatus {
    let Some(dir) = gemini_config_dir() else {
        return HookStatus::Missing("HOME directory is not set");
    };
    hook_status_in(&dir)
}

pub fn hook_status_in(config_dir: &Path) -> HookStatus {
    let path = config_dir.join("hooks.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        return HookStatus::Missing("Prompts & session tracking need the Antigravity hook");
    };
    let Ok(val) = serde_json::from_str::<Value>(&raw) else {
        return HookStatus::Missing("Could not read ~/.gemini/config/hooks.json");
    };
    let installed = val
        .get("super-desktop")
        .and_then(|g| g.get("PreInvocation"))
        .and_then(Value::as_array)
        .is_some_and(|arr| {
            arr.iter().any(|entry| {
                entry
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|cmd| cmd.contains("harness-event antigravity"))
            })
        });
    if installed {
        HookStatus::Connected
    } else {
        HookStatus::Missing("Prompts & session tracking need the Antigravity hook")
    }
}

pub fn connect_hooks() -> Result<(), String> {
    let config_dir = gemini_config_dir().ok_or_else(|| "HOME directory is not set".to_string())?;
    let exe = std::env::current_exe()
        .map(crate::harness_metadata::event_exe)
        .map_err(|e| format!("locate binary: {e}"))?;
    connect_hooks_in(&config_dir, &exe)
}

pub fn connect_hooks_in(config_dir: &Path, exe: &Path) -> Result<(), String> {
    fs::create_dir_all(config_dir)
        .map_err(|e| format!("create {}: {e}", config_dir.display()))?;
    let hooks_path = config_dir.join("hooks.json");
    let mut root = if hooks_path.exists() {
        let raw = fs::read_to_string(&hooks_path)
            .map_err(|e| format!("read {}: {e}", hooks_path.display()))?;
        if raw.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| format!("parse {}: {e}", hooks_path.display()))?
        }
    } else {
        json!({})
    };
    let Some(obj) = root.as_object_mut() else {
        return Err(format!("{} must contain a JSON object", hooks_path.display()));
    };
    let cmd = format!(
        "{} harness-event antigravity",
        crate::harness_metadata::shell_quote(&exe.to_string_lossy())
    );
    obj.insert(
        "super-desktop".into(),
        json!({
            "PreInvocation": [
                {
                    "type": "command",
                    "command": cmd,
                    "timeout": 5
                }
            ]
        }),
    );
    let mut formatted = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("serialize hooks.json: {e}"))?;
    formatted.push('\n');
    crate::harness_record::atomic_bytes(&hooks_path, formatted.as_bytes())
        .map_err(|e| format!("write {}: {e}", hooks_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_inline_rendering_is_launch_scoped_and_capability_gated() {
        let command = "'/path with spaces/agy' --conversation=123";
        let result = with_inline_rendering("antigravity", command, |exe| {
            assert_eq!(exe, "/path with spaces/agy");
            true
        });
        assert_eq!(
            shlex::split(&result).unwrap(),
            [
                "/path with spaces/agy",
                "--conversation=123",
                "--settings",
                r#"{"altScreenMode":"never","clearScrollbackOnResize":false}"#
            ]
        );
        assert_eq!(
            with_inline_rendering("antigravity", command, |_| false),
            command
        );
        assert!(
            with_inline_rendering("custom-test", "antigravity", |_| true).contains("--settings")
        );
    }

    #[test]
    fn antigravity_leaves_custom_settings_and_noninteractive_commands_alone() {
        for command in [
            "agy --settings custom.json",
            "agy --settings=custom.json",
            "agy --print='one shot'",
            "agy --prompt='alias'",
            "agy acp",
            "agy --help",
            "agy; echo done",
            "wrapper agy",
        ] {
            assert_eq!(
                with_inline_rendering("antigravity", command, |_| panic!("unexpected probe")),
                command
            );
        }
    }

    #[test]
    fn antigravity_hooks_connect_preserves_existing_keys_and_summary_title_reads_db() {
        let root = std::env::temp_dir().join(format!(
            "agy-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let cfg = root.join("config");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(
            cfg.join("hooks.json"),
            r#"{"herdr":{"PreInvocation":[{"command":"echo hi","timeout":10,"type":"command"}]}}"#,
        )
        .unwrap();

        assert!(matches!(hook_status_in(&cfg), HookStatus::Missing(_)));
        connect_hooks_in(&cfg, Path::new("/usr/bin/super-desktop-client")).unwrap();
        assert_eq!(hook_status_in(&cfg), HookStatus::Connected);

        let merged: Value =
            serde_json::from_str(&fs::read_to_string(cfg.join("hooks.json")).unwrap()).unwrap();
        assert!(merged.get("herdr").is_some(), "existing hooks must be kept");
        assert!(merged["super-desktop"]["PreInvocation"][0]["command"]
            .as_str()
            .unwrap()
            .contains("harness-event antigravity"));

        let db = root.join("conversation_summaries.db");
        let out = Command::new("sqlite3")
            .arg(&db)
            .arg("CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, title TEXT); INSERT INTO conversation_summaries VALUES ('conv-42', 'Refactor terminal layout');")
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            summary_title_in(&db, "conv-42").as_deref(),
            Some("Refactor terminal layout")
        );

        let _ = fs::remove_dir_all(&root);
    }
}
