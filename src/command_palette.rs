//! Floating fuzzy Command Palette & slash-command launcher + card triage helpers.

use gtk4::{gdk, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const MAX_RESULTS: usize = 24;

#[derive(Clone)]
pub struct PaletteItem {
    pub title: String,
    pub subtitle: String,
    pub badge: String,
    pub keywords: String,
    pub action: Rc<dyn Fn()>,
}

/// Rank `items` against `query` using `slash_commands::fuzzy_score`, returning
/// matched indices ordered by descending relevance (stable on ties).
pub fn rank_items(query: &str, items: &[PaletteItem]) -> Vec<usize> {
    let mut scored: Vec<(usize, i32)> = items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let mut best = crate::slash_commands::fuzzy_score(query, &item.title);
            for field in [&item.keywords, &item.subtitle, &item.badge] {
                if let Some(s) = crate::slash_commands::fuzzy_score(query, field) {
                    let penalized = s - 5;
                    best = Some(best.map_or(penalized, |b| b.max(penalized)));
                }
            }
            best.map(|s| (idx, s))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.into_iter().take(MAX_RESULTS).map(|(idx, _)| idx).collect()
}

/// Find the next card index after `current` whose status is `WAITING` or `ERROR`.
pub fn next_attention_index(statuses: &[&str], current: Option<usize>) -> Option<usize> {
    if statuses.is_empty() {
        return None;
    }
    let start = current.map(|i| (i + 1) % statuses.len()).unwrap_or(0);
    (0..statuses.len())
        .map(|offset| (start + offset) % statuses.len())
        .find(|&idx| Some(idx) != current && matches!(statuses[idx], "WAITING" | "ERROR"))
}

/// Cycle `0..len` forward or backward with wrap-around.
pub fn cycle_mru_index(len: usize, current: Option<usize>, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (current, forward) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(cur), true) => (cur + 1) % len,
        (Some(cur), false) => {
            if cur == 0 { len - 1 } else { cur - 1 }
        }
    })
}

pub struct CommandPalette {
    pub root: gtk4::Box,
    heading: gtk4::Label,
    entry: gtk4::Entry,
    list: gtk4::ListBox,
    items: Rc<RefCell<Vec<PaletteItem>>>,
    shown: Rc<RefCell<Vec<usize>>>,
    on_close: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    suppress_change: Rc<Cell<bool>>,
}

