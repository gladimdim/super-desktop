use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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
    /// Harness keys (see `tmux::HARNESS_KEYS`) the user wants as launch
    /// buttons in the overlay's top bar, chosen in the ⚙ Settings panel.
    ///
    /// `None` = never configured → every harness detected on this machine
    /// (`tmux::detect_harnesses`) is shown, which is what older state files
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
            visible_harnesses: None,
            toggle_shortcut: None,
            workspace_dir: None,
            recent_dirs: Vec::new(),
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

/// How many folders the top bar's combobox remembers.
pub const RECENT_DIRS_MAX: usize = 8;

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
            if let Ok(state) = serde_json::from_str::<AppState>(&content) {
                return state;
            }
        }
    }
    let default_state = AppState::default();
    save_state(&default_state);
    default_state
}

pub fn save_state(state: &AppState) {
    let path = get_state_path();
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let temp_path = path.with_extension("tmp");
        if fs::write(&temp_path, json).is_ok() {
            let _ = fs::rename(temp_path, path);
        }
    }
}

/// Non-blocking variant for hot paths (drag-end, typing, resize-end).
/// Cloning + JSON serialization + file I/O all happen on a background
/// thread so the 120Hz frame clock on the main thread never stalls.
pub fn save_state_async(state: AppState) {
    std::thread::spawn(move || {
        let path = get_state_path();
        if let Ok(json) = serde_json::to_string_pretty(&state) {
            let temp_path = path.with_extension("tmp");
            if fs::write(&temp_path, json).is_ok() {
                let _ = fs::rename(temp_path, path);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

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
        assert_eq!(state.workspace_dir, None);
        assert!(state.recent_dirs.is_empty());
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
        let reloaded: AppState =
            serde_json::from_str(&serde_json::to_string(&picked).unwrap()).unwrap();
        assert_eq!(reloaded.workspace_dir.as_deref(), Some("/tmp"));
        assert_eq!(reloaded.recent_dirs, vec!["/tmp".to_string()]);
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
    fn test_display_dir_shortens_only_the_home_prefix() {
        let home = home_dir_string();
        assert_eq!(display_dir(&home), "~");
        assert_eq!(display_dir(&format!("{home}/GitHub/x")), "~/GitHub/x");
        // A folder that merely starts with the same characters is left alone.
        assert_eq!(display_dir("/tmp"), "/tmp");
        assert_eq!(display_dir(&format!("{home}suffix")), format!("{home}suffix"));
    }
}
