//! SD Editor's text view: GTK's own, with what a code editor adds to it. Line
//! numbers in a gutter, the cursor's line and matching brackets marked, a new
//! line that keeps the indentation, Tab and Shift+Tab that indent by four
//! spaces, and a Backspace that removes one indentation step.
use super::scheme::{self, Palette};
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// One indentation step, in spaces.
pub const TAB: usize = 4;
/// Space right of the line numbers and left of the first digit.
const GUTTER_PAD: i32 = 8;
/// How far a bracket's partner is looked for, in characters.
const BRACKET_REACH: usize = 20_000;

/// The whitespace a line starts with.
fn indent_of(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// The screen column after `prefix`, tabs reaching the next stop.
fn column_after(prefix: &str) -> usize {
    prefix.chars().fold(0, |column, c| if c == '\t' { column + TAB - column % TAB } else { column + 1 })
}

/// How many spaces Tab inserts at `column`.
fn spaces_to_stop(column: usize) -> usize {
    TAB - column % TAB
}

/// How many characters Backspace removes after `prefix` (the line up to the
/// cursor) to go back one indentation step; `None` for an ordinary Backspace.
fn backspace_step(prefix: &str) -> Option<usize> {
    if prefix.is_empty() || !prefix.chars().all(|c| c == ' ') {
        return None;
    }
    Some((prefix.len() - 1) % TAB + 1)
}

/// How many characters Shift+Tab removes from the start of `line`.
fn unindent_step(line: &str) -> usize {
    if line.starts_with('\t') {
        return 1;
    }
    line.chars().take(TAB).take_while(|c| *c == ' ').count()
}

/// A bracket's partner and whether it is ahead.
fn partner(bracket: char) -> Option<(char, bool)> {
    Some(match bracket {
        '(' => (')', true),
        '[' => (']', true),
        '{' => ('}', true),
        ')' => ('(', false),
        ']' => ('[', false),
        '}' => ('{', false),
        _ => return None,
    })
}

/// The bracket that closes or opens the one at `at`.
fn find_partner(at: &gtk4::TextIter) -> Option<gtk4::TextIter> {
    let bracket = at.char();
    let (wanted, forward) = partner(bracket)?;
    let mut iter = *at;
    let mut depth = 0usize;
    for _ in 0..BRACKET_REACH {
        if !(if forward { iter.forward_char() } else { iter.backward_char() }) {
            return None;
        }
        let c = iter.char();
        if c == bracket {
            depth += 1;
        } else if c == wanted {
            if depth == 0 {
                return Some(iter);
            }
            depth -= 1;
        }
    }
    None
}

/// Marks the cursor's line, and the bracket at the cursor with its partner.
pub(super) fn mark_cursor(buffer: &gtk4::TextBuffer) {
    let (start, end) = buffer.bounds();
    buffer.remove_tag_by_name(scheme::CURRENT_LINE, &start, &end);
    buffer.remove_tag_by_name(scheme::BRACKET, &start, &end);
    let at = buffer.iter_at_mark(&buffer.get_insert());
    let mut line_start = at;
    line_start.set_line_offset(0);
    let mut line_end = line_start;
    line_end.forward_line();
    buffer.apply_tag_by_name(scheme::CURRENT_LINE, &line_start, &line_end);
    if buffer.has_selection() {
        return;
    }
    // The bracket right of the cursor, else the one left of it.
    let mut before = at;
    let candidates = [Some(at), before.backward_char().then_some(before)];
    for bracket in candidates.into_iter().flatten() {
        if let Some(other) = find_partner(&bracket) {
            for iter in [bracket, other] {
                let mut next = iter;
                next.forward_char();
                buffer.apply_tag_by_name(scheme::BRACKET, &iter, &next);
            }
            return;
        }
    }
}

/// The first and last line the selection (or the cursor) touches. A
/// selection that ends at the start of a line does not include that line.
fn selected_lines(buffer: &gtk4::TextBuffer) -> (i32, i32) {
    let Some((start, end)) = buffer.selection_bounds() else {
        let line = buffer.iter_at_mark(&buffer.get_insert()).line();
        return (line, line);
    };
    let (first, mut last) = (start.line(), end.line());
    if last > first && end.starts_line() {
        last -= 1;
    }
    (first, last)
}

/// The text of `line` without its line break.
fn line_text(buffer: &gtk4::TextBuffer, line: i32) -> Option<(gtk4::TextIter, String)> {
    let start = buffer.iter_at_line(line)?;
    let mut end = start;
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    Some((start, buffer.text(&start, &end, true).to_string()))
}

/// Enter: a new line that starts with the current line's indentation.
pub(super) fn new_line(view: &gtk4::TextView) {
    let buffer = view.buffer();
    buffer.begin_user_action();
    buffer.delete_selection(true, true);
    let at = buffer.iter_at_mark(&buffer.get_insert());
    let mut line_start = at;
    line_start.set_line_offset(0);
    let prefix = buffer.text(&line_start, &at, true);
    let indent = indent_of(&prefix).to_string();
    buffer.insert_at_cursor(&format!("\n{indent}"));
    buffer.end_user_action();
    view.scroll_mark_onscreen(&buffer.get_insert());
}

/// Tab: indents the selected lines, or moves the cursor to the next stop.
pub(super) fn indent(view: &gtk4::TextView) {
    let buffer = view.buffer();
    let (first, last) = selected_lines(&buffer);
    buffer.begin_user_action();
    if buffer.has_selection() && last > first {
        for line in first..=last {
            if let Some((mut start, text)) = line_text(&buffer, line) {
                if !text.is_empty() {
                    buffer.insert(&mut start, &" ".repeat(TAB));
                }
            }
        }
    } else {
        buffer.delete_selection(true, true);
        let at = buffer.iter_at_mark(&buffer.get_insert());
        let mut line_start = at;
        line_start.set_line_offset(0);
        let column = column_after(&buffer.text(&line_start, &at, true));
        buffer.insert_at_cursor(&" ".repeat(spaces_to_stop(column)));
    }
    buffer.end_user_action();
    view.scroll_mark_onscreen(&buffer.get_insert());
}

/// Shift+Tab: takes one indentation step off the selected lines.
pub(super) fn unindent(view: &gtk4::TextView) {
    let buffer = view.buffer();
    let (first, last) = selected_lines(&buffer);
    buffer.begin_user_action();
    for line in first..=last {
        if let Some((mut start, text)) = line_text(&buffer, line) {
            let step = unindent_step(&text);
            if step > 0 {
                let mut end = start;
                end.forward_chars(step as i32);
                buffer.delete(&mut start, &mut end);
            }
        }
    }
    buffer.end_user_action();
}

/// Backspace in a line's indentation: back one step. `false` when this is
/// an ordinary Backspace.
pub(super) fn backspace(view: &gtk4::TextView, ask_input_method: impl Fn() -> bool) -> bool {
    let buffer = view.buffer();
    if buffer.has_selection() {
        return false;
    }
    let at = buffer.iter_at_mark(&buffer.get_insert());
    let mut line_start = at;
    line_start.set_line_offset(0);
    let Some(step) = backspace_step(&buffer.text(&line_start, &at, true)) else {
        return false;
    };
    if ask_input_method() {
        return true;
    }
    let mut from = at;
    from.backward_chars(step as i32);
    let mut to = at;
    buffer.begin_user_action();
    buffer.delete(&mut from, &mut to);
    buffer.end_user_action();
    true
}

/// The editor's keys, before the text view's own handling.
fn add_keys(view: &gtk4::TextView) {
    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let weak = view.downgrade();
    keys.connect_key_pressed(move |controller, key, _, state| {
        let Some(view) = weak.upgrade() else { return glib::Propagation::Proceed };
        let other = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK | gdk::ModifierType::META_MASK;
        if !view.is_editable() || state.intersects(other) {
            return glib::Propagation::Proceed;
        }
        // A key the input method takes (composing text) is not the editor's.
        let input_method = || controller.current_event().is_some_and(|event| view.im_context_filter_keypress(&event));
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter => {
                if !input_method() {
                    new_line(&view);
                }
            }
            gdk::Key::ISO_Left_Tab => unindent(&view),
            gdk::Key::Tab | gdk::Key::KP_Tab if shift => unindent(&view),
            gdk::Key::Tab | gdk::Key::KP_Tab => {
                if !input_method() {
                    indent(&view);
                }
            }
            gdk::Key::BackSpace if !shift => {
                if !backspace(&view, input_method) {
                    return glib::Propagation::Proceed;
                }
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    view.add_controller(keys);
}

/// The width the gutter needs for `lines` line numbers.
fn gutter_width(area: &gtk4::DrawingArea, lines: i32) -> i32 {
    let digits = lines.max(1).to_string().len().max(2);
    let (width, _) = area.create_pango_layout(Some(&"8".repeat(digits))).pixel_size();
    width + 2 * GUTTER_PAD
}

/// The line numbers left of the text.
fn add_gutter(view: &gtk4::TextView, palette: &Rc<RefCell<Palette>>) -> gtk4::DrawingArea {
    let area = gtk4::DrawingArea::new();
    // The text's font.
    area.add_css_class("sd-editor-text");
    let buffer = view.buffer();
    area.set_content_width(gutter_width(&area, buffer.line_count()));
    area.set_draw_func({
        let weak = view.downgrade();
        let palette = Rc::clone(palette);
        move |area, cr, width, _| {
            let Some(view) = weak.upgrade() else { return };
            let palette = palette.borrow();
            let buffer = view.buffer();
            let current = buffer.iter_at_mark(&buffer.get_insert()).line();
            let visible = view.visible_rect();
            let (mut iter, _) = view.line_at_y(visible.y());
            loop {
                let (y, height) = view.line_yrange(&iter);
                if y > visible.y() + visible.height() {
                    break;
                }
                let (_, top) = view.buffer_to_window_coords(gtk4::TextWindowType::Left, 0, y);
                let line = iter.line();
                let color = if line == current {
                    let fill = scheme::rgba(&palette.line);
                    cr.set_source_rgb(f64::from(fill.red()), f64::from(fill.green()), f64::from(fill.blue()));
                    cr.rectangle(0.0, f64::from(top), f64::from(width), f64::from(height));
                    let _ = cr.fill();
                    scheme::rgba(&palette.foreground)
                } else {
                    scheme::rgba(&palette.gutter)
                };
                let layout = area.create_pango_layout(Some(&(line + 1).to_string()));
                let (text_width, _) = layout.pixel_size();
                cr.set_source_rgb(f64::from(color.red()), f64::from(color.green()), f64::from(color.blue()));
                cr.move_to(f64::from(width - text_width - GUTTER_PAD), f64::from(top));
                pangocairo::functions::show_layout(cr, &layout);
                iter.forward_line();
                if iter.line() == line {
                    break;
                }
            }
        }
    });
    view.set_gutter(gtk4::TextWindowType::Left, Some(&area));
    // Scrolling moves the numbers. The scrolled window gives the view its
    // adjustment when the view becomes its child.
    let follow = {
        let area = area.downgrade();
        move |view: &gtk4::TextView| {
            let Some(adjustment) = view.vadjustment() else { return };
            adjustment.connect_value_changed(redrawer(&area));
            adjustment.connect_changed(redrawer(&area));
        }
    };
    follow(view);
    view.connect_vadjustment_notify(follow);
    area
}

fn redrawer(area: &glib::WeakRef<gtk4::DrawingArea>) -> impl Fn(&gtk4::Adjustment) + 'static {
    let area = area.clone();
    move |_| {
        if let Some(area) = area.upgrade() {
            area.queue_draw();
        }
    }
}

/// A code view on `buffer`, whose tags `scheme::style_buffer` already made.
/// Returns the view and its gutter (redrawn by the caller on a theme change).
pub fn new_view(buffer: &gtk4::TextBuffer, palette: &Rc<RefCell<Palette>>) -> (gtk4::TextView, gtk4::DrawingArea) {
    let view = gtk4::TextView::with_buffer(buffer);
    view.add_css_class("sd-editor-text");
    view.set_monospace(true);
    view.set_left_margin(6);
    view.set_right_margin(6);
    let mut tabs = gtk4::pango::TabArray::new(1, true);
    let (space, _) = view.create_pango_layout(Some(" ")).pixel_size();
    tabs.set_tab(0, gtk4::pango::TabAlign::Left, space.max(1) * TAB as i32);
    view.set_tabs(&tabs);
    add_keys(&view);
    let gutter = add_gutter(&view, palette);

    // After an edit or a cursor move, once the buffer is at rest: the marks,
    // the gutter's current line and its width.
    let pending = Rc::new(Cell::new(false));
    let refresh = {
        let (buffer, gutter, pending) = (buffer.downgrade(), gutter.downgrade(), Rc::clone(&pending));
        move || {
            if pending.replace(true) {
                return;
            }
            let (buffer, gutter, pending) = (buffer.clone(), gutter.clone(), Rc::clone(&pending));
            glib::idle_add_local_once(move || {
                pending.set(false);
                let (Some(buffer), Some(gutter)) = (buffer.upgrade(), gutter.upgrade()) else { return };
                mark_cursor(&buffer);
                let width = gutter_width(&gutter, buffer.line_count());
                if gutter.content_width() != width {
                    gutter.set_content_width(width);
                }
                gutter.queue_draw();
            });
        }
    };
    buffer.connect_cursor_position_notify({
        let refresh = refresh.clone();
        move |_| refresh()
    });
    buffer.connect_changed({
        let refresh = refresh.clone();
        move |_| refresh()
    });
    buffer.connect_has_selection_notify({
        let refresh = refresh.clone();
        move |_| refresh()
    });
    refresh();
    (view, gutter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_indentation_steps() {
        assert_eq!(indent_of("    let x"), "    ");
        assert_eq!(indent_of("\t  x"), "\t  ");
        assert_eq!(indent_of("x  "), "");
        assert_eq!(indent_of("   "), "   ");
        assert_eq!(column_after(""), 0);
        assert_eq!(column_after("ab"), 2);
        assert_eq!(column_after("a\tb"), 5);
        assert_eq!(spaces_to_stop(0), 4);
        assert_eq!(spaces_to_stop(3), 1);
        assert_eq!(spaces_to_stop(4), 4);
        // Backspace in the indentation goes back to the previous stop.
        assert_eq!(backspace_step("        "), Some(4));
        assert_eq!(backspace_step("      "), Some(2));
        assert_eq!(backspace_step(" "), Some(1));
        assert_eq!(backspace_step(""), None);
        assert_eq!(backspace_step("  x "), None);
        assert_eq!(backspace_step("\t"), None);
        assert_eq!(unindent_step("        x"), 4);
        assert_eq!(unindent_step("  x"), 2);
        assert_eq!(unindent_step("\t\tx"), 1);
        assert_eq!(unindent_step("x"), 0);
        assert_eq!(partner('('), Some((')', true)));
        assert_eq!(partner('}'), Some(('{', false)));
        assert_eq!(partner('x'), None);
    }
}
