//! A terminal's prompt history, newest first: the phone's
//! `GET /api/v1/harnesses/<id>/prompts` and the card header's history button.
//!
//! Codex (its own open rollout), Claude Code (its hooks' transcript) and
//! OpenCode (its database) keep every prompt with a time, including prompts
//! from before this desktop recorded anything. Every other harness, and every
//! prompt a native history no longer shows (Claude `/clear` starts another
//! transcript), comes from a per-terminal journal of submitted input
//! (`prompt_history::record`), kept under `~/.local/state/super-desktop/prompts`.
//! Only text a person submitted is listed (`harness_record::is_user_prompt`,
//! see "Card titles" in AGENTS.md).
use crate::harness_record::{is_user_prompt, PromptRecord};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
};

/// Prompts returned per request, and kept per journal.
pub const MAX_PROMPTS: usize = 200;
/// UTF-8 bytes of one prompt's text; longer text is cut and ends with `…`.
pub const MAX_TEXT_BYTES: usize = 8 * 1024;
/// Bytes of a native transcript or rollout read per request, from its end.
const NATIVE_TAIL: u64 = 16 * 1024 * 1024;
/// Journals kept; the least recently written are removed first.
const MAX_JOURNALS: usize = 256;
/// The same text recorded twice within this window is one submission
/// (for example the phone's send and the pane's own input tracking).
const DUPLICATE_MS: i64 = 5_000;
/// A journal entry matching a native prompt's text within this window is
/// the same submission.
const SAME_SUBMISSION_MS: i64 = 120_000;

#[derive(Debug, Default, PartialEq)]
pub struct History {
    /// Newest first; `at` is Unix milliseconds when known.
    pub prompts: Vec<(String, Option<i64>)>,
    pub source: &'static str,
    pub truncated: bool,
}

impl History {
    pub fn to_json(&self) -> Value {
        let prompts: Vec<Value> = self
            .prompts
            .iter()
            .map(|(text, at)| json!({"text": text, "at": at.and_then(iso)}))
            .collect();
        json!({"prompts": prompts, "source": self.source, "truncated": self.truncated})
    }
}

fn iso(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn parse_iso(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|at| at.timestamp_millis())
}

fn now_ms() -> i64 {
    crate::harness_record::now_ms() as i64
}

fn bounded(text: &str) -> String {
    let text = text.trim();
    if text.len() <= MAX_TEXT_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_TEXT_BYTES - '…'.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

fn same_text(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

// ---------- journal ----------

fn journal_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/state/super-desktop/prompts"))
}

/// Session names become file names: only our own `sd_term_*` shape.
fn valid_session(session: &str) -> bool {
    session.starts_with("sd_term_")
        && session.len() <= 128
        && session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Record one submitted prompt in `session`'s journal.
pub fn append(session: &str, text: &str) {
    // Tests keep the user's HOME; they exercise `append_in` on a scratch folder.
    if cfg!(test) {
        return;
    }
    if let Some(dir) = journal_dir() {
        let _ = append_in(&dir, session, text, now_ms());
    }
}

fn append_in(dir: &Path, session: &str, text: &str, at: i64) -> std::io::Result<()> {
    if !valid_session(session) || !is_user_prompt(text) {
        return Ok(());
    }
    let text = bounded(text);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let path = dir.join(format!("{session}.jsonl"));
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join(format!("{session}.lock")))?;
    // The desktop and the bridge both record; flock is released on close.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut entries = read_journal_file(&path);
    if entries
        .last()
        .is_some_and(|(last, last_at)| same_text(last, &text) && (at - last_at).abs() < DUPLICATE_MS)
    {
        return Ok(());
    }
    let line = serde_json::to_string(&json!({"at": at, "text": text}))? + "\n";
    if entries.len() + 1 >= 2 * MAX_PROMPTS {
        entries.push((text, at));
        let keep = &entries[entries.len() - MAX_PROMPTS..];
        let mut bytes = Vec::new();
        for (text, at) in keep {
            bytes.extend(serde_json::to_vec(&json!({"at": at, "text": text}))?);
            bytes.push(b'\n');
        }
        crate::harness_record::atomic_bytes(&path, &bytes)?;
    } else {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?
            .write_all(line.as_bytes())?;
    }
    drop(lock);
    prune(dir, MAX_JOURNALS);
    Ok(())
}

/// Oldest first.
fn read_journal_file(path: &Path) -> Vec<(String, i64)> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .split(b'\n')
        .map_while(Result::ok)
        .filter_map(|line| {
            let record: Value = serde_json::from_slice(&line).ok()?;
            let text = record["text"].as_str().filter(|text| is_user_prompt(text))?;
            Some((text.to_owned(), record["at"].as_i64()?))
        })
        .collect()
}

