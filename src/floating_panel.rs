//! A floating panel (⚙ Settings) the user moves by its header, like a
//! terminal card, at a fixed size.
//!
//! The overlay places it through `get-child-position`: a page with more to
//! show scrolls inside it rather than resizing it, and a drag only changes
//! where it is. It always stays whole on screen and below the top bar, so the
//! bar's Arrange, Settings and Hide are never under it.
use gtk4::{gdk, prelude::*};
use std::cell::Cell;
use std::rc::Rc;

/// Space kept between the panel and the screen's edges.
const MARGIN: i32 = 8;

/// Where a panel of `size` goes on a `bounds` screen whose top bar ends at
/// `top`, as `(x, y, width, height)`: at `wanted` (its top-left) or centered,
/// kept whole on screen and below the bar. A screen smaller than the panel
/// shrinks it to what fits.
pub fn place(bounds: (i32, i32), size: (i32, i32), top: i32, wanted: Option<(i32, i32)>) -> (i32, i32, i32, i32) {
    let (screen_width, screen_height) = bounds;
    let width = size.0.min(screen_width - 2 * MARGIN).max(1);
    let height = size.1.min(screen_height - top - 2 * MARGIN).max(1);
    let (x, y) = wanted.unwrap_or(((screen_width - width) / 2, (screen_height - height) / 2));
    let clamp = |value: i32, min: i32, max: i32| value.clamp(min, max.max(min));
    (
        clamp(x, MARGIN, screen_width - width - MARGIN),
        clamp(y, top + MARGIN, screen_height - height - MARGIN),
        width,
        height,
    )
}

/// Whether a press at `(x, y)` in `panel` is on its header (`term-header`)
/// and not on one of the header's buttons: the only place a drag starts.
fn on_header(panel: &gtk4::Widget, x: f64, y: f64) -> bool {
    let mut widget = panel.pick(x, y, gtk4::PickFlags::DEFAULT);
    while let Some(current) = widget {
        if current == *panel {
            return false;
        }
        if current.is::<gtk4::Button>() {
            return false;
        }
        if current.has_css_class("term-header") {
            return true;
        }
        widget = current.parent();
    }
    false
}

/// Where a drag began: the pointer in surface coordinates, and the panel's top-left.
type DragStart = ((f64, f64), (i32, i32));

/// A panel in `overlay` the user drags by its header; `saved` is where it was
/// left last time (its top-left) and `moved` is told where a drag left it.
pub struct MovablePanel {
    overlay: gtk4::Overlay,
    panel: gtk4::Widget,
    size: (i32, i32),
    top: Rc<dyn Fn() -> i32>,
    /// Top-left the user chose, already kept on screen; `None` is centered.
    wanted: Cell<Option<(i32, i32)>>,
}

