use super::*;

pub(in crate::harness_settings) fn suggested_harness_name(executable: &str) -> Option<String> {
    let filename = Path::new(executable.trim()).file_name()?.to_str()?.trim();
    (!filename.is_empty()).then(|| filename.chars().take(48).collect())
}



pub(in crate::harness_settings) fn custom_row(item: &crate::custom_harness::CustomHarness) -> (Box, Button, Button, Button, Button) {
    let row = Box::new(Orientation::Horizontal, 8);
    row.add_css_class("harness-row");
    let icon = Label::new(Some(&item.icon));
    icon.add_css_class("harness-icon");
    row.append(&icon);
    let name = Label::new(Some(&item.name));
    name.add_css_class("harness-name");
    row.append(&name);
    let command = Label::new(Some(if item.available() { &item.executable } else { "Executable unavailable" }));
    command.add_css_class("harness-cmd");
    command.set_xalign(0.0);
    command.set_hexpand(true);
    command.set_ellipsize(pango::EllipsizeMode::Middle);
    command.set_tooltip_text(Some(&item.command()));
    row.append(&command);
    let edit = Button::with_label("Edit");
    edit.add_css_class("launcher-btn");
    row.append(&edit);
    let clone = Button::with_label("Clone");
    clone.add_css_class("launcher-btn");
    clone.set_tooltip_text(Some("Copy this launcher into a custom launcher of its own to rename and edit"));
    row.append(&clone);
    let remove = Button::with_label("Remove");
    remove.add_css_class("launcher-btn");
    remove.add_css_class("launcher-btn-danger");
    row.append(&remove);
    let toggle = Button::new();
    toggle.add_css_class("harness-toggle");
    toggle.set_sensitive(item.available());
    row.append(&toggle);
    (row, toggle, edit, remove, clone)
}


/// The add / edit form of a custom launcher.
#[derive(Clone)]
pub(in crate::harness_settings) struct CustomForm {
    pub(in crate::harness_settings) name: gtk4::Entry,
    pub(in crate::harness_settings) path: gtk4::Entry,
    pub(in crate::harness_settings) args: gtk4::Entry,
    pub(in crate::harness_settings) status: Label,
    pub(in crate::harness_settings) selected_icon: Rc<Cell<usize>>,
    pub(in crate::harness_settings) paint_icons: Rc<dyn Fn()>,
    /// The launcher being edited; `None` adds a new one.
    pub(in crate::harness_settings) editing: Rc<RefCell<Option<String>>>,
}

impl CustomForm {
    /// Open the form empty for a new launcher, or filled from `item`.
    pub(in crate::harness_settings) fn open(&self, item: Option<&crate::custom_harness::CustomHarness>, nav: &dyn Fn(SettingsPage)) {
        *self.editing.borrow_mut() = item.map(|item| item.id.clone());
        for entry in [&self.name, &self.path, &self.args] { entry.remove_css_class("ws-entry-invalid"); }
        self.status.set_text(""); self.status.set_visible(false);
        match item {
            Some(item) => {
                self.name.set_text(&item.name); self.path.set_text(&item.executable);
                self.args.set_text(&crate::launch_args::display(&item.arguments));
                self.selected_icon.set(crate::custom_harness::ICONS.iter().position(|icon| *icon == item.icon).unwrap_or(0));
            }
            None => {
                self.name.set_text(""); self.path.set_text(""); self.args.set_text("");
                self.selected_icon.set(0);
            }
        }
        (self.paint_icons)(); nav(SettingsPage::CustomHarness); self.name.grab_focus();
    }
}

/// Settings → Harness launchers → Add a harness (or Edit).
pub(in crate::harness_settings) struct CustomPage {
    pub(in crate::harness_settings) root: Box,
    pub(in crate::harness_settings) form: CustomForm,
    pub(in crate::harness_settings) btn_save: Button,
    pub(in crate::harness_settings) btn_cancel: Button,
}