fn read_journal(session: &str) -> Vec<(String, i64)> {
    match journal_dir() {
        Some(dir) if valid_session(session) => read_journal_file(&dir.join(format!("{session}.jsonl"))),
        _ => Vec::new(),
    }
}

fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut journals: Vec<_> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    if journals.len() <= keep {
        return;
    }
    journals.sort();
    for (_, path) in &journals[..journals.len() - keep] {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(path.with_extension("lock"));
    }
}

// ---------- native histories ----------

fn native_records(records: Vec<PromptRecord>) -> Vec<(String, Option<i64>)> {
    records
        .into_iter()
        .map(|record| (record.text, record.at.as_deref().and_then(parse_iso)))
        .collect()
}

fn pane_pid(session: &str) -> Option<u32> {
    let out = std::process::Command::new(crate::tmux::tmux_bin())
        .args(["display-message", "-p", "-t", &format!("={session}:"), "#{pane_pid}"])
        .output()
        .ok()?;
    out.status.success().then_some(())?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// Claude's transcript: the path its hooks reported, else `<session>.jsonl`
/// in any project folder (the conversation may have moved folders).
fn claude_transcript(metadata: &crate::harness_record::Metadata) -> Option<PathBuf> {
    let session = &metadata.native_session;
    if session.is_empty() || !session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    if !metadata.transcript.is_empty() {
        return Some(PathBuf::from(&metadata.transcript));
    }
    let root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude")))?;
    fs::read_dir(root.join("projects"))
        .ok()?
        .flatten()
        .map(|project| project.path().join(format!("{session}.jsonl")))
        .find(|path| path.is_file())
}

/// Oldest-first native prompts and whether older ones were left unread.
fn native(session: &str, agent: &str, persisted: Option<&str>) -> Option<(Vec<(String, Option<i64>)>, bool, &'static str)> {
    match agent {
        "codex" => {
            let (records, cut) = crate::completion::user_prompts_for_pid(pane_pid(session)?, NATIVE_TAIL)?;
            Some((native_records(records), cut, "codex"))
        }
        "claude" => {
            let metadata = crate::harness_metadata::inspect(session, "claude")?;
            let path = claude_transcript(&metadata)?;
            let (records, cut) =
                crate::harness_record::claude_prompts(&path, &metadata.native_session, NATIVE_TAIL)?;
            Some((native_records(records), cut, "claude"))
        }
        "opencode" => {
            let id = crate::tmux::resolve_own_opencode_id(session, persisted)?;
            let mut rows = crate::tmux::get_opencode_user_prompts_by_id(&id, MAX_PROMPTS + 1)?;
            let cut = rows.len() > MAX_PROMPTS;
            rows.reverse();
            Some((rows.into_iter().map(|(text, at)| (text, Some(at))).collect(), cut, "opencode"))
        }
        _ => None,
    }
}

/// `session`'s prompts, newest first.
pub fn history(session: &str, agent: &str, persisted: Option<&str>) -> History {
    merge(native(session, agent, persisted), read_journal(session))
}

fn merge(native: Option<(Vec<(String, Option<i64>)>, bool, &'static str)>, journal: Vec<(String, i64)>) -> History {
    let (mut prompts, mut truncated, source) = native.unwrap_or((Vec::new(), false, "journal"));
    let journal_full = journal.len() >= MAX_PROMPTS;
    for (text, at) in journal {
        let known = prompts.iter().any(|(native, native_at)| {
            same_text(native, &text) && native_at.is_none_or(|native_at| (native_at - at).abs() < SAME_SUBMISSION_MS)
        });
        if !known {
            prompts.push((text, Some(at)));
        }
    }
    // Oldest first in each source; a stable sort keeps untimed entries in place
    // relative to their neighbours' order.
    prompts.sort_by_key(|(_, at)| *at);
    prompts.reverse();
    if prompts.len() > MAX_PROMPTS {
        prompts.truncate(MAX_PROMPTS);
        truncated = true;
    }
    for (text, _) in &mut prompts {
        *text = bounded(text);
    }
    History { prompts, source, truncated: truncated || journal_full }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sd-prompt-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn journal_keeps_submissions_in_order_and_drops_immediate_duplicates() {
        let dir = temp_dir("order");
        let session = "sd_term_1_a";
        append_in(&dir, session, "first prompt\nsecond line", 1_000).unwrap();
        append_in(&dir, session, "first   prompt second line", 2_000).unwrap();
        append_in(&dir, session, "<task-notification>x</task-notification>", 3_000).unwrap();
        append_in(&dir, session, "   ", 3_500).unwrap();
        append_in(&dir, session, "next", 4_000).unwrap();
        append_in(&dir, session, "next", 20_000).unwrap();
        let entries = read_journal_file(&dir.join(format!("{session}.jsonl")));
        assert_eq!(
            entries,
            vec![
                ("first prompt\nsecond line".to_string(), 1_000),
                ("next".to_string(), 4_000),
                ("next".to_string(), 20_000)
            ]
        );
        assert!(append_in(&dir, "../escape", "x", 1).is_ok());
        assert!(!dir.join("../escape.jsonl").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn journal_is_bounded_and_old_journals_are_pruned() {
        let dir = temp_dir("bounded");
        let session = "sd_term_2_b";
        for i in 0..(2 * MAX_PROMPTS + 5) as i64 {
            append_in(&dir, session, &format!("prompt {i}"), i * 10_000).unwrap();
        }
        let entries = read_journal_file(&dir.join(format!("{session}.jsonl")));
        assert!(entries.len() <= 2 * MAX_PROMPTS && entries.len() >= MAX_PROMPTS);
        assert_eq!(entries.last().unwrap().0, format!("prompt {}", 2 * MAX_PROMPTS + 4));
        for i in 0..4 {
            fs::write(dir.join(format!("sd_term_old_{i}.jsonl")), b"").unwrap();
        }
        prune(&dir, 3);
        let left = fs::read_dir(&dir).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "jsonl")).count();
        assert_eq!(left, 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_prompts_are_cut_on_a_character_boundary() {
        let text = "й".repeat(MAX_TEXT_BYTES);
        let cut = bounded(&text);
        assert!(cut.len() <= MAX_TEXT_BYTES && cut.ends_with('…'));
    }

    #[test]
    fn native_history_merges_with_the_journal_newest_first() {
        let native = vec![
            ("old native".to_string(), Some(1_000)),
            ("fix the bug".to_string(), Some(50_000)),
        ];
        let journal = vec![
            ("fix  the bug".to_string(), 49_000),
            ("after /clear".to_string(), 90_000),
        ];
        let history = merge(Some((native, false, "claude")), journal);
        assert_eq!(history.source, "claude");
        assert_eq!(
            history.prompts,
            vec![
                ("after /clear".to_string(), Some(90_000)),
                ("fix the bug".to_string(), Some(50_000)),
                ("old native".to_string(), Some(1_000))
            ]
        );
        let json = history.to_json();
        assert_eq!(json["prompts"][0]["at"], "1970-01-01T00:01:30.000Z");
        assert_eq!(json["truncated"], false);
    }

    #[test]
    fn without_native_history_the_journal_answers_and_reports_truncation() {
        let journal: Vec<_> = (0..MAX_PROMPTS as i64).map(|i| (format!("p{i}"), i)).collect();
        let history = merge(None, journal);
        assert_eq!(history.source, "journal");
        assert_eq!(history.prompts.len(), MAX_PROMPTS);
        assert_eq!(history.prompts[0].0, format!("p{}", MAX_PROMPTS - 1));
        assert!(history.truncated);
        assert_eq!(merge(None, Vec::new()).to_json(), json!({"prompts": [], "source": "journal", "truncated": false}));
    }

    #[test]
    fn native_timestamps_parse_with_and_without_fractions() {
        assert_eq!(parse_iso("2026-10-07T09:12:03Z"), Some(1_791_364_323_000));
        assert_eq!(parse_iso("2026-10-07T09:12:03.250Z"), Some(1_791_364_323_250));
        assert_eq!(parse_iso("2026-10-07T12:12:03+03:00"), Some(1_791_364_323_000));
        assert_eq!(parse_iso("yesterday"), None);
    }
}