impl MovablePanel {
    pub fn install(
        overlay: &gtk4::Overlay,
        panel: &impl IsA<gtk4::Widget>,
        size: (i32, i32),
        top: Rc<dyn Fn() -> i32>,
        saved: Option<(i32, i32)>,
        moved: Rc<dyn Fn((i32, i32))>,
    ) -> Rc<Self> {
        let this = Rc::new(Self {
            overlay: overlay.clone(),
            panel: panel.clone().upcast(),
            size,
            top,
            wanted: Cell::new(saved),
        });
        let weak = Rc::downgrade(&this);
        overlay.connect_get_child_position(move |overlay, child| {
            let this = weak.upgrade()?;
            if *child != this.panel {
                return None;
            }
            let (x, y, width, height) = this.rect(overlay);
            Some(gdk::Rectangle::new(x, y, width, height))
        });

        // Surface coordinates: the panel moves under the pointer, so offsets
        // measured in its own coordinates would chase themselves.
        let drag = gtk4::GestureDrag::new();
        let start: Rc<Cell<Option<DragStart>>> = Rc::new(Cell::new(None));
        let pointer = |gesture: &gtk4::GestureDrag| gesture.current_event().and_then(|event| event.position());
        let weak = Rc::downgrade(&this);
        let begin = Rc::clone(&start);
        drag.connect_drag_begin(move |gesture, x, y| {
            let Some(this) = weak.upgrade() else { return };
            let (Some(at), true) = (pointer(gesture), on_header(&this.panel, x, y)) else {
                gesture.set_state(gtk4::EventSequenceState::Denied);
                return;
            };
            let (left, top, _, _) = this.rect(&this.overlay);
            begin.set(Some((at, (left, top))));
        });
        let weak = Rc::downgrade(&this);
        let update = Rc::clone(&start);
        drag.connect_drag_update(move |gesture, _, _| {
            let (Some(this), Some((from, origin)), Some(at)) = (weak.upgrade(), update.get(), pointer(gesture)) else {
                return;
            };
            gesture.set_state(gtk4::EventSequenceState::Claimed);
            this.move_to((origin.0 + (at.0 - from.0) as i32, origin.1 + (at.1 - from.1) as i32));
        });
        let weak = Rc::downgrade(&this);
        drag.connect_drag_end(move |_, _, _| {
            if start.take().is_none() {
                return;
            }
            if let Some(position) = weak.upgrade().and_then(|this| this.wanted.get()) {
                moved(position);
            }
        });
        this.panel.add_controller(drag);
        this
    }

    fn rect(&self, overlay: &gtk4::Overlay) -> (i32, i32, i32, i32) {
        place((overlay.width(), overlay.height()), self.size, (self.top)(), self.wanted.get())
    }