pub(in crate::harness_settings) fn custom_page(subtitle: &Label) -> CustomPage {
    let root = settings_page_root();
    let (_, custom_body) = section_card(&root, "", "Launcher details");
    let custom_form = Box::new(Orientation::Vertical, 8);
    custom_form.append(&hint("Choose an icon and an executable on this PC. The name defaults to the executable filename. Arguments are optional; use quotes to keep words together."));
    let form_status = Label::new(None);
    form_status.add_css_class("launcher-note");
    form_status.add_css_class("launcher-note-error");
    form_status.add_css_class("custom-harness-error");
    form_status.set_xalign(0.0);
    form_status.set_wrap(true);
    form_status.set_visible(false);
    custom_form.append(&form_status);
    let icon_choices = Box::new(Orientation::Horizontal, 6);
    let selected_icon = Rc::new(Cell::new(0usize));
    let icon_buttons: Rc<Vec<Button>> = Rc::new(crate::custom_harness::ICONS.iter().enumerate().map(|(index, icon)| {
        let button = Button::with_label(icon);
        button.add_css_class("launcher-btn");
        let selected = Rc::clone(&selected_icon);
        button.connect_clicked(move |_| selected.set(index));
        icon_choices.append(&button);
        button
    }).collect());
    let paint_icons: Rc<dyn Fn()> = {
        let selected = Rc::clone(&selected_icon);
        let buttons = Rc::clone(&icon_buttons);
        Rc::new(move || for (index, button) in buttons.iter().enumerate() {
            if selected.get() == index { button.add_css_class("launcher-btn-primary"); }
            else { button.remove_css_class("launcher-btn-primary"); }
        })
    };
    for button in icon_buttons.iter() {
        let paint = Rc::clone(&paint_icons);
        button.connect_clicked(move |_| paint());
    }
    paint_icons();
    custom_form.append(&icon_choices);
    // Field captions stay on one line.
    let field = |caption: &str, placeholder: &str| {
        let label = hint(caption);
        label.set_wrap(false);
        custom_form.append(&label);
        let entry = gtk4::Entry::new();
        entry.set_placeholder_text(Some(placeholder));
        entry
    };
    let custom_name = field("Name (defaults to executable name)", "Harness name");
    custom_name.set_max_length(48);
    custom_name.add_css_class("ws-entry");
    custom_form.append(&custom_name);
    let custom_path = field("Executable path", "/absolute/path/to/executable");
    custom_path.add_css_class("ws-entry");
    custom_form.append(&custom_path);
    let custom_args = field("Arguments (optional)", "Optional arguments, e.g. --model 'my model'");
    custom_args.add_css_class("ws-entry");
    custom_form.append(&custom_args);
    let form_actions = Box::new(Orientation::Horizontal, 8);
    let btn_save = Button::with_label("Save harness");
    btn_save.add_css_class("launcher-btn");
    btn_save.add_css_class("launcher-btn-primary");
    let btn_cancel = Button::with_label("Cancel");
    btn_cancel.add_css_class("launcher-btn");
    form_actions.append(&btn_save);
    form_actions.append(&btn_cancel);
    custom_form.append(&form_actions);
    custom_body.append(&custom_form);
    for entry in [&custom_name, &custom_path, &custom_args] {
        let status = form_status.clone();
        let subtitle = subtitle.clone();
        entry.connect_changed(move |entry| {
            entry.remove_css_class("ws-entry-invalid");
            status.set_visible(false);
            subtitle.remove_css_class("launcher-note-error");
            subtitle.set_label("Icon · name · executable · arguments");
        });
    }
    let form = CustomForm {
        name: custom_name,
        path: custom_path,
        args: custom_args,
        status: form_status,
        selected_icon,
        paint_icons,
        editing: Rc::new(RefCell::new(None)),
    };
    CustomPage { root, form, btn_save, btn_cancel }
}

