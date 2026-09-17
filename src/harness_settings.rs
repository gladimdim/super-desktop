//! ⚙ Harness settings panel for the overlay HUD.
//!
//! A floating card that lives INSIDE the super-desktop layer-shell overlay
//! (centered above notes and terminals) — never a separate Hyprland window. It
//! lists the harnesses that actually resolve on THIS machine (see
//! `tmux::detect_harnesses`) and lets the user pick which of them appear as
//! launch buttons in the top bar.
//!
//! Same visual language as the 📱 launcher panel: `mini-terminal` +
//! `term-header` card chrome whose body is a stack of numbered
//! `.launcher-section` panels (chrome helpers shared with `launcher_settings`).
//! Colours, radii and spacing live in `styles.rs` (see the "Harness Settings
//! Panel" block) — this file only builds widgets and reads detection.
//!
//! The selection itself is owned by the window: this panel reads it, reports
//! changes through `on_change` and re-detects every time it is opened.

use gtk4::prelude::*;
use gtk4::{pango, Align, Box, Button, Image, Label, Orientation, PolicyType, ScrolledWindow};
use std::cell::RefCell;
use std::rc::Rc;

use crate::launcher_settings::{chip, section_card};
use crate::state::AppState;
use crate::tmux::{detect_harnesses, HarnessInfo};

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

/// Build the panel. `on_change` receives the new full selection (harness keys,
/// in detection order) whenever the user flips a row or a bulk button.
pub fn build_harness_settings_panel(
    state: Rc<RefCell<AppState>>,
    on_change: Rc<dyn Fn(Vec<String>)>,
) -> HarnessSettingsPanel {
    let outer = Box::new(Orientation::Vertical, 0);
    outer.add_css_class("mini-terminal");
    outer.add_css_class("harness-panel");
    outer.set_size_request(660, 620);

    // ---- header: badge, title + subtitle, close ----
    let header = Box::new(Orientation::Horizontal, 10);
    header.add_css_class("term-header");

    let badge = Label::new(Some("⚙"));
    badge.add_css_class("launcher-head-badge");
    badge.set_valign(Align::Center);
    header.append(&badge);

    let titles = Box::new(Orientation::Vertical, 0);
    titles.set_hexpand(true);
    titles.set_valign(Align::Center);
    let title = Label::new(Some("Harness settings"));
    title.add_css_class("term-title");
    title.set_halign(Align::Start);
    let subtitle = Label::new(Some("Top bar launch buttons"));
    subtitle.add_css_class("launcher-subtitle");
    subtitle.set_halign(Align::Start);
    titles.append(&title);
    titles.append(&subtitle);
    header.append(&titles);

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

    // ---- 1 · harnesses installed here, each with a show/hide toggle ----
    let (head, body) = section_card(&root, "1", "Harnesses on this machine");
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

    // ---- 2 · bulk actions ----
    let (_, body) = section_card(&root, "2", "Top bar");
    let actions = Box::new(Orientation::Horizontal, 8);
    actions.add_css_class("launcher-actions");
    let btn_all = Button::with_label("✓ Show all");
    btn_all.set_tooltip_text(Some("Show every harness installed on this machine"));
    btn_all.add_css_class("launcher-btn");
    btn_all.add_css_class("launcher-btn-primary");
    let btn_none = Button::with_label("✕ Hide all");
    btn_none.set_tooltip_text(Some("Keep only the note / settings / launcher buttons"));
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

    let footer = Label::new(Some(
        "detected with which / npx · selection stored in state.json",
    ));
    footer.add_css_class("launcher-footer");
    footer.set_xalign(0.5);
    root.append(&footer);

    let scroll = ScrolledWindow::new();
    scroll.add_css_class("launcher-scroll");
    scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroll.set_child(Some(&root));
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    outer.append(&scroll);

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
        Rc::new(move || {
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
        let panel = build_harness_settings_panel(
            Rc::clone(&app_state),
            Rc::new(move |keys: Vec<String>| seen_cb.borrow_mut().push(keys)),
        );
        (panel.refresh)();

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
