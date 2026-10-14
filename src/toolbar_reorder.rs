//! Dragging the top bar's items into another order. A press on an item of a
//! row that moves past [`THRESHOLD`] takes the pointer from the item (it is
//! not clicked, not even on release). While it is dragged every item of the
//! row shows its outline, the dragged one is dimmed, and a vertical line
//! marks the gap it will be dropped in; on release it moves there and the
//! row's owner saves the new order.
//!
//! One gesture on the row, in the capture phase, so it works the same for a
//! launch button and for a plugin's toolbar item: it sees the press before the
//! item does, and claims the pointer only once the press has moved.
use gtk4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

/// How far a press on an item moves before it rearranges instead of clicking.
pub const THRESHOLD: f64 = 8.0;
/// Carried by the item while it is dragged.
pub const CLASS: &str = "toolbar-reordering";
/// Carried by the row while one of its items is dragged: every item shows its
/// outline.
pub const ROW_CLASS: &str = "toolbar-rearranging";
/// The drop line's width.
const LINE_WIDTH: f64 = 2.0;

/// One drag on a row: the pressed item, where the press started (row
/// coordinates), and once it moves, the gap it would be dropped in.
struct Drag {
    item: gtk4::Widget,
    x: f64,
    moving: bool,
    gap: Option<usize>,
    /// Where the line is drawn; it follows the row's layout, which changes
    /// once the drag starts (the row makes room for a line at its ends).
    line_x: Option<f64>,
}

/// Let the user drag `row`'s shown children into another order. `on_moved`
/// runs when a drag ends that put an item somewhere else.
pub fn attach(row: &gtk4::Box, on_moved: Rc<dyn Fn()>) {
    static ROWS: AtomicU32 = AtomicU32::new(0);
    // The drop line is the row's own background, so it takes no room and
    // nothing moves under the pointer; the row's name keeps it to this row.
    let name = format!("toolbar-reorder-{}", ROWS.fetch_add(1, Ordering::Relaxed));
    row.set_widget_name(&name);
    let line = gtk4::CssProvider::new();
    gtk4::style_context_add_provider_for_display(
        &row.display(),
        &line,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );

    let drag_gesture = gtk4::GestureDrag::new();
    drag_gesture.set_button(gtk4::gdk::BUTTON_PRIMARY);
    drag_gesture.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let drag: Rc<RefCell<Option<Drag>>> = Rc::new(RefCell::new(None));
    {
        let drag = Rc::clone(&drag);
        let row = row.downgrade();
        drag_gesture.connect_drag_begin(move |_, x, y| {
            *drag.borrow_mut() = row.upgrade()
                .and_then(|row| child_at(&row, x, y))
                .map(|item| Drag { item, x, moving: false, gap: None, line_x: None });
        });
    }
    {
        let drag = Rc::clone(&drag);
        let row = row.downgrade();
        let (line, name) = (line.clone(), name.clone());
        drag_gesture.connect_drag_update(move |gesture, offset_x, offset_y| {
            let Some(row) = row.upgrade() else { return };
            let starting = match drag.borrow_mut().as_mut() {
                None => return,
                Some(current) if !current.moving => {
                    if offset_x.hypot(offset_y) < THRESHOLD {
                        return;
                    }
                    current.moving = true;
                    Some(current.item.clone())
                }
                Some(_) => None,
            };
            if let Some(item) = starting {
                begin(&row, &item);
                // Claiming cancels the item's own click (no borrow held: the
                // item's cancel handlers run inside this call).
                gesture.set_state(gtk4::EventSequenceState::Claimed);
            }
            let mut state = drag.borrow_mut();
            let Some(current) = state.as_mut() else { return };
            // The row stays put during the drag: offsets in its coordinates
            // are the pointer's.
            let edges = shown_edges(&row);
            let (gap, line_x) = nearest_gap(&edges, row_spacing(&row), current.x + offset_x);
            let line_x = line_x.clamp(0.0, (f64::from(row.width()) - LINE_WIDTH).max(0.0));
            current.gap = Some(gap);
            if current.line_x != Some(line_x) {
                current.line_x = Some(line_x);
                show_line(&line, &name, line_x);
            }
        });
    }
    {
        let row = row.downgrade();
        // Also emitted when the drag is cancelled.
        drag_gesture.connect_drag_end(move |_, _, _| {
            let Some(Drag { item, moving, gap, .. }) = drag.borrow_mut().take() else { return };
            let Some(row) = row.upgrade() else { return };
            if !moving {
                return;
            }
            row.remove_css_class(ROW_CLASS);
            row.set_cursor(None);
            item.remove_css_class(CLASS);
            line.load_from_string("");
            if gap.is_some_and(|gap| drop_at(&row, &item, gap)) {
                on_moved();
            }
        });
    }
    row.add_controller(drag_gesture);
}