impl CommandPalette {
    pub fn new() -> Rc<Self> {
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        root.add_css_class("command-palette-backdrop");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_halign(gtk4::Align::Fill);
        root.set_valign(gtk4::Align::Fill);
        root.set_visible(false);

        let card = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        card.add_css_class("command-palette-card");
        card.set_halign(gtk4::Align::Center);
        card.set_valign(gtk4::Align::Start);
        card.set_margin_top(68);
        card.set_size_request(520, -1);

        let heading = gtk4::Label::new(Some("Command Palette"));
        heading.add_css_class("command-palette-title");
        heading.set_xalign(0.0);
        card.append(&heading);

        let entry = gtk4::Entry::new();
        entry.add_css_class("command-palette-entry");
        entry.set_placeholder_text(Some("Search cards, commands, or type / for slash commands…"));
        entry.set_hexpand(true);
        card.append(&entry);

        let list = gtk4::ListBox::new();
        list.add_css_class("command-palette-list");
        list.set_selection_mode(gtk4::SelectionMode::Single);

        let scroller = gtk4::ScrolledWindow::new();
        scroller.set_min_content_height(120);
        scroller.set_max_content_height(360);
        scroller.set_propagate_natural_height(true);
        scroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scroller.set_child(Some(&list));
        card.append(&scroller);

        root.append(&card);

        let palette = Rc::new(Self {
            root,
            heading,
            entry,
            list,
            items: Rc::new(RefCell::new(Vec::new())),
            shown: Rc::new(RefCell::new(Vec::new())),
            on_close: Rc::new(RefCell::new(None)),
            suppress_change: Rc::new(Cell::new(false)),
        });

        let weak = Rc::downgrade(&palette);
        palette.entry.connect_changed(move |_| {
            if let Some(p) = weak.upgrade() {
                if !p.suppress_change.get() {
                    p.rebuild_rows();
                }
            }
        });

        let weak = Rc::downgrade(&palette);
        palette.list.connect_row_activated(move |_, row| {
            if let Some(p) = weak.upgrade() {
                p.activate_row(row.index());
            }
        });

        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let weak = Rc::downgrade(&palette);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(p) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            if key == gdk::Key::Escape {
                p.close();
                return glib::Propagation::Stop;
            }
            if key == gdk::Key::Return || key == gdk::Key::KP_Enter {
                let idx = p.list.selected_row().map(|r| r.index()).unwrap_or(0);
                p.activate_row(idx);
                return glib::Propagation::Stop;
            }
            if key == gdk::Key::Down || (ctrl && matches!(key, gdk::Key::n | gdk::Key::N | gdk::Key::j | gdk::Key::J)) {
                p.move_selection(1);
                return glib::Propagation::Stop;
            }
            if key == gdk::Key::Up || (ctrl && matches!(key, gdk::Key::p | gdk::Key::P | gdk::Key::k | gdk::Key::K)) {
                p.move_selection(-1);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        palette.root.add_controller(keys);

        let click = gtk4::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        let weak = Rc::downgrade(&palette);
        let card_ref = card;
        click.connect_pressed(move |_, _, x, y| {
            let Some(p) = weak.upgrade() else { return };
            if let Some(bounds) = card_ref.compute_bounds(&p.root) {
                let inside = x >= bounds.x() as f64
                    && x <= (bounds.x() + bounds.width()) as f64
                    && y >= bounds.y() as f64
                    && y <= (bounds.y() + bounds.height()) as f64;
                if !inside {
                    p.close();
                }
            }
        });
        palette.root.add_controller(click);

        palette
    }

    pub fn open(&self, heading: &str, items: Vec<PaletteItem>, on_close: Option<Rc<dyn Fn()>>) {
        self.heading.set_text(heading);
        *self.items.borrow_mut() = items;
        *self.on_close.borrow_mut() = on_close;
        self.suppress_change.set(true);
        self.entry.set_text("");
        self.suppress_change.set(false);
        self.rebuild_rows();
        self.root.set_visible(true);
        self.entry.grab_focus();
    }

    pub fn close(&self) {
        if !self.root.is_visible() {
            return;
        }
        self.root.set_visible(false);
        self.items.borrow_mut().clear();
        self.shown.borrow_mut().clear();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        if let Some(cb) = self.on_close.borrow_mut().take() {
            cb();
        }
    }

    pub fn is_open(&self) -> bool {
        self.root.is_visible()
    }

    fn rebuild_rows(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let query = self.entry.text().to_string();
        let items = self.items.borrow();
        let indices = rank_items(&query, &items);
        *self.shown.borrow_mut() = indices.clone();
        for idx in indices {
            let item = &items[idx];
            let row_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            row_box.add_css_class("command-palette-row");

            let text_col = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
            text_col.set_hexpand(true);
            let title = gtk4::Label::new(Some(&item.title));
            title.add_css_class("command-palette-item-title");
            title.set_xalign(0.0);
            title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            text_col.append(&title);

            if !item.subtitle.is_empty() {
                let sub = gtk4::Label::new(Some(&item.subtitle));
                sub.add_css_class("command-palette-item-sub");
                sub.set_xalign(0.0);
                sub.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                text_col.append(&sub);
            }
            row_box.append(&text_col);

            if !item.badge.is_empty() {
                let badge = gtk4::Label::new(Some(&item.badge));
                badge.add_css_class("command-palette-badge");
                badge.set_valign(gtk4::Align::Center);
                row_box.append(&badge);
            }
            self.list.append(&row_box);
        }
        if let Some(first) = self.list.row_at_index(0) {
            self.list.select_row(Some(&first));
        }
    }

    fn move_selection(&self, delta: i32) {
        let count = self.shown.borrow().len() as i32;
        if count <= 0 {
            return;
        }
        let cur = self.list.selected_row().map(|r| r.index()).unwrap_or(0);
        let next = (cur + delta).rem_euclid(count);
        if let Some(row) = self.list.row_at_index(next) {
            self.list.select_row(Some(&row));
            row.grab_focus();
            self.entry.grab_focus();
        }
    }

    fn activate_row(&self, list_idx: i32) {
        let Some(item_idx) = usize::try_from(list_idx)
            .ok()
            .and_then(|i| self.shown.borrow().get(i).copied())
        else {
            return;
        };
        let action = self.items.borrow().get(item_idx).map(|it| Rc::clone(&it.action));
        // Clear `on_close` before closing so the activated action decides focus.
        *self.on_close.borrow_mut() = None;
        self.close();
        if let Some(action) = action {
            action();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, keywords: &str, hit: &Rc<Cell<usize>>, id: usize) -> PaletteItem {
        let hit = Rc::clone(hit);
        PaletteItem {
            title: title.into(),
            subtitle: "action".into(),
            badge: "TEST".into(),
            keywords: keywords.into(),
            action: Rc::new(move || hit.set(id)),
        }
    }

    #[test]
    fn rank_items_and_triage_helpers_work_purely() {
        let hit = Rc::new(Cell::new(0));
        let items = vec![
            item("Open Files", "files", &hit, 1),
            item("Run /compact", "claude", &hit, 2),
            item("/compact", "slash", &hit, 3),
        ];
        let ranked = rank_items("/comp", &items);
        assert_eq!(ranked.first().copied(), Some(2));
        assert!(rank_items("zzz", &items).is_empty());

        let statuses = ["IDLE", "WORKING", "WAITING", "IDLE", "ERROR"];
        assert_eq!(next_attention_index(&["WORKING", "WAITING"], Some(1)), None);
        assert_eq!(next_attention_index(&["ERROR"], None), Some(0));
        assert_eq!(next_attention_index(&statuses, None), Some(2));
        assert_eq!(next_attention_index(&statuses, Some(2)), Some(4));
        assert_eq!(next_attention_index(&statuses, Some(4)), Some(2));
        assert_eq!(next_attention_index(&["IDLE", "WORKING"], None), None);

        assert_eq!(cycle_mru_index(3, None, true), Some(0));
        assert_eq!(cycle_mru_index(3, Some(2), true), Some(0));
        assert_eq!(cycle_mru_index(3, Some(0), false), Some(2));
        assert_eq!(cycle_mru_index(0, None, true), None);
    }

    #[test]
    fn attention_shortcut_uses_the_latest_status_after_focus_changes() {
        let mut cards = ["WORKING", "WAITING", "ERROR"];
        let focus = next_attention_index(&cards, Some(0));
        assert_eq!(focus, Some(1));
        cards[1] = "WORKING";
        let focus = next_attention_index(&cards, focus);
        assert_eq!(focus, Some(2));
        assert_eq!(next_attention_index(&cards, focus), None);
        cards[0] = "WAITING";
        assert_eq!(next_attention_index(&cards, focus), Some(0));
    }

    #[test]
    fn command_palette_opens_filters_and_activates_item() {
        crate::gtk_test::run_in_child_process(
            "command_palette::tests::command_palette_opens_filters_and_activates_item_child",
        );
    }

    #[test]
    fn command_palette_opens_filters_and_activates_item_child() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let _ = gtk4::init();
        let palette = CommandPalette::new();
        let win = gtk4::Window::new();
        win.set_child(Some(&palette.root));
        win.present();

        let hit = Rc::new(Cell::new(0));
        palette.open(
            "Command Palette",
            vec![
                item("Arrange Workspace", "layout", &hit, 1),
                item("Restart fresh", "session", &hit, 2),
                item("/compact", "claude", &hit, 3),
            ],
            None,
        );
        assert!(palette.is_open());
        palette.entry.set_text("restart");
        assert_eq!(*palette.shown.borrow(), vec![1]);
        palette.activate_row(0);
        assert_eq!(hit.get(), 2);
        assert!(!palette.is_open());
        win.close();
    }
}
