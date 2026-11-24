#[allow(unused_imports)]
use super::*;

/// Keys of `detected`, in detection order.
pub fn detected_keys(detected: &[HarnessInfo]) -> Vec<String> {
    detected.iter().map(|h| h.key.to_string()).collect()
}

/// Harness keys that should have a launch button in the top bar.
///
/// `configured == None` means the user never opened the settings panel: offer
/// every harness detected on this machine. An explicit list is intersected
/// with detection (in detection order), so uninstalling a harness drops its
/// button and a freshly installed one waits until it is enabled — while an
/// empty list still means "no launch buttons at all".
pub fn resolve_visible(configured: Option<&[String]>, detected: &[HarnessInfo]) -> Vec<String> {
    match configured {
        None => detected_keys(detected),
        Some(keys) => detected
            .iter()
            .filter(|h| keys.iter().any(|k| k == h.key))
            .map(|h| h.key.to_string())
            .collect(),
    }
}

pub fn visible_keys(state: &AppState, detected: &[HarnessInfo]) -> Vec<String> {
    let mut keys = resolve_visible(state.visible_harnesses.as_deref(), detected);
    for item in &state.custom_harnesses {
        if item.validate().is_ok() && state.visible_harnesses.as_ref().is_none_or(|list| list.contains(&item.id)) {
            keys.push(item.id.clone());
        }
    }
    keys
}

/// Flip `key` in `selection`, re-inserting it at its `order` position when it
/// is turned back on.
pub fn toggle(selection: &mut Vec<String>, key: &str, order: &[String]) {
    if let Some(pos) = selection.iter().position(|k| k == key) {
        selection.remove(pos);
        return;
    }
    let Some(target) = order.iter().position(|k| k == key) else {
        return;
    };
    let at = selection
        .iter()
        .position(|sel| order.iter().position(|k| k == sel).is_some_and(|j| j > target))
        .unwrap_or(selection.len());
    selection.insert(at, key.to_string());
}

pub(in crate::harness_settings) fn summary_text(shown: usize, total: usize) -> String {
    match total {
        0 => "No harness CLIs detected on this machine.".to_string(),
        1 => format!("{shown} of 1 harness shown in the top bar."),
        _ => format!("{shown} of {total} harnesses shown in the top bar."),
    }
}

pub(crate) fn paint_toggle(btn: &Button, shown: bool) {
    btn.set_label(if shown { "ON" } else { "OFF" });
    for (class, active) in [
        ("harness-toggle-on", shown),
        ("harness-toggle-off", !shown),
    ] {
        if active {
            btn.add_css_class(class);
        } else {
            btn.remove_css_class(class);
        }
    }
}

pub(in crate::harness_settings) fn paint_size_button(btn: &Button, selected: bool) {
    if selected {
        btn.add_css_class("top-bar-size-active");
    } else {
        btn.remove_css_class("top-bar-size-active");
    }
}

/// One `[logo] name …… resolved command [Parameters] [Clone] [ON/OFF]` row.
pub(in crate::harness_settings) fn harness_row(info: &HarnessInfo, light_theme: bool) -> (Box, Button, Button, Button) {
    let row = Box::new(Orientation::Horizontal, 8);
    row.add_css_class("harness-row");

    match crate::brand::logo_path(info.key, light_theme) {
        Some(logo) => {
            let img = Image::from_file(&logo);
            img.set_pixel_size(16);
            img.set_valign(Align::Center);
            row.append(&img);
        }
        None => {
            let icon = Label::new(Some(info.icon));
            icon.add_css_class("harness-icon");
            icon.set_valign(Align::Center);
            row.append(&icon);
        }
    }

    let name = Label::new(Some(info.name));
    name.add_css_class("harness-name");
    name.set_valign(Align::Center);
    row.append(&name);

    if let Some(successor) = crate::tmux::retired_successor(info.key) {
        let retired = Label::new(Some("No longer maintained"));
        retired.add_css_class("harness-retired");
        retired.set_valign(Align::Center);
        retired.set_tooltip_text(Some(&format!(
            "Upstream stopped maintaining it; {} replaces it",
            crate::tmux::get_agent_config(successor).name
        )));
        row.append(&retired);
    }

    let cmd = Label::new(Some(&info.command));
    cmd.add_css_class("harness-cmd");
    cmd.set_xalign(0.0);
    cmd.set_hexpand(true);
    cmd.set_ellipsize(pango::EllipsizeMode::Middle);
    cmd.set_tooltip_text(Some(&info.command));
    cmd.set_valign(Align::Center);
    row.append(&cmd);

    let params = Button::with_label("Parameters");
    params.add_css_class("launcher-btn");
    params.add_css_class("harness-params");
    params.set_valign(Align::Center);
    params.set_tooltip_text(Some("Edit the parameters new cards start with"));
    row.append(&params);

    let clone = Button::with_label("Clone");
    clone.add_css_class("launcher-btn");
    clone.set_valign(Align::Center);
    clone.set_tooltip_text(Some("Copy this launcher into a custom launcher of its own to rename and edit"));
    row.append(&clone);

    let btn = Button::new();
    btn.add_css_class("harness-toggle");
    btn.set_valign(Align::Center);
    btn.set_tooltip_text(Some("Show / hide this harness in the top bar"));
    row.append(&btn);

    (row, params, btn, clone)
}


