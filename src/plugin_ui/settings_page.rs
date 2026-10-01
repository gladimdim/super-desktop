//! Settings → Plugins: the AI provider plugins use, and every installed
//! plugin with an on/off switch, its state, its permissions in plain words
//! and a settings form generated from its manifest.
use crate::plugin_host::manifest::{Manifest, Setting};
use crate::plugin_host::{cli, llm, store};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, Label, Orientation};
use serde_json::Value;
use std::rc::Rc;

pub struct Page {
    pub widget: Box,
    pub refresh: Rc<dyn Fn()>,
}

pub fn build() -> Page {
    let root = Box::new(Orientation::Vertical, 10);
    root.add_css_class("launcher-body");
    let (_, provider_body) = crate::launcher_settings::section_card(&root, "", "AI provider for plugins");
    let provider_note = note("Plugins that write text (commit messages, summaries) ask this provider. They never see its credentials.");
    provider_body.append(&provider_note);
    let provider_row = Box::new(Orientation::Horizontal, 8);
    provider_body.append(&provider_row);

    let (_, list_body) = crate::launcher_settings::section_card(&root, "", "Installed plugins");
    let list = Box::new(Orientation::Vertical, 10);
    list_body.append(&list);
    let footer = note("Add a plugin folder with `super-desktop plugin link <folder>`; `super-desktop plugin describe` lists what this build supports. Plugins run as you.");
    list_body.append(&footer);

    let refresh: Rc<dyn Fn()> = {
        let (list, provider_row) = (list.clone(), provider_row.clone());
        Rc::new(move || {
            clear(&provider_row);
            provider_row.append(&provider_choice());
            clear(&list);
            let store = store::Store::load();
            if store.plugins.is_empty() {
                list.append(&note("No plugins installed."));
            }
            for installed in &store.plugins {
                list.append(&plugin_card(installed));
            }
        })
    };
    {
        let refresh = Rc::clone(&refresh);
        super::manager().on_change(Rc::new(move || {
            // Only while the page is on screen; it refreshes when shown.
            refresh()
        }));
    }
    refresh();
    Page { widget: root, refresh }
}

