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
//! The field shows the folder in use (`~` while it is the home directory) and
//! the ▾ list is the re-use history, one row per folder with its own ✕ to
//! forget it.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, Entry, GestureClick, Label, Orientation, Popover, PositionType,
    PropagationPhase,
};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use crate::state::{
    clean_dir, display_dir, effective_workspace_dir, home_dir_string, push_recent_dir,
    resolve_workspace_input, AppState,
};

/// The top bar field and the history popover it owns.
///
/// The entry itself is not exposed: the app only needs to mount the bar and to
/// close the list on Esc (see `SuperDesktopWindow`).
pub struct WorkspaceBar {
    pub widget: GtkBox,
    pub popover: Popover,
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
    let bar = GtkBox::new(Orientation::Horizontal, 4);
    bar.add_css_class("ws-bar");
    bar.set_valign(Align::Center);

    let icon = Label::new(Some("📁"));
    icon.add_css_class("ws-icon");
    icon.set_tooltip_text(Some(
        "Working directory for new harness cards. Existing cards keep theirs.",
    ));
    bar.append(&icon);

    let entry = Entry::new();
    entry.add_css_class("ws-entry");
    entry.set_width_chars(18);
    entry.set_max_width_chars(42);
    entry.set_valign(Align::Center);
    // GTK entries expand by default, which would stretch the whole centred HUD
    // bar across the screen: the field claims its own text width and stops.
    entry.set_hexpand(false);
    entry.set_placeholder_text(Some(&display_dir(&home_dir_string())));
    entry.set_text(&display_dir(&effective_workspace_dir(&state.borrow())));
    entry.set_tooltip_text(Some(&effective_workspace_dir(&state.borrow())));
    bar.append(&entry);

    let menu_btn = Button::with_label("▾");
    menu_btn.add_css_class("ws-menu-btn");
    menu_btn.set_valign(Align::Center);
    menu_btn.set_tooltip_text(Some("Folders used before"));
    bar.append(&menu_btn);

    let popover = Popover::new();
    popover.add_css_class("ws-pop");
    popover.set_position(PositionType::Bottom);
    popover.set_has_arrow(false);
    popover.set_offset(0, 6);
    popover.set_can_focus(false);
    popover.set_parent(&entry);

    // Open the history on a click in the field — the combobox behaviour.
    //
    // Deliberately a click and not a focus handler: the window hands focus to
    // its first focusable widget when the overlay is shown, so a focus handler
    // would pop this list open on every SUPER + SHIFT + Q, and typing anywhere
    // would land in the field instead of the card the user meant.
    let pop_click = popover.clone();
    let state_click = Rc::clone(&state);
    let entry_click = entry.clone();
    let on_change_click = Rc::clone(&on_change);
    let click = GestureClick::new();
    // Capture phase: the entry places the caret on the same press.
    click.set_propagation_phase(PropagationPhase::Capture);
    click.connect_pressed(move |_, _, _, _| {
        rebuild_recent(
            &pop_click,
            &state_click,
            &entry_click,
            Rc::clone(&on_change_click),
        );
        pop_click.popup();
    });
    entry.add_controller(click);

    // ▾ toggles the same list for people who never click the field itself.
    let pop_toggle = popover.clone();
    let state_toggle = Rc::clone(&state);
    let entry_toggle = entry.clone();
    let on_change_toggle = Rc::clone(&on_change);
    menu_btn.connect_clicked(move |_| {
        if pop_toggle.is_visible() {
            pop_toggle.popdown();
            return;
        }
        rebuild_recent(
            &pop_toggle,
            &state_toggle,
            &entry_toggle,
            Rc::clone(&on_change_toggle),
        );
        pop_toggle.popup();
    });

    // Enter commits the typed folder; invalid text is reported in place and
    // the previous folder stays in use.
    let state_commit = Rc::clone(&state);
    let entry_commit = entry.clone();
    let pop_commit = popover.clone();
    entry.connect_activate(move |_| {
        if commit_entry(&state_commit, &entry_commit, &on_change) {
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

    WorkspaceBar { widget: bar, popover }
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
        push_recent_dir(&mut s.recent_dirs, dir);
        s.clone()
    };
    on_change(snapshot);

    entry.remove_css_class("ws-entry-invalid");
    entry.set_text(&display_dir(dir));
    entry.set_position(-1); // caret at the end after a programmatic set
    entry.set_tooltip_text(Some(dir));
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
) {
    let container = GtkBox::new(Orientation::Vertical, 2);
    container.add_css_class("ws-pop-box");

    let (recent, current) = {
        let s = state.borrow();
        (s.recent_dirs.clone(), s.workspace_dir.clone())
    };

    if recent.is_empty() {
        let empty = Label::new(Some("No folders used yet — type a path above."));
        empty.add_css_class("ws-empty");
        empty.set_halign(Align::Start);
        container.append(&empty);
    }

    for dir in recent {
        container.append(&recent_row(
            &dir,
            current.as_deref() == Some(dir.as_str()),
            Rc::clone(state),
            popover.clone(),
            entry.clone(),
            Rc::clone(&on_change),
        ));
    }

    popover.set_child(Some(&container));
}

/// One history row: `[✓] name  path` picks the folder, `✕` forgets it.
fn recent_row<FChange: Fn(AppState) + 'static>(
    dir: &str,
    active: bool,
    state: Rc<RefCell<AppState>>,
    popover: Popover,
    entry: Entry,
    on_change: Rc<FChange>,
) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 4);
    row.add_css_class("ws-row");
    row.add_css_class(if active { "ws-row-active" } else { "ws-row-idle" });

    let pick = Button::new();
    pick.add_css_class("ws-row-pick");
    pick.set_hexpand(true);
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

    let del = Button::with_label("✕");
    del.add_css_class("ws-del");
    del.set_tooltip_text(Some("Remove from this list (the folder itself stays)"));
    row.append(&del);

    {
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
            glib::idle_add_local_once(move || rebuild_recent(&pop, &state, &entry, on_change));
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
    use crate::state::RECENT_DIRS_MAX;

    #[test]
    fn test_row_name_uses_the_last_component() {
        assert_eq!(row_name("/home/me/GitHub/super-desktop"), "super-desktop");
        assert_eq!(row_name("/tmp"), "tmp");
        // Degenerate paths fall back to themselves instead of panicking.
        assert_eq!(row_name("/"), "/");
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

        // The field starts on the home directory, shown as `~`.
        assert_eq!(entry.text().as_str(), "~");
        assert!(entry.has_css_class("ws-entry"));

        // The ▾ list has one row per remembered folder, each with its own ✕.
        rebuild_recent(&bar.popover, &state, &entry, Rc::clone(&persist));
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
        assert_eq!(
            effective_workspace_dir(&state.borrow()),
            std::fs::canonicalize(home_dir_string())
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_else(|_| home_dir_string())
        );

        // ✕ forgets a history row without disturbing the folder in use.
        // Home was used too, so two folders are remembered by now.
        state.borrow_mut().workspace_dir = Some(project_s.clone());
        rebuild_recent(&bar.popover, &state, &entry, Rc::clone(&persist));
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

        // Emptying the list swaps the rows for the hint.
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
