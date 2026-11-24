use super::*;

pub(crate) fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

pub(in crate::tmux) fn opencode_db_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })?;
    let db = base.join("opencode/opencode.db");
    if db.exists() {
        Some(db)
    } else {
        None
    }
}

/// Size and modification time of the database and its WAL: opencode writes
/// through the WAL, and a checkpoint rewrites the database itself.
pub(in crate::tmux) type DbStamp = [(u64, i64, i64); 2];

pub(in crate::tmux) fn db_stamp(db: &std::path::Path) -> Option<DbStamp> {
    use std::os::unix::fs::MetadataExt;
    let stamp = |path: &std::path::Path| {
        std::fs::metadata(path)
            .map(|m| (m.len(), m.mtime(), m.mtime_nsec()))
            .unwrap_or((0, 0, 0))
    };
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    let main = std::fs::metadata(db).ok()?;
    Some([
        (main.len(), main.mtime(), main.mtime_nsec()),
        stamp(std::path::Path::new(&wal)),
    ])
}

/// `sqlite3` over a large database costs a process and a query per call, and
/// cards ask the same questions every second: an unchanged database (and
/// WAL) answers from memory.
pub(crate) fn sqlite_query(db: &std::path::Path, sql: &str) -> Option<String> {
    sqlite_query_as(db, sql, "-list")
}

/// `sqlite_query` with an explicit `sqlite3` output mode (`-list` or `-json`).
pub(crate) fn sqlite_query_as(db: &std::path::Path, sql: &str, mode: &'static str) -> Option<String> {
    type Cache = std::collections::HashMap<(std::path::PathBuf, String, &'static str), (DbStamp, Option<String>)>;
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (db.to_path_buf(), sql.to_string(), mode);
    let Some(before) = db_stamp(db) else {
        return sqlite_query_uncached(db, sql, mode);
    };
    if let Some((stamp, value)) = cache.lock().unwrap().get(&key) {
        if *stamp == before {
            return value.clone();
        }
    }
    let value = sqlite_query_uncached(db, sql, mode);
    // Only remember an answer that describes one unchanged database state.
    if db_stamp(db) == Some(before) {
        let mut cache = cache.lock().unwrap();
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, (before, value.clone()));
    }
    value
}

