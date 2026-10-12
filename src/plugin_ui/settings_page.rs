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
    // A renderer the user switched off with its toggle stays off until toggled.
    let state = match &loaded {
        Ok(m) if m.contributes.renderer.is_some() && live.is_some() && !super::manager().renderer_running(&installed.id) => {
            let how = m.contributes.commands.iter().find(|c| c.action.as_deref() == Some("renderer.toggle")).map_or(String::new(), |c| format!(" Turn it on with “{}”.", c.title));
            format!("{state}, effect off.{how}")
        }
        _ => state,
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
                if let Err(why) = cli::installed_manifest(&installed)
                    .and_then(|m| cli::check_activatable(&installed, &m).and_then(|_| cli::check_exclusive(&m, &cli::active_manifests())))
                {
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
        for shortcut in &manifest.contributes.shortcuts {
            card.append(&shortcut_row(installed, manifest, shortcut));
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

/// One shortcut: on/off, its key (Hyprland spelling), and why it is not
/// active when it is not. Saved as an override in `plugins.json`.
fn shortcut_row(installed: &store::Installed, manifest: &Manifest, shortcut: &crate::plugin_host::manifest::Shortcut) -> gtk4::Widget {
    let row = Box::new(Orientation::Horizontal, 8);
    let override_value = installed.shortcuts.get(&shortcut.id).cloned();
    let combo = match &override_value {
        Some(Some(combo)) => combo.clone(),
        _ => shortcut.default.clone(),
    };
    let switch = gtk4::Switch::new();
    switch.set_valign(Align::Center);
    switch.set_active(!matches!(override_value, Some(None)));
    let title = manifest.contributes.commands.iter().find(|c| c.id == shortcut.command).map(|c| c.title.as_str()).unwrap_or(&shortcut.command);
    let scope = if shortcut.scope == "global" { "everywhere" } else { "inside SUPER DESKTOP" };
    let label = Label::new(Some(&format!("⌨ {title} ({scope})")));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_wrap(true);
    let entry = gtk4::Entry::new();
    entry.set_text(&combo);
    entry.set_width_chars(18);
    entry.set_tooltip_text(Some("Write it like Hyprland: SUPER + SHIFT + G, or F7. Press Enter to save."));
    row.append(&switch);
    row.append(&label);
    row.append(&entry);
    let outer = Box::new(Orientation::Vertical, 2);
    outer.append(&row);
    let problem = note(&super::manager().shortcut_problem(&shortcut.id).unwrap_or_default());
    problem.add_css_class("launcher-note-error");
    problem.set_visible(!problem.text().is_empty());
    outer.append(&problem);
    let save: Rc<dyn Fn(Option<String>)> = {
        let (id, key) = (installed.id.clone(), shortcut.id.clone());
        Rc::new(move |value: Option<String>| {
            let (id, key) = (id.clone(), key.clone());
            let _ = store::update(move |s| {
                if let Some(p) = s.get_mut(&id) {
                    p.shortcuts.insert(key, value);
                }
            });
            glib::idle_add_local_once(|| super::manager().apply_shortcuts());
        })
    };
    {
        let (save, entry) = (Rc::clone(&save), entry.clone());
        switch.connect_active_notify(move |s| save(s.is_active().then(|| entry.text().to_string())));
    }
    {
        let (save, switch, problem) = (Rc::clone(&save), switch.clone(), problem.clone());
        entry.connect_activate(move |e| {
            let combo = e.text().trim().to_string();
            if crate::plugin_host::manifest::is_combo(&combo) {
                problem.set_visible(false);
                switch.set_active(true);
                save(Some(combo));
            } else {
                problem.set_text("Write it like Hyprland: SUPER + SHIFT + G, or F7.");
                problem.set_visible(true);
            }
        });
    }
    outer.upcast()
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
        "path" => {
            let outer = Box::new(Orientation::Vertical, 4);
            let row = Box::new(Orientation::Horizontal, 6);
            let entry = gtk4::Entry::new();
            entry.set_hexpand(true);
            entry.set_text(value.and_then(Value::as_str).unwrap_or(""));
            let choose = Button::with_label("Choose…");
            choose.add_css_class("launcher-btn");
            row.append(&entry);
            row.append(&choose);
            outer.append(&row);
            let browser = path_browser(setting.path_kind.as_deref() != Some("file"), {
                let (entry, save) = (entry.clone(), Rc::clone(&save));
                Rc::new(move |path: std::path::PathBuf| {
                    entry.set_text(&path.display().to_string());
                    save(Value::String(path.display().to_string()));
                })
            });
            let open = Rc::clone(&browser.open);
            choose.connect_clicked(move |_| open());
            {
                let save = Rc::clone(&save);
                entry.connect_activate(move |e| save(Value::String(e.text().to_string())));
            }
            outer.append(&browser.widget);
            outer.upcast()
        }
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

/// A list of folders (or files): each row has ✕, and "Add…" opens a browser
/// inside the card.
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
    let browser = path_browser(directory, {
        let (paths, save, render) = (Rc::clone(&paths), Rc::clone(&save), Rc::clone(&render));
        Rc::new(move |path: std::path::PathBuf| {
            let text = path.display().to_string();
            if !paths.borrow().contains(&text) {
                paths.borrow_mut().push(text);
                save(Value::Array(paths.borrow().iter().map(|p| Value::String(p.clone())).collect()));
                render();
            }
        })
    });
    {
        let open = Rc::clone(&browser.open);
        add.connect_clicked(move |_| open());
    }
    outer.append(&add);
    outer.append(&browser.widget);
    outer.upcast()
}

/// An in-card file or folder browser.
///
/// Not a system file dialog: SUPER DESKTOP is a layer-shell overlay, and a
/// portal dialog needs a normal window to belong to (GTK exits when it tries to
/// export the overlay's surface for one) and would open underneath the
/// overlay anyway.
struct PathBrowser {
    widget: Box,
    open: Rc<dyn Fn()>,
}

/// The sub-folders (and, for files, the files) of `dir`, sorted, hidden ones
/// left out: (name, is a folder).
fn list_dir(dir: &std::path::Path, directories_only: bool) -> Result<Vec<(String, bool)>, String> {
    let mut entries: Vec<(String, bool)> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot open {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.path().is_dir();
            (!name.starts_with('.') && (is_dir || !directories_only)).then_some((name, is_dir))
        })
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    entries.truncate(500);
    Ok(entries)
}

/// `~`, `~/x` and absolute paths; anything else is relative to the home folder.
fn expand_path(text: &str) -> std::path::PathBuf {
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
    let text = text.trim();
    match text.strip_prefix('~') {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None if text.starts_with('/') => std::path::PathBuf::from(text),
        None => home.join(text),
    }
}

fn path_browser(directory: bool, on_pick: Rc<dyn Fn(std::path::PathBuf)>) -> PathBrowser {
    let widget = Box::new(Orientation::Vertical, 6);
    widget.add_css_class("plugin-path-browser");
    widget.set_visible(false);
    let top = Box::new(Orientation::Horizontal, 6);
    let up = Button::with_label("↑");
    up.set_tooltip_text(Some("Parent folder"));
    up.add_css_class("term-btn");
    let entry = gtk4::Entry::new();
    entry.set_hexpand(true);
    entry.set_placeholder_text(Some("Type a path, then Enter"));
    top.append(&up);
    top.append(&entry);
    widget.append(&top);
    let list = Box::new(Orientation::Vertical, 0);
    let scroll = gtk4::ScrolledWindow::builder().hscrollbar_policy(gtk4::PolicyType::Never).min_content_height(160).max_content_height(240).propagate_natural_height(true).child(&list).build();
    widget.append(&scroll);
    let message = note("");
    message.set_visible(false);
    widget.append(&message);
    let bottom = Box::new(Orientation::Horizontal, 6);
    let pick = Button::with_label(if directory { "Add this folder" } else { "Add the selected file" });
    pick.add_css_class("launcher-btn");
    pick.add_css_class("launcher-btn-primary");
    let cancel = Button::with_label("Cancel");
    cancel.add_css_class("launcher-btn");
    bottom.append(&pick);
    bottom.append(&cancel);
    widget.append(&bottom);

    let current: Rc<std::cell::RefCell<std::path::PathBuf>> = Rc::new(std::cell::RefCell::new(expand_path("~")));
    let chosen_file: Rc<std::cell::RefCell<Option<std::path::PathBuf>>> = Rc::default();
    let show: Rc<std::cell::RefCell<Option<Rc<dyn Fn(std::path::PathBuf)>>>> = Rc::default();
    let navigate: Rc<dyn Fn(std::path::PathBuf)> = {
        let (list, entry, message, current, chosen_file, pick, show) =
            (list.clone(), entry.clone(), message.clone(), Rc::clone(&current), Rc::clone(&chosen_file), pick.clone(), Rc::clone(&show));
        Rc::new(move |dir: std::path::PathBuf| {
            clear(&list);
            chosen_file.replace(None);
            match list_dir(&dir, directory) {
                Ok(entries) => {
                    message.set_visible(false);
                    current.replace(dir.clone());
                    entry.set_text(&dir.display().to_string());
                    if entries.is_empty() {
                        list.append(&note(if directory { "No sub-folders." } else { "Empty folder." }));
                    }
                    for (name, is_dir) in entries {
                        let button = Button::with_label(&format!("{} {name}", if is_dir { "📁" } else { "📄" }));
                        button.add_css_class("flat");
                        button.set_halign(Align::Start);
                        let target = dir.join(&name);
                        let (show, chosen_file, pick) = (Rc::clone(&show), Rc::clone(&chosen_file), pick.clone());
                        button.connect_clicked(move |_| {
                            if is_dir {
                                if let Some(show) = show.borrow().clone() {
                                    show(target.clone());
                                }
                            } else {
                                chosen_file.replace(Some(target.clone()));
                                pick.set_label(&format!("Add {}", target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                            }
                        });
                        list.append(&button);
                    }
                }
                Err(why) => {
                    message.set_text(&why);
                    message.set_visible(true);
                }
            }
            pick.set_sensitive(directory || chosen_file.borrow().is_some());
        })
    };
    show.replace(Some(Rc::clone(&navigate)));
    {
        let (navigate, current) = (Rc::clone(&navigate), Rc::clone(&current));
        up.connect_clicked(move |_| {
            let parent = current.borrow().parent().map(std::path::Path::to_path_buf);
            if let Some(parent) = parent {
                navigate(parent);
            }
        });
    }
    {
        let navigate = Rc::clone(&navigate);
        entry.connect_activate(move |e| navigate(expand_path(&e.text())));
    }
    {
        let (widget, current, chosen_file) = (widget.clone(), Rc::clone(&current), Rc::clone(&chosen_file));
        pick.connect_clicked(move |_| {
            let picked = if directory { Some(current.borrow().clone()) } else { chosen_file.borrow().clone() };
            if let Some(path) = picked {
                on_pick(path);
                widget.set_visible(false);
            }
        });
    }
    {
        let widget = widget.clone();
        cancel.connect_clicked(move |_| widget.set_visible(false));
    }
    let open: Rc<dyn Fn()> = {
        let (widget, navigate, current, entry) = (widget.clone(), Rc::clone(&navigate), Rc::clone(&current), entry.clone());
        Rc::new(move || {
            let start = current.borrow().clone();
            navigate(start);
            widget.set_visible(true);
            entry.grab_focus();
        })
    };
    PathBrowser { widget, open }
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
                renderer_off: false,
                extra: Default::default(),
            })
        })
        .unwrap();
        gtk4::init().unwrap();
        let page = build();
        let root: gtk4::Widget = page.widget.clone().upcast();
        // The settings' own fields, not the path field inside a folder browser.
        let in_browser = |w: &gtk4::Widget| {
            let mut parent = w.parent();
            while let Some(p) = parent {
                if p.has_css_class("plugin-path-browser") {
                    return true;
                }
                parent = p.parent();
            }
            false
        };
        let entries: Vec<gtk4::Entry> = find::<gtk4::Entry>(&root).into_iter().filter(|e| !in_browser(e.upcast_ref())).collect();
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

        // Folders are picked in the card, never through a system dialog.
        let tree = home.join("code/web");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::create_dir_all(home.join("code/.hidden")).unwrap();
        let add = find::<gtk4::Button>(&root).into_iter().find(|b| b.label().as_deref() == Some("Add folder…")).unwrap();
        add.emit_clicked();
        let browser = find::<gtk4::Box>(&root).into_iter().find(|b| b.has_css_class("plugin-path-browser")).unwrap();
        assert!(browser.is_visible());
        let path_entry = find::<gtk4::Entry>(&browser.clone().upcast()).into_iter().next().unwrap();
        path_entry.set_text("~/code");
        path_entry.emit_activate();
        let names: Vec<String> = find::<gtk4::Button>(&browser.clone().upcast()).iter().filter_map(|b| b.label()).map(|l| l.to_string()).collect();
        assert!(names.contains(&"📁 web".to_string()), "{names:?}");
        assert!(!names.iter().any(|n| n.contains(".hidden")), "hidden folders are not listed");
        find::<gtk4::Button>(&browser.clone().upcast()).into_iter().find(|b| b.label().as_deref() == Some("📁 web")).unwrap().emit_clicked();
        assert_eq!(path_entry.text(), tree.display().to_string());
        find::<gtk4::Button>(&browser.clone().upcast()).into_iter().find(|b| b.label().as_deref() == Some("Add this folder")).unwrap().emit_clicked();
        assert!(!browser.is_visible());
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".config/super-desktop/plugins/form/settings.json")).unwrap()).unwrap();
        assert_eq!(saved["folders"], serde_json::json!(["/srv/a", tree.display().to_string()]));

        switches[0].set_active(true);
        assert!(store::Store::load().get("form").unwrap().active, "the switch turns it on");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Settings → Plugins with a real plugin, rendered to a PNG on the private
    /// display: `SD_SCREENSHOT_PLUGIN=<plugin dir> SD_SCREENSHOT_OUT=out.png
    /// cargo test plugin_settings_screenshot -- --nocapture`.
    #[test]
    fn plugin_settings_screenshot() {
        if std::env::var_os("SD_SCREENSHOT_PLUGIN").is_none() {
            return;
        }
        crate::gtk_test::run_in_child_process("plugin_ui::settings_page::tests::plugin_settings_screenshot_inner");
    }

    #[test]
    fn plugin_settings_screenshot_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let (Some(plugin), Some(out)) = (std::env::var_os("SD_SCREENSHOT_PLUGIN"), std::env::var_os("SD_SCREENSHOT_OUT")) else { return };
        let plugin = std::path::PathBuf::from(plugin);
        // The real theme is read before HOME moves to a scratch folder.
        gtk4::init().unwrap();
        if std::env::var_os("SD_SCREENSHOT_DARK").is_some() {
            // What an Omarchy desktop sets (Adwaita-dark); Broadway starts light.
            gtk4::Settings::default().unwrap().set_gtk_application_prefer_dark_theme(true);
        }
        crate::styles::apply_styles();
        let home = std::env::temp_dir().join(format!("sd-settings-shot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("Github/super-desktop")).unwrap();
        std::fs::create_dir_all(home.join("Github/website")).unwrap();
        // SAFETY: single-threaded child process.
        unsafe { std::env::set_var("HOME", &home) };
        let manifest = crate::plugin_host::manifest::load_dir(&plugin).manifest.unwrap();
        store::update(|s| {
            s.plugins.push(store::Installed {
                id: manifest.id.clone(),
                dir: plugin.clone(),
                source: store::Source::Linked { path: plugin.clone() },
                version: manifest.version.clone(),
                active: true,
                granted: manifest.permissions.clone(),
                shortcuts: Default::default(),
                renderer_off: false,
                extra: Default::default(),
            })
        })
        .unwrap();
        store::set_setting(&manifest, "folders", serde_json::json!([home.join("Github").display().to_string()])).unwrap();
        let page = build();
        let root: gtk4::Widget = page.widget.clone().upcast();
        if let Some(add) = find::<gtk4::Button>(&root).into_iter().find(|b| b.label().as_deref() == Some("Add folder…")) {
            add.emit_clicked();
            if let Some(entry) = find::<gtk4::Box>(&root).into_iter().find(|b| b.has_css_class("plugin-path-browser")).and_then(|b| find::<gtk4::Entry>(&b.upcast()).into_iter().next()) {
                entry.set_text("~/Github");
                entry.emit_activate();
            }
        }
        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_child(Some(&page.widget));
        scroll.add_css_class("harness-page");
        let window = gtk4::Window::new();
        window.add_css_class("super-desktop");
        window.set_default_size(820, 1000);
        let card = gtk4::Box::new(Orientation::Vertical, 0);
        card.add_css_class("mini-terminal");
        card.add_css_class("harness-panel");
        scroll.set_vexpand(true);
        card.append(&scroll);
        window.set_child(Some(&card));
        window.present();
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(600) || (card.width() == 0 && start.elapsed() < std::time::Duration::from_secs(5)) {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let paintable = gtk4::WidgetPaintable::new(Some(&card));
        let node = loop {
            let snapshot = gtk4::Snapshot::new();
            paintable.snapshot(&snapshot, card.width() as f64, card.height() as f64);
            if let Some(node) = snapshot.to_node() {
                break node;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(8), "nothing was drawn");
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let renderer = gtk4::gsk::CairoRenderer::new();
        renderer.realize(None::<&gtk4::gdk::Surface>).unwrap();
        renderer.render_texture(&node, None).save_to_png(out).unwrap();
        renderer.unrealize();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn plugin_settings_paths_are_listed_and_expanded() {
        let dir = std::env::temp_dir().join(format!("sd-list-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["b", "A", ".git"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("file.txt"), "").unwrap();
        assert_eq!(list_dir(&dir, true).unwrap(), vec![("A".to_string(), true), ("b".to_string(), true)]);
        assert_eq!(list_dir(&dir, false).unwrap().last().unwrap(), &("file.txt".to_string(), false));
        assert!(list_dir(&dir.join("missing"), true).unwrap_err().contains("cannot open"));
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        assert_eq!(expand_path("~"), home);
        assert_eq!(expand_path("~/code"), home.join("code"));
        assert_eq!(expand_path("code"), home.join("code"));
        assert_eq!(expand_path("/srv"), std::path::PathBuf::from("/srv"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
