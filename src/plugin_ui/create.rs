//! ✨ Create Plugin: the user says what they want, picks an agent and where
//! it goes; SUPER DESKTOP makes the plugin folder (with the skill), opens the
//! agent's card in it and types the request. The plugin appears while the
//! agent builds it.
use super_desktop::plugins::skill::{self, Template};
use gtk4::prelude::*;
use gtk4::{Align, Orientation};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const KEY: &str = "plugin-create";
const PLACES: &[(&str, &str)] = &[
    ("dock.right", "Dock on the right edge"),
    ("dock.left", "Dock on the left edge"),
    ("toolbar.item", "Item in the top bar"),
    ("panel", "Panel opened from the top bar"),
    ("settings.page", "Section in Settings → Plugins"),
    ("card", "Card on the desktop (a game, tool or terminal)"),
    ("fullscreen", "Full screen, opened from the top bar (a game, a screensaver)"),
    ("popup", "Popup under a top-bar item (a menu, a quick form)"),
];

thread_local! {
    static PANELS: crate::floating_panel::CardPanels = crate::floating_panel::CardPanels::new((560, 480), (420, 380));
    /// The agent picked last time, so the next plugin starts with it.
    static LAST_HARNESS: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The agent to edit a plugin with: the one used last, else Claude, else
/// the first offered. `None` when no agent is offered.
pub fn preferred_harness() -> Option<String> {
    let offered: Vec<String> = super::api::hooks()
        .map(|h| (h.harnesses)())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|h| h["id"].as_str().map(str::to_owned))
        .collect();
    let last = LAST_HARNESS.with(|last| last.borrow().clone());
    last.filter(|id| offered.contains(id))
        .or_else(|| offered.iter().find(|id| *id == "claude").cloned())
        .or_else(|| offered.first().cloned())
}

fn label(text: &str, class: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.add_css_class(class);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}