#[cfg(test)]
thread_local! {
    /// `sqlite3` processes started by this test thread.
    pub(in crate::tmux) static SQLITE_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(in crate::tmux) fn sqlite_query_uncached(db: &std::path::Path, sql: &str, mode: &str) -> Option<String> {
    #[cfg(test)]
    SQLITE_RUNS.with(|runs| runs.set(runs.get() + 1));
    command_stdout(Command::new("sqlite3").args([
        "-readonly",
        "-noheader",
        mode,
        db.to_str()?,
        sql,
    ]))
    .and_then(non_empty)
}

/// Read `/proc/<pid>/cmdline` as a space-joined string (`None` when the
/// process is gone or unreadable).
pub(in crate::tmux) fn read_cmdline(pid: u32) -> Option<String> {
    let args = crate::platform::process::cmdline(pid)?;
    (!args.is_empty()).then(|| args.join(" "))
}

/// Pure helper: pull `--session <id>` / `--session=<id>` out of a cmdline
/// string (testable).
pub fn extract_session_flag(cmdline: &str) -> Option<String> {
    let mut words = cmdline.split_whitespace();
    while let Some(w) = words.next() {
        if w == "--session" {
            return words.next().filter(|s| !s.is_empty()).map(|s| s.to_string());
        }
        if let Some(id) = w.strip_prefix("--session=") {
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Deterministic per-pane opencode session id: walk the pane's process tree
/// looking for an opencode process launched with `--session <id>`.
///
/// Reboot-resumed cards are relaunched exactly that way (see
/// `resolve_resume_command_with_session`), so this is ground truth for the
/// prompt typed INTO this pane's harness — it can never shift onto a
/// neighbour's session when other consoles open or close, unlike
/// chronological matching.
pub fn pane_opencode_session_flag(pane_pid: u32) -> Option<String> {
    if pane_pid == 0 {
        return None;
    }
    let mut stack = vec![pane_pid];
    let mut seen = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(cmd) = read_cmdline(pid) {
            let base = cmd
                .split_whitespace()
                .next()
                .and_then(|s| s.rsplit('/').next())
                .unwrap_or("");
            if base == "opencode" {
                if let Some(id) = extract_session_flag(&cmd) {
                    return Some(id);
                }
            }
        }
        stack.extend(get_direct_children(pid));
    }
    None
}

/// `session_name -> (agent_type, persisted agent_session_id)` for every card
/// in `state.json`. Lets the opencode matcher tell harness panes apart
/// without depending on process state.
pub(in crate::tmux) fn state_terminal_info() -> std::collections::HashMap<String, (String, Option<String>)> {
    let mut out = std::collections::HashMap::new();
    let path = crate::state::get_state_path();
    let content = std::fs::read_to_string(path).unwrap_or_default();
    if content.is_empty() {
        return out;
    }
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return out,
    };
    if let Some(terms) = v.get("terminals").and_then(|t| t.as_array()) {
        for t in terms {
            let name = t
                .get("session_name")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let agent = t
                .get("agent_type")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let sid = t
                .get("agent_session_id")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            out.insert(name, (agent, sid));
        }
    }
    out
}

/// Live `sd_term_*` panes that may own an opencode session: only panes whose
/// card is an opencode harness participate. Shell/reasonix/… cards must never
/// consume an opencode session in the chronological assignment — that stole a
/// neighbour's session and surfaced its prompt in the wrong card's title.
/// Panes unknown to `state.json` are kept (nothing proves they are not
/// opencode).
pub(in crate::tmux) fn opencode_panes_live() -> Option<Vec<(String, i64)>> {
    let panes: Vec<(String, i64)> = tmux_stdout(&["list-sessions", "-F", "#{session_name}|#{session_created}"])?
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(2, '|');
            let name = p.next()?.to_string();
            let t: i64 = p.next()?.parse().ok()?;
            if name.starts_with("sd_term_") {
                Some((name, t))
            } else {
                None
            }
        })
        .collect();
    let mut panes = keep_opencode_panes(&panes, &state_terminal_info());
    panes.sort_by_key(|(_, t)| *t);
    Some(panes)
}

/// All same-directory opencode sessions born at/after `not_before` (seconds),
/// oldest first.
pub(in crate::tmux) fn opencode_sessions_since(cwd: &str, not_before: i64) -> Option<Vec<(String, i64)>> {
    let db = opencode_db_path()?;
    let sql = format!(
        "SELECT id, time_created FROM session WHERE directory = '{}' AND time_created/1000 >= {} ORDER BY time_created;",
        sql_escape(cwd),
        not_before
    );
    let rows = sqlite_query(&db, &sql)?;
    let mut sessions: Vec<(String, i64)> = rows
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(2, '|');
            let id = p.next()?.to_string();
            let t_ms: i64 = p.next()?.parse().ok()?;
            Some((id, t_ms / 1000))
        })
        .collect();
    sessions.sort_by_key(|(_, t)| *t);
    Some(sessions)
}

/// `session_name -> pane_pid` for every live pane, in a single `tmux` call.
pub(in crate::tmux) fn live_pane_pids() -> Option<std::collections::HashMap<String, u32>> {
    let listing = tmux_stdout(&["list-panes", "-a", "-F", "#{session_name} #{pane_pid}"])?;
    let mut map = std::collections::HashMap::new();
    for line in listing.lines() {
        let mut p = line.split_whitespace();
        if let (Some(name), Some(pid)) = (p.next(), p.next()) {
            if name.starts_with("sd_term_") {
                if let Ok(pid) = pid.parse::<u32>() {
                    map.insert(name.to_string(), pid);
                }
            }
        }
    }
    Some(map)
}

