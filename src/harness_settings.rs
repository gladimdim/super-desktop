//! ⚙ Settings card for the overlay HUD.
//!
//! A floating card that lives INSIDE the super-desktop layer-shell overlay
//! (centered above notes and terminals) — never a separate Hyprland window. It
//! is a two-page panel; the header (badge, title, ← back, ✕) is shared:
//!
//! 1. **Settings page**
//!    - **Show / hide shortcut** — a recorder: click Record, press a
//!      combination, and it becomes the Hyprland binding for
//!      `super-desktop toggle`. Capture, spelling and the Hyprland side live in
//!      `crate::shortcut`; this file owns the widget state machine and every
//!      way out of a recording.
//!    - **Top bar launch buttons** — the harnesses that actually resolve on THIS
//!      machine (see `tmux::detect_harnesses`), each with a show/hide toggle.
//!    - **Launcher connection** — an entry that navigates to page 2.
//! 2. **Launcher connection page** — the 📱 bridge status, addresses and pairing
//!    PIN, built by `crate::launcher_settings`; ← returns to the settings page.
//!
//! Same visual language as notes/terminals: `mini-terminal` + `term-header`
//! card chrome whose body is a stack of numbered `.launcher-section` panels
//! (chrome helpers shared with `launcher_settings`). Colours, radii and spacing
//! live in `styles.rs` — this file only builds widgets, reads detection, drives
//! the recorder and swaps the pages.
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
use crate::state::AppState;
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