/// The children of `row`, in order.
pub fn children(row: &gtk4::Box) -> Vec<gtk4::Widget> {
    let mut found = Vec::new();
    let mut child = row.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        found.push(widget);
    }
    found
}

/// Put `row`'s children in `order` (by `key`); children it does not name
/// follow, in the order they had.
pub fn sort(row: &gtk4::Box, order: &[String], key: impl Fn(&gtk4::Widget) -> Option<String>) {
    let mut items = children(row);
    items.sort_by_key(|item| key(item).and_then(|key| order.iter().position(|named| *named == key)).unwrap_or(usize::MAX));
    let mut previous: Option<gtk4::Widget> = None;
    for item in &items {
        row.reorder_child_after(item, previous.as_ref());
        previous = Some(item.clone());
    }
}

/// `keys` in their new order, followed by the `saved` keys that are not among
/// them (an item that is off now keeps its place for when it comes back).
pub fn merged(keys: Vec<String>, saved: &[String]) -> Vec<String> {
    let rest: Vec<String> = saved.iter().filter(|key| !keys.contains(key)).cloned().collect();
    keys.into_iter().chain(rest).collect()
}

/// The gap nearest `x` among the shown items' `edges` (left, right; in order):
/// gap `i` is just before item `i`, gap `edges.len()` after the last. Returns
/// it with the x of its line: halfway between the two items, or `spacing / 2`
/// outside the first or the last.
fn nearest_gap(edges: &[(f64, f64)], spacing: f64, x: f64) -> (usize, f64) {
    let Some((first, last)) = edges.first().zip(edges.last()) else { return (0, 0.0) };
    let line = |gap: usize| -> f64 {
        let middle = if gap == 0 {
            first.0 - spacing / 2.0
        } else if gap == edges.len() {
            last.1 + spacing / 2.0
        } else {
            (edges[gap - 1].1 + edges[gap].0) / 2.0
        };
        middle - LINE_WIDTH / 2.0
    };
    (0..=edges.len())
        .map(|gap| (gap, line(gap)))
        .min_by(|a, b| (a.1 + LINE_WIDTH / 2.0 - x).abs().total_cmp(&(b.1 + LINE_WIDTH / 2.0 - x).abs()))
        .unwrap()
}

/// The left and right edges of `row`'s shown children, in order.
fn shown_edges(row: &gtk4::Box) -> Vec<(f64, f64)> {
    shown(row).iter().filter_map(|child| {
        let bounds = child.compute_bounds(row)?;
        Some((f64::from(bounds.x()), f64::from(bounds.x() + bounds.width())))
    }).collect()
}

fn shown(row: &gtk4::Box) -> Vec<gtk4::Widget> {
    children(row).into_iter().filter(|child| child.is_visible()).collect()
}

fn row_spacing(row: &gtk4::Box) -> f64 {
    f64::from(row.spacing())
}

fn child_at(row: &gtk4::Box, x: f64, y: f64) -> Option<gtk4::Widget> {
    shown(row).into_iter().find(|child| {
        child.compute_bounds(row).is_some_and(|bounds| {
            bounds.contains_point(&gtk4::graphene::Point::new(x as f32, y as f32))
        })
    })
}

fn begin(row: &gtk4::Box, item: &gtk4::Widget) {
    row.add_css_class(ROW_CLASS);
    row.set_cursor_from_name(Some("grabbing"));
    item.add_css_class(CLASS);
    // A hover card is parented to its button and would stay open.
    let mut child = item.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Some(popover) = widget.downcast_ref::<gtk4::Popover>() {
            popover.popdown();
        }
    }
}

/// Draw the drop line `LINE_WIDTH` wide at `x` in the row.
fn show_line(line: &gtk4::CssProvider, name: &str, x: f64) {
    let accent = crate::theme::current_theme().accent;
    let end = x + LINE_WIDTH;
    line.load_from_string(&format!(
        "#{name}.{ROW_CLASS} {{ background-image: linear-gradient(to right, \
         transparent {x}px, {accent} {x}px, {accent} {end}px, transparent {end}px); }}"
    ));
    #[cfg(test)]
    LINE.with(|line| line.set(Some(x)));
}

/// Drop `item` in shown gap `gap`. Returns whether that moved it: the gaps on
/// either side of the item are where it already is.
fn drop_at(row: &gtk4::Box, item: &gtk4::Widget, gap: usize) -> bool {
    let shown = shown(row);
    let Some(at) = shown.iter().position(|child| child == item) else { return false };
    if gap == at || gap == at + 1 {
        return false;
    }
    match gap.checked_sub(1).and_then(|before| shown.get(before)) {
        Some(before) => row.reorder_child_after(item, Some(before)),
        // Before the first one shown, after whatever hidden child precedes it.
        None => row.reorder_child_after(item, shown[0].prev_sibling().as_ref()),
    }
    true
}

