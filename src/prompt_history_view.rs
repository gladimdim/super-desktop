//! A card's prompt history panel: every prompt submitted to that terminal,
//! newest first, with its date and time (`prompt_log`). Reading the history
//! happens off GTK's thread.
use gtk4::{gdk, gio, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const DEFAULT_SIZE: (i32, i32) = (640, 560);
const MIN_SIZE: (i32, i32) = (380, 320);

struct Open {
    session: String,
    panel: Rc<crate::floating_panel::MovablePanel>,
    generation: Rc<Cell<u64>>,
}

thread_local! {
    static OPEN: RefCell<Vec<Open>> = const { RefCell::new(Vec::new()) };
    /// Where and how big the user last left a history panel.
    static LAST: Cell<(Option<(i32, i32)>, Option<(i32, i32)>)> = const { Cell::new((None, None)) };
}

pub fn button(session: String, title: String) -> gtk4::Button {
    let button = gtk4::Button::from_icon_name("document-open-recent-symbolic");
    button.update_property(&[gtk4::accessible::Property::Label("Prompt history")]);
    button.add_css_class("term-btn");
    button.set_tooltip_text(Some("Prompts submitted to this terminal"));
    button.connect_clicked(move |_| open(&session, &title));
    button
}

/// Show the history panel of `session`, bringing it to the front and
/// reloading it when it is already open.
fn open(session: &str, title: &str) {
    let Some((overlay, top, ceiling)) = crate::asset_view::host() else {
        return;
    };
    let existing = OPEN.with(|open| {
        open.borrow().iter().find(|o| o.session == session).map(|o| Rc::clone(&o.panel))
    });
    if let Some(panel) = existing {
        if panel.is_in(&overlay) {
            panel.raise(ceiling.as_ref());
            return;
        }
        close(session);
    }
    let drawer = build(session, title);
    let (mut saved_pos, saved_size) = LAST.get();
    let others = OPEN.with(|open| open.borrow().len()) as i32;
    saved_pos = saved_pos.map(|(x, y)| (x + 32 * others, y + 32 * others));
    let panel = crate::floating_panel::MovablePanel::install(
        &overlay,
        &drawer.widget,
        crate::floating_panel::PanelLayout { default_size: DEFAULT_SIZE, min_size: MIN_SIZE, saved_pos, saved_size },
        top,
        Rc::new(|position, size| LAST.set((Some(position), Some(size)))),
    );
    panel.raise(ceiling.as_ref());
    let press = gtk4::GestureClick::new();
    press.set_button(0);
    press.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let weak = Rc::downgrade(&panel);
    let ceiling = ceiling.map(|c| c.downgrade());
    press.connect_pressed(move |_, _, _, _| {
        if let Some(panel) = weak.upgrade() {
            panel.raise(ceiling.as_ref().and_then(|c| c.upgrade()).as_ref());
        }
    });
    drawer.widget.add_controller(press);
    let target = session.to_string();
    drawer.close.connect_clicked(move |_| close(&target));
    OPEN.with(|open| {
        open.borrow_mut().push(Open {
            session: session.to_string(),
            panel,
            generation: Rc::clone(&drawer.generation),
        })
    });
    (drawer.reload)();
}

/// Close the history panel of `session`; a history still loading is dropped.
fn close(session: &str) {
    let closed = OPEN.with(|open| {
        let mut open = open.borrow_mut();
        let index = open.iter().position(|o| o.session == session)?;
        Some(open.remove(index))
    });
    if let Some(closed) = closed {
        closed.generation.set(closed.generation.get() + 1);
        closed.panel.remove();
    }
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
    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    outer.add_css_class("mini-terminal");
    outer.add_css_class("harness-panel");
    outer.add_css_class("asset-drawer");
    outer.set_size_request(MIN_SIZE.0, MIN_SIZE.1);
    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    header.add_css_class("term-header");
    let badge = gtk4::Image::from_icon_name("document-open-recent-symbolic");
    badge.add_css_class("launcher-head-badge");
    badge.set_valign(gtk4::Align::Center);
    header.append(&badge);
    let titles = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    titles.set_hexpand(true);
    titles.set_valign(gtk4::Align::Center);
    let title = gtk4::Label::new(Some("Prompt history"));
    title.add_css_class("term-title");
    title.set_halign(gtk4::Align::Start);
    let subtitle = gtk4::Label::new(Some(card_title));
    subtitle.add_css_class("launcher-subtitle");
    subtitle.set_halign(gtk4::Align::Start);
    subtitle.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    titles.append(&title);
    titles.append(&subtitle);
    header.append(&titles);
    let refresh = gtk4::Button::with_label("Refresh");
    refresh.set_valign(gtk4::Align::Center);
    header.append(&refresh);
    let close = gtk4::Button::with_label("✕");
    close.set_tooltip_text(Some("Close prompt history"));
    close.add_css_class("term-btn");
    close.set_valign(gtk4::Align::Center);
    header.append(&close);
    outer.append(&header);

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
        let button = button("sd_term_1_a".into(), "Codex".into());
        assert_eq!(button.icon_name().as_deref(), Some("document-open-recent-symbolic"));
    }
}