/// Opens the form (or brings it to the front).
pub fn open() {
    if !PANELS.with(|p| p.wants_new(KEY)) {
        return;
    }
    let badge = gtk4::Label::new(Some("✨"));
    let (outer, header) = crate::floating_panel::card_panel(&badge, "Create a plugin", "An AI agent builds it while you watch", (420, 380));
    let close = header.close_button("Close");
    close.connect_clicked(|_| PANELS.with(|p| p.close(KEY)));

    let body = gtk4::Box::new(Orientation::Vertical, 10);
    body.add_css_class("launcher-body");
    body.set_vexpand(true);
    outer.append(&body);

    body.append(&label("What should it do?", "launcher-section-title"));
    let request = gtk4::TextView::new();
    request.set_wrap_mode(gtk4::WrapMode::WordChar);
    request.add_css_class("plugin-create-request");
    request.set_accepts_tab(false);
    request.set_top_margin(8);
    request.set_bottom_margin(8);
    request.set_left_margin(8);
    request.set_right_margin(8);
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_child(Some(&request));
    scroll.set_min_content_height(110);
    scroll.set_vexpand(true);
    scroll.add_css_class("plugin-create-request-frame");
    body.append(&scroll);
    body.append(&label(
        "For example: “a dock with buttons for my git repos”, “a Pomodoro timer in the top bar”, “a snake game panel”.",
        "launcher-note",
    ));

    let grid = gtk4::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(10);
    let name = gtk4::Entry::new();
    name.set_placeholder_text(Some("Optional; made from the request"));
    name.set_hexpand(true);
    grid.attach(&label("Name", "launcher-note"), 0, 0, 1, 1);
    grid.attach(&name, 1, 0, 1, 1);

    let place_names: Vec<&str> = PLACES.iter().map(|(_, text)| *text).collect();
    let place = gtk4::DropDown::from_strings(&place_names);
    grid.attach(&label("Where", "launcher-note"), 0, 1, 1, 1);
    grid.attach(&place, 1, 1, 1, 1);

    let harnesses: Vec<(String, String)> = super::api::hooks()
        .map(|h| (h.harnesses)())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|h| Some((h["id"].as_str()?.to_owned(), h["name"].as_str()?.to_owned())))
        .collect();
    let harness_names: Vec<&str> = harnesses.iter().map(|(_, name)| name.as_str()).collect();
    let harness = gtk4::DropDown::from_strings(&harness_names);
    let preferred = LAST_HARNESS.with(|last| last.borrow().clone()).unwrap_or_else(|| "claude".into());
    if let Some(index) = harnesses.iter().position(|(id, _)| *id == preferred) {
        harness.set_selected(index as u32);
    }
    grid.attach(&label("Agent", "launcher-note"), 0, 2, 1, 1);
    grid.attach(&harness, 1, 2, 1, 1);
    body.append(&grid);

    let status = label("", "launcher-note");
    status.set_visible(false);
    body.append(&status);

    let actions = gtk4::Box::new(Orientation::Horizontal, 8);
    actions.add_css_class("launcher-actions");
    actions.set_halign(Align::End);
    let create = gtk4::Button::with_label("✨ Create");
    create.add_css_class("launcher-btn");
    create.add_css_class("launcher-btn-primary");
    actions.append(&create);
    body.append(&actions);

    if harnesses.is_empty() {
        create.set_sensitive(false);
        status.set_text("No AI agent is offered in the top bar. Turn one on in Settings → Harness launchers.");
        status.add_css_class("launcher-note-error");
        status.set_visible(true);
    }

    let busy = Rc::new(Cell::new(false));
    create.connect_clicked({
        let status = status.clone();
        let name = name.clone();
        let request = request.clone();
        move |button| {
            if busy.get() {
                return;
            }
            let buffer = request.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).trim().to_owned();
            let show = |message: &str, error: bool| {
                status.set_text(message);
                if error { status.add_css_class("launcher-note-error") } else { status.remove_css_class("launcher-note-error") }
                status.set_visible(true);
            };
            if text.is_empty() {
                show("Say what the plugin should do.", true);
                return;
            }
            let Some((harness_id, harness_name)) = harnesses.get(harness.selected() as usize).cloned() else {
                show("Pick an agent.", true);
                return;
            };
            let template = PLACES
                .get(place.selected() as usize)
                .and_then(|(slot, _)| Template::parse(slot))
                .unwrap_or(Template::DockRight);
            let root = super_desktop::plugins::paths::plugins_dir();
            let typed_name = name.text().trim().to_owned();
            let id = skill::unique_id(&root, if typed_name.is_empty() { &text } else { &typed_name });
            let display_name = if typed_name.is_empty() { id.replace('-', " ") } else { typed_name };
            busy.set(true);
            button.set_sensitive(false);
            let result = skill::new_plugin(&root, &id, &display_name, template, &text)
                .and_then(|dir| super::agent::open(&harness_id, &dir, Some(skill::first_prompt(&id, &text))).map(|()| dir));
            busy.set(false);
            button.set_sensitive(true);
            match result {
                Ok(dir) => {
                    LAST_HARNESS.with(|last| *last.borrow_mut() = Some(harness_id));
                    super_desktop::plugins::paths::append_log(&id, &format!("✨ Create Plugin: {harness_name} opened in {}", dir.display()));
                    // The form has done its job: the agent's card takes over,
                    // in view. After this click handler returns: closing
                    // removes the button.
                    gtk4::glib::idle_add_local_once(|| PANELS.with(|p| p.close(KEY)));
                }
                Err(e) => show(&e, true),
            }
        }
    });

    PANELS.with(|p| p.show(KEY, &outer, Rc::new(Cell::new(0)), None));
    request.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_create_makes_the_folder_and_opens_the_agent_there() {
        crate::gtk_test::run_in_child_process("plugin_ui::create::tests::create_inner");
    }

    #[test]
    fn create_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-create-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
        std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let opened: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        super::super::api::set_hooks(super::super::api::Hooks {
            hide: Rc::new(|| {}),
            harnesses: Rc::new(|| serde_json::json!([
                {"id": "codex", "name": "Codex", "icon": ""},
                {"id": "claude", "name": "Claude", "icon": ""}
            ])),
            launch: Rc::new(|_, _| Ok(())),
            cards: Rc::new(|| serde_json::json!([])),
            focus_card: Rc::new(|_| Ok(())),
            card_session: Rc::new(|_| None),
            open_card: Rc::new({
                let opened = Rc::clone(&opened);
                move |harness: &str, folder: &str| {
                    opened.borrow_mut().push((harness.to_owned(), folder.to_owned()));
                    // No session: the request is not typed in this test.
                    Ok("sd-test-no-such-session".into())
                }
            }),
        });
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&gtk4::Box::new(Orientation::Vertical, 0)));
        let ceiling = gtk4::Box::new(Orientation::Vertical, 0);
        ceiling.set_visible(false);
        overlay.add_overlay(&ceiling);
        crate::floating_panel::set_host(&overlay, Rc::new(|| 46), &ceiling);
        let window = gtk4::Window::new();
        window.set_default_size(1000, 700);
        window.set_child(Some(&overlay));
        window.present();

        open();
        crate::gtk_test::pump(100);
        let widgets = crate::gtk_test::descendants(overlay.upcast_ref());
        let find_button = |label: &str| widgets.iter()
            .filter_map(|w| w.clone().downcast::<gtk4::Button>().ok())
            .find(|b| b.label().as_deref() == Some(label))
            .unwrap_or_else(|| panic!("no {label} button"));
        let request = widgets.iter().find_map(|w| w.clone().downcast::<gtk4::TextView>().ok()).unwrap();
        let place = widgets.iter().filter_map(|w| w.clone().downcast::<gtk4::DropDown>().ok()).next().unwrap();
        let harness = widgets.iter().filter_map(|w| w.clone().downcast::<gtk4::DropDown>().ok()).nth(1).unwrap();
        // Claude is preferred even when it is not listed first.
        assert_eq!(harness.selected(), 1);

        // Nothing is made without a request.
        find_button("✨ Create").emit_clicked();
        assert!(opened.borrow().is_empty());

        request.buffer().set_text("A Pomodoro timer in the top bar");
        place.set_selected(2); // toolbar.item
        find_button("✨ Create").emit_clicked();
        let dir = root.join("plugins/a-pomodoro-timer-in");
        assert_eq!(opened.borrow().as_slice(), &[("claude".to_owned(), dir.display().to_string())]);
        let manifest = super_desktop::plugins::manifest::load(&dir).unwrap();
        assert_eq!(manifest.surfaces[0].slot, super_desktop::plugins::manifest::Slot::ToolbarItem);
        assert_eq!(manifest.description, "A Pomodoro timer in the top bar");
        let agents = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(agents.contains("A Pomodoro timer in the top bar"));
        assert!(dir.join(".claude/skills/super-desktop-plugin/SKILL.md").is_file());
        assert_eq!(preferred_harness().as_deref(), Some("claude"));
        // The form closes at once (within a second): the agent's card is in view.
        let form_open = || crate::gtk_test::descendants(overlay.upcast_ref()).iter()
            .filter_map(|w| w.clone().downcast::<gtk4::Button>().ok())
            .any(|b| b.label().as_deref() == Some("✨ Create"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while form_open() && std::time::Instant::now() < deadline {
            crate::gtk_test::pump(20);
        }
        assert!(!form_open(), "the Create Plugin form stayed open after Create");
        let _ = std::fs::remove_dir_all(&root);
    }
}