/// Deterministic ownership map: `tmux pane -> opencode session id` for panes
/// whose own process tree carries `--session <id>` (reboot-resumed cards are
/// launched exactly that way). Ground truth that never shifts when other
/// consoles open or close.
pub(in crate::tmux) fn flag_claims(pids: &std::collections::HashMap<String, u32>) -> std::collections::HashMap<String, String> {
    pids.iter()
        .filter_map(|(name, pid)| {
            pane_opencode_session_flag(*pid).map(|id| (name.clone(), id))
        })
        .collect()
}

/// Pure helper: drop panes whose session is deterministically known (they are
/// satisfied and must not shift the chronological ranking of the rest), plus
/// their owned sessions from the candidates.
pub fn drop_determined_panes(
    panes: &[(String, i64)],
    sessions: &[(String, i64)],
    flags: &std::collections::HashMap<String, String>,
) -> (Vec<(String, i64)>, Vec<(String, i64)>) {
    let owned: std::collections::HashSet<&str> =
        flags.values().map(|s| s.as_str()).collect();
    let free_panes = panes
        .iter()
        .filter(|(name, _)| !flags.contains_key(name))
        .cloned()
        .collect();
    let free_sessions = sessions
        .iter()
        .filter(|(id, _)| !owned.contains(id.as_str()))
        .cloned()
        .collect();
    (free_panes, free_sessions)
}

