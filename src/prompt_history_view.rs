//! A card's prompt history panel: every prompt submitted to that terminal,
//! newest first, with its date and time (`prompt_log`). Reading the history
//! happens off GTK's thread.
use gtk4::{gdk, gio, glib, prelude::*};
use std::cell::Cell;
use std::rc::Rc;

const DEFAULT_SIZE: (i32, i32) = (640, 560);
const MIN_SIZE: (i32, i32) = (380, 320);

thread_local! {
    static PANELS: crate::floating_panel::CardPanels = crate::floating_panel::CardPanels::new(DEFAULT_SIZE, MIN_SIZE);
}

/// Harnesses whose cards cannot list their prompts: Codex 0.160 and newer
/// keep the conversation in a shared background process, so a card cannot
/// tell which conversation is its own.
fn supported(agent: &str) -> bool {
    agent != "codex"
}

pub fn button(session: String, title: String, agent: &str) -> gtk4::Button {
    let button = gtk4::Button::from_icon_name("document-open-recent-symbolic");
    button.update_property(&[gtk4::accessible::Property::Label("Prompt history")]);
    button.add_css_class("term-btn");
    if !supported(agent) {
        button.set_sensitive(false);
        button.set_tooltip_text(Some("harness does not support this"));
        return button;
    }
    button.set_tooltip_text(Some("Prompts submitted to this terminal"));
    button.connect_clicked(move |_| open(&session, &title));
    button
}

/// Show the history panel of `session`, bringing it to the front when it
/// is already open.
fn open(session: &str, title: &str) {
    if !PANELS.with(|panels| panels.wants_new(session)) {
        return;
    }
    let drawer = build(session, title);
    let target = session.to_string();
    drawer.close.connect_clicked(move |_| close(&target));
    PANELS.with(|panels| panels.show(session, &drawer.widget, Rc::clone(&drawer.generation), None));
    (drawer.reload)();
}

/// Close the history panel of `session`; a history still loading is dropped.
fn close(session: &str) {
    PANELS.with(|panels| panels.close(session));
}

struct Drawer {
    widget: gtk4::Box,
    close: gtk4::Button,
    generation: Rc<Cell<u64>>,
    reload: Rc<dyn Fn()>,
}

/// "Today 14:05", "Yesterday 09:12", else "Oct 3, 2026 18:40", local time.
pub(crate) fn when(at_ms: Option<i64>, now: chrono::DateTime<chrono::Local>) -> String {
    let Some(at) = at_ms
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|at| at.with_timezone(&chrono::Local))
    else {
        return "Time unknown".into();
    };
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        0 => format!("Today {}", at.format("%H:%M")),
        1 => format!("Yesterday {}", at.format("%H:%M")),
        _ => at.format("%b %-d, %Y %H:%M").to_string(),
    }
}

fn row(text: &str, at: Option<i64>, now: chrono::DateTime<chrono::Local>) -> gtk4::Box {
    let item = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    item.add_css_class("asset-group");
    let head = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    let time = gtk4::Label::new(Some(&when(at, now)));
    time.add_css_class("launcher-subtitle");
    time.set_halign(gtk4::Align::Start);
    time.set_hexpand(true);
    head.append(&time);
    let copy = gtk4::Button::with_label("Copy");
    copy.set_tooltip_text(Some("Copy this prompt"));
    let value = text.to_string();
    copy.connect_clicked(move |button| {
        if let Some(display) = gdk::Display::default() {
            display.clipboard().set_text(&value);
            button.set_label("Copied");
            let button = button.downgrade();
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                if let Some(button) = button.upgrade() {
                    button.set_label("Copy");
                }
            });
        }
    });
    head.append(&copy);
    item.append(&head);
    let body = gtk4::Label::new(Some(text));
    body.set_wrap(true);
    body.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    body.set_xalign(0.0);
    body.set_selectable(true);
    body.add_css_class("prompt-history-text");
    item.append(&body);
    item
}