fn clear(container: &Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn note(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.add_css_class("launcher-hint");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}

fn provider_choice() -> gtk4::Widget {
    let installed = llm::available();
    let chosen = store::Store::load().llm_provider;
    let mut options = vec![("auto".to_string(), match installed.first() {
        Some(first) => format!("Automatic ({first})"),
        None => "Automatic (none installed)".to_string(),
    })];
    for provider in llm::PROVIDERS {
        let name = match provider {
            "claude" => "Claude Code",
            _ => "Codex",
        };
        let state = if installed.contains(&provider) { "" } else { " — not installed" };
        options.push((provider.to_string(), format!("{name}{state}")));
    }
    let labels: Vec<&str> = options.iter().map(|(_, label)| label.as_str()).collect();
    let dropdown = gtk4::DropDown::from_strings(&labels);
    if let Some(index) = options.iter().position(|(id, _)| *id == chosen) {
        dropdown.set_selected(index as u32);
    }
    dropdown.connect_selected_notify(move |dropdown| {
        if let Some((id, _)) = options.get(dropdown.selected() as usize) {
            let id = id.clone();
            let _ = store::update(|s| s.llm_provider = id);
        }
    });
    dropdown.upcast()
}

fn plugin_card(installed: &store::Installed) -> gtk4::Widget {
    let card = Box::new(Orientation::Vertical, 6);
    card.add_css_class("plugin-settings-card");
    let head = Box::new(Orientation::Horizontal, 10);
    let titles = Box::new(Orientation::Vertical, 2);
    titles.set_hexpand(true);
    let loaded = cli::installed_manifest(installed);
    let name = loaded.as_ref().map(|m| m.name.clone()).unwrap_or_else(|_| installed.id.clone());
    let heading = Label::new(Some(&format!("{name}  ·  {}", installed.version)));
    heading.add_css_class("settings-entry-title");
    heading.set_xalign(0.0);
    titles.append(&heading);
    let live = super::manager().rows().into_iter().find(|row| row.0 == installed.id);
    let state = match (&live, installed.active) {
        (Some((_, _, _, state, detail)), _) if state == "failed" => format!("Failed: {detail}"),
        (Some((_, _, _, state, _)), _) => format!("On ({state})"),
        (None, true) => "On, but not running: see `super-desktop plugin logs`".to_string(),
        (None, false) => "Off".to_string(),
    };
    let summary = match &loaded {
        Ok(m) => format!("{} — {state}", m.description),
        Err(why) => format!("Manifest problem: {why}"),
    };
    let detail = Label::new(Some(&summary));
    detail.add_css_class("settings-entry-summary");
    detail.set_xalign(0.0);
    detail.set_wrap(true);
    titles.append(&detail);
    head.append(&titles);
    let switch = gtk4::Switch::new();
    switch.set_valign(Align::Center);
    switch.set_active(installed.active);
    switch.set_tooltip_text(Some("Turn the plugin on or off. Off removes everything it added."));
    head.append(&switch);
    card.append(&head);
    let message = note("");
    message.set_visible(false);
    card.append(&message);
    {
        let installed = installed.clone();
        let message = message.clone();
        switch.connect_state_set(move |switch, on| {
            if on {
                if let Err(why) = cli::installed_manifest(&installed).and_then(|m| cli::check_activatable(&installed, &m)) {
                    message.set_text(&why);
                    message.set_visible(true);
                    switch.set_active(false);
                    return glib::Propagation::Stop;
                }
            }
            let id = installed.id.clone();
            let _ = store::update(|s| {
                if let Some(p) = s.get_mut(&id) {
                    p.active = on;
                }
            });
            message.set_visible(false);
            // After this handler: sync rebuilds the page through on_change.
            glib::idle_add_local_once(|| super::manager().sync());
            glib::Propagation::Proceed
        });
    }
    if let Ok(manifest) = &loaded {
        let permissions: Vec<&str> = manifest.permissions.iter().map(|p| cli::permission_words(p)).collect();
        if !permissions.is_empty() {
            card.append(&note(&format!("It {}.", permissions.join(", "))));
        }
        if !manifest.contributes.settings.is_empty() {
            let form = settings_form(manifest);
            card.append(&form);
        }
    }
    let log = note(&format!("Log: {}", crate::plugin_host::log_file(&installed.id).display()));
    log.set_selectable(true);
    card.append(&log);
    card.upcast()
}

/// A form for the plugin's declared settings. Each change is checked against
/// its declaration, saved, and sent to the running plugin.
fn settings_form(manifest: &Manifest) -> gtk4::Widget {
    let form = Box::new(Orientation::Vertical, 8);
    form.add_css_class("plugin-settings-form");
    let values = store::settings(manifest);
    let manifest = Rc::new(manifest.clone());
    for setting in &manifest.contributes.settings {
        let row = Box::new(Orientation::Vertical, 4);
        let title = Label::new(Some(&setting.title));
        title.set_xalign(0.0);
        title.add_css_class("plugin-setting-title");
        row.append(&title);
        if let Some(description) = &setting.description {
            row.append(&note(description));
        }
        let error = note("");
        error.add_css_class("launcher-note-error");
        error.set_visible(false);
        let save: Rc<dyn Fn(Value)> = {
            let (manifest, key, error) = (Rc::clone(&manifest), setting.key.clone(), error.clone());
            Rc::new(move |value: Value| match store::set_setting(&manifest, &key, value) {
                Ok(()) => {
                    error.set_visible(false);
                    super::manager().settings_changed(&manifest.id);
                }
                Err(why) => {
                    error.set_text(&why);
                    error.set_visible(true);
                }
            })
        };
        row.append(&editor(setting, values.get(&setting.key), save));
        row.append(&error);
        form.append(&row);
    }
    form.upcast()
}

fn editor(setting: &Setting, value: Option<&Value>, save: Rc<dyn Fn(Value)>) -> gtk4::Widget {
    match setting.kind.as_str() {
        "bool" => {
            let switch = gtk4::Switch::new();
            switch.set_halign(Align::Start);
            switch.set_active(value.and_then(Value::as_bool).unwrap_or(false));
            switch.connect_active_notify(move |s| save(Value::Bool(s.is_active())));
            switch.upcast()
        }
        "number" => {
            let (min, max) = (setting.min.unwrap_or(f64::MIN / 2.0), setting.max.unwrap_or(f64::MAX / 2.0));
            let spin = gtk4::SpinButton::with_range(min, max, 1.0);
            spin.set_halign(Align::Start);
            spin.set_value(value.and_then(Value::as_f64).unwrap_or(min.max(0.0)));
            spin.connect_value_changed(move |s| save(serde_json::json!(s.value())));
            spin.upcast()
        }
        "enum" => {
            let values = setting.values.clone().unwrap_or_default();
            let labels: Vec<&str> = values.iter().map(String::as_str).collect();
            let dropdown = gtk4::DropDown::from_strings(&labels);
            dropdown.set_halign(Align::Start);
            if let Some(index) = value.and_then(Value::as_str).and_then(|v| values.iter().position(|x| x == v)) {
                dropdown.set_selected(index as u32);
            }
            dropdown.connect_selected_notify(move |d| {
                if let Some(v) = values.get(d.selected() as usize) {
                    save(Value::String(v.clone()));
                }
            });
            dropdown.upcast()
        }
        "paths" => paths_editor(setting, value, save),
        kind => {
            let entry = if kind == "secret" { gtk4::PasswordEntry::new().upcast::<gtk4::Widget>() } else { gtk4::Entry::new().upcast() };
            let editable = entry.clone().dynamic_cast::<gtk4::Editable>().expect("entries are editable");
            if kind == "secret" {
                editable.set_text("");
                entry.set_tooltip_text(Some("Stored only on this PC, readable by you alone. Type a new value and press Enter."));
            } else {
                editable.set_text(value.and_then(Value::as_str).unwrap_or(""));
            }
            entry.set_hexpand(true);
            let commit = {
                let save = Rc::clone(&save);
                move |e: &gtk4::Editable| save(Value::String(e.text().to_string()))
            };
            if let Some(e) = entry.downcast_ref::<gtk4::Entry>() {
                let commit = commit.clone();
                e.connect_activate(move |e| commit(e.upcast_ref()));
            }
            if let Some(e) = entry.downcast_ref::<gtk4::PasswordEntry>() {
                let commit = commit.clone();
                e.connect_activate(move |e| commit(e.upcast_ref()));
            }
            // Saving on leaving the field too: nobody expects to press Enter.
            let focus = gtk4::EventControllerFocus::new();
            let leave = editable.clone();
            focus.connect_leave(move |_| commit(&leave));
            entry.add_controller(focus);
            entry
        }
    }
}

/// A list of folders (or files): each row has ✕, and "Add…" opens a chooser.
fn paths_editor(setting: &Setting, value: Option<&Value>, save: Rc<dyn Fn(Value)>) -> gtk4::Widget {
    let directory = setting.path_kind.as_deref() != Some("file");
    let paths: Rc<std::cell::RefCell<Vec<String>>> = Rc::new(std::cell::RefCell::new(
        value.and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default(),
    ));
    let outer = Box::new(Orientation::Vertical, 4);
    let rows = Box::new(Orientation::Vertical, 2);
    outer.append(&rows);
    let paint: Rc<std::cell::RefCell<Option<Rc<dyn Fn()>>>> = Rc::default();
    let render: Rc<dyn Fn()> = {
        let (rows, paths, save, paint) = (rows.clone(), Rc::clone(&paths), Rc::clone(&save), Rc::clone(&paint));
        Rc::new(move || {
            clear(&rows);
            for (index, path) in paths.borrow().iter().enumerate() {
                let row = Box::new(Orientation::Horizontal, 6);
                let label = Label::new(Some(path));
                label.set_xalign(0.0);
                label.set_hexpand(true);
                label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
                let remove = Button::with_label("✕");
                remove.add_css_class("term-btn");
                remove.set_tooltip_text(Some("Remove"));
                let (paths, save, paint) = (Rc::clone(&paths), Rc::clone(&save), Rc::clone(&paint));
                remove.connect_clicked(move |_| {
                    paths.borrow_mut().remove(index);
                    save(Value::Array(paths.borrow().iter().map(|p| Value::String(p.clone())).collect()));
                    if let Some(paint) = paint.borrow().clone() {
                        paint();
                    }
                });
                row.append(&label);
                row.append(&remove);
                rows.append(&row);
            }
        })
    };
    paint.replace(Some(Rc::clone(&render)));
    render();
    let add = Button::with_label(if directory { "Add folder…" } else { "Add file…" });
    add.add_css_class("launcher-btn");
    add.set_halign(Align::Start);
    add.connect_clicked(move |button| {
        let dialog = gtk4::FileDialog::new();
        dialog.set_modal(true);
        let parent = button.root().and_downcast::<gtk4::Window>();
        let (paths, save, render) = (Rc::clone(&paths), Rc::clone(&save), Rc::clone(&render));
        let done = move |result: Result<gtk4::gio::File, glib::Error>| {
            let Some(path) = result.ok().and_then(|f| f.path()) else { return };
            let text = path.display().to_string();
            if !paths.borrow().contains(&text) {
                paths.borrow_mut().push(text);
                save(Value::Array(paths.borrow().iter().map(|p| Value::String(p.clone())).collect()));
                render();
            }
        };
        if directory {
            dialog.select_folder(parent.as_ref(), gtk4::gio::Cancellable::NONE, done);
        } else {
            dialog.open(parent.as_ref(), gtk4::gio::Cancellable::NONE, done);
        }
    });
    outer.append(&add);
    outer.upcast()
}

/// For the hub entry's chip: plugins on / installed.
pub fn count_summary() -> String {
    let store = store::Store::load();
    format!("{}/{}", store.plugins.iter().filter(|p| p.active).count(), store.plugins.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_settings_page_edits_and_switches() {
        crate::gtk_test::run_in_child_process("plugin_ui::settings_page::tests::plugin_settings_page_inner");
    }

    fn find<T: IsA<gtk4::Widget>>(root: &gtk4::Widget) -> Vec<T> {
        let mut found = Vec::new();
        let mut child = root.first_child();
        while let Some(widget) = child {
            if let Ok(hit) = widget.clone().downcast::<T>() {
                found.push(hit);
            }
            found.extend(find::<T>(&widget));
            child = widget.next_sibling();
        }
        found
    }

    #[test]
    fn plugin_settings_page_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        // A private HOME: this child process is single-threaded.
        let home = std::env::temp_dir().join(format!("sd-settings-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let plugin = home.join("plugin");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("main.py"), "").unwrap();
        std::fs::write(
            plugin.join(crate::plugin_host::manifest::FILE_NAME),
            serde_json::json!({
                "manifestVersion": 1, "id": "form", "name": "Form", "version": "0.1.0", "description": "Settings form test.",
                "engines": {"superDesktop": ">=1.0.0", "pluginApi": "1"},
                "main": {"command": ["python3", "main.py"]},
                "permissions": ["llm"],
                "contributes": {"settings": [
                    {"key": "name", "type": "string", "title": "Name", "default": "x"},
                    {"key": "on", "type": "bool", "title": "On", "default": true},
                    {"key": "n", "type": "number", "title": "N", "min": 1, "max": 9, "default": 3},
                    {"key": "mode", "type": "enum", "title": "Mode", "values": ["a", "b"], "default": "b"},
                    {"key": "folders", "type": "paths", "kind": "directory", "title": "Folders", "default": ["/srv/a"]}
                ]}
            })
            .to_string(),
        )
        .unwrap();
        // SAFETY: single-threaded child process, before GTK starts threads.
        unsafe { std::env::set_var("HOME", &home) };
        store::update(|s| {
            s.plugins.push(store::Installed {
                id: "form".into(),
                dir: plugin.clone(),
                source: store::Source::Linked { path: plugin.clone() },
                version: "0.1.0".into(),
                active: false,
                granted: vec!["llm".into()],
                shortcuts: Default::default(),
                extra: Default::default(),
            })
        })
        .unwrap();
        gtk4::init().unwrap();
        let page = build();
        let root: gtk4::Widget = page.widget.clone().upcast();
        let entries = find::<gtk4::Entry>(&root);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text(), "x");
        let switches = find::<gtk4::Switch>(&root);
        // The plugin's on/off switch, then its bool setting.
        assert_eq!(switches.len(), 2);
        assert!(!switches[0].is_active() && switches[1].is_active());
        assert!(find::<gtk4::Label>(&root).iter().any(|l| l.text() == "/srv/a"));
        assert!(find::<gtk4::Label>(&root).iter().any(|l| l.text().contains("uses your AI provider")));

        entries[0].set_text("changed");
        entries[0].emit_activate();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".config/super-desktop/plugins/form/settings.json")).unwrap()).unwrap();
        assert_eq!(saved["name"], "changed");
        switches[1].set_active(false);
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".config/super-desktop/plugins/form/settings.json")).unwrap()).unwrap();
        assert_eq!(saved["on"], false);

        switches[0].set_active(true);
        assert!(store::Store::load().get("form").unwrap().active, "the switch turns it on");
        let _ = std::fs::remove_dir_all(&home);
    }
}
