//! SD Editor's find and replace bar (Ctrl+F, Ctrl+H). Every match in the
//! open file is marked and counted, and marked again when the text changes.
use super::scheme;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// More matches than this are not marked or counted one by one.
const MAX_MATCHES: i32 = 10_000;

/// The open file the bar searches.
#[derive(Clone)]
struct Context {
    buffer: gtk4::TextBuffer,
    view: gtk4::TextView,
    /// Its `changed` handler: the matches follow the text.
    changed: Rc<glib::SignalHandlerId>,
}

pub struct Find {
    pub widget: gtk4::Revealer,
    pub search: gtk4::Entry,
    pub replace: gtk4::Entry,
    replace_row: gtk4::Box,
    count: gtk4::Label,
    case: gtk4::CheckButton,
    context: RefCell<Option<Context>>,
    /// How many matches the open file has; -1 without a search text.
    matches: Cell<i32>,
    /// The text changed and the matches are about to be marked again.
    rescan_pending: Cell<bool>,
}

fn button(label: &str, tip: &str) -> gtk4::Button {
    let button = gtk4::Button::with_label(label);
    button.add_css_class("term-btn");
    button.set_tooltip_text(Some(tip));
    button
}

impl Find {
    pub fn new() -> Rc<Self> {
        let search = gtk4::Entry::new();
        search.set_placeholder_text(Some("Find"));
        search.set_hexpand(true);
        let count = gtk4::Label::new(None);
        count.add_css_class("sd-editor-count");
        let case = gtk4::CheckButton::with_label("Aa");
        case.set_tooltip_text(Some("Match case"));
        let (previous, next, close) = (button("↑", "Previous (Shift+Enter)"), button("↓", "Next (Enter)"), button("✕", "Close (Esc)"));
        let find_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        for widget in [search.upcast_ref::<gtk4::Widget>(), count.upcast_ref(), case.upcast_ref(), previous.upcast_ref(), next.upcast_ref(), close.upcast_ref()] {
            find_row.append(widget);
        }
        let replace = gtk4::Entry::new();
        replace.set_placeholder_text(Some("Replace with"));
        replace.set_hexpand(true);
        let (one, all) = (button("Replace", "Replace this match"), button("All", "Replace every match"));
        let replace_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        replace_row.append(&replace);
        replace_row.append(&one);
        replace_row.append(&all);
        let column = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        column.add_css_class("sd-editor-bar");
        column.append(&find_row);
        column.append(&replace_row);
        let widget = gtk4::Revealer::new();
        widget.set_child(Some(&column));
        widget.set_transition_type(gtk4::RevealerTransitionType::SlideUp);

        let find = Rc::new(Self {
            widget,
            search,
            replace,
            replace_row,
            count,
            case,
            context: RefCell::new(None),
            matches: Cell::new(-1),
            rescan_pending: Cell::new(false),
        });
        find.search.connect_changed({
            let weak = Rc::downgrade(&find);
            move |_| {
                if let Some(find) = weak.upgrade() {
                    find.rescan();
                    find.select_from_cursor(true);
                }
            }
        });
        find.case.connect_toggled({
            let weak = Rc::downgrade(&find);
            move |_| {
                if let Some(find) = weak.upgrade() {
                    find.rescan();
                }
            }
        });
        let act = |find: &Rc<Self>, f: fn(&Self)| {
            let weak = Rc::downgrade(find);
            move |_: &gtk4::Button| {
                if let Some(find) = weak.upgrade() {
                    f(&find);
                }
            }
        };
        next.connect_clicked(act(&find, |f| f.step(true)));
        previous.connect_clicked(act(&find, |f| f.step(false)));
        close.connect_clicked(act(&find, Self::hide));
        one.connect_clicked(act(&find, Self::replace_one));
        all.connect_clicked(act(&find, Self::replace_all));
        find.search.connect_activate({
            let weak = Rc::downgrade(&find);
            move |_| {
                if let Some(find) = weak.upgrade() {
                    find.step(true);
                }
            }
        });
        find.replace.connect_activate({
            let weak = Rc::downgrade(&find);
            move |_| {
                if let Some(find) = weak.upgrade() {
                    find.replace_one();
                }
            }
        });
        // Shift+Enter: the previous match.
        let keys = gtk4::EventControllerKey::new();
        keys.connect_key_pressed({
            let weak = Rc::downgrade(&find);
            move |_, key, _, state| {
                let Some(find) = weak.upgrade() else { return gtk4::glib::Propagation::Proceed };
                if matches!(key, gtk4::gdk::Key::Return | gtk4::gdk::Key::KP_Enter) && state.contains(gtk4::gdk::ModifierType::SHIFT_MASK) {
                    find.step(false);
                    return gtk4::glib::Propagation::Stop;
                }
                gtk4::glib::Propagation::Proceed
            }
        });
        find.search.add_controller(keys);
        find
    }

