//! Top-bar workspace field: the folder new harness sessions start in.
//!
//! Cards used to be launched with `tmux new-session -c $HOME`, so every
//! harness started in the home directory and the user had to walk it to the
//! right project by hand — the popup-on-every-run problem this field removes.
//!
//! One remembered folder is what makes that work in two ways:
//!
//! * every harness scopes its own history to the cwd (`claude --continue`,
//!   `opencode --continue`, `codex resume --last`, Reasonix sessions), so a
//!   card started elsewhere silently belongs to another project;
//! * Reasonix keys its workspace write lease on the process cwd, and `$HOME`
//!   is an ancestor of every project below it: a `~`-rooted session and a
//!   project-rooted session intersect, and one of them halts with "another
//!   session is writing to this workspace" until the other finishes. Pointing
//!   each card at its own project folder is what keeps parallel cards working
//!   at the same time.
//!
//! The field shows the full folder path. Typing completes matching folder
//! names in place (longest common prefix, unique match gets a trailing `/`).
//! Folders a harness has actually run in — and sibling folders that match the
//! prefix — appear in the dropdown; ↑/↓ move the highlight, Enter adopts it.
//! The ▾ list is the re-use history, one row per folder with its own ✕ to
//! forget it.

use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, Entry, EventControllerKey, GestureClick, Label, Orientation,
    Popover, PositionType, PropagationPhase,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::state::{
    clean_dir, display_dir, effective_workspace_dir, home_dir_string, remember_workspace_dir,
    resolve_workspace_input, AppState,
};

/// Cap on the autocomplete dropdown. Enough to scan; short enough for the HUD.
const SUGGEST_MAX: usize = 10;
/// Hard cap when listing a parent directory so `/` cannot stall a keystroke.
const FS_MATCH_CAP: usize = 24;

/// The top bar field and the history popover it owns.
pub struct WorkspaceBar {
    pub widget: GtkBox,
    pub popover: Popover,
    pub entry: Entry,
}

/// Live autocomplete / history-list state for one workspace field.
struct CompleteState {
    suggestions: Vec<String>,
    /// `None` = highlight stays on the field (Enter commits whatever is typed).
    selected: Option<usize>,
    /// ▾ history (with ✕) vs the filtered autocomplete list.
    history: bool,
    /// Skip `changed` while we write the entry from code (inline complete, apply).
    suppress: bool,
    /// Text as of the last handled `changed`, so Backspace does not re-expand.
    last_typed: String,
}

/// GTK4 delegates Entry editing to an internal GtkText child. During real
/// typing that child, rather than the outer Entry, owns root focus, so checking
/// only `entry.has_focus()` incorrectly classifies user input as a
/// programmatic update and suppresses autocomplete.
fn entry_is_being_edited(entry: &Entry) -> bool {
    if entry.has_focus() {
        return true;
    }
    let Some(root) = entry.root() else {
        return false;
    };
    let Some(focused) = root.focus() else {
        return false;
    };
    focused == entry.clone().upcast::<gtk4::Widget>() || focused.is_ancestor(entry)
}

/// Show autocomplete without letting the popover interrupt typing. GTK may
/// move focus to a newly mapped popover child after the current input event,
/// so restoration runs on the next main-loop turn and preserves both the
/// caret and an inline-completion selection.
fn popup_for_entry(popover: &Popover, entry: &Entry) {
    let caret = entry.position();
    let selection = entry.selection_bounds();
    // An autohide Popover installs a modal input grab. On a layer-shell
    // surface that grab stops the parent Entry receiving further keys even
    // when GTK still paints its caret. Autocomplete closes explicitly, so it
    // must be non-modal while the user is typing.
    popover.set_autohide(false);
    popover.popup();
    let popover = popover.clone();
    let entry = entry.clone();
    glib::idle_add_local_once(move || {
        if !popover.is_visible() || entry_is_being_edited(&entry) {
            return;
        }
        entry.grab_focus_without_selecting();
        if let Some((start, end)) = selection {
            entry.select_region(start, end);
        } else {
            entry.set_position(caret);
        }
    });
}

