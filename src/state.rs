use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// User-selectable scale for the full-width top dock. Large deliberately
/// matches the toolbar's original control sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TopBarSize {
    Small,
    Medium,
    Large,
}

impl Default for TopBarSize {
    fn default() -> Self {
        Self::Large
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteData {
    pub id: String,
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub color: String,
    pub updated_at: f64,
    /// Group color tag: 0 = none, 1..=8 = palette index (see crate::tag).
    #[serde(default)]
    pub tag: u8,
}

fn default_term_width() -> i32 {
    380
}

fn default_term_height() -> i32 {
    240
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalData {
    pub id: String,
    pub session_name: String,
    pub agent_type: String,
    pub command: String,
    pub x: i32,
    pub y: i32,
    #[serde(default = "default_term_width")]
    pub width: i32,
    #[serde(default = "default_term_height")]
    pub height: i32,
    #[serde(default = "default_term_width")]
    pub restored_width: i32,
    #[serde(default = "default_term_height")]
    pub restored_height: i32,
    #[serde(default)]
    pub iconified: bool,
    /// Where the 128×128 icon form sits, kept separate from `x`/`y` (the
    /// expanded card position) so minimizing returns the card to its own
    /// remembered spot and restoring returns it to the card position.
    /// `None` = the icon was never moved yet → fall back to `x`/`y`.
    #[serde(default)]
    pub icon_x: Option<i32>,
    #[serde(default)]
    pub icon_y: Option<i32>,
    pub created_at: f64,
    /// Group color tag: 0 = none, 1..=8 = palette index (see crate::tag).
    #[serde(default)]
    pub tag: u8,
    /// Persisted agent-side session id (e.g. opencode `ses_...`).
    /// Used on reboot to resume THIS card's conversation with
    /// `opencode --session <id>` instead of `--continue` (which would make
    /// every card share the single latest session for the cwd).
    #[serde(default)]
    pub agent_session_id: Option<String>,
    /// Working directory this card's harness was launched in, captured from
    /// the top bar's workspace field at creation time.
    ///
    /// Stored per card, not read from the field at resume time, because every
    /// harness scopes its own resume/history lookup to the cwd (`claude
    /// --continue`, `opencode --continue`, `codex resume --last`, reasonix
    /// sessions): a card restored after a daemon restart must come back in the
    /// directory it was working in, whatever the field says by then.
    ///
    /// `None` = a card created before this field existed → the app-wide
    /// folder, i.e. the pre-existing behaviour.
    #[serde(default)]
    pub workspace_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    pub notes: Vec<NoteData>,
    pub terminals: Vec<TerminalData>,
    #[serde(default)]
    pub custom_harnesses: Vec<crate::custom_harness::CustomHarness>,
    /// Terminal card IDs, back to front. Missing legacy entries are appended
    /// in creation order; notes retain their existing independent stacking.
    #[serde(default)]
    pub terminal_order: Vec<String>,
    /// Harness keys (see `tmux::HARNESS_KEYS`) the user wants as launch
    /// buttons in the overlay's top bar, chosen in the ⚙ Settings panel.
    ///
    /// `None` = never configured → every built-in harness detected on this
    /// machine and every available custom launcher is shown. Older state files
    /// (written before this field existed) deserialize to.
    #[serde(default)]
    pub visible_harnesses: Option<Vec<String>>,
    /// Key combination that shows/hides the overlay, in Hyprland's spelling
    /// ("SUPER + SHIFT + K"), recorded in the ⚙ Settings panel.
    ///
    /// `None` = never changed → `crate::shortcut::DEFAULT_COMBO`, which is also
    /// what older state files (written before this field existed) deserialize
    /// to. The binding Hyprland actually acts on lives in
    /// `~/.config/hypr/bindings.lua`; this is the copy the UI reads.
    #[serde(default)]
    pub toggle_shortcut: Option<String>,
    /// Directory new harness sessions start in — the top bar's workspace
    /// field. `None` = never set → the home directory.
    ///
    /// On a machine with `$HOME` as the default, every harness is started at
    /// `~`, and `~` is an ancestor of every project below it: Reasonix's
    /// workspace write lease keys on the process cwd, so a `~`-rooted session
    /// collides with every project session. Pointing this at the project
    /// folder is what keeps parallel harnesses from blocking each other.
    #[serde(default)]
    pub workspace_dir: Option<String>,
    /// Folders used before, newest first — the combobox history under the
    /// field. Capped at [`RECENT_DIRS_MAX`] and deduplicated by
    /// [`push_recent_dir`].
    #[serde(default)]
    pub recent_dirs: Vec<String>,
    /// Every folder ever used for a harness, newest first. Unlike
    /// `recent_dirs`, this is not the visible dropdown: it is the persistent
    /// search index used when the user types any part of a folder name.
    #[serde(default)]
    pub used_dirs: Vec<String>,
    /// Scale of the full-width top dock. Missing in older state files means
    /// Large, preserving the size those users already had.
    #[serde(default)]
    pub top_bar_size: TopBarSize,
    /// Prevent suspend while external power is connected. Off by default.
    #[serde(default)]
    pub sleep_lock_on_ac: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            notes: vec![NoteData {
                id: "welcome_note".to_string(),
                text: "✨ Welcome to SUPER DESKTOP (Rust Edition)!\n\n• Shortcut: SUPER + SHIFT + Q to show / hide (change it in ⚙ Settings).\n• Drag: Grab any header to reposition smoothly!\n• Click 📝 + Note in the top bar to create a new sticky note.\n• Double-click a terminal card to expand it to 80% inside the overlay.\n• Double-click the header (or 🗕) to collapse it back.\n• ⚙ Settings picks the toggle shortcut and which harnesses show up in the top bar.\n• Built in Rust for maximum 240Hz responsiveness.".to_string(),
                x: 80,
                y: 140,
                width: 300,
                height: 230,
                color: "omarchy".to_string(),
                updated_at: 0.0,
                tag: 0,
            }],
            terminals: Vec::new(),
            custom_harnesses: Vec::new(),
            terminal_order: Vec::new(),
            visible_harnesses: None,
            toggle_shortcut: None,
            workspace_dir: None,
            recent_dirs: Vec::new(),
            used_dirs: Vec::new(),
            top_bar_size: TopBarSize::Large,
            sleep_lock_on_ac: false,
        }
    }
}

/// The user's home directory, as an absolute path. Fallback: `/tmp`, matching
/// the state-file fallback below.
pub fn home_dir() -> PathBuf {
    get_home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn home_dir_string() -> String {
    home_dir().to_string_lossy().into_owned()
}

/// Shorten `/home/me/GitHub/super-desktop` to `~/GitHub/super-desktop` for
/// display only — never for storage, so the state file stays machine-readable.
pub fn display_dir(path: &str) -> String {
    let home = home_dir_string();
    match path.strip_prefix(&home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// How many folders the top bar's open dropdown shows directly.
pub const RECENT_DIRS_MAX: usize = 7;

/// The folder new harness sessions start in: the configured one when it is
/// still a usable directory, otherwise the home directory.
///
/// A stored folder can disappear (deleted checkout, unmounted drive); falling
/// back to `~` keeps `add-term` working instead of failing to create a session.
pub fn effective_workspace_dir(state: &AppState) -> String {
    state
        .workspace_dir
        .as_deref()
        .and_then(clean_dir)
        .unwrap_or_else(home_dir_string)
}

/// `Some(absolute directory)` when `input` names one, else `None`.
///
/// Accepts what a person types: a leading `~`, surrounding spaces, a trailing
/// slash. Symlinks are resolved, so two spellings of the same folder share one
/// combobox entry (and one workspace lease).
pub fn clean_dir(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let expanded = match trimmed {
        "~" => home_dir_string(),
        _ => match trimmed.strip_prefix("~/") {
            Some(rest) => format!("{}/{}", home_dir_string(), rest),
            None => trimmed.to_string(),
        },
    };
    let path = PathBuf::from(&expanded);
    if !path.is_dir() {
        return None;
    }
    Some(
        fs::canonicalize(&path)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned(),
    )
}

/// Record `dir` as the most recently used folder: move it to the front,
/// drop any duplicate, and trim the tail to [`RECENT_DIRS_MAX`].
pub fn push_recent_dir(recent: &mut Vec<String>, dir: &str) {
    recent.retain(|d| d != dir);
    recent.insert(0, dir.to_string());
    recent.truncate(RECENT_DIRS_MAX);
}

/// Remember a folder for future substring autocomplete. This history is kept
/// independently from the seven-row dropdown so an older project remains
/// discoverable after it falls out of the recent list.
pub fn push_used_dir(used: &mut Vec<String>, dir: &str) {
    used.retain(|d| d != dir);
    used.insert(0, dir.to_string());
}

/// Record one workspace in both the short dropdown and the complete search
/// history.
pub fn remember_workspace_dir(state: &mut AppState, dir: &str) {
    push_recent_dir(&mut state.recent_dirs, dir);
    push_used_dir(&mut state.used_dirs, dir);
}

/// Upgrade history written by older versions. Capture every formerly recent,
/// current, or still-open card directory before trimming the visible list to
/// seven, so an update never makes an older workspace undiscoverable.
fn normalize_workspace_history(state: &mut AppState) -> bool {
    let before_recent = state.recent_dirs.clone();
    let before_used = state.used_dirs.clone();
    let mut known = state.recent_dirs.clone();
    if let Some(dir) = state.workspace_dir.clone() {
        known.push(dir);
    }
    known.extend(
        state
            .terminals
            .iter()
            .filter_map(|terminal| terminal.workspace_dir.clone()),
    );
    for dir in known {
        if !state.used_dirs.iter().any(|used| used == &dir) {
            state.used_dirs.push(dir);
        }
    }
    state.recent_dirs.truncate(RECENT_DIRS_MAX);
    state.recent_dirs != before_recent || state.used_dirs != before_used
}

/// Repair missing, duplicate and deleted IDs without moving valid entries.
pub fn normalize_terminal_order(state: &mut AppState) -> bool {
    let before = state.terminal_order.clone();
    let known: std::collections::HashSet<_> = state.terminals.iter().map(|t| t.id.as_str()).collect();
    let mut seen = std::collections::HashSet::new();
    state.terminal_order.retain(|id| known.contains(id.as_str()) && seen.insert(id.clone()));
    for card in &state.terminals {
        if seen.insert(card.id.clone()) { state.terminal_order.push(card.id.clone()); }
    }
    before != state.terminal_order
}

/// What the workspace field's text means: [`clean_dir`] for an absolute or
/// `~` path, plus the two convenient readings of a bare name — relative to the
/// folder in use, then relative to the home directory. So `super-desktop`,
/// `GitHub/super-desktop` and `~/GitHub/super-desktop` all resolve.
///
/// `None` = no such folder anywhere, which the field reports instead of
/// silently keeping the old one.
pub fn resolve_workspace_input(input: &str, current: &str) -> Option<String> {
    if let Some(dir) = clean_dir(input) {
        return Some(dir);
    }
    if input.trim().starts_with('/') {
        return None;
    }
    for base in [current, &home_dir_string()] {
        if let Some(dir) = clean_dir(&format!("{}/{input}", base.trim_end_matches('/'))) {
            return Some(dir);
        }
    }
    None
}

fn get_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn get_state_path() -> PathBuf {
    let mut path = get_home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    path.push(".config");
    path.push("super-desktop");
    let _ = fs::create_dir_all(&path);
    path.push("state.json");
    path
}

pub fn load_state() -> AppState {
    let path = get_state_path();
    if path.exists() {
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(mut state) = serde_json::from_str::<AppState>(&content) {
                let changed_history = normalize_workspace_history(&mut state);
                let changed_order = normalize_terminal_order(&mut state);
                if changed_history || changed_order {
                    // Persist the migration immediately. Otherwise the eighth
                    // old dropdown entry would only live in memory and could
                    // be lost if the daemon exits before another UI change.
                    save_state(&state);
                }
                return state;
            }
        }
    }
    let default_state = fresh_install_state(&crate::tmux::detect_harnesses());
    save_state(&default_state);
    default_state
}

/// Save the initial choice explicitly, so rescanning does not grow the toolbar
/// and existing installations (including legacy `None` selections) keep theirs.
fn fresh_install_state(detected: &[crate::tmux::HarnessInfo]) -> AppState {
    AppState {
        visible_harnesses: Some(
            detected.iter().take(3).map(|h| h.key.to_string()).collect(),
        ),
        ..AppState::default()
    }
}

pub fn save_state(state: &AppState) {
    save_state_async(state.clone());
    flush_state_saves();
}

static STATE_WRITER: OnceLock<StateWriter> = OnceLock::new();

/// The caller snapshots the state; serialization and disk I/O use one worker.
/// Pending snapshots coalesce, so a burst never creates threads or a backlog
/// of obsolete writes. The single writer also prevents older saves winning
/// the rename race and overwriting newer edits.
pub fn save_state_async(state: AppState) {
    STATE_WRITER
        .get_or_init(|| StateWriter::new(get_state_path()))
        .submit(state);
}

/// Only used at shutdown and by the synchronous initialization path.
pub fn flush_state_saves() {
    if let Some(writer) = STATE_WRITER.get() {
        if let Err(error) = writer.flush() {
            eprintln!("SUPER DESKTOP: saving state failed: {error}");
        }
    }
}

#[derive(Default)]
struct SaveQueue {
    pending: Option<(u64, AppState)>,
    submitted: u64,
    completed: u64,
    error: Option<String>,
    closed: bool,
}

struct StateWriter {
    queue: Arc<(Mutex<SaveQueue>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StateWriter {
    fn new(path: PathBuf) -> Self {
        Self::with_writer(move |state| {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            // A different process loading state must not share our temp file.
            let temp = path.with_extension(format!("{}.tmp", std::process::id()));
            let file = fs::OpenOptions::new().write(true).create(true).truncate(true)
                .mode(0o600).open(&temp)?;
            // A stale temp file from an older build may have wider permissions.
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            let mut file = std::io::BufWriter::new(file);
            serde_json::to_writer_pretty(&mut file, state)?;
            file.flush()?;
            fs::rename(temp, &path)
        })
    }

    fn with_writer(
        mut write: impl FnMut(&AppState) -> std::io::Result<()> + Send + 'static,
    ) -> Self {
        let queue = Arc::new((Mutex::new(SaveQueue::default()), Condvar::new()));
        let worker_queue = Arc::clone(&queue);
        let thread = std::thread::Builder::new()
            .name("state-writer".into())
            .spawn(move || {
                let (lock, ready) = &*worker_queue;
                loop {
                    let mut queue = lock.lock().unwrap();
                    while queue.pending.is_none() && !queue.closed {
                        queue = ready.wait(queue).unwrap();
                    }
                    let Some((generation, state)) = queue.pending.take() else {
                        break;
                    };
                    drop(queue);
                    let error = write(&state).err().map(|e| e.to_string());
                    if let Some(error) = &error {
                        eprintln!("SUPER DESKTOP: saving state failed: {error}");
                    }
                    let mut queue = lock.lock().unwrap();
                    queue.completed = generation;
                    queue.error = error;
                    ready.notify_all();
                }
            })
            .expect("start state writer");
        Self {
            queue,
            thread: Some(thread),
        }
    }

    fn submit(&self, state: AppState) {
        let (lock, ready) = &*self.queue;
        let mut queue = lock.lock().unwrap();
        queue.submitted += 1;
        queue.pending = Some((queue.submitted, state));
        ready.notify_all();
    }

    fn flush(&self) -> Result<(), String> {
        let (lock, ready) = &*self.queue;
        let mut queue = lock.lock().unwrap();
        let target = queue.submitted;
        while queue.completed < target {
            queue = ready.wait(queue).unwrap();
        }
        queue.error.clone().map_or(Ok(()), Err)
    }
}

impl Drop for StateWriter {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        lock.lock().unwrap().closed = true;
        ready.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn saves_coalesce_and_shutdown_flushes_the_latest_snapshot() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let saved = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&saved);
        let writer = StateWriter::with_writer(move |state| {
            if output.lock().unwrap().is_empty() {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            output.lock().unwrap().push(state.notes[0].text.clone());
            Ok(())
        });
        let mut state = AppState::default();
        state.notes[0].text = "first".into();
        writer.submit(state.clone());
        started_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        for n in 0..1000 {
            state.notes[0].text = n.to_string();
            writer.submit(state.clone());
        }
        release_tx.send(()).unwrap();
        writer.flush().unwrap();
        assert_eq!(*saved.lock().unwrap(), ["first", "999"]);
        state.notes[0].text = "on shutdown".into();
        writer.submit(state);
        drop(writer);
        assert_eq!(saved.lock().unwrap().last().unwrap(), "on shutdown");
    }

    #[test]
    fn state_writer_publishes_complete_json_and_reports_write_errors() {
        let path = env::temp_dir().join(format!("sd-save-{}.json", std::process::id()));
        let writer = StateWriter::new(path.clone());
        let mut state = AppState::default();
        for n in 0..100 {
            state.notes[0].text = format!("{n}: {}", "🦀".repeat(4096));
            writer.submit(state.clone());
        }
        writer.flush().unwrap();
        let read: AppState = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(read.notes[0].text, state.notes[0].text);
        drop(writer);
        fs::remove_file(path).unwrap();

        let writer = StateWriter::with_writer(|_| Err(std::io::Error::other("disk full")));
        writer.submit(state);
        assert_eq!(writer.flush().unwrap_err(), "disk full");
    }

    #[test]
    fn test_state_without_visible_harnesses_reads_as_unconfigured() {
        // state.json written before the ⚙ settings panel existed has no such
        // key; it must deserialize to `None` (= "show everything detected")
        // rather than dropping the user's notes/terminals or all buttons.
        let legacy = r#"{
            "notes": [],
            "terminals": [{
                "id": "sd_term_1_aa",
                "session_name": "sd_term_1_aa",
                "agent_type": "claude",
                "command": "/usr/bin/claude",
                "x": 0, "y": 0, "created_at": 1.0
            }]
        }"#;
        let state: AppState = serde_json::from_str(legacy).expect("legacy state must load");
        assert_eq!(state.visible_harnesses, None);
        assert_eq!(state.terminals.len(), 1);
        // And it round-trips: the panel writes the chosen keys explicitly.
        let saved = serde_json::to_string(&state).unwrap();
        assert!(saved.contains("\"visible_harnesses\":null"), "got: {saved}");

        let mut picked = state.clone();
        picked.visible_harnesses = Some(vec!["claude".to_string()]);
        let reloaded: AppState =
            serde_json::from_str(&serde_json::to_string(&picked).unwrap()).unwrap();
        assert_eq!(reloaded.visible_harnesses, Some(vec!["claude".to_string()]));
    }

    #[test]
    fn test_default_state_starts_unconfigured() {
        assert_eq!(AppState::default().visible_harnesses, None);
        assert_eq!(AppState::default().top_bar_size, TopBarSize::Large);
    }

    #[test]
    fn fresh_install_selects_at_most_three_detected_harnesses() {
        let detected: Vec<_> = ["claude", "codex", "opencode", "gemini", "shell"]
            .into_iter()
            .map(|key| crate::tmux::HarnessInfo {
                key,
                name: key,
                icon: "",
                command: key.to_string(),
            })
            .collect();
        for count in 0..=detected.len() {
            let state = fresh_install_state(&detected[..count]);
            let expected: Vec<_> = detected[..count.min(3)]
                .iter().map(|h| h.key.to_string()).collect();
            assert_eq!(state.visible_harnesses, Some(expected.clone()));
            let saved = serde_json::to_string(&state).unwrap();
            let restored: AppState = serde_json::from_str(&saved).unwrap();
            assert_eq!(restored.visible_harnesses, Some(expected));
        }
    }

    #[test]
    fn test_legacy_state_leaves_workspace_unset() {
        // A state.json written before the workspace field existed has neither
        // key: it must load with the pre-existing behaviour (start harnesses
        // in $HOME) instead of failing to parse and silently resetting the
        // user's notes and terminals to the welcome defaults.
        let legacy = r#"{
            "notes": [],
            "terminals": [{
                "id": "sd_term_9_ff",
                "session_name": "sd_term_9_ff",
                "agent_type": "reasonix",
                "command": "reasonix code",
                "x": 0, "y": 0, "created_at": 1.0
            }],
            "toggle_shortcut": "SUPER + SHIFT + Q"
        }"#;
        let state: AppState = serde_json::from_str(legacy).expect("legacy state must load");
        assert_eq!(state.top_bar_size, TopBarSize::Large);
        assert_eq!(state.workspace_dir, None);
        assert!(state.recent_dirs.is_empty());
        assert!(state.used_dirs.is_empty());
        assert_eq!(state.terminals.len(), 1);
        assert_eq!(state.terminals[0].workspace_dir, None);
        assert_eq!(
            effective_workspace_dir(&state),
            fs::canonicalize(home_dir()).unwrap_or_else(|_| home_dir())
                .to_string_lossy()
                .into_owned(),
            "an unset field must fall back to the home directory"
        );

        // Both new fields survive a round trip once they are set.
        let mut picked = state.clone();
        picked.workspace_dir = Some("/tmp".to_string());
        picked.recent_dirs = vec!["/tmp".to_string()];
        picked.used_dirs = vec!["/tmp".to_string(), "/var/tmp".to_string()];
        picked.top_bar_size = TopBarSize::Small;
        let reloaded: AppState =
            serde_json::from_str(&serde_json::to_string(&picked).unwrap()).unwrap();
        assert_eq!(reloaded.workspace_dir.as_deref(), Some("/tmp"));
        assert_eq!(reloaded.recent_dirs, vec!["/tmp".to_string()]);
        assert_eq!(reloaded.used_dirs, vec!["/tmp", "/var/tmp"]);
        assert_eq!(reloaded.top_bar_size, TopBarSize::Small);
    }

    #[test]
    fn test_effective_workspace_dir_falls_back_when_the_folder_is_gone() {
        let mut state = AppState::default();
        let gone = format!("{}/sd-workspace-gone-{}", env::temp_dir().display(), std::process::id());
        state.workspace_dir = Some(gone);
        // A deleted checkout / unmounted drive must not break `add-term`.
        assert_eq!(
            effective_workspace_dir(&state),
            fs::canonicalize(home_dir()).unwrap_or_else(|_| home_dir())
                .to_string_lossy()
                .into_owned()
        );
    }

    #[test]
    fn test_clean_dir_accepts_what_a_person_types() {
        let dir = env::temp_dir().join(format!("sd-clean-dir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dir_s = fs::canonicalize(&dir).unwrap().to_string_lossy().into_owned();

        assert_eq!(clean_dir(&dir_s).as_deref(), Some(dir_s.as_str()));
        assert_eq!(clean_dir(&format!("  {dir_s}/  ")).as_deref(), Some(dir_s.as_str()));

        // `~` and `~/` both mean the home directory.
        let home = fs::canonicalize(home_dir()).unwrap_or_else(|_| home_dir());
        let home_s = home.to_string_lossy().into_owned();
        assert_eq!(clean_dir("~").as_deref(), Some(home_s.as_str()));
        assert_eq!(clean_dir("~/").as_deref(), Some(home_s.as_str()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_dir_rejects_non_directories() {
        assert_eq!(clean_dir(""), None);
        assert_eq!(clean_dir("   "), None);
        assert_eq!(clean_dir("/definitely/not/a/folder/xyz"), None);
        // An existing file is not a workspace.
        assert_eq!(clean_dir("/etc/hostname"), None);
    }

    #[test]
    fn test_push_recent_dir_keeps_mru_order_without_duplicates() {
        let mut recent = Vec::new();
        for d in ["/a", "/b", "/c"] {
            push_recent_dir(&mut recent, d);
        }
        assert_eq!(recent, vec!["/c", "/b", "/a"]);

        // Re-using a folder moves it back to the front instead of adding a
        // second row for it.
        push_recent_dir(&mut recent, "/a");
        assert_eq!(recent, vec!["/a", "/c", "/b"]);

        // The history is bounded.
        for i in 0..RECENT_DIRS_MAX * 2 {
            push_recent_dir(&mut recent, &format!("/dir-{i}"));
        }
        assert_eq!(recent.len(), RECENT_DIRS_MAX);
        assert_eq!(recent[0], format!("/dir-{}", RECENT_DIRS_MAX * 2 - 1));
    }

    #[test]
    fn test_complete_workspace_history_outlives_the_seven_row_dropdown() {
        let mut state = AppState::default();
        for i in 0..12 {
            remember_workspace_dir(&mut state, &format!("/project-{i}"));
        }
        assert_eq!(state.recent_dirs.len(), 7);
        assert_eq!(state.used_dirs.len(), 12);
        assert_eq!(state.recent_dirs[0], "/project-11");
        assert!(state.used_dirs.contains(&"/project-0".to_string()));

        // Re-use moves a directory to the front of both lists without
        // duplicating it or discarding any autocomplete history.
        remember_workspace_dir(&mut state, "/project-0");
        assert_eq!(state.recent_dirs[0], "/project-0");
        assert_eq!(state.used_dirs[0], "/project-0");
        assert_eq!(state.used_dirs.len(), 12);
    }

    #[test]
    fn test_history_migration_preserves_entries_trimmed_from_old_dropdowns() {
        let mut state = AppState::default();
        state.recent_dirs = (0..9).map(|i| format!("/old-{i}")).collect();
        assert!(normalize_workspace_history(&mut state));
        assert_eq!(state.recent_dirs.len(), 7);
        assert_eq!(state.used_dirs.len(), 9);
        assert!(state.used_dirs.contains(&"/old-8".to_string()));
    }

    #[test]
    fn test_display_dir_shortens_only_the_home_prefix() {
        let home = home_dir_string();
        assert_eq!(display_dir(&home), "~");
        assert_eq!(display_dir(&format!("{home}/GitHub/x")), "~/GitHub/x");
        // A folder that merely starts with the same characters is left alone.
        assert_eq!(display_dir("/tmp"), "/tmp");
        assert_eq!(display_dir(&format!("{home}suffix")), format!("{home}suffix"));
    }
}