/// Save and Cancel on the custom launcher form.
pub(in crate::harness_settings) fn wire_custom_page(
    page: &CustomPage,
    state: &Rc<RefCell<AppState>>,
    subtitle: &Label,
    nav: &Rc<dyn Fn(SettingsPage)>,
    on_change: &Rc<dyn Fn(Vec<String>)>,
    refresh: &RefreshSlot,
) {
    page.btn_cancel.connect_clicked({
        let nav = Rc::clone(nav);
        move |_| nav(SettingsPage::Harnesses)
    });
    page.btn_save.connect_clicked({
        let state = Rc::clone(state); let name = page.form.name.clone();
        let path = page.form.path.clone(); let args = page.form.args.clone();
        let selected = Rc::clone(&page.form.selected_icon); let editing = Rc::clone(&page.form.editing);
        let status = page.form.status.clone(); let subtitle = subtitle.clone(); let nav = Rc::clone(nav);
        let on_change = Rc::clone(on_change); let refresh = Rc::downgrade(refresh);
        move |_| {
            let entered_name = name.text();
            let effective_name = if entered_name.trim().is_empty() {
                suggested_harness_name(&path.text()).unwrap_or_default()
            } else {
                entered_name.to_string()
            };
            let icon = crate::custom_harness::ICONS[selected.get()];
            match crate::custom_harness::CustomHarness::create(&effective_name, icon, &path.text(), &args.text()) {
                Ok(mut item) => {
                    let mut s = state.borrow_mut();
                    if let Some(id) = editing.borrow().as_ref() { item.id = id.clone(); }
                    if s.custom_harnesses.iter().any(|old| old.id == item.id) {
                        if let Some(old) = s.custom_harnesses.iter_mut().find(|old| old.id == item.id) { *old = item; }
                    } else {
                        let id = item.id.clone();
                        s.custom_harnesses.push(item);
                        if let Some(visible) = &mut s.visible_harnesses { visible.push(id); }
                    }
                    let selected = visible_keys(&s, &detect_harnesses());
                    drop(s);
                    on_change(selected);
                    crate::state::flush_state_saves();
                    status.set_text(""); status.set_visible(false);
                    run_refresh(&refresh);
                    nav(SettingsPage::Harnesses);
                }
                Err(error) => {
                    status.set_text(&error);
                    status.set_visible(true);
                    subtitle.set_label(&error);
                    subtitle.add_css_class("launcher-note-error");
                    let invalid = match error.as_str() {
                        "Enter a name of up to 48 characters" => &name,
                        "Arguments have an unmatched quote" | "Use at most 32 arguments of up to 1024 characters each" => &args,
                        _ => &path,
                    };
                    invalid.add_css_class("ws-entry-invalid");
                    invalid.grab_focus();
                }
            }
        }
    });
}

/// Settings → Harness launchers → a built-in harness's starting parameters.
pub(in crate::harness_settings) struct ArgsPage {
    pub(in crate::harness_settings) root: Box,
    pub(in crate::harness_settings) help: Label,
    pub(in crate::harness_settings) default: Label,
    pub(in crate::harness_settings) status: Label,
    pub(in crate::harness_settings) entry: gtk4::Entry,
    pub(in crate::harness_settings) btn_save: Button,
    pub(in crate::harness_settings) btn_default: Button,
    pub(in crate::harness_settings) btn_cancel: Button,
    /// The built-in harness whose parameters the page is showing.
    pub(in crate::harness_settings) editing: Rc<Cell<Option<&'static str>>>,
    pub(in crate::harness_settings) paint_preview: Rc<dyn Fn()>,
}