/// Settings → Harness launchers: the harnesses installed here, each with a
/// show/hide toggle, and the supported ones that are not installed.
/// `wire_harness_list` fills it.
pub(in crate::harness_settings) struct HarnessesPage {
    pub(in crate::harness_settings) root: Box,
    pub(in crate::harness_settings) count_chip: Label,
    pub(in crate::harness_settings) btn_rescan: Button,
    pub(in crate::harness_settings) rows: Box,
    pub(in crate::harness_settings) empty: Label,
    pub(in crate::harness_settings) rescan_status: Label,
    pub(in crate::harness_settings) summary: Label,
    pub(in crate::harness_settings) btn_all: Button,
    pub(in crate::harness_settings) btn_none: Button,
    pub(in crate::harness_settings) btn_add_custom: Button,
    pub(in crate::harness_settings) missing_count: Label,
    pub(in crate::harness_settings) missing_rows: Box,
}

pub(in crate::harness_settings) fn harnesses_page() -> HarnessesPage {
    let root = settings_page_root();
    let (head, body) = section_card(&root, "", "Harnesses on this machine");
    let count_chip = chip("…");
    head.append(&count_chip);
    let btn_rescan = Button::with_label("⟳ Rescan");
    btn_rescan.add_css_class("launcher-btn");
    btn_rescan.set_tooltip_text(Some("Find harnesses installed since SUPER DESKTOP started"));
    head.append(&btn_rescan);

    let rows = Box::new(Orientation::Vertical, 2);
    rows.add_css_class("harness-rows");
    body.append(&rows);

    let empty = hint("");
    body.append(&empty);
    body.append(&hint(
        "Built-in harnesses appear when installed. Custom launchers remain editable if their executable goes missing.",
    ));
    let rescan_status = hint("");
    body.append(&rescan_status);

    let summary = Label::new(None);
    summary.add_css_class("launcher-status-text");
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    body.append(&summary);

    let actions = Box::new(Orientation::Horizontal, 8);
    actions.add_css_class("launcher-actions");
    let btn_all = Button::with_label("✓ Show all");
    btn_all.set_tooltip_text(Some("Show every harness installed on this machine"));
    btn_all.add_css_class("launcher-btn");
    btn_all.add_css_class("launcher-btn-primary");
    let btn_none = Button::with_label("✕ Hide all");
    btn_none.set_tooltip_text(Some("Keep only the note and settings buttons"));
    btn_none.add_css_class("launcher-btn");
    btn_none.add_css_class("launcher-btn-danger");
    actions.append(&btn_all);
    actions.append(&btn_none);
    body.append(&actions);

    let (btn_add_custom, _) = settings_entry(
        "＋", "Add a harness", "Choose an icon and executable for a new launcher.",
        "settings-add-harness-entry",
    );
    body.append(&btn_add_custom);
    body.append(&hint(
        "Hiding a launcher only removes its top-bar button; it does not remove the executable or saved configuration.",
    ));

    let (missing_head, missing_body) = section_card(&root, "", "More supported launchers");
    let missing_count = chip("…");
    missing_head.append(&missing_count);
    missing_body.append(&hint("These are available once installed on this PC. Herder runs a job worker."));
    let missing_rows = Box::new(Orientation::Vertical, 2);
    missing_rows.add_css_class("harness-rows");
    missing_body.append(&missing_rows);

    let footer = Label::new(Some(
        "Built-ins detected from PATH and common user bin folders · custom launchers stored in state.json",
    ));
    footer.add_css_class("launcher-footer");
    footer.set_xalign(0.5);
    root.append(&footer);

    HarnessesPage {
        root,
        count_chip,
        btn_rescan,
        rows,
        empty,
        rescan_status,
        summary,
        btn_all,
        btn_none,
        btn_add_custom,
        missing_count,
        missing_rows,
    }
}