/// Pure helper: keep only panes that may own an opencode session — panes
/// whose card is an opencode harness, plus panes unknown to `state.json`
/// (nothing proves they are not opencode). Shell/reasonix/… cards must never
/// consume an opencode session in the chronological assignment.
pub fn keep_opencode_panes(
    panes: &[(String, i64)],
    info: &std::collections::HashMap<String, (String, Option<String>)>,
) -> Vec<(String, i64)> {
    panes
        .iter()
        .filter(|(name, _)| {
            info.get(name)
                .map(|(agent, _)| agent == "opencode")
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// Pure heal policy for `resolve_own_opencode_id`: adopt heuristic match `q`
/// (born `q_birth`) over persisted `p` (born `p_birth`, if known) for a pane
/// born at `created` only when `q` was born strictly nearer to the pane —
/// the signature of a stale guess made before a neighbouring console closed.
/// A persisted id whose birth is unknown (pre-reboot ground truth outside
/// the live window, or garbage) is always kept: its DB lookup either hits
/// the card's own old session or yields nothing (and the title falls back to
/// the pane's own text — never another console's prompt).
pub fn should_adopt_heuristic_match(
    p_birth: Option<i64>,
    q_birth: i64,
    created: i64,
) -> bool {
    match p_birth {
        Some(pb) => (q_birth - created).abs() < (pb - created).abs(),
        None => false,
    }
}

/// Heuristic match for a pane with no deterministic id: chronological 1:1
/// assignment over live OPENCODE panes, minus panes/sessions deterministically
/// owned via `--session` flags. Returns the matched id plus its birth
/// (seconds) for the heal policy in `resolve_own_opencode_id`.
pub(in crate::tmux) fn heuristic_opencode_match(
    session_name: &str,
    flags: &std::collections::HashMap<String, String>,
) -> Option<(String, i64)> {
    let cwd = tmux_display(session_name, "#{pane_current_path}")?;
    let created: i64 = tmux_display(session_name, "#{session_created}")?
        .parse()
        .ok()?;
    let panes = opencode_panes_live()?;
    if !panes.iter().any(|(n, _)| n == session_name) {
        return None;
    }
    let first = panes.iter().map(|(_, t)| *t).min().unwrap_or(created);
    let sessions = opencode_sessions_since(&cwd, first - 30)?;
    let (free_panes, free_sessions) = drop_determined_panes(&panes, &sessions, flags);

    assign_opencode_sessions(&free_panes, &free_sessions)
        .into_iter()
        .find(|(pane, _)| pane == session_name)
        .and_then(|(_, sess)| sess)
        .filter(|(id, t)| {
            !id.is_empty() && (*t - created).abs() <= 600 && *t >= created - 30
        })
}

/// Birth (seconds) of one opencode session id, or `None` when it is not in
/// the local store (pre-reboot ground truth pruned from the query window, or
/// garbage that can safely never match).
pub(in crate::tmux) fn opencode_session_birth(session_id: &str) -> Option<i64> {
    let db = opencode_db_path()?;
    let sql = format!(
        "SELECT time_created FROM session WHERE id = '{}' LIMIT 1;",
        sql_escape(session_id)
    );
    let row = sqlite_query(&db, &sql)?;
    row.parse::<i64>().ok().map(|ms| ms / 1000)
}

/// Resolve the opencode session id OWNED by this tmux pane — i.e. the session
/// behind the prompt typed INTO this card's harness, never a neighbour's.
///
/// Layers, most-trusted first:
/// 1. `--session <id>` in the pane's own process tree (reboot-resumed cards
///    are launched exactly that way) — deterministic ground truth.
/// 2. Claims-aware chronological match over live OPENCODE panes (flag-owned
///    pane/session pairs leave the ranking entirely; other harness types
///    never participate).
/// 3. `persisted` fallback (pre-reboot ground truth older than the live
///    window).
///
/// Heal policy when the heuristic `q` disagrees with `persisted` `p`: adopt
/// `q` only when `q` was born strictly nearer to this pane than `p` was —
/// the signature of a stale guess made before a neighbouring console closed
/// and shifted the ranking. Otherwise keep `p`: it is either ground truth
/// older than any live session (reboot resume — a newer `q` there belongs to
/// somebody else) or an id whose DB lookup yields nothing, in which case the
/// title safely falls back to this pane's own text.
pub fn resolve_own_opencode_id(
    session_name: &str,
    persisted: Option<&str>,
) -> Option<String> {
    if let Some(metadata) = crate::harness_metadata::inspect(session_name, "opencode") {
        if !metadata.native_session.is_empty() { return Some(metadata.native_session); }
    }
    let flags = flag_claims(&live_pane_pids().unwrap_or_default());
    if let Some(id) = flags.get(session_name) {
        return Some(id.clone());
    }
    let clean_persisted = persisted.map(str::trim).filter(|s| !s.is_empty());
    let q = heuristic_opencode_match(session_name, &flags);
    match (clean_persisted, q) {
        (_, None) => clean_persisted.map(str::to_string),
        (None, Some((id, _))) => Some(id),
        (Some(p), Some((qid, _))) if qid == p => Some(p.to_string()),
        (Some(p), Some((qid, qb))) => {
            let created: i64 = tmux_display(session_name, "#{session_created}")?
                .parse()
                .ok()?;
            let pb = opencode_session_birth(p)?;
            if should_adopt_heuristic_match(Some(pb), qb, created) {
                Some(qid)
            } else {
                Some(p.to_string())
            }
        }
    }
}

/// Pure helper: chronological 1:1 assignment of tmux panes to opencode
/// sessions (both sorted oldest-first). Each pane claims the earliest
/// still-unclaimed session born at/after the pane (30s grace for clock
/// skew). Panes with no session left get None.
pub fn assign_opencode_sessions(
    panes: &[(String, i64)],
    sessions: &[(String, i64)],
) -> Vec<(String, Option<(String, i64)>)> {
    let mut used = vec![false; sessions.len()];
    panes
        .iter()
        .map(|(pane, created)| {
            let mut match_idx: Option<usize> = None;
            for (i, (_, s_created)) in sessions.iter().enumerate() {
                if !used[i] && *s_created >= *created - 30 {
                    match_idx = Some(i);
                    break;
                }
            }
            if let Some(i) = match_idx {
                used[i] = true;
                (pane.clone(), Some(sessions[i].clone()))
            } else {
                (pane.clone(), None)
            }
        })
        .collect()
}

/// The last prompt the user submitted to an opencode session (an already
/// resolved session id, so callers can cache the tmux-pane → opencode-session
/// mapping), as a title-friendly snippet. Reads opencode's local database:
/// synthetic and ignored parts and injected turns are skipped, exactly as in
/// the prompt history.
pub fn get_opencode_user_text_by_id(opencode_session_id: &str) -> Option<String> {
    opencode_last_prompt_in(&opencode_db_path()?, opencode_session_id)
}

pub(in crate::tmux) fn opencode_last_prompt_in(db: &std::path::Path, opencode_session_id: &str) -> Option<String> {
    // A few messages back, so a run of injected turns still finds the prompt.
    let json = sqlite_query_as(db, &opencode_prompts_sql(opencode_session_id, 8), "-json")?;
    let (text, _) = opencode_prompt_rows(&json).into_iter().next()?;
    let cleaned: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (cleaned.chars().count() >= 2).then(|| truncate_prompt_title(&cleaned))
}

pub(in crate::tmux) fn opencode_prompts_sql(opencode_session_id: &str, limit: usize) -> String {
    format!(
        "SELECT m.id AS id, m.time_created AS t, p.data AS d FROM message m JOIN part p ON p.message_id = m.id WHERE m.session_id = '{0}' AND m.id IN (SELECT id FROM message WHERE session_id = '{0}' AND data LIKE '%\"role\":\"user\"%' ORDER BY time_created DESC LIMIT {1}) AND p.data LIKE '%\"type\":\"text\"%' ORDER BY m.time_created DESC, m.id DESC, p.id ASC;",
        sql_escape(opencode_session_id),
        limit
    )
}

/// Every prompt submitted to an opencode session, newest first (at most
/// `limit` messages), with the message time in Unix milliseconds. Synthetic
/// and ignored parts (attached file contents, injected reminders) are skipped;
/// the text parts of one message are joined. Read fresh: the history panel
/// asks rarely and the multi-row answer is not worth caching.
pub fn get_opencode_user_prompts_by_id(opencode_session_id: &str, limit: usize) -> Option<Vec<(String, i64)>> {
    let db = opencode_db_path()?;
    let sql = opencode_prompts_sql(opencode_session_id, limit);
    let json = command_stdout(Command::new("sqlite3").args(["-readonly", "-json", db.to_str()?, &sql]))?;
    Some(opencode_prompt_rows(&json))
}

/// Pure helper: group `sqlite3 -json` rows (`id`, `t`, `d`) into prompts.
pub fn opencode_prompt_rows(json: &str) -> Vec<(String, i64)> {
    let rows: Vec<serde_json::Value> = serde_json::from_str(json.trim()).unwrap_or_default();
    let mut prompts: Vec<(String, String, i64)> = Vec::new();
    for row in rows {
        let (Some(id), Some(t), Some(data)) = (row["id"].as_str(), row["t"].as_i64(), row["d"].as_str()) else {
            continue;
        };
        let Ok(part) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if part["type"] != "text" || part["synthetic"] == true || part["ignored"] == true {
            continue;
        }
        let Some(text) = part["text"].as_str().map(str::trim).filter(|t| !t.is_empty()) else {
            continue;
        };
        match prompts.last_mut() {
            Some((last, joined, _)) if last == id => {
                joined.push('\n');
                joined.push_str(text);
            }
            _ => prompts.push((id.to_owned(), text.to_owned(), t)),
        }
    }
    prompts
        .into_iter()
        .filter(|(_, text, _)| crate::harness_record::is_user_prompt(text))
        .map(|(_, text, t)| (text, t))
        .collect()
}

pub fn get_opencode_title_by_id(session_id: &str) -> Option<String> {
    let title = sqlite_query(&opencode_db_path()?, &format!(
        "SELECT title FROM session WHERE id = '{}' LIMIT 1;", sql_escape(session_id)))?;
    opencode_db_title(&title)
}

/// A title read from OpenCode's database; its "New session - <timestamp>"
/// placeholder is no title, so the card shows the submitted prompt instead.
pub(crate) fn opencode_db_title(raw: &str) -> Option<String> {
    let title = strip_terminal_escapes(raw).split_whitespace().collect::<Vec<_>>().join(" ");
    if crate::harness_record::is_placeholder_title("opencode", &title) {
        return None;
    }
    (!title.is_empty()).then(|| title.chars().take(240).collect())
}