pub(in crate::harness_settings) fn args_page(subtitle: &Label) -> ArgsPage {
    let root = settings_page_root();
    let (_, args_body) = section_card(&root, "", "Starting parameters");
    let args_help = hint("");
    args_body.append(&args_help);
    let args_default = hint("");
    args_default.add_css_class("harness-args-default");
    args_default.set_selectable(true);
    args_body.append(&args_default);
    let args_status = Label::new(None);
    args_status.add_css_class("launcher-note");
    args_status.add_css_class("launcher-note-error");
    args_status.add_css_class("harness-args-error");
    args_status.set_xalign(0.0);
    args_status.set_wrap(true);
    args_status.set_visible(false);
    args_body.append(&args_status);
    let args_entry = gtk4::Entry::new();
    args_entry.set_placeholder_text(Some("No parameters"));
    args_entry.add_css_class("ws-entry");
    args_entry.add_css_class("harness-args-entry");
    args_body.append(&args_entry);
    let args_preview = Label::new(None);
    args_preview.add_css_class("launcher-status-text");
    args_preview.add_css_class("harness-args-preview");
    args_preview.set_xalign(0.0);
    args_preview.set_wrap(true);
    args_preview.set_wrap_mode(pango::WrapMode::WordChar);
    args_preview.set_selectable(true);
    args_body.append(&args_preview);
    let args_actions = Box::new(Orientation::Horizontal, 8);
    let btn_save = Button::with_label("Save parameters");
    btn_save.add_css_class("launcher-btn");
    btn_save.add_css_class("launcher-btn-primary");
    let btn_default = Button::with_label("Use built-in default");
    btn_default.add_css_class("launcher-btn");
    let btn_cancel = Button::with_label("Cancel");
    btn_cancel.add_css_class("launcher-btn");
    args_actions.append(&btn_save);
    args_actions.append(&btn_default);
    args_actions.append(&btn_cancel);
    args_body.append(&args_actions);
    args_body.append(&hint(
        "New cards started from the top bar, the phone or another PC use these parameters. Open cards keep their command; restored cards keep theirs and gain parameters added here. Built-in defaults include the display modes the phone needs for scrollback, such as Codex's --no-alt-screen: removing one can hide history on Android.",
    ));
    let editing: Rc<Cell<Option<&'static str>>> = Rc::new(Cell::new(None));
    let paint_preview: Rc<dyn Fn()> = {
        let entry = args_entry.clone();
        let preview = args_preview.clone();
        let editing = Rc::clone(&editing);
        Rc::new(move || {
            let Some(key) = editing.get() else { return };
            match crate::launch_args::parse(&entry.text()) {
                Ok(args) => preview.set_text(&format!(
                    "Starts: {}",
                    crate::tmux::harness_command_with(key, &args).unwrap_or_default()
                )),
                Err(error) => preview.set_text(&error),
            }
        })
    };
    args_entry.connect_changed({
        let status = args_status.clone();
        let subtitle = subtitle.clone();
        let paint = Rc::clone(&paint_preview);
        move |entry| {
            entry.remove_css_class("ws-entry-invalid");
            status.set_visible(false);
            subtitle.remove_css_class("launcher-note-error");
            paint();
        }
    });
    ArgsPage {
        root,
        help: args_help,
        default: args_default,
        status: args_status,
        entry: args_entry,
        btn_save,
        btn_default,
        btn_cancel,
        editing,
        paint_preview,
    }
}