/// The harness list's selection: what is shown, in detection order, and the
/// rows' show/hide toggles.
#[derive(Clone)]
pub(in crate::harness_settings) struct HarnessList {
    pub(in crate::harness_settings) selection: Rc<RefCell<Vec<String>>>,
    pub(in crate::harness_settings) order: Rc<RefCell<Vec<String>>>,
    pub(in crate::harness_settings) row_buttons: Rc<RefCell<Vec<(String, Button)>>>,
    /// Single funnel for every selection change: repaint, then let the window
    /// persist it and sync the top bar.
    pub(in crate::harness_settings) apply: Rc<dyn Fn(Vec<String>)>,
}

impl HarnessList {
    /// A row's toggle shows or hides `key`.
    pub(in crate::harness_settings) fn connect_toggle(&self, button: &Button, key: String) {
        let order = Rc::clone(&self.order);
        let selection = Rc::clone(&self.selection);
        let apply = Rc::clone(&self.apply);
        button.connect_clicked(move |_| {
            let mut sel = selection.borrow().clone();
            toggle(&mut sel, &key, &order.borrow());
            apply(sel);
        });
    }
}

/// What the harness list reaches outside its own page.
pub(in crate::harness_settings) struct HarnessListHooks {
    pub(in crate::harness_settings) state: Rc<RefCell<AppState>>,
    pub(in crate::harness_settings) on_change: Rc<dyn Fn(Vec<String>)>,
    pub(in crate::harness_settings) on_detected: Rc<dyn Fn(Vec<String>)>,
    pub(in crate::harness_settings) open_args: Rc<dyn Fn(&'static str)>,
    pub(in crate::harness_settings) custom: CustomForm,
    pub(in crate::harness_settings) nav: Rc<dyn Fn(SettingsPage)>,
    pub(in crate::harness_settings) refresh: std::rc::Weak<RefCell<Option<Rc<dyn Fn()>>>>,
    /// Runs at the start of every refresh, before detection.
    pub(in crate::harness_settings) reset: Rc<dyn Fn()>,
}

/// Fill the harness page and connect its buttons. Returns the refresh that
/// re-detects the harnesses and rebuilds the rows; the overlay runs it on
/// every open.
pub(in crate::harness_settings) fn wire_harness_list(page: &HarnessesPage, hooks: HarnessListHooks) -> Rc<dyn Fn()> {
    let selection: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let order: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let row_buttons: Rc<RefCell<Vec<(String, Button)>>> = Rc::new(RefCell::new(Vec::new()));

    // Repaint every row + the counters from `selection`.
    let paint: Rc<dyn Fn()> = {
        let selection = Rc::clone(&selection);
        let order = Rc::clone(&order);
        let row_buttons = Rc::clone(&row_buttons);
        let summary = page.summary.clone();
        let count_chip = page.count_chip.clone();
        let empty = page.empty.clone();
        Rc::new(move || {
            let selected = selection.borrow();
            let total = order.borrow().len();
            for (key, btn) in row_buttons.borrow().iter() {
                paint_toggle(btn, selected.iter().any(|k| k == key));
            }
            summary.set_text(&summary_text(selected.len(), total));
            count_chip.set_text(&format!("{} / {}", selected.len(), total));
            if total == 0 {
                empty.set_text("No harness CLIs found on this machine.");
            }
            empty.set_visible(total == 0);
        })
    };

    let apply: Rc<dyn Fn(Vec<String>)> = {
        let selection = Rc::clone(&selection);
        let paint = Rc::clone(&paint);
        let on_change = Rc::clone(&hooks.on_change);
        Rc::new(move |keys: Vec<String>| {
            *selection.borrow_mut() = keys.clone();
            paint();
            on_change(keys);
        })
    };
    let list = HarnessList { selection, order, row_buttons, apply };

    // Both Clone buttons funnel here: push a fresh copy into state, persist
    // the selection, then repaint and refresh the list. Returns the copy so
    // the caller can open the edit form for it.
    let clone_into: Rc<dyn Fn(crate::custom_harness::CustomHarness) -> crate::custom_harness::CustomHarness> = {
        let state = Rc::clone(&hooks.state);
        let on_change = Rc::clone(&hooks.on_change);
        let refresh = hooks.refresh.clone();
        Rc::new(move |copy: crate::custom_harness::CustomHarness| {
            let selected = {
                let mut s = state.borrow_mut();
                s.custom_harnesses.push(copy.clone());
                if let Some(visible) = &mut s.visible_harnesses { visible.push(copy.id.clone()); }
                visible_keys(&s, &detect_harnesses())
            };
            on_change(selected);
            crate::state::flush_state_saves();
            run_refresh(&refresh);
            copy
        })
    };

    let refresh: Rc<dyn Fn()> = {
        let rows = page.rows.clone();
        let rescan_status = page.rescan_status.clone();
        let missing_rows = page.missing_rows.clone();
        let missing_count = page.missing_count.clone();
        let list = list.clone();
        Rc::new(move || {
            rescan_status.set_text("");
            (hooks.reset)();

            let detected = detect_harnesses();
            let light_theme = crate::theme::current_theme().mode == "light";

            while let Some(child) = rows.first_child() {
                rows.remove(&child);
            }
            while let Some(child) = missing_rows.first_child() {
                missing_rows.remove(&child);
            }
            list.row_buttons.borrow_mut().clear();

            missing_count.set_text(&fill_missing_rows(&missing_rows, &detected, light_theme).to_string());

            for info in &detected {
                let (row, params, btn, clone) = harness_row(info, light_theme);
                params.connect_clicked({
                    let open = Rc::clone(&hooks.open_args);
                    let key = info.key;
                    move |_| open(key)
                });
                clone.connect_clicked({
                    let clone_into = Rc::clone(&clone_into);
                    let form = hooks.custom.clone();
                    let nav = Rc::clone(&hooks.nav);
                    let key = info.key;
                    let name = info.name;
                    let icon =
                        if crate::custom_harness::ICONS.iter().any(|choice| *choice == info.icon) {
                            info.icon
                        } else {
                            crate::custom_harness::ICONS[0]
                        };
                    move |_| {
                        let Some(executable) = crate::tmux::harness_binary(key) else { return };
                        let arguments =
                            crate::launch_args::display(&crate::launch_args::effective(key));
                        if let Some(copy) =
                            crate::custom_harness::CustomHarness::create(
                                name, icon, &executable, &arguments
                            ).ok()
                        {
                            let copy = (clone_into)(copy);
                            form.open(Some(&copy), &*nav);
                        }
                    }
                });
                let key = info.key.to_string();
                list.connect_toggle(&btn, key.clone());
                rows.append(&row);
                match info.key {
                    "openclaw" => rows.append(&openclaw_plugin_notice(OpenClawActions::SYSTEM)),
                    "antigravity" => rows.append(&super::openclaw::antigravity_hook_notice()),
                    "hermes" => rows.append(&super::openclaw::hermes_plugin_notice()),
                    _ => {}
                }
                list.row_buttons.borrow_mut().push((key, btn));
            }

            for item in hooks.state.borrow().custom_harnesses.clone() {
                let (row, btn) = custom_harness_row(item, &list, &hooks, &clone_into);
                rows.append(&row);
                list.row_buttons.borrow_mut().push(btn);
            }

            *list.order.borrow_mut() = detected_keys(&detected).into_iter()
                .chain(hooks.state.borrow().custom_harnesses.iter().filter(|item| item.available()).map(|item| item.id.clone())).collect();
            *list.selection.borrow_mut() = visible_keys(&hooks.state.borrow(), &detected);
            paint();
            (hooks.on_detected)(list.selection.borrow().clone());
        })
    };

    page.btn_rescan.connect_clicked({
        let refresh = Rc::clone(&refresh);
        let order = Rc::clone(&list.order);
        let status = page.rescan_status.clone();
        move |_| {
            let before = order.borrow().clone();
            refresh();
            let new: Vec<_> = order.borrow().iter()
                .filter(|key| !before.contains(key) && crate::tmux::HARNESS_KEYS.contains(&key.as_str()))
                .map(|key| crate::tmux::get_agent_config(key).name)
                .collect();
            let message = if new.is_empty() {
                format!("Scan complete. {} installed launchers available.",
                    order.borrow().iter().filter(|key| crate::tmux::HARNESS_KEYS.contains(&key.as_str())).count())
            } else {
                format!("Found: {}. The top bar is updated; use the toggles to change visibility.", new.join(", "))
            };
            status.set_text(&message);
        }
    });
    page.btn_all.connect_clicked({
        let order = Rc::clone(&list.order);
        let apply = Rc::clone(&list.apply);
        move |_| apply(order.borrow().clone())
    });
    page.btn_none.connect_clicked({
        let apply = Rc::clone(&list.apply);
        move |_| apply(Vec::new())
    });
    refresh
}

/// The supported harnesses that are not installed, as rows without controls.
/// Returns how many there are.
pub(in crate::harness_settings) fn fill_missing_rows(missing_rows: &Box, detected: &[HarnessInfo], light_theme: bool) -> usize {
    let mut missing = 0;
    for key in crate::tmux::HARNESS_KEYS {
        if detected.iter().any(|info| info.key == *key) { continue; }
        let cfg = crate::tmux::get_agent_config(key);
        let info = HarnessInfo {
            key, name: cfg.name, icon: cfg.icon,
            command: format!("Not installed · {}", cfg.commands[0]),
        };
        let (row, params, toggle, clone) = harness_row(&info, light_theme);
        row.remove(&params);
        row.remove(&toggle);
        row.remove(&clone);
        missing_rows.append(&row);
        missing += 1;
    }
    missing
}