/// Build the panel.
///
/// `on_change` receives the new full selection (harness keys, in detection
/// order) whenever the user flips a row or a bulk button;
/// `on_shortcut_change` receives a freshly recorded key combination once it has
/// been written into Hyprland's config.
pub fn build_harness_settings_panel(
    state: Rc<RefCell<AppState>>,
    on_change: Rc<dyn Fn(Vec<String>)>,
    on_shortcut_change: Rc<dyn Fn(String)>,
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
    let subtitle = Label::new(Some("Shortcut · top bar launch buttons · launcher"));
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

    let root = Box::new(Orientation::Vertical, 10);
    root.add_css_class("launcher-body");

    // ---- 1 · the overlay's own show / hide shortcut ----
    //
    // Recording seizes the keyboard for a few seconds (`shortcut::begin_capture`
    // parks Hyprland in a throw-away submap so no global bind can eat the key),
    // so every exit — Esc, the button turning into Cancel, the watchdog, the
    // panel being closed, the panel being reopened — funnels through
    // `stop_recording`, which is also the only thing that releases the guard.
    let (_, body) = section_card(&root, "1", "Show / hide shortcut");
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

    // ---- 2 · harnesses installed here, each with a show/hide toggle ----
    let (head, body) = section_card(&root, "2", "Harnesses on this machine");
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
        "Only harnesses whose CLI resolves here are listed · install one, then reopen this panel.",
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

    // ---- 3 · bulk actions ----
    let (_, body) = section_card(&root, "3", "Top bar");
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

    let note = Label::new(Some(
        "Hidden harnesses stay installed — `super-desktop add-term <key>` still launches one.",
    ));
    note.add_css_class("launcher-hint");
    note.set_xalign(0.0);
    note.set_wrap(true);
    body.append(&note);

    // ---- 4 · launcher connection: navigates to the 📱 page ----
    let (_, body) = section_card(&root, "4", "Launcher connection");
    let launcher_hint = Label::new(Some(
        "Bridge status, LAN / Tailscale addresses and the pairing PIN for the \
         OmarchyAILauncher Android app.",
    ));
    launcher_hint.add_css_class("launcher-hint");
    launcher_hint.set_xalign(0.0);
    launcher_hint.set_wrap(true);
    body.append(&launcher_hint);

    let btn_launcher = Button::with_label("📱 Open launcher connection");
    btn_launcher.set_tooltip_text(Some("Bridge status, IPs and the pairing PIN"));
    btn_launcher.add_css_class("launcher-btn");
    btn_launcher.add_css_class("launcher-btn-primary");
    let launcher_row = Box::new(Orientation::Horizontal, 8);
    launcher_row.add_css_class("launcher-actions");
    launcher_row.append(&btn_launcher);
    body.append(&launcher_row);

    let footer = Label::new(Some(
        "detected with which / npx · selection stored in state.json",
    ));
    footer.add_css_class("launcher-footer");
    footer.set_xalign(0.5);
    root.append(&footer);

    let scroll = ScrolledWindow::new();
    scroll.add_css_class("launcher-scroll");
    scroll.add_css_class("harness-page");
    scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroll.set_child(Some(&root));
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);

    // ---- pages: settings (above) ⇄ 📱 launcher connection ----
    // Both pages live in the same card and are swapped by visibility, so the
    // header, the recorder state and the harness rows survive navigating away
    // and back. The launcher page is built here (`launcher_settings`) because
    // its content belongs to that file; it is refreshed on every entry.
    let launcher_page = crate::launcher_settings::build_launcher_page();
    let launcher_view = launcher_page.widget.clone();

    let pages = Box::new(Orientation::Vertical, 0);
    pages.add_css_class("harness-pages");
    pages.set_vexpand(true);
    pages.append(&scroll);
    pages.append(&launcher_view);
    launcher_view.set_visible(false);
    outer.append(&pages);

    // `nav(true)` selects the 📱 launcher page, `nav(false)` the settings page.
    let nav: Rc<dyn Fn(bool)> = {
        let settings_view = scroll.clone();
        let launcher_view = launcher_view.clone();
        let btn_back = btn_back.clone();
        let badge = badge.clone();
        let title = title.clone();
        let subtitle = subtitle.clone();
        let launcher_refresh = Rc::clone(&launcher_page.refresh);
        Rc::new(move |launcher: bool| {
            settings_view.set_visible(!launcher);
            launcher_view.set_visible(launcher);
            btn_back.set_visible(launcher);
            if launcher {
                badge.set_label("📱");
                title.set_label("Launcher connection");
                subtitle.set_label("OmarchyAILauncher bridge");
                launcher_refresh();
            } else {
                badge.set_label("⚙");
                title.set_label("Settings");
                subtitle.set_label("Shortcut · top bar launch buttons · launcher");
            }
        })
    };

    btn_launcher.connect_clicked({
        let nav = Rc::clone(&nav);
        move |_| nav(true)
    });
    btn_back.connect_clicked({
        let nav = Rc::clone(&nav);
        move |_| nav(false)
    });
    // Reopening always lands on the settings page: the card is re-shown by the
    // HUD gear, and coming back to a half-finished launcher page (or to a
    // stale bridge reading) would be a surprise. While the card stays open a
    // theme switch may refresh it — the page must not jump.
    outer.connect_visible_notify({
        let nav = Rc::clone(&nav);
        move |o| {
            if o.is_visible() {
                nav(false);
            }
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
        // Reopening (or restyling on a theme switch) must never leave a
        // recording armed with the keyboard held.
        let stop_recording = Rc::clone(&stop_recording);
        let paint_recorder = Rc::clone(&paint_recorder);
        Rc::new(move || {
            stop_recording(None);
            paint_recorder();

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

            *order.borrow_mut() = detected_keys(&detected);
            *selection.borrow_mut() =
                resolve_visible(state.borrow().visible_harnesses.as_deref(), &detected);
            paint();
        })
    };

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

    HarnessSettingsPanel {
        widget: outer.upcast(),
        refresh: Rc::clone(&refresh),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let panel = build_harness_settings_panel(
            Rc::clone(&app_state),
            Rc::new(move |keys: Vec<String>| seen_cb.borrow_mut().push(keys)),
            Rc::new(move |combo: String| shortcut_cb.borrow_mut().push(combo)),
        );
        (panel.refresh)();

        // Two pages in one card: the settings page — 4 numbered sections
        // (shortcut, harnesses, top bar, launcher entry) — and the 📱 launcher
        // page (5: bridge, firewall, addresses, pairing, phone steps).
        let pages = find_widgets(&panel.widget, "harness-page");
        assert_eq!(pages.len(), 2, "settings + launcher pages");
        let sections: Vec<usize> = pages
            .iter()
            .map(|p| count_class(p, "launcher-section"))
            .collect();
        assert_eq!(sections, vec![4, 5]);
        assert_eq!(count_class(&panel.widget, "launcher-section-num"), 9);
        assert_eq!(count_class(&panel.widget, "launcher-section-title"), 9);

        // The card opens on the settings page, and ← shows up only while the
        // launcher page does.
        let btn_back = find_buttons(&panel.widget, "term-btn")
            .into_iter()
            .find(|b| b.label().as_deref() == Some("←"))
            .expect("the header must offer a back button");
        assert!(shown(&pages[0]), "settings page is the landing page");
        assert!(!shown(&pages[1]));
        assert!(!shown(&btn_back));
        assert_eq!(title_text(&panel.widget), "Settings");

        // The launcher section navigates to the 📱 page, ← navigates back, and
        // neither throws away the settings page.
        let btn_launcher = find_buttons(&panel.widget, "launcher-btn")
            .into_iter()
            .find(|b| b.label().as_deref() == Some("📱 Open launcher connection"))
            .expect("the launcher section must offer the navigation button");
        btn_launcher.emit_clicked();
        assert!(!shown(&pages[0]));
        assert!(shown(&pages[1]));
        assert!(shown(&btn_back));
        assert_eq!(title_text(&panel.widget), "Launcher connection");

        btn_back.emit_clicked();
        assert!(shown(&pages[0]));
        assert!(!shown(&pages[1]));
        assert!(!shown(&btn_back));
        assert_eq!(title_text(&panel.widget), "Settings");

        // Reopening the card lands on the settings page too, even when the
        // launcher page was the last thing up: the window hides the card with
        // `set_visible(false)` and the HUD gear re-shows it.
        panel.widget.set_visible(false);
        btn_launcher.emit_clicked();
        assert!(shown(&pages[1]));
        panel.widget.set_visible(true);
        assert!(shown(&pages[0]), "reopening resets to the settings page");
        assert!(!shown(&pages[1]));
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