/// Build the workspace field that sits right after the brand label.
///
/// `on_change` persists a new folder. It is injected rather than called
/// directly so the ⚙-panel convention holds: the app passes
/// `state::save_state_async`, tests pass a recorder and never touch the real
/// `state.json`.
pub fn build_workspace_bar<FChange: Fn(AppState) + 'static>(
    state: Rc<RefCell<AppState>>,
    on_change: Rc<FChange>,
) -> WorkspaceBar {
    let bar = GtkBox::new(Orientation::Vertical, 0);
    bar.add_css_class("ws-bar");
    bar.set_valign(Align::Center);
    bar.set_hexpand(false);

    let subtitle = Label::new(Some("working directory for harness"));
    subtitle.add_css_class("ws-subtitle");
    subtitle.set_halign(Align::Start);
    bar.append(&subtitle);

    let row = GtkBox::new(Orientation::Horizontal, 4);
    row.set_valign(Align::Center);

    let icon = Label::new(Some("📁"));
    icon.add_css_class("ws-icon");
    row.append(&icon);

    let entry = Entry::new();
    entry.add_css_class("ws-entry");
    // Twice the previous compact field, so a project path remains readable
    // without opening the history or moving the caret through it.
    entry.set_width_chars(18);
    entry.set_max_width_chars(42);
    entry.set_valign(Align::Center);
    // GTK entries expand by default; the field claims its configured text
    // width so the other dock controls keep their own space.
    entry.set_hexpand(false);
    entry.set_can_focus(true);
    entry.set_editable(true);
    entry.set_focus_on_click(true);
    let current_dir = effective_workspace_dir(&state.borrow());
    entry.set_placeholder_text(Some(&home_dir_string()));
    entry.set_text(&current_dir);
    // No hover tooltip of the path: on a layer-shell HUD the tooltip is its
    // own popup and sits on top of this small field, so clicks never reach it.
    entry.set_has_tooltip(false);
    row.append(&entry);

    let menu_btn = Button::with_label("▾");
    menu_btn.add_css_class("ws-menu-btn");
    menu_btn.set_valign(Align::Center);
    menu_btn.set_tooltip_text(Some("Folders used before"));
    row.append(&menu_btn);
    bar.append(&row);

    let popover = Popover::new();
    popover.add_css_class("ws-pop");
    popover.set_position(PositionType::Bottom);
    popover.set_has_arrow(false);
    popover.set_offset(0, 6);
    popover.set_can_focus(false);
    popover.set_parent(&entry);

    let complete = Rc::new(RefCell::new(CompleteState {
        suggestions: Vec::new(),
        selected: None,
        history: false,
        suppress: false,
        last_typed: current_dir.clone(),
    }));

    // Clicks on the caption or folder icon land in the field, so the whole
    // control is a typing target — not only the (still compact) entry itself.
    {
        let entry_focus = entry.clone();
        let click = GestureClick::new();
        click.connect_pressed(move |_, _, _, _| {
            entry_focus.grab_focus();
        });
        subtitle.add_controller(click);
    }
    {
        let entry_focus = entry.clone();
        let click = GestureClick::new();
        click.connect_pressed(move |_, _, _, _| {
            entry_focus.grab_focus();
        });
        icon.add_controller(click);
    }

    // ▾ opens the unfiltered history (with ✕). Typing switches the same
    // popover over to the filtered autocomplete list.
    let pop_toggle = popover.clone();
    let state_toggle = Rc::clone(&state);
    let entry_toggle = entry.clone();
    let on_change_toggle = Rc::clone(&on_change);
    let complete_toggle = Rc::clone(&complete);
    menu_btn.connect_clicked(move |_| {
        if pop_toggle.is_visible() {
            pop_toggle.popdown();
            return;
        }
        let recent = state_toggle.borrow().recent_dirs.clone();
        {
            let mut c = complete_toggle.borrow_mut();
            c.suggestions = recent;
            c.selected = None;
            c.history = true;
        }
        rebuild_recent(
            &pop_toggle,
            &state_toggle,
            &entry_toggle,
            Rc::clone(&on_change_toggle),
            None,
        );
        // The explicit history menu behaves like a normal menu and may close
        // when the user clicks elsewhere. Autocomplete switches this back off
        // before showing matches from typing.
        pop_toggle.set_autohide(true);
        pop_toggle.popup();
    });

    // Typing: inline-complete folder names and, when something a harness has
    // used matches, show the dropdown. Only while the field is focused — a
    // programmatic `set_text` from tests / apply must not pop the list.
    let state_changed = Rc::clone(&state);
    let entry_changed = entry.clone();
    let pop_changed = popover.clone();
    let on_change_changed = Rc::clone(&on_change);
    let complete_changed = Rc::clone(&complete);
    entry.connect_changed(move |entry| {
        if complete_changed.borrow().suppress {
            return;
        }
        let typed = entry.text().to_string();
        if !entry_is_being_edited(entry) {
            complete_changed.borrow_mut().last_typed = typed;
            return;
        }
        let deleting = typed.len() < complete_changed.borrow().last_typed.len();
        let current = effective_workspace_dir(&state_changed.borrow());
        let used = collect_used_dirs(&state_changed.borrow());
        let expanded = expand_tilde(&typed);
        let (parent, partial) = split_parent_partial(&typed, &current);
        let used_m = matching_used_dirs(&expanded, &partial, &used);
        let fs_m = list_matching_dirs(&parent, &partial);
        let suggestions = merge_suggestions(used_m.clone(), fs_m);

        let caret_at_end = entry.position() as usize >= typed.chars().count();
        let mut shown = typed.clone();
        if caret_at_end && !deleting {
            if let Some(completed) = inline_completion(&typed, &current) {
                if completed != typed {
                    complete_changed.borrow_mut().suppress = true;
                    let from = typed.chars().count() as i32;
                    entry.set_text(&completed);
                    complete_changed.borrow_mut().suppress = false;
                    if completed.ends_with('/') {
                        entry.set_position(-1);
                    } else {
                        entry.select_region(from, -1);
                    }
                    shown = completed;
                }
            }
        }

        {
            let mut c = complete_changed.borrow_mut();
            c.last_typed = shown;
            c.suggestions = suggestions.clone();
            c.selected = c.selected.and_then(|i| {
                if suggestions.is_empty() {
                    None
                } else {
                    Some(i.min(suggestions.len() - 1))
                }
            });
            c.history = false;
        }

        let exact_one = suggestions.len() == 1
            && clean_dir(typed.trim()).as_deref() == Some(suggestions[0].as_str());
        // A finished unique path has nothing to pick; ↓ still opens the list.
        let show = !exact_one && (!used_m.is_empty() || suggestions.len() >= 2);
        if show && !suggestions.is_empty() {
            let selected = complete_changed.borrow().selected;
            paint_rows(
                &pop_changed,
                &suggestions,
                selected,
                state_changed.borrow().workspace_dir.as_deref(),
                false,
                &state_changed,
                &entry_changed,
                &on_change_changed,
            );
            if !pop_changed.is_visible() {
                popup_for_entry(&pop_changed, &entry_changed);
            }
        } else if pop_changed.is_visible() && !complete_changed.borrow().history {
            pop_changed.popdown();
        }
    });

    // ↑/↓ move the highlight from the field into the list; Tab accepts the
    // highlighted (or unique) folder into the field without persisting yet.
    let key = EventControllerKey::new();
    key.set_propagation_phase(PropagationPhase::Capture);
    let state_key = Rc::clone(&state);
    let entry_key = entry.clone();
    let pop_key = popover.clone();
    let on_change_key = Rc::clone(&on_change);
    let complete_key = Rc::clone(&complete);
    key.connect_key_pressed(move |_, keyval, _, _| {
        if keyval == gdk::Key::Down || keyval == gdk::Key::KP_Down {
            nudge_selection(
                1,
                &complete_key,
                &state_key,
                &entry_key,
                &pop_key,
                &on_change_key,
            );
            return glib::Propagation::Stop;
        }
        if keyval == gdk::Key::Up || keyval == gdk::Key::KP_Up {
            nudge_selection(
                -1,
                &complete_key,
                &state_key,
                &entry_key,
                &pop_key,
                &on_change_key,
            );
            return glib::Propagation::Stop;
        }
        if keyval == gdk::Key::Tab {
            accept_into_field(&complete_key, &entry_key);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    entry.add_controller(key);

    // Enter: highlighted row wins, otherwise the typed folder is committed.
    let state_commit = Rc::clone(&state);
    let entry_commit = entry.clone();
    let pop_commit = popover.clone();
    let complete_commit = Rc::clone(&complete);
    let on_change_commit = Rc::clone(&on_change);
    entry.connect_activate(move |_| {
        let picked = {
            let c = complete_commit.borrow();
            c.selected
                .and_then(|i| c.suggestions.get(i).cloned())
        };
        if let Some(dir) = picked {
            complete_commit.borrow_mut().suppress = true;
            apply_dir(&dir, &state_commit, &entry_commit, &on_change_commit);
            {
                let mut c = complete_commit.borrow_mut();
                c.suppress = false;
                c.selected = None;
                c.suggestions.clear();
                c.last_typed = dir;
            }
            pop_commit.popdown();
            return;
        }
        if commit_entry(&state_commit, &entry_commit, &on_change_commit) {
            let mut c = complete_commit.borrow_mut();
            c.selected = None;
            c.suggestions.clear();
            c.last_typed = entry_commit.text().to_string();
            pop_commit.popdown();
        }
    });

    // GTK warns when a widget with children is finalized; detach the popover
    // with the field (the HUD lives as long as the daemon, but the window can
    // be rebuilt).
    let pop_destroy = popover.clone();
    entry.connect_destroy(move |_| {
        pop_destroy.unparent();
    });

    WorkspaceBar {
        widget: bar,
        popover,
        entry,
    }
}

/// Move the dropdown highlight by `delta` (−1 up, +1 down). Down from the
/// field (no highlight) lands on the first row; Up from the first row returns
/// to the field. Opens the list if it was closed.
fn nudge_selection<FChange: Fn(AppState) + 'static>(
    delta: i32,
    complete: &Rc<RefCell<CompleteState>>,
    state: &Rc<RefCell<AppState>>,
    entry: &Entry,
    popover: &Popover,
    on_change: &Rc<FChange>,
) {
    if complete.borrow().suggestions.is_empty() {
        let current = effective_workspace_dir(&state.borrow());
        let typed = entry.text().to_string();
        let used = collect_used_dirs(&state.borrow());
        let mut suggestions = suggestions_for(&typed, &current, &used);
        if suggestions.is_empty() {
            suggestions = used;
        }
        complete.borrow_mut().suggestions = suggestions;
        complete.borrow_mut().history = false;
        complete.borrow_mut().selected = None;
    }
    let len = complete.borrow().suggestions.len();
    if len == 0 {
        return;
    }
    let next = match complete.borrow().selected {
        None if delta > 0 => Some(0),
        None => None,
        Some(0) if delta < 0 => None,
        Some(i) => {
            let n = i as i32 + delta;
            if n < 0 {
                None
            } else {
                Some((n as usize).min(len - 1))
            }
        }
    };
    complete.borrow_mut().selected = next;
    let suggestions = complete.borrow().suggestions.clone();
    let history = complete.borrow().history;
    paint_rows(
        popover,
        &suggestions,
        next,
        state.borrow().workspace_dir.as_deref(),
        history,
        state,
        entry,
        on_change,
    );
    if !popover.is_visible() {
        popup_for_entry(popover, entry);
    }
}

/// Tab: put the highlighted (or only) suggestion in the field, caret at the
/// end. Does not persist — Enter still commits.
fn accept_into_field(complete: &Rc<RefCell<CompleteState>>, entry: &Entry) {
    let fill = {
        let c = complete.borrow();
        if let Some(i) = c.selected {
            c.suggestions.get(i).cloned()
        } else if c.suggestions.len() == 1 {
            c.suggestions.first().cloned()
        } else {
            None
        }
    };
    if let Some(dir) = fill {
        complete.borrow_mut().suppress = true;
        entry.set_text(&dir);
        complete.borrow_mut().suppress = false;
        entry.set_position(-1);
        return;
    }
    // Accept an in-progress inline suffix (selection → caret at end).
    entry.set_position(-1);
}

/// Adopt the text in the field as the workspace folder.
///
/// Returns `false` when the text is not a folder anywhere it was looked for
/// (see [`resolve_workspace_input`]); the field then carries
/// `.ws-entry-invalid` and keeps the old folder, rather than a card silently
/// starting somewhere the user did not ask for.
fn commit_entry<FChange: Fn(AppState) + 'static>(
    state: &Rc<RefCell<AppState>>,
    entry: &Entry,
    on_change: &Rc<FChange>,
) -> bool {
    let typed = entry.text().to_string();
    let current = effective_workspace_dir(&state.borrow());
    let target = if typed.trim().is_empty() {
        // An emptied field means the default (the home directory), not an
        // error.
        clean_dir(&home_dir_string())
    } else {
        resolve_workspace_input(&typed, &current)
    };

    let Some(dir) = target else {
        entry.add_css_class("ws-entry-invalid");
        entry.set_has_tooltip(true);
        entry.set_tooltip_text(Some(&format!(
            "Not a folder: {typed}\nType an existing path, or press ▾ to reuse one."
        )));
        return false;
    };

    apply_dir(&dir, state, entry, on_change);
    true
}

/// Put `dir` in use: remember it as the current folder, move it to the front
/// of the history, and show it in the field.
fn apply_dir<FChange: Fn(AppState) + 'static>(
    dir: &str,
    state: &Rc<RefCell<AppState>>,
    entry: &Entry,
    on_change: &Rc<FChange>,
) {
    let snapshot = {
        let mut s = state.borrow_mut();
        s.workspace_dir = Some(dir.to_string());
        remember_workspace_dir(&mut s, dir);
        s.clone()
    };
    on_change(snapshot);

    entry.remove_css_class("ws-entry-invalid");
    entry.set_text(dir);
    entry.set_position(-1); // caret at the end after a programmatic set
    entry.set_has_tooltip(false);
}