#[cfg(test)]
thread_local! {
    /// Where the last drop line was drawn, for tests.
    static LINE: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

/// Where a test's drag of `item` presses: 5 px into it, halfway down, in
/// `row`'s coordinates.
#[cfg(test)]
pub fn press_point(row: &gtk4::Box, item: &impl IsA<gtk4::Widget>) -> (f64, f64) {
    let bounds = item.compute_bounds(row).expect("the item is laid out");
    (f64::from(bounds.x()) + 5.0, f64::from(bounds.y() + bounds.height() / 2.0))
}

/// What a test's drag showed while the pointer was at its end: whether the
/// item was marked as dragged, the row's outlines shown, and the drop line's x.
#[cfg(test)]
pub struct Shown {
    pub marked: bool,
    pub outlines: bool,
    pub line: Option<f64>,
}

/// Press `item` of `row` and move the pointer to `to_x` (row coordinates)
/// through the row's own gesture, then let go.
#[cfg(test)]
pub fn drag_to(row: &gtk4::Box, item: &impl IsA<gtk4::Widget>, to_x: f64) -> Shown {
    let gesture = row.observe_controllers().into_iter()
        .filter_map(|c| c.ok()?.downcast::<gtk4::GestureDrag>().ok())
        .next()
        .expect("the row can be rearranged");
    let (x, y) = press_point(row, item);
    LINE.with(|line| line.set(None));
    gesture.emit_by_name::<()>("drag-begin", &[&x, &y]);
    gesture.emit_by_name::<()>("drag-update", &[&(to_x - x), &0.0f64]);
    let shown = Shown {
        marked: item.as_ref().has_css_class(CLASS),
        outlines: row.has_css_class(ROW_CLASS),
        line: LINE.with(|line| line.get()),
    };
    gesture.emit_by_name::<()>("drag-end", &[&(to_x - x), &0.0f64]);
    assert!(!item.as_ref().has_css_class(CLASS) && !row.has_css_class(ROW_CLASS), "the marks go when the drag ends");
    shown
}

/// Run the main loop until `row`'s shown children are laid out in their
/// order, each right of the last.
#[cfg(test)]
pub fn wait_for_layout(row: &gtk4::Box) {
    crate::gtk_test::pump_until("the row's layout", || {
        let mut right = f32::MIN;
        shown(row).iter().all(|c| {
            let Some(bounds) = c.compute_bounds(row) else { return false };
            let ok = c.width() > 0 && bounds.x() >= right;
            right = bounds.x() + bounds.width();
            ok
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_reorder_keeps_the_places_of_items_that_are_off() {
        let saved = ["b".to_string(), "off".into(), "a".into()];
        assert_eq!(merged(vec!["a".into(), "b".into()], &saved), ["a", "b", "off"]);
        assert_eq!(merged(vec!["a".into()], &[]), ["a"]);
    }

    #[test]
    fn toolbar_reorder_line_css_parses() {
        crate::gtk_test::run_in_child_process("toolbar_reorder::tests::line_css_inner");
    }

    #[test]
    fn line_css_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let provider = gtk4::CssProvider::new();
        let errors = Rc::new(RefCell::new(Vec::new()));
        provider.connect_parsing_error({
            let errors = Rc::clone(&errors);
            move |_, _, error| errors.borrow_mut().push(error.to_string())
        });
        show_line(&provider, "toolbar-reorder-0", 84.5);
        assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
        assert!(provider.to_str().contains("linear-gradient"), "{}", provider.to_str());
    }

    #[test]
    fn toolbar_reorder_draws_the_line_in_the_nearest_gap() {
        // Three items, 10 px apart: 0–80, 90–170, 180–260.
        let edges = [(0.0, 80.0), (90.0, 170.0), (180.0, 260.0)];
        // Halfway between two items, 1 px either side of the middle.
        assert_eq!(nearest_gap(&edges, 10.0, 100.0), (1, 84.0));
        assert_eq!(nearest_gap(&edges, 10.0, 130.0), (1, 84.0));
        assert_eq!(nearest_gap(&edges, 10.0, 131.0), (2, 174.0));
        // Before the first and after the last, half a gap outside them.
        assert_eq!(nearest_gap(&edges, 10.0, -50.0), (0, -6.0));
        assert_eq!(nearest_gap(&edges, 10.0, 400.0), (3, 264.0));
        assert_eq!(nearest_gap(&[], 10.0, 5.0), (0, 0.0));
    }
}
