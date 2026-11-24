//! Hermes Agent plugin status/installation and SQLite session/history readers.

use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const PLUGIN_NAME: &str = "super-desktop";
const PLUGIN_YAML: &str = "name: super-desktop\nversion: 1.0.0\ndescription: SUPER DESKTOP status, prompt, and title tracking\n";
const PLUGIN_PY: &str = include_str!("../assets/harness/hermes_plugin.py");

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    Connected,
    Missing(&'static str),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HermesSessionInfo {
    pub id: String,
    pub model: Option<String>,
    pub title: Option<String>,
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HERMES_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".hermes")))
}

pub fn db_path() -> Option<PathBuf> {
    let path = home_dir()?.join("state.db");
    path.is_file().then_some(path)
}

pub fn plugin_dir_in(hermes_home: &Path) -> PathBuf {
    hermes_home.join("plugins").join(PLUGIN_NAME)
}

pub fn plugin_status() -> PluginStatus {
    let Some(home) = home_dir() else {
        return PluginStatus::Missing("HOME directory is not set");
    };
    plugin_status_in(&home)
}

pub fn plugin_status_in(hermes_home: &Path) -> PluginStatus {
    let dir = plugin_dir_in(hermes_home);
    let yaml_ok = fs::read_to_string(dir.join("plugin.yaml")).ok().as_deref() == Some(PLUGIN_YAML);
    let py_ok = fs::read_to_string(dir.join("__init__.py")).ok().as_deref() == Some(PLUGIN_PY);
    if yaml_ok && py_ok {
        PluginStatus::Connected
    } else {
        PluginStatus::Missing("Status, prompts & titles need the Hermes plugin")
    }
}

pub fn connect_plugin() -> Result<(), String> {
    let home = home_dir().ok_or_else(|| "HOME directory is not set".to_string())?;
    connect_plugin_in(&home)
}

pub fn connect_plugin_in(hermes_home: &Path) -> Result<(), String> {
    let dir = plugin_dir_in(hermes_home);
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    crate::harness_record::atomic_bytes(&dir.join("plugin.yaml"), PLUGIN_YAML.as_bytes())
        .map_err(|e| format!("write plugin.yaml: {e}"))?;
    crate::harness_record::atomic_bytes(&dir.join("__init__.py"), PLUGIN_PY.as_bytes())
        .map_err(|e| format!("write __init__.py: {e}"))?;
    Ok(())
}

pub fn conversation_exists(id: &str) -> bool {
    let Some(db) = db_path() else {
        return false;
    };
    conversation_exists_in(&db, id)
}

pub fn conversation_exists_in(db: &Path, id: &str) -> bool {
    if !crate::tmux::is_resumable_id(id) || !db.is_file() {
        return false;
    }
    let esc = crate::tmux::sql_escape(id);
    let sql = format!("SELECT 1 FROM sessions WHERE id = '{esc}' LIMIT 1;");
    crate::tmux::sqlite_query(db, &sql).is_some_and(|out| out.trim() == "1")
}

pub fn session_info(id: &str) -> Option<HermesSessionInfo> {
    let db = db_path()?;
    session_info_in(&db, id)
}

