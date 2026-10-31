//! SD Editor's find and replace bar (Ctrl+F, Ctrl+H): GtkSourceView's own
//! search, which highlights every match in the open file.
use gtk4::prelude::*;
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub struct Find {
    pub widget: gtk4::Revealer,
    pub search: gtk4::Entry,
    pub replace: gtk4::Entry,
    replace_row: gtk4::Box,
    count: gtk4::Label,
    case: gtk4::CheckButton,
    settings: sourceview5::SearchSettings,
    /// The open file's buffer, view and search.
    context: RefCell<Option<(sourceview5::Buffer, sourceview5::View, sourceview5::SearchContext)>>,
}

fn button(label: &str, tip: &str) -> gtk4::Button {
    let button = gtk4::Button::with_label(label);
    button.add_css_class("term-btn");
    button.set_tooltip_text(Some(tip));
    button
}

impl Find {
    pub fn new() -> Rc<Self> {
        let settings = sourceview5::SearchSettings::new();
        settings.set_wrap_around(true);
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

        let find = Rc::new(Self { widget, search, replace, replace_row, count, case, settings, context: RefCell::new(None) });
        find.search.connect_changed({
            let weak = Rc::downgrade(&find);
            move |entry| {
                if let Some(find) = weak.upgrade() {
                    find.settings.set_search_text(Some(&entry.text()).filter(|t| !t.is_empty()).map(|t| t.as_str()));
                    find.select_from_cursor(true);
                }
            }
        });
        find.case.connect_toggled({
            let weak = Rc::downgrade(&find);
            move |case| {
                if let Some(find) = weak.upgrade() {
                    find.settings.set_case_sensitive(case.is_active());
                    find.refresh_count();
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
    pub fn show(&self, buffer: &sourceview5::Buffer, view: &sourceview5::View, replace: bool) {
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
        self.refresh_count();
    }

    /// Follows another open file while the bar is open.
    pub fn attach(&self, buffer: &sourceview5::Buffer, view: &sourceview5::View) {
        let same = self.context.borrow().as_ref().is_some_and(|(b, _, _)| b == buffer);
        if !same {
            let context = sourceview5::SearchContext::new(buffer, Some(&self.settings));
            context.connect_occurrences_count_notify({
                let count = self.count.clone();
                move |context| count.set_text(&count_text(context.occurrences_count()))
            });
            *self.context.borrow_mut() = Some((buffer.clone(), view.clone(), context));
        }
        self.refresh_count();
    }

    pub fn hide(&self) {
        self.widget.set_reveal_child(false);
        // No highlighted matches left behind.
        self.context.borrow_mut().take();
    }

    fn refresh_count(&self) {
        let n = self.context.borrow().as_ref().map_or(0, |(_, _, c)| c.occurrences_count());
        self.count.set_text(&count_text(n));
    }

    /// Selects the first match at or after the cursor (as the text changes).
    fn select_from_cursor(&self, include_cursor: bool) {
        let Some((buffer, view, context)) = self.context.borrow().clone() else { return };
        let mut from = buffer.iter_at_mark(&buffer.get_insert());
        if include_cursor {
            if let Some((start, _)) = buffer.selection_bounds() {
                from = start;
            }
        }
        if let Some((start, end, _)) = context.forward(&from) {
            buffer.select_range(&start, &end);
            view.scroll_to_iter(&mut start.clone(), 0.2, false, 0.0, 0.5);
        }
        self.refresh_count();
    }

    pub fn step(&self, forward: bool) {
        let Some((buffer, view, context)) = self.context.borrow().clone() else { return };
        let (start, end) = buffer.selection_bounds().unwrap_or_else(|| {
            let at = buffer.iter_at_mark(&buffer.get_insert());
            (at, at)
        });
        let found = if forward { context.forward(&end) } else { context.backward(&start) };
        if let Some((start, end, _)) = found {
            buffer.select_range(&start, &end);
            view.scroll_to_iter(&mut start.clone(), 0.2, false, 0.0, 0.5);
        }
    }

    pub fn replace_one(&self) {
        let Some((buffer, _, context)) = self.context.borrow().clone() else { return };
        // The selected match, else the next one first.
        let selected = buffer.selection_bounds().filter(|(s, e)| {
            context.forward(s).is_some_and(|(ms, me, _)| ms == *s && me == *e)
        });
        let Some((mut start, mut end)) = selected else {
            self.step(true);
            return;
        };
        let with = self.replace.text();
        if context.replace(&mut start, &mut end, &with).is_ok() {
            buffer.place_cursor(&end);
            self.step(true);
        }
    }

    pub fn replace_all(&self) {
        let Some((_, _, context)) = self.context.borrow().clone() else { return };
        let _ = context.replace_all(&self.replace.text());
        self.refresh_count();
    }
}

fn count_text(n: i32) -> String {
    match n {
        n if n < 0 => String::new(),
        0 => "no match".into(),
        1 => "1 match".into(),
        n => format!("{n} matches"),
    }
}