/// Expand a leading `~` the way a person types it. Display spelling is
/// otherwise left alone so inline completion can stitch onto `~/…`.
fn expand_tilde(typed: &str) -> String {
    let t = typed.trim();
    if t == "~" {
        home_dir_string()
    } else if let Some(rest) = t.strip_prefix("~/") {
        format!("{}/{}", home_dir_string().trim_end_matches('/'), rest)
    } else {
        t.to_string()
    }
}

/// Directory whose children we list, and the last component currently typed.
fn split_parent_partial(typed: &str, current: &str) -> (PathBuf, String) {
    let expanded = expand_tilde(typed);
    let trimmed = typed.trim();
    if trimmed.is_empty() {
        return (PathBuf::from(current), String::new());
    }
    // Bare name: complete against the folder in use; used-dirs are matched
    // globally by name in [`matching_used_dirs`].
    if !trimmed.starts_with('/') && !trimmed.starts_with('~') && !trimmed.contains('/') {
        return (PathBuf::from(current), expanded);
    }
    if expanded.ends_with('/') {
        return (PathBuf::from(&expanded), String::new());
    }
    let path = Path::new(&expanded);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"));
    let partial = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (parent, partial)
}

/// The portion of `typed` up to and including the last `/`, so a completed
/// name can be stitched back in the user's spelling (`~/Gi` → `~/Git`).
fn user_parent_prefix(typed: &str) -> String {
    let t = typed.trim();
    match t.rfind('/') {
        Some(i) => t[..=i].to_string(),
        None => String::new(),
    }
}