/// The parameter page's buttons. Returns what opens the page for a harness.
pub(in crate::harness_settings) fn wire_args_page(
    page: &ArgsPage,
    state: &Rc<RefCell<AppState>>,
    subtitle: &Label,
    nav: &Rc<dyn Fn(SettingsPage)>,
    refresh: &RefreshSlot,
) -> Rc<dyn Fn(&'static str)> {
    let open_args: Rc<dyn Fn(&'static str)> = {
        let editing = Rc::clone(&page.editing);
        let help = page.help.clone();
        let default = page.default.clone();
        let entry = page.entry.clone();
        let status = page.status.clone();
        let paint = Rc::clone(&page.paint_preview);
        let nav = Rc::clone(nav);
        Rc::new(move |key| {
            editing.set(Some(key));
            help.set_text(&format!(
                "Added after the {} command when a new card starts. Quotes keep words together, and every word is passed as typed (no ~ or $VARIABLE expansion). An empty field starts it with no parameters.",
                crate::tmux::get_agent_config(key).name,
            ));
            let builtin = crate::launch_args::builtin(key);
            default.set_text(&if builtin.is_empty() {
                "Built-in default: none".to_string()
            } else {
                format!("Built-in default: {}", crate::launch_args::display(&builtin))
            });
            entry.set_text(&crate::launch_args::display(&crate::launch_args::effective(key)));
            entry.remove_css_class("ws-entry-invalid");
            status.set_visible(false);
            paint();
            nav(SettingsPage::HarnessArgs);
            entry.grab_focus();
        })
    };
    page.btn_save.connect_clicked({
        let state = Rc::clone(state);
        let editing = Rc::clone(&page.editing);
        let entry = page.entry.clone();
        let status = page.status.clone();
        let subtitle = subtitle.clone();
        let nav = Rc::clone(nav);
        let refresh = Rc::downgrade(refresh);
        move |_| {
            let Some(key) = editing.get() else { return };
            match crate::launch_args::parse(&entry.text()) {
                Ok(args) => {
                    let snapshot = {
                        let mut s = state.borrow_mut();
                        crate::launch_args::store(&mut s.harness_args, key, args);
                        crate::launch_args::install(&s.harness_args);
                        s.clone()
                    };
                    crate::state::save_state_async(snapshot);
                    run_refresh(&refresh);
                    nav(SettingsPage::Harnesses);
                }
                Err(error) => {
                    status.set_text(&error);
                    status.set_visible(true);
                    subtitle.set_label(&error);
                    subtitle.add_css_class("launcher-note-error");
                    entry.add_css_class("ws-entry-invalid");
                    entry.grab_focus();
                }
            }
        }
    });
    page.entry.connect_activate({
        let save = page.btn_save.clone();
        move |_| save.emit_clicked()
    });
    page.btn_default.connect_clicked({
        let editing = Rc::clone(&page.editing);
        let entry = page.entry.clone();
        move |_| {
            if let Some(key) = editing.get() {
                entry.set_text(&crate::launch_args::display(&crate::launch_args::builtin(key)));
            }
        }
    });
    page.btn_cancel.connect_clicked({
        let nav = Rc::clone(nav);
        move |_| nav(SettingsPage::Harnesses)
    });
    open_args
}


/// A custom launcher's row, with its toggle, Edit, Clone and Remove connected.
/// Returns the row and its toggle, keyed by the launcher's id.
pub(in crate::harness_settings) fn custom_harness_row(
    item: crate::custom_harness::CustomHarness,
    list: &HarnessList,
    hooks: &HarnessListHooks,
    clone_into: &Rc<dyn Fn(crate::custom_harness::CustomHarness) -> crate::custom_harness::CustomHarness>,
) -> (Box, (String, Button)) {
    let (row, btn, edit, remove, clone) = custom_row(&item);
    let key = item.id.clone();
    let row_id = item.id.clone();
    list.connect_toggle(&btn, key.clone());
    edit.connect_clicked({
        let form = hooks.custom.clone();
        let nav = Rc::clone(&hooks.nav);
        let item = item.clone();
        move |_| form.open(Some(&item), &*nav)
    });
    clone.connect_clicked({
        let clone_into = Rc::clone(clone_into);
        let form = hooks.custom.clone();
        let nav = Rc::clone(&hooks.nav);
        let base = item;
        move |_| {
            let copy = (clone_into)(base.cloned());
            form.open(Some(&copy), &*nav);
        }
    });
    remove.connect_clicked({
        let state = Rc::clone(&hooks.state); let on_change = Rc::clone(&hooks.on_change);
        let refresh = hooks.refresh.clone();
        move |_| {
            let mut s = state.borrow_mut();
            s.custom_harnesses.retain(|item| item.id != key);
            if let Some(visible) = &mut s.visible_harnesses { visible.retain(|id| id != &key); }
            let selected = visible_keys(&s, &detect_harnesses());
            drop(s);
            on_change(selected);
            run_refresh(&refresh);
        }
    });
    (row, (row_id, btn))
}

