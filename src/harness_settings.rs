//! ⚙ Settings card for the overlay HUD.
//!
//! A floating card that lives INSIDE the super-desktop layer-shell overlay
//! (centered above notes and terminals) — never a separate Hyprland window. It
//! is a settings hub with dedicated pages for the keyboard shortcut, harness
//! launchers, top bar, and Android connection. The header (badge, title, ←
//! back, ✕) is shared by every page.
//!
//! Same visual language as notes/terminals: `mini-terminal` + `term-header`
//! card chrome, with compact navigation on the hub and a focused page for each
//! setting. Colours, radii and spacing live in `styles.rs` — this file builds
//! widgets, reads detection, drives the recorder and swaps pages.
//!
//! The selection itself is owned by the window: this panel reads it, reports
//! changes through `on_change` (harnesses) and `on_shortcut_change` (shortcut)
//! and re-detects every time it is opened.

use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    pango, Align, Box, Button, EventControllerKey, Image, Label, Orientation, PolicyType,
    PropagationPhase, ScrolledWindow,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use crate::launcher_settings::{chip, section_card};
use crate::shortcut::{Capture, CaptureGuard};
use crate::state::{AppState, TopBarSize};
use crate::tmux::{detect_harnesses, HarnessInfo};

/// How long a recording may stay armed before it cancels itself. The recorder
/// holds the keyboard (see `shortcut::begin_capture`), so "the user clicked
/// Record and walked away" has to end on its own.
const RECORD_WATCHDOG: Duration = Duration::from_millis(10_000);
/// The watchdog is checked by a 250ms tick, not by a timer of its own.
const RECORD_TICK: Duration = Duration::from_millis(250);

/// The one way out of a recording; `None` keeps the note as it is.
type StopRecording = Rc<dyn Fn(Option<&str>)>;
/// Commits a captured combination together with its physical (X11) keycode.
type CommitShortcut = Rc<dyn Fn(&str, u32)>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Home,
    Shortcut,
    Harnesses,
    TopBar,
    SleepLock,
    Android,
}

fn settings_entry(icon: &str, title: &str, summary: &str, class: &str) -> (Button, Box) {
    let button = Button::new();
    button.add_css_class("settings-entry");
    button.add_css_class(class);
    button.set_hexpand(true);

    let row = Box::new(Orientation::Horizontal, 12);
    let icon = Label::new(Some(icon));
    icon.add_css_class("settings-entry-icon");
    icon.set_valign(Align::Center);
    row.append(&icon);

    let words = Box::new(Orientation::Vertical, 3);
    words.set_hexpand(true);
    let heading = Label::new(Some(title));
    heading.add_css_class("settings-entry-title");
    heading.set_xalign(0.0);
    words.append(&heading);
    let description = Label::new(Some(summary));
    description.add_css_class("settings-entry-summary");
    description.set_xalign(0.0);
    description.set_wrap(true);
    words.append(&description);
    row.append(&words);

    let arrow = Label::new(Some("›"));
    arrow.add_css_class("settings-entry-arrow");
    arrow.set_valign(Align::Center);
    let trailing = Box::new(Orientation::Horizontal, 6);
    trailing.set_valign(Align::Center);
    trailing.append(&arrow);
    row.append(&trailing);
    button.set_child(Some(&row));

    (button, trailing)
}

fn settings_scroll(content: &Box) -> ScrolledWindow {
    let scroll = ScrolledWindow::new();
    scroll.add_css_class("launcher-scroll");
    scroll.add_css_class("harness-page");
    scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroll.set_child(Some(content));
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll
}


/// Floating card + its refresh handle. The overlay adds `widget` centered and
/// toggles visibility; `refresh` re-detects the harnesses on this machine and
/// re-reads the stored selection.
pub struct HarnessSettingsPanel {
    pub widget: gtk4::Widget,
    pub refresh: Rc<dyn Fn()>,
}

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

fn summary_text(shown: usize, total: usize) -> String {
    match total {
        0 => "No harness CLIs detected on this machine.".to_string(),
        1 => format!("{shown} of 1 harness shown in the top bar."),
        _ => format!("{shown} of {total} harnesses shown in the top bar."),
    }
}