/// Sibling directories of `parent` whose names start with `partial`
/// (case-insensitive). Hidden folders are skipped unless `partial` itself
/// starts with `.`.
fn list_matching_dirs(parent: &Path, partial: &str) -> Vec<String> {
    let Ok(iter) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let partial_lower = partial.to_lowercase();
    let show_hidden = partial.starts_with('.');
    let mut out = Vec::new();
    for ent in iter.flatten() {
        if out.len() >= FS_MATCH_CAP {
            break;
        }
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if name == "." || name == ".." {
            continue;
        }
        if name.starts_with('.') && !show_hidden {
            continue;
        }
        if !partial_lower.is_empty() && !name.to_lowercase().starts_with(&partial_lower) {
            continue;
        }
        let full = ent.path();
        if !full.is_dir() {
            continue;
        }
        out.push(full.to_string_lossy().into_owned());
    }
    out.sort_by(|a, b| {
        let na = Path::new(a).file_name().unwrap_or_default();
        let nb = Path::new(b).file_name().unwrap_or_default();
        na.cmp(&nb)
    });
    out
}

/// Folders a harness has actually run in, newest first, existing on disk.
///
/// `recent_dirs` is the seven-row visible list; `used_dirs` is the persistent
/// autocomplete index. Each card also contributes its cwd so sessions written
/// by an older version become searchable before the next state save.
fn collect_used_dirs(state: &AppState) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |raw: &str| {
        if let Some(dir) = clean_dir(raw) {
            if seen.insert(dir.clone()) {
                out.push(dir);
            }
        }
    };
    for d in &state.recent_dirs {
        push(d);
    }
    for d in &state.used_dirs {
        push(d);
    }
    for t in &state.terminals {
        if let Some(d) = t.workspace_dir.as_deref() {
            push(d);
        }
    }
    out
}

/// Previously-used folders that match what is being typed.
fn matching_used_dirs(expanded: &str, partial: &str, used: &[String]) -> Vec<String> {
    let exp_l = expanded.trim_end_matches('/').to_lowercase();
    let part_l = partial.to_lowercase();
    if exp_l.is_empty() && part_l.is_empty() {
        return Vec::new();
    }
    used.iter()
        .filter(|dir| {
            let dl = dir.to_lowercase();
            let name = Path::new(dir.as_str())
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            (!exp_l.is_empty() && dl.starts_with(&exp_l))
                || (!part_l.is_empty() && name.starts_with(&part_l))
                || (part_l.len() >= 2 && name.contains(&part_l))
        })
        .cloned()
        .collect()
}