fn build(session: &str, card_title: &str) -> Drawer {
    let (outer, header) = crate::floating_panel::card_panel(
        &gtk4::Image::from_icon_name("document-open-recent-symbolic"),
        "Prompt history",
        card_title,
        MIN_SIZE,
    );
    let refresh = header.button("Refresh", None, None);
    let close = header.close_button("Close prompt history");

    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    body.set_vexpand(true);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    outer.append(&body);
    let status = gtk4::Label::new(Some("Loading prompts…"));
    status.set_wrap(true);
    status.set_xalign(0.0);
    body.append(&status);
    let scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .min_content_height(120)
        .build();
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    scroll.set_child(Some(&content));
    body.append(&scroll);

    let generation = Rc::new(Cell::new(0u64));
    let reload: Rc<dyn Fn()> = {
        let generation = Rc::clone(&generation);
        let session = session.to_string();
        Rc::new(move || {
            generation.set(generation.get() + 1);
            let ticket = generation.get();
            status.set_text("Loading prompts…");
            let generation = Rc::clone(&generation);
            let session = session.clone();
            let status = status.clone();
            let content = content.clone();
            glib::MainContext::default().spawn_local(async move {
                let history = gio::spawn_blocking(move || {
                    let (agent, persisted) = crate::state::load_state()
                        .terminals
                        .iter()
                        .find(|t| t.session_name == session)
                        .map(|t| (t.agent_type.clone(), t.agent_session_id.clone()))
                        .unwrap_or_else(|| ("shell".to_string(), None));
                    crate::prompt_log::history(&session, &agent, persisted.as_deref())
                })
                .await;
                if generation.get() != ticket {
                    return;
                }
                while let Some(child) = content.first_child() {
                    content.remove(&child);
                }
                let Ok(history) = history else {
                    status.set_text("Prompt history is unavailable.");
                    return;
                };
                let now = chrono::Local::now();
                for (text, at) in &history.prompts {
                    content.append(&row(text, *at, now));
                }
                let mut notes = Vec::new();
                notes.push(match history.prompts.len() {
                    0 => "No prompts yet.".to_string(),
                    1 => "1 prompt, newest first.".to_string(),
                    n => format!("{n} prompts, newest first."),
                });
                if history.truncated {
                    notes.push(format!("Showing the latest {}.", crate::prompt_log::MAX_PROMPTS));
                }
                if history.source == "journal" {
                    notes.push("Recorded by SUPER DESKTOP since its update.".into());
                }
                status.set_text(&notes.join(" "));
            });
        })
    };
    let again = Rc::clone(&reload);
    refresh.connect_clicked(move |_| again());
    Drawer { widget: outer, close, generation, reload }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn times_read_as_today_yesterday_or_a_date() {
        let now = chrono::Local.with_ymd_and_hms(2026, 10, 7, 15, 0, 0).unwrap();
        let ms = |d, h, m| chrono::Local.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap().timestamp_millis();
        assert_eq!(when(Some(ms(7, 14, 5)), now), "Today 14:05");
        assert_eq!(when(Some(ms(6, 9, 12)), now), "Yesterday 09:12");
        assert_eq!(when(Some(ms(3, 18, 40)), now), "Oct 3, 2026 18:40");
        assert_eq!(when(None, now), "Time unknown");
    }

    #[test]
    fn history_panel_builds_its_header_and_rows() {
        crate::gtk_test::run_in_child_process("prompt_history_view::tests::panel_inner");
    }

    #[test]
    fn panel_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let drawer = build("sd_term_1_a", "Codex · ~/project");
        let mut labels = Vec::new();
        let mut stack = vec![drawer.widget.clone().upcast::<gtk4::Widget>()];
        while let Some(widget) = stack.pop() {
            if let Some(label) = widget.downcast_ref::<gtk4::Label>() {
                labels.push(label.text().to_string());
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                child = next.next_sibling();
                stack.push(next);
            }
        }
        assert!(labels.iter().any(|l| l == "Prompt history"));
        assert!(labels.iter().any(|l| l == "Codex · ~/project"));
        let now = chrono::Local::now();
        let item = row("first line\nsecond line", Some(now.timestamp_millis()), now);
        let body = item.last_child().unwrap().downcast::<gtk4::Label>().unwrap();
        assert_eq!(body.text(), "first line\nsecond line");
        assert!(body.is_selectable());
        let button = button("sd_term_1_a".into(), "Claude".into(), "claude");
        assert_eq!(button.icon_name().as_deref(), Some("document-open-recent-symbolic"));
        assert!(button.is_sensitive());
        let codex = super::button("sd_term_1_b".into(), "Codex".into(), "codex");
        assert!(!codex.is_sensitive());
        assert_eq!(codex.tooltip_text().as_deref(), Some("harness does not support this"));
    }
}
