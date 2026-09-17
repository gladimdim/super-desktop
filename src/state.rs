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
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            notes: vec![NoteData {
                id: "welcome_note".to_string(),
                text: "✨ Welcome to SUPER DESKTOP (Rust Edition)!\n\n• Shortcut: SUPER + SHIFT + Q to show / hide.\n• Drag: Grab any header to reposition smoothly!\n• Click 📝 + Note in the top bar to create a new sticky note.\n• Double-click a terminal card to expand it to 80% inside the overlay.\n• Double-click the header (or 🗕) to collapse it back.\n• ⚙ Settings picks which harnesses show up in the top bar.\n• Built in Rust for maximum 240Hz responsiveness.".to_string(),
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
        }
    }
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
}
