//! One label colour per project folder.
//!
//! A terminal card's group colour (`TerminalData::tag`, the header dot) comes
//! from the folder its harness starts in, so every harness working on the same
//! project wears the same colour: start Claude in `Star_Chumaki` with a red
//! label, then Codex in `Star_Chumaki`, and Codex is red too. A folder seen for
//! the first time gets a palette colour no other folder uses yet. Picking a
//! different colour on any card (the header dot) becomes that folder's colour
//! for the next launches; cards already open keep their own.
//!
//! The choice lives in `AppState::folder_tags`. Cards opened before this
//! existed have no entry: their folder inherits the colour of its newest open
//! card instead, so upgrading keeps what the user already sees.

use crate::state::{AppState, TerminalData};
use crate::tag::{normalize_tag, TAG_CYAN, TAG_NONE};

/// First-seen folders take these in turn: distinct hues first, cyan (the old
/// default for every card) first of all so a single project looks unchanged.
const PICK_ORDER: [u8; 8] = [TAG_CYAN, 1, 4, 7, 2, 6, 3, 8];

/// The map key for a folder: the stored path without a trailing slash.
pub fn folder_key(dir: &str) -> String {
    let trimmed = dir.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

fn card_folder(card: &TerminalData) -> Option<String> {
    card.workspace_dir.as_deref().map(folder_key)
}

/// The colour a new card started in `folder` gets.
pub fn tag_for_new_card(state: &AppState, folder: &str) -> u8 {
    let key = folder_key(folder);
    if let Some(tag) = state.folder_tags.get(&key) {
        return normalize_tag(*tag);
    }
    if let Some(card) = newest_open_card(state, &key) {
        return normalize_tag(card.tag);
    }
    unused_colour(state)
}

/// Record `tag` as `folder`'s colour (a new launch, or a colour picked on one
/// of its cards). `TAG_NONE` is a choice too: later launches get no colour.
pub fn remember(state: &mut AppState, folder: &str, tag: u8) {
    state.folder_tags.insert(folder_key(folder), normalize_tag(tag));
}

/// A saved card changed from `previous` to `card`: a new colour picked on it
/// becomes its folder's colour. Moves, resizes and other saves change nothing.
pub fn note_card_saved(state: &mut AppState, previous: Option<&TerminalData>, card: &TerminalData) {
    let Some(previous) = previous else { return };
    if previous.tag == card.tag {
        return;
    }
    if let Some(folder) = card.workspace_dir.as_deref() {
        remember(state, folder, card.tag);
    }
}

fn newest_open_card<'a>(state: &'a AppState, key: &str) -> Option<&'a TerminalData> {
    state
        .terminals
        .iter()
        .filter(|card| card_folder(card).as_deref() == Some(key))
        .max_by(|a, b| a.created_at.total_cmp(&b.created_at))
}

/// The least-used palette colour among known folders, in `PICK_ORDER`.
fn unused_colour(state: &AppState) -> u8 {
    let mut folders: std::collections::BTreeMap<String, u8> = state.folder_tags.clone();
    for card in &state.terminals {
        if let Some(key) = card_folder(card) {
            folders.entry(key).or_insert_with(|| {
                newest_open_card(state, &card_folder(card).unwrap_or_default())
                    .map_or(card.tag, |newest| newest.tag)
            });
        }
    }
    let mut uses = [0usize; 9];
    for tag in folders.values().map(|tag| normalize_tag(*tag)) {
        if tag != TAG_NONE {
            uses[tag as usize] += 1;
        }
    }
    *PICK_ORDER
        .iter()
        .min_by_key(|tag| (uses[**tag as usize], PICK_ORDER.iter().position(|t| t == *tag)))
        .expect("palette is not empty")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, folder: &str, tag: u8, created_at: f64) -> TerminalData {
        serde_json::from_value(serde_json::json!({
            "id": id, "session_name": id, "agent_type": "claude", "command": "claude",
            "x": 0, "y": 0, "created_at": created_at, "tag": tag,
            "workspace_dir": folder,
        }))
        .unwrap()
    }

    fn empty() -> AppState {
        AppState { notes: Vec::new(), ..AppState::default() }
    }

    /// Launch a card the way the window does: pick, remember, open.
    fn launch(state: &mut AppState, id: &str, folder: &str) -> u8 {
        let tag = tag_for_new_card(state, folder);
        remember(state, folder, tag);
        let at = state.terminals.len() as f64;
        state.terminals.push(card(id, folder, tag, at));
        tag
    }

    #[test]
    fn a_second_harness_in_the_same_folder_inherits_its_colour() {
        let mut state = empty();
        let chumaki = launch(&mut state, "claude", "/home/u/Github/Star_Chumaki");
        let desktop = launch(&mut state, "shell", "/home/u/Github/super-desktop");
        assert_ne!(chumaki, desktop, "different folders get different colours");
        assert_eq!(launch(&mut state, "codex", "/home/u/Github/Star_Chumaki/"), chumaki);
    }

    #[test]
    fn a_colour_picked_on_a_card_is_what_the_next_launch_in_that_folder_gets() {
        let mut state = empty();
        launch(&mut state, "claude", "/p/Star_Chumaki");
        // The user makes the Claude card red (tag 1) with its header dot.
        let before = state.terminals[0].clone();
        state.terminals[0].tag = 1;
        let after = state.terminals[0].clone();
        note_card_saved(&mut state, Some(&before), &after);
        assert_eq!(launch(&mut state, "codex", "/p/Star_Chumaki"), 1);
        // A plain move (same colour) does not touch the folder's colour.
        let moved = TerminalData { x: 40, ..after.clone() };
        note_card_saved(&mut state, Some(&after), &moved);
        assert_eq!(state.folder_tags["/p/Star_Chumaki"], 1);
    }

    #[test]
    fn no_colour_is_a_choice_that_is_inherited_too() {
        let mut state = empty();
        launch(&mut state, "claude", "/p/a");
        let before = state.terminals[0].clone();
        state.terminals[0].tag = TAG_NONE;
        let after = state.terminals[0].clone();
        note_card_saved(&mut state, Some(&before), &after);
        assert_eq!(launch(&mut state, "codex", "/p/a"), TAG_NONE);
    }

    #[test]
    fn cards_from_before_folder_colours_keep_their_colour_for_their_folder() {
        let mut state = empty();
        state.terminals.push(card("old", "/p/legacy", 3, 1.0));
        state.terminals.push(card("older", "/p/legacy", 6, 0.5));
        assert_eq!(tag_for_new_card(&state, "/p/legacy"), 3, "newest open card wins");
        // A new folder avoids the colour the legacy folder already shows.
        assert_ne!(tag_for_new_card(&state, "/p/new"), 3);
    }

    #[test]
    fn new_folders_use_every_palette_colour_before_repeating_one() {
        let mut state = empty();
        let tags: Vec<u8> = (0..8).map(|n| launch(&mut state, &format!("c{n}"), &format!("/p/{n}"))).collect();
        assert_eq!(tags, PICK_ORDER, "one distinct colour per folder, cyan first");
        let ninth = launch(&mut state, "c8", "/p/8");
        assert!(PICK_ORDER.contains(&ninth), "a ninth folder reuses a palette colour");
    }

    #[test]
    fn folder_colours_survive_a_restart_and_old_state_files_still_load() {
        let mut state = empty();
        launch(&mut state, "claude", "/p/x");
        let saved: AppState = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(saved.folder_tags, state.folder_tags);
        let old: AppState = serde_json::from_str(r#"{"notes":[],"terminals":[]}"#).unwrap();
        assert!(old.folder_tags.is_empty());
    }
}