    /// Put the panel's top-left at `position`, kept on screen.
    pub fn move_to(&self, position: (i32, i32)) {
        let (x, y, _, _) = place(
            (self.overlay.width(), self.overlay.height()),
            self.size,
            (self.top)(),
            Some(position),
        );
        if self.wanted.replace(Some((x, y))) != Some((x, y)) {
            self.overlay.queue_allocate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk4::glib;

    #[test]
    fn a_panel_starts_centered_and_stays_on_screen_below_the_bar() {
        let screen = (1920, 1080);
        let size = (660, 620);
        // Centered by default.
        assert_eq!(place(screen, size, 46, None), (630, 230, 660, 620));
        // Where the user left it.
        assert_eq!(place(screen, size, 46, Some((100, 300))), (100, 300, 660, 620));
        // Never past an edge, never over the top bar.
        assert_eq!(place(screen, size, 46, Some((-500, -500))), (8, 54, 660, 620));
        assert_eq!(place(screen, size, 46, Some((5000, 5000))), (1252, 452, 660, 620));
        // A larger bar pushes it down.
        assert_eq!(place(screen, size, 80, Some((100, 60))).1, 88);
        // A screen smaller than the panel: whatever fits, still below the bar.
        let (x, y, width, height) = place((600, 500), size, 46, Some((40, 40)));
        assert_eq!((x, y, width, height), (8, 54, 584, 438));
        // The size never follows the position.
        for wanted in [None, Some((0, 0)), Some((9999, 9999)), Some((700, 400))] {
            let (_, _, width, height) = place(screen, size, 46, wanted);
            assert_eq!((width, height), size);
        }
    }

    #[test]
    fn the_settings_panel_moves_by_its_header_at_a_fixed_size() {
        crate::gtk_test::run_in_child_process("floating_panel::tests::movable_inner");
    }

    #[test]
    fn movable_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&gtk4::Box::new(gtk4::Orientation::Vertical, 0)));
        // A stand-in with the real panel's parts: a header with a button, and
        // content that can grow.
        let panel = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        header.add_css_class("term-header");
        let title = gtk4::Label::new(Some("Settings"));
        title.set_hexpand(true);
        header.append(&title);
        let close = gtk4::Button::with_label("✕");
        header.append(&close);
        panel.append(&header);
        let content = gtk4::Label::new(Some("page"));
        content.set_vexpand(true);
        panel.append(&content);
        overlay.add_overlay(&panel);
        let saved: Rc<Cell<Option<(i32, i32)>>> = Rc::new(Cell::new(None));
        let movable = MovablePanel::install(&overlay, &panel, (400, 300), Rc::new(|| 46), None, Rc::new({
            let saved = Rc::clone(&saved);
            move |position| saved.set(Some(position))
        }));
        let window = gtk4::Window::new();
        window.set_default_size(1000, 700);
        window.set_child(Some(&overlay));
        window.present();
        let bounds = |until: &dyn Fn(&gtk4::graphene::Rect) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(5));
                if let Some(rect) = panel.compute_bounds(&overlay) {
                    if until(&rect) {
                        return rect;
                    }
                }
                assert!(std::time::Instant::now() < deadline, "panel never placed as expected");
            }
        };
        // Centered at its fixed size.
        let first = bounds(&|r| r.width() == 400.0 && overlay.width() > 1);
        let (ow, oh) = (overlay.width() as f32, overlay.height() as f32);
        assert!(((first.x() - (ow - 400.0) / 2.0).abs() <= 1.0) && ((first.y() - (oh - 300.0) / 2.0).abs() <= 1.0), "{first:?}");
        assert_eq!(first.height(), 300.0);
        // Moved: where it was put, and still the same size.
        movable.move_to((120, 200));
        let moved = bounds(&|r| r.x() == 120.0);
        assert_eq!((moved.y(), moved.width(), moved.height()), (200.0, 400.0, 300.0));
        // Content that wants more room does not resize it.
        content.set_text(&"a much longer line of settings text ".repeat(20));
        content.set_size_request(-1, 900);
        let after = bounds(&|r| r.x() == 120.0);
        assert_eq!((after.width(), after.height()), (400.0, 300.0));
        // Dragged past an edge: kept whole on screen, below the bar.
        movable.move_to((-300, -300));
        let clamped = bounds(&|r| r.x() == 8.0);
        assert_eq!(clamped.y(), 54.0);
        // Only the header's empty area starts a drag; its buttons and the page do not.
        let header_at = header.compute_point(&panel, &gtk4::graphene::Point::new(4.0, 4.0)).unwrap();
        assert!(on_header(panel.upcast_ref(), header_at.x() as f64, header_at.y() as f64));
        let close_at = close.compute_point(&panel, &gtk4::graphene::Point::new(2.0, 2.0)).unwrap();
        assert!(!on_header(panel.upcast_ref(), close_at.x() as f64, close_at.y() as f64));
        let page_at = content.compute_point(&panel, &gtk4::graphene::Point::new(4.0, 4.0)).unwrap();
        assert!(!on_header(panel.upcast_ref(), page_at.x() as f64, page_at.y() as f64));
        assert!(saved.get().is_none(), "nothing is saved until a drag ends");
        window.close();
    }

    #[test]
    fn the_real_settings_panel_fits_its_fixed_size() {
        crate::gtk_test::run_in_child_process("floating_panel::tests::real_size_inner");
    }

    #[test]
    fn real_size_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        // No page may need more than the size the panel is locked to: it
        // scrolls inside it instead.
        let state = Rc::new(std::cell::RefCell::new(crate::state::AppState::default()));
        let panel = crate::harness_settings::build_harness_settings_panel(
            state,
            Rc::new(|_| {}),
            Rc::new(|_| {}),
            Rc::new(|_| {}),
            Rc::new(|_| {}),
            crate::launcher_settings::ConnectionHooks::inert(),
        );
        let (width, height) = crate::harness_settings::SETTINGS_PANEL_SIZE;
        let min_width = panel.widget.measure(gtk4::Orientation::Horizontal, -1).0;
        let min_height = panel.widget.measure(gtk4::Orientation::Vertical, width).0;
        assert!(min_width <= width && min_height <= height, "needs {min_width}x{min_height}");
    }
}