fn paint_toggle(btn: &Button, shown: bool) {
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

fn paint_size_button(btn: &Button, selected: bool) {
    if selected {
        btn.add_css_class("top-bar-size-active");
    } else {
        btn.remove_css_class("top-bar-size-active");
    }
}

/// One `[logo] name …… resolved command [ON/OFF]` row.
fn harness_row(info: &HarnessInfo, light_theme: bool) -> (Box, Button) {
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

    let cmd = Label::new(Some(&info.command));
    cmd.add_css_class("harness-cmd");
    cmd.set_xalign(0.0);
    cmd.set_hexpand(true);
    cmd.set_ellipsize(pango::EllipsizeMode::Middle);
    cmd.set_tooltip_text(Some(&info.command));
    cmd.set_valign(Align::Center);
    row.append(&cmd);

    let btn = Button::new();
    btn.add_css_class("harness-toggle");
    btn.set_valign(Align::Center);
    btn.set_tooltip_text(Some("Show / hide this harness in the top bar"));
    row.append(&btn);

    (row, btn)
}

fn custom_row(item: &crate::custom_harness::CustomHarness) -> (Box, Button, Button, Button) {
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
    let remove = Button::with_label("Remove");
    remove.add_css_class("launcher-btn");
    remove.add_css_class("launcher-btn-danger");
    row.append(&remove);
    let toggle = Button::new();
    toggle.add_css_class("harness-toggle");
    toggle.set_sensitive(item.available());
    row.append(&toggle);
    (row, toggle, edit, remove)
}

/// Build the panel.
///
/// `on_change` receives the new full selection (harness keys, in detection
/// order) whenever the user flips a row or a bulk button;
/// `on_shortcut_change` receives a freshly recorded key combination once it has
/// been written into Hyprland's config; `on_top_bar_size_change` applies and
/// persists a newly selected dock scale.
/// The overlay only needs a placeholder until Settings is actually opened.
/// Keep the constructed panel afterwards so navigation and controls survive.
pub fn build_lazy_harness_settings_panel(
    state: Rc<RefCell<AppState>>,
    on_change: Rc<dyn Fn(Vec<String>)>,
    on_shortcut_change: Rc<dyn Fn(String)>,
    on_top_bar_size_change: Rc<dyn Fn(TopBarSize)>,
) -> HarnessSettingsPanel {
    let host = Box::new(Orientation::Vertical, 0);
    host.set_visible(false);
    let panel: Rc<RefCell<Option<HarnessSettingsPanel>>> = Rc::new(RefCell::new(None));
    let weak_host = host.downgrade();
    let refresh = Rc::new(move || {
        let Some(host) = weak_host.upgrade() else { return };
        if let Some(panel) = panel.borrow().as_ref() {
            panel.widget.set_visible(host.is_visible());
            (panel.refresh)();
            return;
        }
        if !host.is_visible() { return; }
        let built = build_harness_settings_panel(
            Rc::clone(&state), Rc::clone(&on_change),
            Rc::clone(&on_shortcut_change), Rc::clone(&on_top_bar_size_change),
        );
        // The panel's close button hides its root; mirror that on the host so
        // the next gear click opens it instead of requiring two clicks.
        let weak_host = host.downgrade();
        built.widget.connect_visible_notify(move |widget| {
            if !widget.is_visible() {
                if let Some(host) = weak_host.upgrade() { host.set_visible(false); }
            }
        });
        let weak_panel = built.widget.downgrade();
        host.connect_visible_notify(move |host| {
            if let Some(widget) = weak_panel.upgrade() { widget.set_visible(host.is_visible()); }
        });
        host.append(&built.widget);
        *panel.borrow_mut() = Some(built);
    });
    HarnessSettingsPanel { widget: host.upcast(), refresh }
}

pub fn build_harness_settings_panel(
    state: Rc<RefCell<AppState>>,
    on_change: Rc<dyn Fn(Vec<String>)>,
    on_shortcut_change: Rc<dyn Fn(String)>,
    on_top_bar_size_change: Rc<dyn Fn(TopBarSize)>,
) -> HarnessSettingsPanel {
    let outer = Box::new(Orientation::Vertical, 0);
    outer.add_css_class("mini-terminal");
    outer.add_css_class("harness-panel");
    outer.set_size_request(660, 620);

    // ---- header: badge, title + subtitle, ← back, close ----
    let header = Box::new(Orientation::Horizontal, 10);
    header.add_css_class("term-header");

    let badge = Label::new(Some("⚙"));
    badge.add_css_class("launcher-head-badge");
    badge.set_valign(Align::Center);
    header.append(&badge);

    let titles = Box::new(Orientation::Vertical, 0);
    titles.set_hexpand(true);
    titles.set_valign(Align::Center);
    let title = Label::new(Some("Settings"));
    title.add_css_class("term-title");
    title.set_halign(Align::Start);
    let subtitle = Label::new(Some("Android · shortcuts · top bar"));
    subtitle.add_css_class("launcher-subtitle");
    subtitle.set_halign(Align::Start);
    titles.append(&title);
    titles.append(&subtitle);
    header.append(&titles);

    // Shown only while the 📱 launcher page is up (see `nav`).
    let btn_back = Button::with_label("←");
    btn_back.set_tooltip_text(Some("Back to settings"));
    btn_back.add_css_class("term-btn");
    btn_back.set_valign(Align::Center);
    btn_back.set_visible(false);
    header.append(&btn_back);

    let btn_close = Button::with_label("✕");
    btn_close.set_tooltip_text(Some("Close panel"));
    btn_close.add_css_class("term-btn");
    btn_close.set_valign(Align::Center);
    header.append(&btn_close);
    outer.append(&header);

    let weak_outer = outer.downgrade();
    btn_close.connect_clicked(move |_| {
        if let Some(o) = weak_outer.upgrade() {
            o.set_visible(false);
        }
    });

    // The landing page keeps the choices short. Each substantial setting gets
    // its own scrollable destination below, so a user never has to hunt through
    // an ever-growing stack of unrelated controls.
    let home_root = Box::new(Orientation::Vertical, 10);
    home_root.add_css_class("launcher-body");
    home_root.add_css_class("settings-home");

    let firewall_notice = Box::new(Orientation::Horizontal, 10);
    firewall_notice.add_css_class("settings-firewall-warning");
    firewall_notice.set_visible(false);
    let firewall_words = Box::new(Orientation::Vertical, 2);
    firewall_words.set_hexpand(true);
    let firewall_title = Label::new(Some("Android connection needs attention"));
    firewall_title.add_css_class("settings-firewall-warning-title");
    firewall_title.set_xalign(0.0);
    firewall_words.append(&firewall_title);
    let firewall_text = Label::new(None);
    firewall_text.add_css_class("settings-firewall-warning-text");
    firewall_text.set_xalign(0.0);
    firewall_text.set_wrap(true);
    firewall_words.append(&firewall_text);
    firewall_notice.append(&firewall_words);
    let btn_review_firewall = Button::with_label("Review");
    btn_review_firewall.add_css_class("launcher-btn");
    btn_review_firewall.add_css_class("launcher-btn-primary");
    btn_review_firewall.set_valign(Align::Center);
    firewall_notice.append(&btn_review_firewall);
    home_root.append(&firewall_notice);

    let (btn_shortcut_page, _) = settings_entry(
        "⌨",
        "Keyboard shortcut",
        "Record the shortcut that shows or hides SUPER DESKTOP.",
        "settings-shortcut-entry",
    );
    btn_shortcut_page.set_tooltip_text(Some("Change the overlay shortcut"));
    home_root.append(&btn_shortcut_page);

    let (btn_harnesses_page, _) = settings_entry(
        "⌘",
        "Harness launchers",
        "Choose which installed coding agents appear in the top bar.",
        "settings-harnesses-entry",
    );
    btn_harnesses_page.set_tooltip_text(Some("Manage harness launch buttons"));
    home_root.append(&btn_harnesses_page);

    let (btn_top_bar_page, _) = settings_entry(
        "▤",
        "Top bar",
        "Choose the size of the desktop dock.",
        "settings-top-bar-entry",
    );
    btn_top_bar_page.set_tooltip_text(Some("Change the top-bar size"));
    home_root.append(&btn_top_bar_page);

    let (btn_sleep_lock, _) = settings_entry(
        "☀", "Sleep lock", "Keep AI harnesses awake while the laptop is on charger power.",
        "settings-sleep-lock-entry",
    );
    home_root.append(&btn_sleep_lock);

    let (btn_launcher, launcher_trailing) = settings_entry(
        "▣",
        "SUPER DESKTOP on Android",
        "Pair devices and manage secure connections.",
        "android-settings-entry",
    );
    btn_launcher.set_tooltip_text(Some("Manage Android devices and secure pairing"));
    let counts = chip("…/…");
    counts.add_css_class("android-connection-count");
    counts.set_tooltip_text(Some(
        "Active / registered phones. Active means connected or seen in the last 60 seconds.",
    ));
    launcher_trailing.prepend(&counts);
    home_root.append(&btn_launcher);
    let count_refresh = crate::launcher_settings::background_refresh(
        crate::bridge::paired_devices,
        move |devices| {
            let active = devices.iter().filter(|d| d["active"] == true).count();
            let text = format!("{active}/{}", devices.len());
            if counts.text().as_str() != text {
                counts.set_text(&text);
            }
        },
    );
    btn_launcher.connect_map({
        let refresh = Rc::clone(&count_refresh);
        move |_| refresh()
    });
    let entry_weak = btn_launcher.downgrade();
    gtk4::glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
        let Some(entry) = entry_weak.upgrade() else {
            return gtk4::glib::ControlFlow::Break;
        };
        if entry.is_mapped() {
            count_refresh();
        }
        gtk4::glib::ControlFlow::Continue
    });

    let home_footer = Label::new(Some("Settings are saved as you change them."));
    home_footer.add_css_class("launcher-footer");
    home_footer.set_xalign(0.5);
    home_root.append(&home_footer);

    let shortcut_root = Box::new(Orientation::Vertical, 10);
    shortcut_root.add_css_class("launcher-body");

    // ---- the overlay's own show / hide shortcut ----
    //
    // Recording seizes the keyboard for a few seconds (`shortcut::begin_capture`
    // parks Hyprland in a throw-away submap so no global bind can eat the key),
    // so every exit — Esc, the button turning into Cancel, the watchdog, the
    // panel being closed, the panel being reopened — funnels through
    // `stop_recording`, which is also the only thing that releases the guard.
    let (_, body) = section_card(&shortcut_root, "", "Show / hide shortcut");
    let combo_label = Label::new(None);
    combo_label.add_css_class("shortcut-combo");
    combo_label.set_valign(Align::Center);
    combo_label.set_xalign(0.0);
    combo_label.set_hexpand(true);
    combo_label.set_selectable(true);
    combo_label.set_tooltip_text(Some("Written to ~/.config/hypr/bindings.lua"));

    let btn_record = Button::with_label("⏺ Record");
    btn_record.set_tooltip_text(Some("Press the key combination you want to use"));
    btn_record.add_css_class("launcher-btn");
    btn_record.add_css_class("launcher-btn-primary");
    btn_record.set_valign(Align::Center);

    let combo_row = Box::new(Orientation::Horizontal, 10);
    combo_row.add_css_class("shortcut-row");
    combo_row.append(&combo_label);
    combo_row.append(&btn_record);
    body.append(&combo_row);

    let shortcut_note = Label::new(None);
    shortcut_note.add_css_class("launcher-note");
    shortcut_note.set_xalign(0.0);
    shortcut_note.set_wrap(true);
    shortcut_note.set_visible(false);
    body.append(&shortcut_note);
    let shortcut_hint = Label::new(None);
    shortcut_hint.add_css_class("launcher-hint");
    shortcut_hint.set_xalign(0.0);
    shortcut_hint.set_wrap(true);
    body.append(&shortcut_hint);

    let harnesses_root = Box::new(Orientation::Vertical, 10);
    harnesses_root.add_css_class("launcher-body");

    // ---- harnesses installed here, each with a show/hide toggle ----
    let (head, body) = section_card(&harnesses_root, "", "Harnesses on this machine");
    let count_chip = chip("…");
    head.append(&count_chip);

    let rows = Box::new(Orientation::Vertical, 2);
    rows.add_css_class("harness-rows");
    body.append(&rows);

    let empty = Label::new(None);
    empty.add_css_class("launcher-hint");
    empty.set_xalign(0.0);
    empty.set_wrap(true);
    body.append(&empty);

    let detected_hint = Label::new(Some(
        "Built-in harnesses appear when installed. Custom launchers remain editable if their executable goes missing.",
    ));
    detected_hint.add_css_class("launcher-hint");
    detected_hint.set_xalign(0.0);
    detected_hint.set_wrap(true);
    body.append(&detected_hint);

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

    let btn_add_custom = Button::with_label("＋ Add a harness");
    btn_add_custom.add_css_class("launcher-btn");
    btn_add_custom.add_css_class("launcher-btn-primary");
    body.append(&btn_add_custom);
    let custom_form = Box::new(Orientation::Vertical, 8);
    custom_form.set_visible(false);
    let form_help = Label::new(Some("Choose an icon, name the launcher, and point it to an executable on this PC. Arguments are optional; use quotes to keep words together."));
    form_help.set_wrap(true);
    form_help.set_xalign(0.0);
    form_help.add_css_class("launcher-hint");
    custom_form.append(&form_help);
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
    let name_label = Label::new(Some("Name"));
    name_label.add_css_class("launcher-hint");
    name_label.set_xalign(0.0);
    custom_form.append(&name_label);
    let custom_name = gtk4::Entry::new();
    custom_name.set_placeholder_text(Some("Harness name"));
    custom_name.set_max_length(48);
    custom_name.add_css_class("ws-entry");
    custom_form.append(&custom_name);
    let path_label = Label::new(Some("Executable path"));
    path_label.add_css_class("launcher-hint");
    path_label.set_xalign(0.0);
    custom_form.append(&path_label);
    let custom_path = gtk4::Entry::new();
    custom_path.set_placeholder_text(Some("/absolute/path/to/executable"));
    custom_path.add_css_class("ws-entry");
    custom_form.append(&custom_path);
    let args_label = Label::new(Some("Arguments (optional)"));
    args_label.add_css_class("launcher-hint");
    args_label.set_xalign(0.0);
    custom_form.append(&args_label);
    let custom_args = gtk4::Entry::new();
    custom_args.set_placeholder_text(Some("Optional arguments, e.g. --model 'my model'"));
    custom_args.add_css_class("ws-entry");
    custom_form.append(&custom_args);
    let form_status = Label::new(None);
    form_status.add_css_class("launcher-hint");
    form_status.set_xalign(0.0);
    form_status.set_wrap(true);
    custom_form.append(&form_status);
    let form_actions = Box::new(Orientation::Horizontal, 8);
    let btn_save_custom = Button::with_label("Save harness");
    btn_save_custom.add_css_class("launcher-btn");
    btn_save_custom.add_css_class("launcher-btn-primary");
    let btn_cancel_custom = Button::with_label("Cancel");
    btn_cancel_custom.add_css_class("launcher-btn");
    form_actions.append(&btn_save_custom);
    form_actions.append(&btn_cancel_custom);
    custom_form.append(&form_actions);
    body.append(&custom_form);
    let editing_custom: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let refresh_custom: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    btn_add_custom.connect_clicked({
        let form = custom_form.clone(); let name = custom_name.clone(); let path = custom_path.clone();
        let args = custom_args.clone(); let status = form_status.clone();
        let editing = Rc::clone(&editing_custom); let selected = Rc::clone(&selected_icon);
        let paint = Rc::clone(&paint_icons);
        move |_| {
            *editing.borrow_mut() = None;
            name.set_text(""); path.set_text(""); args.set_text(""); status.set_text("");
            selected.set(0); paint(); form.set_visible(true); name.grab_focus();
        }
    });
    btn_cancel_custom.connect_clicked({
        let form = custom_form.clone();
        move |_| form.set_visible(false)
    });

    let note = Label::new(Some(
        "Hiding a launcher only removes its top-bar button; it does not remove the executable or saved configuration.",
    ));
    note.add_css_class("launcher-hint");
    note.set_xalign(0.0);
    note.set_wrap(true);
    body.append(&note);

    let footer = Label::new(Some(
        "Built-ins detected with which / npx · custom launchers stored in state.json",
    ));
    footer.add_css_class("launcher-footer");
    footer.set_xalign(0.5);
    harnesses_root.append(&footer);

    let top_bar_root = Box::new(Orientation::Vertical, 10);
    top_bar_root.add_css_class("launcher-body");

    // ---- top-bar scale ----
    let (_, body) = section_card(&top_bar_root, "", "Top bar");
    let size_label = Label::new(Some("Size"));
    size_label.add_css_class("launcher-status-text");
    size_label.set_xalign(0.0);
    body.append(&size_label);

    let size_actions = Box::new(Orientation::Horizontal, 8);
    size_actions.add_css_class("launcher-actions");
    let btn_small = Button::with_label("Small");
    let btn_medium = Button::with_label("Medium");
    let btn_large = Button::with_label("Large");
    for button in [&btn_small, &btn_medium, &btn_large] {
        button.add_css_class("launcher-btn");
        button.add_css_class("top-bar-size");
        size_actions.append(button);
    }
    body.append(&size_actions);

    let size_buttons = Rc::new(vec![
        (TopBarSize::Small, btn_small),
        (TopBarSize::Medium, btn_medium),
        (TopBarSize::Large, btn_large),
    ]);
    let paint_size: Rc<dyn Fn()> = {
        let state = Rc::clone(&state);
        let size_buttons = Rc::clone(&size_buttons);
        Rc::new(move || {
            let selected = state.borrow().top_bar_size;
            for (size, button) in size_buttons.iter() {
                paint_size_button(button, *size == selected);
            }
        })
    };
    for (size, button) in size_buttons.iter() {
        let size = *size;
        let state = Rc::clone(&state);
        let paint_size = Rc::clone(&paint_size);
        let on_top_bar_size_change = Rc::clone(&on_top_bar_size_change);
        button.connect_clicked(move |_| {
            state.borrow_mut().top_bar_size = size;
            paint_size();
            on_top_bar_size_change(size);
        });
    }
    paint_size();

    let sleep_root = Box::new(Orientation::Vertical, 10);
    sleep_root.add_css_class("launcher-body");
    let (_, sleep_body) = section_card(&sleep_root, "", "Stay awake on charger");
    let sleep_row = Box::new(Orientation::Horizontal, 12);
    let sleep_label = Label::new(Some("Prevent sleep while plugged in"));
    sleep_label.set_hexpand(true);
    sleep_label.set_xalign(0.0);
    sleep_label.set_wrap(true);
    let sleep_toggle = gtk4::Switch::new();
    sleep_toggle.add_css_class("sleep-lock-toggle");
    sleep_toggle.set_valign(Align::Center);
    sleep_toggle.set_active(state.borrow().sleep_lock_on_ac);
    sleep_toggle.set_tooltip_text(Some("Keep AI harnesses running on charger power, including with the lid closed"));
    sleep_row.append(&sleep_label);
    sleep_row.append(&sleep_toggle);
    sleep_body.append(&sleep_row);
    let sleep_help = Label::new(Some("Keeps this laptop awake, including with the lid closed, while SUPER DESKTOP is running and charger power is detected. Normal sleep behavior returns on battery. The screen can still turn off and lock. Turn this off before manually suspending."));
    sleep_help.set_wrap(true);
    sleep_help.set_xalign(0.0);
    sleep_help.add_css_class("launcher-hint");
    sleep_body.append(&sleep_help);
    let sleep_status = Label::new(Some(&crate::sleep_lock::status()));
    sleep_status.set_wrap(true);
    sleep_status.set_xalign(0.0);
    sleep_status.add_css_class("sleep-lock-status");
    sleep_body.append(&sleep_status);
    sleep_toggle.connect_active_notify({
        let state = Rc::clone(&state);
        move |toggle| {
            let enabled = toggle.is_active();
            state.borrow_mut().sleep_lock_on_ac = enabled;
            crate::state::save_state_async(state.borrow().clone());
            crate::sleep_lock::set_enabled(enabled);
        }
    });
    let weak_status = sleep_status.downgrade();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        let Some(label) = weak_status.upgrade() else { return glib::ControlFlow::Break; };
        if label.is_mapped() {
            let text = crate::sleep_lock::status();
            if label.text().as_str() != text { label.set_text(&text); }
        }
        glib::ControlFlow::Continue
    });

    let home_view = settings_scroll(&home_root);
    let shortcut_view = settings_scroll(&shortcut_root);
    let harnesses_view = settings_scroll(&harnesses_root);
    let top_bar_view = settings_scroll(&top_bar_root);
    let sleep_view = settings_scroll(&sleep_root);

    // The Android page owns its live bridge controls. It is one destination in
    // the settings hub and refreshes only when entered.
    let launcher_page = crate::launcher_settings::build_launcher_page();
    let launcher_view = launcher_page.widget.clone();

    let pages = Box::new(Orientation::Vertical, 0);
    pages.add_css_class("harness-pages");
    pages.set_vexpand(true);
    pages.append(&home_view);
    pages.append(&shortcut_view);
    pages.append(&harnesses_view);
    pages.append(&top_bar_view);
    pages.append(&launcher_view);
    pages.append(&sleep_view);
    shortcut_view.set_visible(false);
    harnesses_view.set_visible(false);
    top_bar_view.set_visible(false);
    launcher_view.set_visible(false);
    sleep_view.set_visible(false);
    outer.append(&pages);

    let nav: Rc<dyn Fn(SettingsPage)> = {
        let home_view = home_view.clone();
        let shortcut_view = shortcut_view.clone();
        let harnesses_view = harnesses_view.clone();
        let top_bar_view = top_bar_view.clone();
        let launcher_view = launcher_view.clone();
        let sleep_view = sleep_view.clone();
        let btn_back = btn_back.clone();
        let badge = badge.clone();
        let title = title.clone();
        let subtitle = subtitle.clone();
        let launcher_refresh = Rc::clone(&launcher_page.refresh);
        Rc::new(move |page| {
            home_view.set_visible(page == SettingsPage::Home);
            shortcut_view.set_visible(page == SettingsPage::Shortcut);
            harnesses_view.set_visible(page == SettingsPage::Harnesses);
            top_bar_view.set_visible(page == SettingsPage::TopBar);
            launcher_view.set_visible(page == SettingsPage::Android);
            sleep_view.set_visible(page == SettingsPage::SleepLock);
            btn_back.set_visible(page != SettingsPage::Home);

            match page {
                SettingsPage::Home => {
                    badge.set_label("⚙");
                    title.set_label("Settings");
                    subtitle.set_label("Choose a section");
                }
                SettingsPage::Shortcut => {
                    badge.set_label("⌨");
                    title.set_label("Keyboard shortcut");
                    subtitle.set_label("Show or hide SUPER DESKTOP");
                }
                SettingsPage::Harnesses => {
                    badge.set_label("⌘");
                    title.set_label("Harness launchers");
                    subtitle.set_label("Choose what appears in the top bar");
                }
                SettingsPage::TopBar => {
                    badge.set_label("▤");
                    title.set_label("Top bar");
                    subtitle.set_label("Choose the desktop dock size");
                }
                SettingsPage::SleepLock => {
                    badge.set_label("☀");
                    title.set_label("Sleep lock");
                    subtitle.set_label("Keep harnesses available on charger power");
                }
                SettingsPage::Android => {
                    badge.set_label("📱");
                    title.set_label("SUPER DESKTOP on Android");
                    subtitle.set_label("Devices · encrypted connections");
                    launcher_refresh();
                }
            }
        })
    };

    for (button, page) in [
        (&btn_shortcut_page, SettingsPage::Shortcut),
        (&btn_harnesses_page, SettingsPage::Harnesses),
        (&btn_top_bar_page, SettingsPage::TopBar),
        (&btn_launcher, SettingsPage::Android),
        (&btn_sleep_lock, SettingsPage::SleepLock),
    ] {
        let nav = Rc::clone(&nav);
        button.connect_clicked(move |_| nav(page));
    }
    btn_review_firewall.connect_clicked({
        let nav = Rc::clone(&nav);
        let show_network = Rc::clone(&launcher_page.show_network);
        move |_| {
            nav(SettingsPage::Android);
            show_network();
        }
    });
    // Reopening always lands on the hub. A theme refresh while the card stays
    // open deliberately leaves the current page alone.
    outer.connect_visible_notify({
        let nav = Rc::clone(&nav);
        move |o| {
            if o.is_visible() {
                nav(SettingsPage::Home);
            }
        }
    });

    let firewall_notice_refresh = crate::launcher_settings::background_refresh(
        crate::bridge::firewall_summary,
        move |(summary, can_unlock)| {
            firewall_notice.set_visible(can_unlock);
            if can_unlock {
                firewall_text.set_text(&format!(
                    "{summary}. Allow {}/tcp so Android devices can reach this computer.",
                    crate::bridge::BRIDGE_PORT,
                ));
            }
        },
    );
    btn_back.connect_clicked({
        let nav = Rc::clone(&nav);
        let refresh = Rc::clone(&firewall_notice_refresh);
        move |_| {
            nav(SettingsPage::Home);
            refresh();
        }
    });

    // ---- shortcut recorder ----
    // `armed` holds the keymap guard for exactly as long as the listener is
    // live: dropping it is what gives the user their global shortcuts back.
    let recording = Rc::new(Cell::new(false));
    let armed: Rc<RefCell<Option<CaptureGuard>>> = Rc::new(RefCell::new(None));

    // Paint the combo and the Record/Cancel button from `recording`.
    let paint_recorder: Rc<dyn Fn()> = {
        let recording = Rc::clone(&recording);
        let state = Rc::clone(&state);
        let combo_label = combo_label.clone();
        let btn_record = btn_record.clone();
        let shortcut_hint = shortcut_hint.clone();
        Rc::new(move || {
            if recording.get() {
                combo_label.set_text("Press your combination…");
                combo_label.add_css_class("shortcut-recording");
                btn_record.set_label("✕ Cancel");
                btn_record.remove_css_class("launcher-btn-primary");
                btn_record.add_css_class("launcher-btn-danger");
                shortcut_hint.set_text(
                    "Listening — global shortcuts are paused until you press one. \
                     Esc keeps the current combination.",
                );
            } else {
                combo_label.set_text(&crate::shortcut::current_combo(
                    state.borrow().toggle_shortcut.as_deref(),
                ));
                combo_label.remove_css_class("shortcut-recording");
                btn_record.set_label("⏺ Record");
                btn_record.remove_css_class("launcher-btn-danger");
                btn_record.add_css_class("launcher-btn-primary");
                shortcut_hint.set_text(
                    "Click Record, then press the combination you want — SUPER/CTRL/ALT \
                     plus a key, or F1-F12 on their own. Global shortcuts pause while \
                     recording, so any combination can be captured.",
                );
            }
        })
    };

    // The one way out of a recording. `message` is the outcome to show (None
    // keeps whatever is already there) — every cancel path calls this.
    let stop_recording: StopRecording = {
        let recording = Rc::clone(&recording);
        let armed = Rc::clone(&armed);
        let paint = Rc::clone(&paint_recorder);
        let shortcut_note = shortcut_note.clone();
        Rc::new(move |message: Option<&str>| {
            // Release first: this is what un-pauses the user's shortcuts.
            if let Some(guard) = armed.borrow_mut().take() {
                guard.end();
            }
            let was_recording = recording.replace(false);
            if let Some(message) = message {
                shortcut_note.remove_css_class("launcher-note-error");
                shortcut_note.set_text(message);
                shortcut_note.set_visible(true);
            } else {
                shortcut_note.set_visible(false);
            }
            if was_recording {
                paint();
            }
        })
    };

    // A captured combination: leave the recording, then write it to Hyprland.
    let commit_shortcut: CommitShortcut = {
        let state = Rc::clone(&state);
        let on_shortcut_change = Rc::clone(&on_shortcut_change);
        let stop_recording = Rc::clone(&stop_recording);
        let paint = Rc::clone(&paint_recorder);
        let shortcut_note = shortcut_note.clone();
        Rc::new(move |combo: &str, keycode: u32| {
            stop_recording(None);
            let (message, failed) = match crate::shortcut::apply_combo(combo, Some(keycode)) {
                Ok(applied) => {
                    {
                        let mut s = state.borrow_mut();
                        s.toggle_shortcut = Some(applied.combo.clone());
                    }
                    let snapshot = state.borrow().clone();
                    crate::state::save_state_async(snapshot);
                    on_shortcut_change(applied.combo.clone());
                    match (applied.warning, applied.conflict) {
                        (Some(warning), _) => (format!("⚠ {warning}"), true),
                        (None, Some(other)) => (
                            format!("● {combo} toggles SUPER DESKTOP — it used to run “{other}”."),
                            false,
                        ),
                        (None, None) => (format!("● {combo} toggles SUPER DESKTOP."), false),
                    }
                }
                Err(e) => (format!("⚠ {e}"), true),
            };
            // Restyle from scratch: an outcome shown after an earlier failure
            // must not inherit the red left over from it.
            shortcut_note.remove_css_class("launcher-note-error");
            if failed {
                shortcut_note.add_css_class("launcher-note-error");
            }
            shortcut_note.set_text(&message);
            shortcut_note.set_visible(true);
            paint();
        })
    };

    let start_recording: Rc<dyn Fn()> = {
        let recording = Rc::clone(&recording);
        let armed = Rc::clone(&armed);
        let paint = Rc::clone(&paint_recorder);
        let shortcut_note = shortcut_note.clone();
        let btn_record = btn_record.clone();
        Rc::new(move || {
            if recording.get() {
                return;
            }
            // The keys have to reach THIS surface: clicking Record focuses the
            // button, and with it the panel the capture controller sits on.
            btn_record.grab_focus();
            let guard = crate::shortcut::begin_capture();
            // Without the guard Hyprland keeps handling the shortcuts it owns
            // before they ever reach this window, so say so instead of letting
            // the user press a taken combination and watch nothing happen.
            let unguarded = !guard.armed();
            *armed.borrow_mut() = Some(guard);
            recording.set(true);
            shortcut_note.set_visible(false);
            if unguarded {
                shortcut_note.add_css_class("launcher-note-error");
                shortcut_note.set_text(
                    "Could not pause Hyprland's own shortcuts — a combination that is \
                     already bound will not reach this window.",
                );
                shortcut_note.set_visible(true);
            }
            paint();
        })
    };

    // The listener. Capture phase, on the whole panel: it must see the keys
    // before the widget the user last clicked, and before the window's own Esc
    // handler — while recording, Esc cancels the recording, it does not hide
    // the overlay.
    let key_ctrl = EventControllerKey::new();
    key_ctrl.set_propagation_phase(PropagationPhase::Capture);
    {
        let recording = Rc::clone(&recording);
        let stop_recording = Rc::clone(&stop_recording);
        let commit_shortcut = Rc::clone(&commit_shortcut);
        let shortcut_note = shortcut_note.clone();
        key_ctrl.connect_key_pressed(move |_, key, keycode, mods| {
            if !recording.get() {
                return glib::Propagation::Proceed;
            }
            if key == gdk::Key::Escape {
                stop_recording(Some("Recording cancelled — the shortcut is unchanged."));
                return glib::Propagation::Stop;
            }
            match crate::shortcut::interpret(key, mods) {
                Capture::Combo(combo) => {
                    commit_shortcut(&combo, keycode);
                    glib::Propagation::Stop
                }
                Capture::NeedsModifier => {
                    shortcut_note.add_css_class("launcher-note-error");
                    shortcut_note.set_text(
                        "Hold SUPER, CTRL or ALT with the key — or use F1-F12 on their own.",
                    );
                    shortcut_note.set_visible(true);
                    glib::Propagation::Stop
                }
                Capture::Waiting => glib::Propagation::Stop,
            }
        });
    }
    outer.add_controller(key_ctrl);

    btn_record.connect_clicked({
        let recording = Rc::clone(&recording);
        let start_recording = Rc::clone(&start_recording);
        let stop_recording = Rc::clone(&stop_recording);
        move |_| {
            if recording.get() {
                stop_recording(Some("Recording cancelled — the shortcut is unchanged."));
            } else {
                start_recording();
            }
        }
    });

    // Two ways out of a recording nobody finishes: the panel/overlay going
    // away, and the watchdog. Both matter because the recorder holds the
    // keyboard — a stuck recording is a desktop with no working shortcuts.
    {
        let recording = Rc::clone(&recording);
        let stop_recording = Rc::clone(&stop_recording);
        let panel = outer.downgrade();
        let mut armed_ticks = 0u32;
        let max_ticks = (RECORD_WATCHDOG.as_millis() / RECORD_TICK.as_millis()).max(1) as u32;
        glib::timeout_add_local(RECORD_TICK, move || {
            let Some(panel) = panel.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !recording.get() {
                armed_ticks = 0;
                return glib::ControlFlow::Continue;
            }
            armed_ticks += 1;
            // `is_mapped`, not `is_visible`: hiding the whole overlay unmaps the
            // window without ever touching the panel's own visibility flag.
            if !panel.is_mapped() {
                stop_recording(Some("Recording cancelled — the panel was closed."));
            } else if armed_ticks >= max_ticks {
                stop_recording(Some("No combination captured — try again."));
            }
            glib::ControlFlow::Continue
        });
    }

    // ---- shared state: selection, detection order, row buttons ----
    let selection: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let order: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let row_buttons: Rc<RefCell<Vec<(String, Button)>>> = Rc::new(RefCell::new(Vec::new()));

    // Repaint every row + the counters from `selection`.
    let paint: Rc<dyn Fn()> = {
        let selection = Rc::clone(&selection);
        let order = Rc::clone(&order);
        let row_buttons = Rc::clone(&row_buttons);
        let summary = summary.clone();
        let count_chip = count_chip.clone();
        let empty = empty.clone();
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

    // Single funnel for every selection change: repaint, then let the window
    // persist it and sync the top bar.
    let apply: Rc<dyn Fn(Vec<String>)> = {
        let selection = Rc::clone(&selection);
        let paint = Rc::clone(&paint);
        let on_change = Rc::clone(&on_change);
        Rc::new(move |keys: Vec<String>| {
            *selection.borrow_mut() = keys.clone();
            paint();
            on_change(keys);
        })
    };

    // Re-detect and rebuild the rows; the overlay calls this on every open.
    let refresh: Rc<dyn Fn()> = {
        let rows = rows.clone();
        let order = Rc::clone(&order);
        let row_buttons = Rc::clone(&row_buttons);
        let selection = Rc::clone(&selection);
        let paint = Rc::clone(&paint);
        let apply = Rc::clone(&apply);
        let state = Rc::clone(&state);
        let custom_form = custom_form.clone();
        let custom_name = custom_name.clone();
        let custom_path = custom_path.clone();
        let custom_args = custom_args.clone();
        let form_status = form_status.clone();
        let selected_icon = Rc::clone(&selected_icon);
        let paint_icons = Rc::clone(&paint_icons);
        let editing_custom = Rc::clone(&editing_custom);
        let refresh_custom = Rc::downgrade(&refresh_custom);
        let on_change = Rc::clone(&on_change);
        // Reopening (or restyling on a theme switch) must never leave a
        // recording armed with the keyboard held.
        let stop_recording = Rc::clone(&stop_recording);
        let paint_recorder = Rc::clone(&paint_recorder);
        let paint_size = Rc::clone(&paint_size);
        let firewall_notice_refresh = Rc::clone(&firewall_notice_refresh);
        Rc::new(move || {
            stop_recording(None);
            paint_recorder();
            paint_size();
            firewall_notice_refresh();

            let detected = detect_harnesses();
            let light_theme = crate::theme::current_theme().mode == "light";

            while let Some(child) = rows.first_child() {
                rows.remove(&child);
            }
            row_buttons.borrow_mut().clear();

            for info in &detected {
                let (row, btn) = harness_row(info, light_theme);
                let key = info.key.to_string();
                let order = Rc::clone(&order);
                let selection = Rc::clone(&selection);
                let apply = Rc::clone(&apply);
                btn.connect_clicked({
                    let key = key.clone();
                    move |_| {
                        let mut sel = selection.borrow().clone();
                        toggle(&mut sel, &key, &order.borrow());
                        apply(sel);
                    }
                });
                rows.append(&row);
                row_buttons.borrow_mut().push((key, btn));
            }

            for item in state.borrow().custom_harnesses.clone() {
                let (row, btn, edit, remove) = custom_row(&item);
                let key = item.id.clone();
                let order = Rc::clone(&order);
                let selection = Rc::clone(&selection);
                let apply = Rc::clone(&apply);
                btn.connect_clicked({
                    let key = key.clone();
                    move |_| {
                        let mut sel = selection.borrow().clone();
                        toggle(&mut sel, &key, &order.borrow());
                        apply(sel);
                    }
                });
                edit.connect_clicked({
                    let form = custom_form.clone(); let name = custom_name.clone();
                    let path = custom_path.clone(); let args = custom_args.clone();
                    let status = form_status.clone();
                    let selected = Rc::clone(&selected_icon); let paint = Rc::clone(&paint_icons);
                    let editing = Rc::clone(&editing_custom); let item = item.clone();
                    move |_| {
                        *editing.borrow_mut() = Some(item.id.clone());
                        status.set_text("");
                        name.set_text(&item.name); path.set_text(&item.executable);
                        args.set_text(&item.arguments.iter().map(|arg| format!("'{}'", arg.replace('\'', "'\"'\"'"))).collect::<Vec<_>>().join(" "));
                        selected.set(crate::custom_harness::ICONS.iter().position(|icon| *icon == item.icon).unwrap_or(0));
                        paint(); form.set_visible(true); name.grab_focus();
                    }
                });
                remove.connect_clicked({
                    let state = Rc::clone(&state); let on_change = Rc::clone(&on_change);
                    let refresh = refresh_custom.clone();
                    move |_| {
                        let mut s = state.borrow_mut();
                        s.custom_harnesses.retain(|item| item.id != key);
                        if let Some(visible) = &mut s.visible_harnesses { visible.retain(|id| id != &key); }
                        let selected = visible_keys(&s, &detect_harnesses());
                        drop(s);
                        on_change(selected);
                        if let Some(refresh) = refresh.upgrade().and_then(|slot| slot.borrow().clone()) { refresh(); }
                    }
                });
                rows.append(&row);
                row_buttons.borrow_mut().push((item.id.clone(), btn));
            }

            *order.borrow_mut() = detected_keys(&detected).into_iter()
                .chain(state.borrow().custom_harnesses.iter().filter(|item| item.available()).map(|item| item.id.clone())).collect();
            *selection.borrow_mut() = visible_keys(&state.borrow(), &detected);
            paint();
        })
    };

    *refresh_custom.borrow_mut() = Some(Rc::clone(&refresh));
    btn_save_custom.connect_clicked({
        let state = Rc::clone(&state); let name = custom_name.clone();
        let path = custom_path.clone(); let args = custom_args.clone();
        let selected = Rc::clone(&selected_icon); let editing = Rc::clone(&editing_custom);
        let status = form_status.clone(); let form = custom_form.clone();
        let on_change = Rc::clone(&on_change); let refresh = Rc::downgrade(&refresh_custom);
        move |_| {
            let icon = crate::custom_harness::ICONS[selected.get()];
            match crate::custom_harness::CustomHarness::create(&name.text(), icon, &path.text(), &args.text()) {
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
                    form.set_visible(false);
                    status.set_text("");
                    if let Some(refresh) = refresh.upgrade().and_then(|slot| slot.borrow().clone()) { refresh(); }
                }
                Err(error) => status.set_text(&error),
            }
        }
    });

    btn_all.connect_clicked({
        let order = Rc::clone(&order);
        let apply = Rc::clone(&apply);
        move |_| apply(order.borrow().clone())
    });
    btn_none.connect_clicked({
        let apply = Rc::clone(&apply);
        move |_| apply(Vec::new())
    });

    refresh();

    let refresh: Rc<dyn Fn()> = Rc::new({
        let slot = Rc::clone(&refresh_custom);
        let run = Rc::clone(&refresh);
        move || { let _keep_alive = &slot; run(); }
    });
    HarnessSettingsPanel {
        widget: outer.upcast(),
        refresh: Rc::clone(&refresh),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_panel_builds_only_on_open_and_reopens_after_close() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process("harness_settings::tests::lazy_panel_builds_only_on_open_and_reopens_after_close");
            return;
        }
        if gtk4::init().is_err() { return; }
        let panel = build_lazy_harness_settings_panel(
            Rc::new(RefCell::new(AppState::default())),
            Rc::new(|_| {}), Rc::new(|_| {}), Rc::new(|_| {}),
        );
        assert!(panel.widget.first_child().is_none());
        (panel.refresh)(); // Theme changes while unopened must remain cheap.
        assert!(panel.widget.first_child().is_none());
        panel.widget.set_visible(true);
        (panel.refresh)();
        let child = panel.widget.first_child().expect("built on first open");
        child.set_visible(false); // Inner close button.
        assert!(!panel.widget.is_visible());
        panel.widget.set_visible(true);
        (panel.refresh)();
        assert_eq!(panel.widget.first_child(), Some(child.clone()));
        assert!(child.is_visible());
        panel.widget.set_visible(false); // Overlay hide.
        assert!(!child.is_visible());
        (panel.refresh)();
        assert!(!panel.widget.is_visible());
        assert!(!child.is_visible());
    }

    fn info(key: &'static str) -> HarnessInfo {
        HarnessInfo {
            key,
            name: key,
            icon: "💻",
            command: format!("/usr/bin/{key}"),
        }
    }

    #[test]
    fn test_resolve_visible_defaults_to_everything_detected() {
        let detected = vec![info("claude"), info("shell")];
        assert_eq!(
            resolve_visible(None, &detected),
            vec!["claude".to_string(), "shell".to_string()]
        );
    }

    #[test]
    fn saved_custom_harness_respects_visibility_and_availability() {
        let mut state = AppState::default();
        let item = crate::custom_harness::CustomHarness::create("Echo", "💻", "/bin/echo", "hello").unwrap();
        let id = item.id.clone();
        state.custom_harnesses.push(item);
        let detected = vec![info("shell")];
        assert_eq!(visible_keys(&state, &detected), vec!["shell".to_string(), id.clone()]);
        state.visible_harnesses = Some(vec![id.clone()]);
        assert_eq!(visible_keys(&state, &detected), vec![id]);
    }

    #[test]
    fn custom_launcher_form_saves_and_removes() {
        use std::os::unix::fs::PermissionsExt;
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process("harness_settings::tests::custom_launcher_form_saves_and_removes");
            return;
        }
        gtk4::init().unwrap();
        let home = std::env::temp_dir().join(format!("sd-custom-ui-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        let state = Rc::new(RefCell::new(AppState::default()));
        let saved = Rc::clone(&state);
        let panel = build_harness_settings_panel(
            Rc::clone(&state),
            Rc::new(move |keys| {
                saved.borrow_mut().visible_harnesses = Some(keys);
                crate::state::save_state_async(saved.borrow().clone());
            }),
            Rc::new(|_| {}), Rc::new(|_| {}),
        );
        let buttons = find_buttons(&panel.widget, "launcher-btn");
        buttons.iter().find(|button| button.label().as_deref() == Some("＋ Add a harness")).unwrap().emit_clicked();
        buttons.iter().find(|button| button.label().as_deref() == Some("🧭")).unwrap().emit_clicked();
        let entries = find_widgets(&panel.widget, "ws-entry");
        for (placeholder, value) in [
            ("Harness name", "My Echo"),
            ("/absolute/path/to/executable", "/bin/echo"),
            ("Optional arguments, e.g. --model 'my model'", "hello 'two words'"),
        ] {
            entries.iter().filter_map(|widget| widget.clone().downcast::<gtk4::Entry>().ok())
                .find(|entry| entry.placeholder_text().as_deref() == Some(placeholder)).unwrap().set_text(value);
        }
        buttons.iter().find(|button| button.label().as_deref() == Some("Save harness")).unwrap().emit_clicked();
        let item = state.borrow().custom_harnesses[0].clone();
        assert_eq!(item.name, "My Echo");
        assert_eq!(item.icon, "🧭");
        assert_eq!(item.arguments, ["hello", "two words"]);
        assert_eq!(crate::state::load_state().custom_harnesses, vec![item.clone()]);
        assert_eq!(std::fs::metadata(crate::state::get_state_path()).unwrap().permissions().mode() & 0o777, 0o600);
        let edit = find_buttons(&panel.widget, "launcher-btn").into_iter()
            .find(|button| button.label().as_deref() == Some("Edit")).unwrap();
        edit.emit_clicked();
        entries.iter().filter_map(|widget| widget.clone().downcast::<gtk4::Entry>().ok())
            .find(|entry| entry.placeholder_text().as_deref() == Some("Harness name")).unwrap().set_text("Echo again");
        buttons.iter().find(|button| button.label().as_deref() == Some("Save harness")).unwrap().emit_clicked();
        assert_eq!(state.borrow().custom_harnesses.len(), 1);
        assert_eq!(state.borrow().custom_harnesses[0].name, "Echo again");
        assert_eq!(state.borrow().custom_harnesses[0].id, item.id);
        let remove = find_buttons(&panel.widget, "launcher-btn").into_iter()
            .find(|button| button.label().as_deref() == Some("Remove")).unwrap();
        remove.emit_clicked();
        crate::state::flush_state_saves();
        assert!(state.borrow().custom_harnesses.is_empty());
        assert!(crate::state::load_state().custom_harnesses.is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn test_resolve_visible_filters_to_detection_in_detection_order() {
        let detected = vec![info("claude"), info("codex"), info("shell")];
        // Unknown (uninstalled) keys are dropped, and the stored order does
        // not decide the top-bar order — detection does.
        let stored = vec![
            "shell".to_string(),
            "grok".to_string(),
            "claude".to_string(),
        ];
        assert_eq!(
            resolve_visible(Some(&stored), &detected),
            vec!["claude".to_string(), "shell".to_string()]
        );
        // "Hide all" must survive a reopen as "nothing shown".
        assert!(resolve_visible(Some(&[]), &detected).is_empty());
    }

    #[test]
    fn test_toggle_keeps_detection_order() {
        let order = vec![
            "claude".to_string(),
            "codex".to_string(),
            "grok".to_string(),
        ];
        // Turning a harness back on puts it where detection says it belongs.
        let mut selection = vec!["codex".to_string(), "grok".to_string()];
        toggle(&mut selection, "claude", &order);
        assert_eq!(selection, vec!["claude", "codex", "grok"]);
        // …and turning it off removes exactly that one.
        toggle(&mut selection, "codex", &order);
        assert_eq!(selection, vec!["claude", "grok"]);
        // Harnesses that are not detected here can never be selected.
        toggle(&mut selection, "aider", &order);
        assert_eq!(selection, vec!["claude", "grok"]);
    }

    #[test]
    fn test_panel_gtk_structure() {
        // GTK may only be used from one thread per process, so the widget
        // assertions run in a child process (see `crate::gtk_test`).
        crate::gtk_test::run_in_child_process(
            "harness_settings::tests::panel_gtk_structure_child",
        );
    }

    /// Only meaningful when re-run as the single test of a fresh process.
    #[test]
    fn panel_gtk_structure_child() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let _ = gtk4::init();

        let detected = detect_harnesses();
        let app_state = Rc::new(RefCell::new(AppState::default()));
        let seen: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
        let seen_cb = Rc::clone(&seen);
        let shortcut_changes: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let shortcut_cb = Rc::clone(&shortcut_changes);
        let size_changes: Rc<RefCell<Vec<TopBarSize>>> = Rc::new(RefCell::new(Vec::new()));
        let size_cb = Rc::clone(&size_changes);
        let panel = build_harness_settings_panel(
            Rc::clone(&app_state),
            Rc::new(move |keys: Vec<String>| seen_cb.borrow_mut().push(keys)),
            Rc::new(move |combo: String| shortcut_cb.borrow_mut().push(combo)),
            Rc::new(move |size| size_cb.borrow_mut().push(size)),
        );
        (panel.refresh)();

        // The hub is deliberately short. Each substantial setting has its own
        // page, with Android keeping its existing connection, pairing and
        // device sections together.
        let pages = find_widgets(&panel.widget, "harness-page");
        assert_eq!(pages.len(), 6, "hub + five destination pages");
        let sections: Vec<usize> = pages
            .iter()
            .map(|p| count_class(p, "launcher-section"))
            .collect();
        assert_eq!(sections, vec![0, 1, 1, 1, 3, 1]);
        assert_eq!(count_class(&panel.widget, "launcher-section-num"), 0);
        assert_eq!(count_class(&panel.widget, "launcher-section-title"), 7);
        assert_eq!(count_class(&panel.widget, "settings-firewall-warning"), 1);

        // The card opens on the hub, and ← appears on every destination page.
        let btn_back = find_buttons(&panel.widget, "term-btn")
            .into_iter()
            .find(|b| b.label().as_deref() == Some("←"))
            .expect("the header must offer a back button");
        assert!(shown(&pages[0]), "settings hub is the landing page");
        assert!(pages[1..].iter().all(|page| !shown(page)));
        assert!(!shown(&btn_back));
        assert_eq!(title_text(&panel.widget), "Settings");

        for (class, page_index, page_title) in [
            ("settings-shortcut-entry", 1, "Keyboard shortcut"),
            ("settings-harnesses-entry", 2, "Harness launchers"),
            ("settings-top-bar-entry", 3, "Top bar"),
            ("android-settings-entry", 4, "SUPER DESKTOP on Android"),
            ("settings-sleep-lock-entry", 5, "Sleep lock"),
        ] {
            let button = find_buttons(&panel.widget, class)
                .into_iter()
                .next()
                .expect("each settings destination needs a navigation button");
            button.emit_clicked();
            assert!(shown(&pages[page_index]));
            assert!(pages
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != page_index)
                .all(|(_, page)| !shown(page)));
            assert!(shown(&btn_back));
            assert_eq!(title_text(&panel.widget), page_title);
            btn_back.emit_clicked();
            assert!(shown(&pages[0]));
            assert!(!shown(&btn_back));
            assert_eq!(title_text(&panel.widget), "Settings");
        }

        // Reopening returns to the hub even when Android was the last page.
        let btn_launcher = find_buttons(&panel.widget, "android-settings-entry")
            .into_iter()
            .next()
            .expect("the Android destination must offer a navigation button");
        panel.widget.set_visible(false);
        btn_launcher.emit_clicked();
        assert!(shown(&pages[4]));
        panel.widget.set_visible(true);
        assert!(shown(&pages[0]), "reopening resets to the settings hub");
        assert!(pages[1..].iter().all(|page| !shown(page)));
        assert!(!shown(&btn_back));
        assert_eq!(title_text(&panel.widget), "Settings");

        // The recorder shows the shipped shortcut until one is recorded, and
        // switches to the stored one as soon as state.json has it. Note what
        // this test must NOT do: click Record. That would arm the real keymap
        // guard and park the shortcuts of the machine running the tests (see
        // `shortcut::begin_capture`).
        let record = find_buttons(&panel.widget, "launcher-btn")
            .into_iter()
            .find(|b| b.label().as_deref() == Some("⏺ Record"))
            .expect("the shortcut section must offer a Record button");
        assert_eq!(combo_text(&panel.widget), crate::shortcut::DEFAULT_COMBO);
        assert!(shortcut_changes.borrow().is_empty(), "nothing recorded yet");

        app_state.borrow_mut().toggle_shortcut = Some("SUPER + SHIFT + K".to_string());
        (panel.refresh)();
        assert_eq!(combo_text(&panel.widget), "SUPER + SHIFT + K");
        assert_eq!(record.label().as_deref(), Some("⏺ Record"));

        // Large preserves the original toolbar scale. Choosing another size
        // updates state, selection styling and the live-dock callback.
        let size_buttons = find_buttons(&panel.widget, "top-bar-size");
        assert_eq!(size_buttons.len(), 3);
        assert!(size_buttons[2].has_css_class("top-bar-size-active"));
        size_buttons[0].emit_clicked();
        assert_eq!(app_state.borrow().top_bar_size, TopBarSize::Small);
        assert_eq!(*size_changes.borrow(), [TopBarSize::Small]);
        assert!(size_buttons[0].has_css_class("top-bar-size-active"));
        assert!(!size_buttons[2].has_css_class("top-bar-size-active"));

        // One toggle row per detected harness, all ON by default.
        let toggles = find_buttons(&panel.widget, "harness-toggle");
        assert_eq!(
            toggles.len(),
            detected.len(),
            "expected one toggle per detected harness"
        );
        assert!(
            toggles.iter().all(|b| b.label().as_deref() == Some("ON")),
            "every detected harness must start enabled"
        );

        // Flipping the first row reports exactly that harness as hidden, and
        // reopening the panel keeps it off.
        toggles[0].emit_clicked();
        let hidden = detected[0].key.to_string();
        let expected: Vec<String> = detected_keys(&detected)
            .into_iter()
            .filter(|k| *k != hidden)
            .collect();
        assert_eq!(seen.borrow().len(), 1, "one change per click");
        assert_eq!(seen.borrow()[0], expected);

        app_state.borrow_mut().visible_harnesses = Some(expected.clone());
        (panel.refresh)();
        let toggles = find_buttons(&panel.widget, "harness-toggle");
        assert_eq!(
            toggles[0].label().as_deref(),
            Some("OFF"),
            "stored selection must stick"
        );
        assert!(toggles[1..]
            .iter()
            .all(|b| b.label().as_deref() == Some("ON")));
    }

    /// Every widget carrying `class` in the subtree rooted at `w`.
    fn find_widgets(w: &gtk4::Widget, class: &str) -> Vec<gtk4::Widget> {
        let mut out = Vec::new();
        if w.has_css_class(class) {
            out.push(w.clone());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            out.extend(find_widgets(&c, class));
            child = c.next_sibling();
        }
        out
    }

    /// How many widgets carry `class` in the subtree rooted at `w`.
    fn count_class(w: &gtk4::Widget, class: &str) -> usize {
        find_widgets(w, class).len()
    }

    /// The text of the recorder's combo label.
    fn combo_text(w: &gtk4::Widget) -> String {
        let found = find_widgets(w, "shortcut-combo");
        assert_eq!(found.len(), 1, "exactly one combo label");
        found[0]
            .downcast_ref::<Label>()
            .expect("combo label is a Label")
            .label()
            .to_string()
    }

    /// The widget's own `visible` property. NOT `is_visible()`: GTK4's walks
    /// up to the root, so every page of an unshown card would read as hidden
    /// and the navigation assertions below could not tell them apart.
    fn shown<W: IsA<gtk4::Widget>>(w: &W) -> bool {
        w.property::<bool>("visible")
    }

    /// The card header's title, which follows the current page.
    fn title_text(w: &gtk4::Widget) -> String {
        let found = find_widgets(w, "term-title");
        assert_eq!(found.len(), 1, "exactly one card title");
        found[0]
            .downcast_ref::<Label>()
            .expect("card title is a Label")
            .label()
            .to_string()
    }

    /// Every button carrying `class` in the subtree rooted at `w`.
    fn find_buttons(w: &gtk4::Widget, class: &str) -> Vec<Button> {
        let mut out = Vec::new();
        if w.has_css_class(class) {
            if let Some(b) = w.downcast_ref::<Button>() {
                out.push(b.clone());
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            out.extend(find_buttons(&c, class));
            child = c.next_sibling();
        }
        out
    }
}