pub fn session_info_in(db: &Path, id: &str) -> Option<HermesSessionInfo> {
    if !crate::tmux::is_resumable_id(id) || !db.is_file() {
        return None;
    }
    let esc = crate::tmux::sql_escape(id);
    let sql = format!(
        "SELECT id, COALESCE(model, '') AS model, COALESCE(title, '') AS title \
         FROM sessions WHERE id = '{esc}' LIMIT 1;"
    );
    let raw = crate::tmux::sqlite_query_as(db, &sql, "-json")?;
    let rows: Vec<Value> = serde_json::from_str(&raw).ok()?;
    let row = rows.into_iter().next()?;
    let non_empty = |k: &str| {
        row.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Some(HermesSessionInfo {
        id: id.to_string(),
        model: non_empty("model"),
        title: non_empty("title"),
    })
}

pub fn session_title(id: &str) -> Option<String> {
    let title = session_info(id)?.title?;
    (!crate::harness_record::is_placeholder_title("hermes", &title)).then_some(title)
}

pub fn prompts_for_session(id: &str, limit: usize) -> Vec<(String, Option<u64>)> {
    let Some(db) = db_path() else {
        return Vec::new();
    };
    prompts_for_session_in(&db, id, limit)
}

pub fn prompts_for_session_in(db: &Path, id: &str, limit: usize) -> Vec<(String, Option<u64>)> {
    if !crate::tmux::is_resumable_id(id) || !db.is_file() {
        return Vec::new();
    }
    let fetch_limit = limit.max(1).saturating_mul(3).min(500);
    let esc = crate::tmux::sql_escape(id);
    let sql = format!(
        "SELECT COALESCE(content, '') AS content, \
                CAST(COALESCE(timestamp, 0) * 1000 AS INTEGER) AS ts_ms \
         FROM messages \
         WHERE session_id = '{esc}' \
           AND role = 'user' \
           AND COALESCE(_compressed_summary, 0) = 0 \
           AND COALESCE(active, 1) = 1 \
         ORDER BY id DESC \
         LIMIT {fetch_limit};"
    );
    let Some(raw) = crate::tmux::sqlite_query_as(db, &sql, "-json") else {
        return Vec::new();
    };
    let Ok(rows) = serde_json::from_str::<Vec<Value>>(&raw) else {
        return Vec::new();
    };
    let mut prompts: Vec<(String, Option<u64>)> = rows
        .into_iter()
        .filter_map(|row| {
            let content = row.get("content")?.as_str()?;
            let clean = crate::harness_record::clean(content);
            if !crate::harness_record::is_user_prompt(&clean) {
                return None;
            }
            let ts = row
                .get("ts_ms")
                .and_then(Value::as_u64)
                .filter(|&ms| ms > 0);
            Some((clean, ts))
        })
        .collect();
    prompts.reverse();
    if limit > 0 && prompts.len() > limit {
        prompts.drain(..prompts.len() - limit);
    }
    prompts
}

pub fn latest_prompt(id: &str) -> Option<String> {
    prompts_for_session(id, 1).pop().map(|(p, _)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hermes-test-{name}-{}-{}",
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

    fn init_db(db: &Path) {
        let sql = "\
            CREATE TABLE sessions (id TEXT PRIMARY KEY, model TEXT, title TEXT);\
            CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, role TEXT, content TEXT, timestamp REAL, _compressed_summary INTEGER DEFAULT 0, active INTEGER DEFAULT 1);\
            INSERT INTO sessions (id, model, title) VALUES ('hermes-1', 'claude-sonnet-4', 'Build API gateway');\
            INSERT INTO messages (session_id, role, content, timestamp, _compressed_summary, active) VALUES ('hermes-1', 'user', '<task-notification>bg</task-notification>', 1700000000.0, 0, 1);\
            INSERT INTO messages (session_id, role, content, timestamp, _compressed_summary, active) VALUES ('hermes-1', 'user', 'Summary of earlier work', 1700000001.0, 1, 1);\
            INSERT INTO messages (session_id, role, content, timestamp, _compressed_summary, active) VALUES ('hermes-1', 'user', 'Inactive turn', 1700000002.0, 0, 0);\
            INSERT INTO messages (session_id, role, content, timestamp, _compressed_summary, active) VALUES ('hermes-1', 'user', 'Add rate limiting middleware', 1700000010.5, 0, 1);\
            INSERT INTO messages (session_id, role, content, timestamp, _compressed_summary, active) VALUES ('hermes-1', 'assistant', 'Done.', 1700000020.0, 0, 1);\
            INSERT INTO messages (session_id, role, content, timestamp, _compressed_summary, active) VALUES ('hermes-1', 'user', 'Write integration tests', 1700000030.0, 0, 1);\
        ";
        let out = Command::new("sqlite3")
            .arg(db)
            .arg(sql)
            .output()
            .expect("sqlite3 binary available for hermes tests");
        assert!(out.status.success(), "sqlite3 failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn hermes_plugin_connect_and_status_are_idempotent() {
        let home = temp_dir("plugin");
        assert!(matches!(plugin_status_in(&home), PluginStatus::Missing(_)));
        connect_plugin_in(&home).unwrap();
        assert_eq!(plugin_status_in(&home), PluginStatus::Connected);
        let dir = plugin_dir_in(&home);
        assert!(dir.join("plugin.yaml").is_file());
        assert!(dir.join("__init__.py").is_file());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn card_title_hermes_sqlite_filters_summaries_and_injected_turns() {
        let dir = temp_dir("db");
        let db = dir.join("state.db");
        init_db(&db);

        assert!(conversation_exists_in(&db, "hermes-1"));
        assert!(!conversation_exists_in(&db, "nonexistent"));
        assert_eq!(
            session_info_in(&db, "hermes-1"),
            Some(HermesSessionInfo {
                id: "hermes-1".into(),
                model: Some("claude-sonnet-4".into()),
                title: Some("Build API gateway".into()),
            })
        );
        let prompts = prompts_for_session_in(&db, "hermes-1", 10);
        assert_eq!(
            prompts,
            vec![
                ("Add rate limiting middleware".into(), Some(1_700_000_010_500)),
                ("Write integration tests".into(), Some(1_700_000_030_000)),
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