fn merge_suggestions(used: Vec<String>, fs: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for dir in used.into_iter().chain(fs) {
        let key = fs::canonicalize(&dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or(dir);
        if seen.insert(key.clone()) {
            out.push(key);
        }
        if out.len() >= SUGGEST_MAX {
            break;
        }
    }
    out
}

/// Folders to offer for `typed`. `current` is the workspace in use (bare names).
fn suggestions_for(typed: &str, current: &str, used: &[String]) -> Vec<String> {
    if typed.trim().is_empty() {
        return Vec::new();
    }
    let expanded = expand_tilde(typed);
    let (parent, partial) = split_parent_partial(typed, current);
    let used_m = matching_used_dirs(&expanded, &partial, used);
    let fs_m = list_matching_dirs(&parent, &partial);
    merge_suggestions(used_m, fs_m)
}

fn name_lcp(paths: &[String]) -> String {
    let names: Vec<String> = paths
        .iter()
        .map(|p| {
            Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    if names.is_empty() {
        return String::new();
    }
    let mut prefix = names[0].clone();
    for n in &names[1..] {
        while !n.starts_with(&prefix) {
            prefix.pop();
            if prefix.is_empty() {
                return prefix;
            }
        }
    }
    prefix
}

/// Inline completion in the user's spelling. `None` = nothing to add.
fn inline_completion(typed: &str, current: &str) -> Option<String> {
    let trimmed = typed.trim();
    if trimmed.is_empty() || trimmed.ends_with('/') {
        return None;
    }
    let (parent, partial) = split_parent_partial(typed, current);
    if partial.is_empty() {
        return None;
    }
    let fs = list_matching_dirs(&parent, &partial);
    if fs.is_empty() {
        return None;
    }
    let lcp = name_lcp(&fs);
    if lcp.len() <= partial.len() {
        return None;
    }
    // Leave the suggested suffix selected in the Entry. Appending `/` here
    // used to put the caret after the completion, so the next typed character
    // was appended to `Github/` instead of replacing the `thub` suggestion.
    let completed = format!("{}{}", user_parent_prefix(typed), lcp);
    if completed == trimmed {
        return None;
    }
    Some(completed)
}

/// (Re)fill the popover with one row per remembered folder, newest first.
///
/// Called on every open and after a delete, so the list can never disagree
/// with `state.json`. Public so the GTK structure test can drive it without a
/// display-side focus event.
pub fn rebuild_recent<FChange: Fn(AppState) + 'static>(
    popover: &Popover,
    state: &Rc<RefCell<AppState>>,
    entry: &Entry,
    on_change: Rc<FChange>,
    selected: Option<usize>,
) {
    let (recent, current) = {
        let s = state.borrow();
        (s.recent_dirs.clone(), s.workspace_dir.clone())
    };
    paint_rows(
        popover,
        &recent,
        selected,
        current.as_deref(),
        true,
        state,
        entry,
        &on_change,
    );
}

fn paint_rows<FChange: Fn(AppState) + 'static>(
    popover: &Popover,
    dirs: &[String],
    selected: Option<usize>,
    current: Option<&str>,
    show_delete: bool,
    state: &Rc<RefCell<AppState>>,
    entry: &Entry,
    on_change: &Rc<FChange>,
) {
    let container = GtkBox::new(Orientation::Vertical, 2);
    container.add_css_class("ws-pop-box");

    if dirs.is_empty() {
        let empty = Label::new(Some("No folders used yet — type a path above."));
        empty.add_css_class("ws-empty");
        empty.set_halign(Align::Start);
        container.append(&empty);
        popover.set_child(Some(&container));
        return;
    }

    for (i, dir) in dirs.iter().enumerate() {
        container.append(&list_row(
            dir,
            current == Some(dir.as_str()),
            selected == Some(i),
            show_delete,
            Rc::clone(state),
            popover.clone(),
            entry.clone(),
            Rc::clone(on_change),
        ));
    }

    popover.set_child(Some(&container));
}

/// One list row: `[✓] name  path` picks the folder; `✕` (history only) forgets it.
fn list_row<FChange: Fn(AppState) + 'static>(
    dir: &str,
    active: bool,
    selected: bool,
    show_delete: bool,
    state: Rc<RefCell<AppState>>,
    popover: Popover,
    entry: Entry,
    on_change: Rc<FChange>,
) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 4);
    row.add_css_class("ws-row");
    row.add_css_class(if active { "ws-row-active" } else { "ws-row-idle" });
    if selected {
        row.add_css_class("ws-row-selected");
    }

    let pick = Button::new();
    pick.add_css_class("ws-row-pick");
    pick.set_hexpand(true);
    pick.set_can_focus(false);
    pick.set_focus_on_click(false);
    pick.set_tooltip_text(Some(dir));

    let label_row = GtkBox::new(Orientation::Horizontal, 8);
    let mark = Label::new(Some(if active { "✓" } else { " " }));
    mark.add_css_class("ws-row-mark");
    label_row.append(&mark);

    let name = Label::new(Some(&row_name(dir)));
    name.add_css_class("ws-row-name");
    label_row.append(&name);

    let path = Label::new(Some(&display_dir(dir)));
    path.add_css_class("ws-row-path");
    path.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    path.set_halign(Align::End);
    path.set_hexpand(true);
    label_row.append(&path);

    pick.set_child(Some(&label_row));
    row.append(&pick);

    {
        let pick_dir = dir.to_string();
        let state_pick = Rc::clone(&state);
        let entry_pick = entry.clone();
        let pop_pick = popover.clone();
        let on_change_pick = Rc::clone(&on_change);
        pick.connect_clicked(move |_| {
            apply_dir(&pick_dir, &state_pick, &entry_pick, &on_change_pick);
            pop_pick.popdown();
        });
    }

    if show_delete {
        let del = Button::with_label("✕");
        del.add_css_class("ws-del");
        del.set_can_focus(false);
        del.set_focus_on_click(false);
        del.set_tooltip_text(Some("Remove from this list (the folder itself stays)"));
        row.append(&del);

        let dir_del = dir.to_string();
        let state_del = Rc::clone(&state);
        let entry_del = entry;
        let pop_del = popover;
        let on_change_del = on_change;
        del.connect_clicked(move |_| {
            {
                let mut s = state_del.borrow_mut();
                s.recent_dirs.retain(|d| d != &dir_del);
                let snapshot = s.clone();
                drop(s);
                on_change_del(snapshot);
            }
            // Rebuild from the next idle: this handler runs on a button the
            // rebuild is about to destroy (removing a widget from inside its
            // own signal handler is how GTK crashes).
            let (pop, state, entry, on_change) = (
                pop_del.clone(),
                Rc::clone(&state_del),
                entry_del.clone(),
                Rc::clone(&on_change_del),
            );
            glib::idle_add_local_once(move || {
                rebuild_recent(&pop, &state, &entry, on_change, None);
            });
        });
    }

    row
}