    pub fn is_open(&self) -> bool {
        self.widget.reveals_child()
    }

    /// Opens the bar for `buffer`/`view`, with the replace row when asked,
    /// starting from the selected text.
    pub fn show(self: &Rc<Self>, buffer: &gtk4::TextBuffer, view: &gtk4::TextView, replace: bool) {
        self.attach(buffer, view);
        if let Some((start, end)) = buffer.selection_bounds() {
            let selected = buffer.text(&start, &end, false);
            if !selected.contains('\n') {
                self.search.set_text(&selected);
            }
        }
        self.replace_row.set_visible(replace);
        self.widget.set_reveal_child(true);
        self.search.grab_focus();
        self.rescan();
    }

    /// Follows another open file while the bar is open.
    pub fn attach(self: &Rc<Self>, buffer: &gtk4::TextBuffer, view: &gtk4::TextView) {
        let same = self.context.borrow().as_ref().is_some_and(|context| &context.buffer == buffer);
        if !same {
            self.detach();
            let weak = Rc::downgrade(self);
            let changed = buffer.connect_changed(move |_| {
                let Some(find) = weak.upgrade() else { return };
                if find.rescan_pending.replace(true) {
                    return;
                }
                // Once the edit is over: its iterators must stay valid.
                let weak = Rc::downgrade(&find);
                glib::idle_add_local_once(move || {
                    if let Some(find) = weak.upgrade() {
                        find.rescan_pending.set(false);
                        find.rescan();
                    }
                });
            });
            *self.context.borrow_mut() = Some(Context { buffer: buffer.clone(), view: view.clone(), changed: Rc::new(changed) });
        }
        self.rescan();
    }

    /// Lets go of the open file: no marked matches left behind.
    fn detach(&self) {
        if let Some(context) = self.context.borrow_mut().take() {
            let (start, end) = context.buffer.bounds();
            context.buffer.remove_tag_by_name(scheme::SEARCH_MATCH, &start, &end);
            if let Ok(changed) = Rc::try_unwrap(context.changed) {
                context.buffer.disconnect(changed);
            }
        }
        self.matches.set(-1);
    }

    pub fn hide(&self) {
        self.widget.set_reveal_child(false);
        self.detach();
    }

    fn flags(&self) -> gtk4::TextSearchFlags {
        let flags = gtk4::TextSearchFlags::TEXT_ONLY | gtk4::TextSearchFlags::VISIBLE_ONLY;
        if self.case.is_active() { flags } else { flags | gtk4::TextSearchFlags::CASE_INSENSITIVE }
    }

    /// Marks and counts every match of the search text in the open file.
    fn rescan(&self) {
        let Some(context) = self.context.borrow().clone() else { return };
        let buffer = &context.buffer;
        let (start, end) = buffer.bounds();
        buffer.remove_tag_by_name(scheme::SEARCH_MATCH, &start, &end);
        let text = self.search.text();
        let mut matches = -1;
        if !text.is_empty() {
            matches = 0;
            let mut from = start;
            while let Some((match_start, match_end)) = from.forward_search(&text, self.flags(), None) {
                buffer.apply_tag_by_name(scheme::SEARCH_MATCH, &match_start, &match_end);
                matches += 1;
                if matches >= MAX_MATCHES {
                    break;
                }
                from = match_end;
            }
        }
        self.matches.set(matches);
        self.count.set_text(&count_text(matches));
    }

    /// The match at or after `from`, starting over at the top after the last.
    fn forward(&self, buffer: &gtk4::TextBuffer, from: &gtk4::TextIter) -> Option<(gtk4::TextIter, gtk4::TextIter)> {
        let text = self.search.text();
        if text.is_empty() {
            return None;
        }
        from.forward_search(&text, self.flags(), None).or_else(|| buffer.start_iter().forward_search(&text, self.flags(), None))
    }

    /// The match before `from`, starting over at the end before the first.
    fn backward(&self, buffer: &gtk4::TextBuffer, from: &gtk4::TextIter) -> Option<(gtk4::TextIter, gtk4::TextIter)> {
        let text = self.search.text();
        if text.is_empty() {
            return None;
        }
        from.backward_search(&text, self.flags(), None).or_else(|| buffer.end_iter().backward_search(&text, self.flags(), None))
    }

    fn select(context: &Context, start: &gtk4::TextIter, end: &gtk4::TextIter) {
        context.buffer.select_range(start, end);
        context.view.scroll_to_iter(&mut start.clone(), 0.2, false, 0.0, 0.5);
    }

    /// Selects the first match at or after the cursor (as the text changes).
    fn select_from_cursor(&self, include_cursor: bool) {
        let Some(context) = self.context.borrow().clone() else { return };
        let buffer = &context.buffer;
        let mut from = buffer.iter_at_mark(&buffer.get_insert());
        if include_cursor {
            if let Some((start, _)) = buffer.selection_bounds() {
                from = start;
            }
        }
        if let Some((start, end)) = self.forward(buffer, &from) {
            Self::select(&context, &start, &end);
        }
    }

    pub fn step(&self, forward: bool) {
        let Some(context) = self.context.borrow().clone() else { return };
        let buffer = &context.buffer;
        let (start, end) = buffer.selection_bounds().unwrap_or_else(|| {
            let at = buffer.iter_at_mark(&buffer.get_insert());
            (at, at)
        });
        let found = if forward { self.forward(buffer, &end) } else { self.backward(buffer, &start) };
        if let Some((start, end)) = found {
            Self::select(&context, &start, &end);
        }
    }

    pub fn replace_one(&self) {
        let Some(context) = self.context.borrow().clone() else { return };
        let buffer = &context.buffer;
        // The selected match, else the next one first.
        let selected = buffer.selection_bounds().filter(|(start, end)| {
            self.forward(buffer, start).is_some_and(|(match_start, match_end)| match_start == *start && match_end == *end)
        });
        let Some((mut start, mut end)) = selected else {
            self.step(true);
            return;
        };
        let with = self.replace.text();
        buffer.begin_user_action();
        buffer.delete(&mut start, &mut end);
        buffer.insert(&mut start, &with);
        buffer.end_user_action();
        buffer.place_cursor(&start);
        self.rescan();
        self.step(true);
    }

    /// Replaces every match, as one step for Undo.
    pub fn replace_all(&self) {
        let Some(context) = self.context.borrow().clone() else { return };
        let buffer = &context.buffer;
        let (text, with) = (self.search.text(), self.replace.text());
        if text.is_empty() {
            return;
        }
        buffer.begin_user_action();
        let mut from = buffer.start_iter();
        while let Some((mut start, mut end)) = from.forward_search(&text, self.flags(), None) {
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, &with);
            from = start;
        }
        buffer.end_user_action();
        self.rescan();
    }

    /// How many matches the open file has; -1 without a search text.
    #[cfg(test)]
    pub fn matches(&self) -> i32 {
        self.matches.get()
    }
}

fn count_text(n: i32) -> String {
    match n {
        n if n < 0 => String::new(),
        0 => "no match".into(),
        1 => "1 match".into(),
        n if n >= MAX_MATCHES => format!("{MAX_MATCHES}+ matches"),
        n => format!("{n} matches"),
    }
}