/// Display name of a remembered folder: its last path component, falling back
/// to the path itself for anything unusual (`/`, a trailing `..`).
fn row_name(dir: &str) -> String {
    Path::new(dir)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{TerminalData, RECENT_DIRS_MAX};

    #[test]
    fn test_row_name_uses_the_last_component() {
        assert_eq!(row_name("/home/me/GitHub/super-desktop"), "super-desktop");
        assert_eq!(row_name("/tmp"), "tmp");
        // Degenerate paths fall back to themselves instead of panicking.
        assert_eq!(row_name("/"), "/");
    }

    #[test]
    fn test_split_parent_partial_keeps_bare_names_on_the_current_folder() {
        let (parent, partial) = split_parent_partial("super", "/home/me/GitHub");
        assert_eq!(parent, PathBuf::from("/home/me/GitHub"));
        assert_eq!(partial, "super");

        let (parent, partial) = split_parent_partial("/home/me/Gi", "/tmp");
        assert_eq!(parent, PathBuf::from("/home/me"));
        assert_eq!(partial, "Gi");

        let (parent, partial) = split_parent_partial("/home/me/", "/tmp");
        assert_eq!(parent, PathBuf::from("/home/me/"));
        assert_eq!(partial, "");
    }

    #[test]
    fn test_user_parent_prefix_preserves_tilde_spelling() {
        assert_eq!(user_parent_prefix("~/Gi"), "~/");
        assert_eq!(user_parent_prefix("/home/me/Gi"), "/home/me/");
        assert_eq!(user_parent_prefix("Gi"), "");
    }

    #[test]
    fn test_list_matching_dirs_is_prefix_and_directories_only() {
        let root = std::env::temp_dir().join(format!("sd-ac-fs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Github")).unwrap();
        fs::create_dir_all(root.join("Gitlab")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join("notes.txt"), b"x").unwrap();

        let names = |partial: &str| -> Vec<String> {
            list_matching_dirs(&root, partial)
                .into_iter()
                .map(|p| row_name(&p))
                .collect()
        };

        assert_eq!(names("Gi"), vec!["Github".to_string(), "Gitlab".to_string()]);
        assert_eq!(names("GitH"), vec!["Github".to_string()]);
        assert!(names("n").is_empty(), "files must not complete as folders");
        assert!(names("").contains(&"Github".to_string()));
        assert!(
            !names("").iter().any(|n| n == ".hidden"),
            "hidden folders stay hidden until the user types a dot"
        );
        assert_eq!(names("."), vec![".hidden".to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_inline_completion_fills_unique_and_lcp() {
        let root = std::env::temp_dir().join(format!("sd-ac-inline-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Github")).unwrap();
        fs::create_dir_all(root.join("Gitlab")).unwrap();
        fs::create_dir_all(root.join("onlyone")).unwrap();
        let root_s = root.to_string_lossy().into_owned();

        let lcp = inline_completion(&format!("{root_s}/Gi"), "/tmp").unwrap();
        assert_eq!(lcp, format!("{root_s}/Git"));

        let unique = inline_completion(&format!("{root_s}/on"), "/tmp").unwrap();
        assert_eq!(unique, format!("{root_s}/onlyone"));

        assert!(
            inline_completion(&format!("{root_s}/Git"), "/tmp").is_none(),
            "already at the common prefix: nothing more to fill"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_matching_used_dirs_requires_existing_and_ranks_prefix() {
        let root = std::env::temp_dir().join(format!("sd-ac-used-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let project = root.join("super-desktop");
        let other = root.join("notes-app");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&other).unwrap();
        let project_s = fs::canonicalize(&project)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let other_s = fs::canonicalize(&other)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let gone = root.join("deleted-proj").to_string_lossy().into_owned();

        let used = vec![project_s.clone(), other_s.clone(), gone];
        // `gone` is not a directory, so collect_used_dirs would drop it; the
        // matcher itself still filters by name and the caller uses collect.
        let hits = matching_used_dirs(&project_s[..project_s.len() - 3], "super-desk", &used);
        assert!(hits.contains(&project_s));
        assert!(!hits.contains(&other_s));

        let by_name = matching_used_dirs("desk", "desk", &used);
        assert!(
            by_name.contains(&project_s),
            "a short name still finds a used folder by substring"
        );

        let mixed_case = matching_used_dirs("SCiFi", "SCiFi", &[
            project_s.clone(),
            other_s.clone(),
        ]);
        assert!(
            mixed_case.is_empty(),
            "unrelated folders must not match an arbitrary fragment"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_collect_used_dirs_includes_card_cwd_and_skips_missing() {
        let root = std::env::temp_dir().join(format!("sd-ac-collect-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let launched = root.join("launched-once");
        fs::create_dir_all(&launched).unwrap();
        let launched_s = fs::canonicalize(&launched)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let mut state = AppState::default();
        state.recent_dirs = vec!["/definitely/not/here".to_string()];
        state.used_dirs = vec![launched_s.clone()];
        state.terminals = vec![TerminalData {
            id: "t".to_string(),
            session_name: "s".to_string(),
            agent_type: "shell".to_string(),
            command: "/usr/bin/bash".to_string(),
            x: 0,
            y: 0,
            width: 380,
            height: 240,
            restored_width: 380,
            restored_height: 240,
            iconified: false,
            icon_x: None,
            icon_y: None,
            created_at: 0.0,
            tag: 0,
            agent_session_id: None,
            workspace_dir: Some(launched_s.clone()),
        }];

        let used = collect_used_dirs(&state);
        assert_eq!(used, vec![launched_s]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_used_folder_matches_case_insensitive_name_fragment() {
        let root = std::env::temp_dir().join(format!("sd-ac-scifi-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let project = root.join("LocalDesertaSciFi");
        fs::create_dir_all(&project).unwrap();
        let project = fs::canonicalize(project)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        for typed in ["SciFi", "scifi", "SCIFI", "desertascifi"] {
            let hits = matching_used_dirs(typed, typed, std::slice::from_ref(&project));
            assert_eq!(hits, vec![project.clone()], "fragment {typed:?}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_suggestions_for_puts_used_folders_ahead_of_siblings() {
        let root = std::env::temp_dir().join(format!("sd-ac-sug-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("alpha")).unwrap();
        fs::create_dir_all(root.join("alpine")).unwrap();
        let alpine = fs::canonicalize(root.join("alpine"))
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let root_s = root.to_string_lossy().into_owned();

        let sug = suggestions_for(&format!("{root_s}/al"), "/tmp", &[alpine.clone()]);
        assert_eq!(sug[0], alpine, "a used harness folder must lead the list");
        assert!(sug.len() >= 2, "sibling folders still appear after it");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_workspace_bar_gtk_structure() {
        // GTK may only be used from one thread per process, so the widget
        // assertions run in a child process (see `crate::gtk_test`).
        crate::gtk_test::run_in_child_process("workspace_bar::tests::structure_child");
    }

    /// Only meaningful when re-run as the single test of a fresh process.
    #[test]
    fn structure_child() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let _ = gtk4::init();

        let tmp = std::env::temp_dir().join(format!("sd-ws-bar-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let project = tmp.join("project");
        let _ = std::fs::create_dir_all(&project);
        let project_s = std::fs::canonicalize(&project)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let state = Rc::new(RefCell::new(AppState::default()));
        state.borrow_mut().recent_dirs = vec![project_s.clone()];
        // The persist callback is recorded, not executed: a test run must never
        // rewrite the user's real `~/.config/super-desktop/state.json`.
        let saved: Rc<RefCell<Vec<AppState>>> = Rc::new(RefCell::new(Vec::new()));
        let saved_cb = Rc::clone(&saved);
        let bar = build_workspace_bar(
            Rc::clone(&state),
            Rc::new(move |s: AppState| saved_cb.borrow_mut().push(s)),
        );
        let saved_persist = Rc::clone(&saved);
        let persist = Rc::new(move |s: AppState| saved_persist.borrow_mut().push(s));
        let entry = find_entry(bar.widget.upcast_ref(), "ws-entry").expect("the field must exist");
        assert!(bar.popover.child().is_none(), "empty until it is opened");

        let subtitle =
            find_label(bar.widget.upcast_ref(), "ws-subtitle").expect("the subtitle must exist");
        assert_eq!(subtitle.text().as_str(), "working directory for harness");

        // The field starts on the home directory, shown as the full path.
        assert_eq!(entry.text().as_str(), home_dir_string());
        assert!(entry.has_css_class("ws-entry"));
        assert!(entry.is_editable(), "the field must accept typed paths");
        assert!(entry.can_focus(), "clicking the field must be able to focus it");
        assert!(
            !entry.has_tooltip(),
            "a hover tooltip would cover the field and steal clicks"
        );
        assert_eq!(entry.width_chars(), 18);
        assert_eq!(entry.max_width_chars(), 42);

        // The ▾ list has one row per remembered folder, each with its own ✕.
        rebuild_recent(&bar.popover, &state, &entry, Rc::clone(&persist), None);
        let pop_child = bar.popover.child().expect("popover content");
        assert_eq!(count_class(&pop_child, "ws-row"), 1);
        assert_eq!(count_class(&pop_child, "ws-del"), 1);
        assert!(
            count_class(&pop_child, "ws-empty") == 0,
            "no empty-state hint while folders are remembered"
        );

        // Typing a folder that does not exist is reported, and the folder in
        // use does not change.
        entry.set_text("/definitely/not/here");
        entry.emit_activate();
        assert!(entry.has_css_class("ws-entry-invalid"));
        assert_eq!(state.borrow().workspace_dir, None);
        assert!(saved.borrow().is_empty(), "nothing persisted for invalid text");

        // Typing a real one adopts it (and remembers it).
        entry.set_text(&project_s);
        entry.emit_activate();
        assert!(!entry.has_css_class("ws-entry-invalid"));
        assert_eq!(state.borrow().workspace_dir.as_deref(), Some(project_s.as_str()));
        assert_eq!(state.borrow().recent_dirs[0], project_s);
        assert_eq!(state.borrow().used_dirs[0], project_s);
        assert_eq!(effective_workspace_dir(&state.borrow()), project_s);
        assert_eq!(saved.borrow().len(), 1, "one persist per committed folder");
        assert_eq!(
            saved.borrow()[0].workspace_dir.as_deref(),
            Some(project_s.as_str())
        );

        // Typing a bare folder name resolves against the folder in use, so
        // `project` alone means the same folder.
        entry.set_text("project");
        entry.emit_activate();
        assert_eq!(effective_workspace_dir(&state.borrow()), project_s);

        // An emptied field means "back to the home directory".
        entry.set_text("  ");
        entry.emit_activate();
        let home_shown = std::fs::canonicalize(home_dir_string())
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|_| home_dir_string());
        assert_eq!(effective_workspace_dir(&state.borrow()), home_shown);
        assert_eq!(entry.text().as_str(), home_shown);

        // ✕ forgets a history row without disturbing the folder in use.
        // Home was used too, so two folders are remembered by now.
        state.borrow_mut().workspace_dir = Some(project_s.clone());
        rebuild_recent(&bar.popover, &state, &entry, Rc::clone(&persist), None);
        let rows_before = count_class(&bar.popover.child().unwrap(), "ws-row");
        assert_eq!(rows_before, 2, "home and the project are both remembered");

        let del = find_button(&bar.popover.child().unwrap(), "ws-del").expect("a delete button");
        del.emit_clicked();
        pump_idle();
        assert_eq!(
            state.borrow().recent_dirs.len(),
            rows_before - 1,
            "one ✕ must forget exactly one folder"
        );
        assert_eq!(
            state.borrow().workspace_dir.as_deref(),
            Some(project_s.as_str()),
            "forgetting a row must not change the folder in use"
        );

        // Autocomplete list: matching used folders, no ✕, highlight on row 0.
        let other = tmp.join("other-proj");
        let _ = std::fs::create_dir_all(&other);
        let other_s = std::fs::canonicalize(&other)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        state.borrow_mut().recent_dirs = vec![project_s.clone(), other_s.clone()];
        let used = collect_used_dirs(&state.borrow());
        let prefix = &project_s[..project_s.len().saturating_sub(2)];
        let sug = suggestions_for(prefix, &home_dir_string(), &used);
        assert!(
            sug.iter().any(|d| d == &project_s),
            "a folder a harness has used must complete from a prefix of its path"
        );
        paint_rows(
            &bar.popover,
            &sug,
            Some(0),
            Some(project_s.as_str()),
            false,
            &state,
            &entry,
            &persist,
        );
        let child = bar.popover.child().unwrap();
        assert!(count_class(&child, "ws-row") >= 1);
        assert_eq!(
            count_class(&child, "ws-del"),
            0,
            "autocomplete rows must not carry the history ✕"
        );
        assert_eq!(count_class(&child, "ws-row-selected"), 1);

        // Emptying the list swaps the rows for the hint.
        rebuild_recent(&bar.popover, &state, &entry, Rc::clone(&persist), None);
        for _ in 0..RECENT_DIRS_MAX + 2 {
            let Some(child) = bar.popover.child() else { break };
            let Some(del) = find_button(&child, "ws-del") else {
                break;
            };
            del.emit_clicked();
            pump_idle();
        }
        assert!(state.borrow().recent_dirs.is_empty());
        let child = bar.popover.child().unwrap();
        assert_eq!(count_class(&child, "ws-row"), 0);
        assert_eq!(count_class(&child, "ws-empty"), 1);

        // GtkEntry delegates real editing focus to its internal GtkText. A
        // prefix typed with that child focused must still run autocomplete
        // and build the picker rows.
        let host = gtk4::Window::new();
        host.set_child(Some(&bar.widget));
        let text_child = entry.first_child().expect("GtkEntry text delegate");
        gtk4::prelude::RootExt::set_focus(&host, Some(&text_child));
        assert!(entry_is_being_edited(&entry));
        state.borrow_mut().used_dirs = vec![project_s.clone()];
        entry.set_text(&project_s);
        let chars = project_s.chars().count() as i32;
        entry.delete_text(chars - 2, chars);
        pump_idle();
        assert!(
            entry_is_being_edited(&entry),
            "opening autocomplete after Backspace must keep keyboard focus in the field"
        );
        assert!(
            !bar.popover.is_autohide(),
            "autocomplete must not install a modal input grab"
        );
        assert!(
            count_class(&bar.popover.child().expect("autocomplete rows"), "ws-row") >= 1,
            "typing a used path prefix must populate the picker"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Run the deferred rebuild that a ✕ click schedules.
    fn pump_idle() {
        for _ in 0..10 {
            let _ = glib::MainContext::default().iteration(false);
        }
    }

    fn count_class(w: &gtk4::Widget, class: &str) -> usize {
        let mut n = if w.has_css_class(class) { 1 } else { 0 };
        let mut child = w.first_child();
        while let Some(c) = child {
            n += count_class(&c, class);
            child = c.next_sibling();
        }
        n
    }

    fn find_label(w: &gtk4::Widget, class: &str) -> Option<Label> {
        if let Some(l) = w.downcast_ref::<Label>() {
            if l.has_css_class(class) {
                return Some(l.clone());
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(found) = find_label(&c, class) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    fn find_entry(w: &gtk4::Widget, class: &str) -> Option<Entry> {
        if let Some(e) = w.downcast_ref::<Entry>() {
            if e.has_css_class(class) {
                return Some(e.clone());
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(found) = find_entry(&c, class) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    fn find_button(w: &gtk4::Widget, class: &str) -> Option<Button> {
        if let Some(b) = w.downcast_ref::<Button>() {
            if b.has_css_class(class) {
                return Some(b.clone());
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(found) = find_button(&c, class) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }
}
