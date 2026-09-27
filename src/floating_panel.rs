//! A floating panel (⚙ Settings) the user moves by its header and resizes by
//! its edges, like a terminal card.
//!
//! The overlay places it through `get-child-position`: a page with more to
//! show scrolls inside it rather than resizing it, and only a drag on the
//! header or an edge changes where it is or how big it is. It always stays
//! whole on screen and below the top bar, so the bar's Arrange, Settings and
//! Hide are never under it.
use crate::card_resize::{Limits, Rect};
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

/// How big a panel is and where it sits: the size it opens at, the smallest
/// the user may shrink it to, and what they left behind last time.
#[derive(Clone, Copy, Debug)]
pub struct PanelLayout {
    pub default_size: (i32, i32),
    pub min_size: (i32, i32),
    /// The panel's top-left last time; `None` is centered.
    pub saved_pos: Option<(i32, i32)>,
    /// The size the user last dragged it to; `None` is `default_size`.
    pub saved_size: Option<(i32, i32)>,
}

/// A panel in `overlay` the user drags by its header and resizes by its
/// edges. `changed` is told where and how big a drag left it.
pub struct MovablePanel {
    overlay: gtk4::Overlay,
    /// The panel and its resize handles: the child `overlay` places.
    shell: gtk4::Overlay,
    min_size: (i32, i32),
    /// The size the user chose, before it is fitted to this screen.
    size: Cell<(i32, i32)>,
    top: Rc<dyn Fn() -> i32>,
    /// Top-left the user chose, already kept on screen; `None` is centered.
    wanted: Cell<Option<(i32, i32)>>,
}

impl MovablePanel {
    /// Keep the returned handle for as long as the panel lives: the overlay
    /// asks it where the panel goes, and stretches the panel over the whole
    /// screen once it is gone.
    #[must_use]
    pub fn install(
        overlay: &gtk4::Overlay,
        panel: &impl IsA<gtk4::Widget>,
        layout: PanelLayout,
        top: Rc<dyn Fn() -> i32>,
        changed: Rc<dyn Fn((i32, i32), (i32, i32))>,
    ) -> Rc<Self> {
        // The panel gets an overlay of its own to carry the eight resize
        // handles, and follows it in and out of view.
        let panel: gtk4::Widget = panel.clone().upcast();
        let shell = gtk4::Overlay::new();
        shell.set_child(Some(&panel));
        shell.set_visible(panel.get_visible());
        panel.connect_visible_notify({
            let shell = shell.clone();
            move |panel| shell.set_visible(panel.get_visible())
        });
        overlay.add_overlay(&shell);

        let this = Rc::new(Self {
            overlay: overlay.clone(),
            shell: shell.clone(),
            min_size: layout.min_size,
            size: Cell::new(fit_min(layout.saved_size.unwrap_or(layout.default_size), layout.min_size)),
            top,
            wanted: Cell::new(layout.saved_pos),
        });
        let weak = Rc::downgrade(&this);
        overlay.connect_get_child_position(move |overlay, child| {
            let this = weak.upgrade()?;
            if *child != this.shell {
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
            let (Some(at), true) = (pointer(gesture), on_header(this.shell.upcast_ref(), x, y)) else {
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
        let moved = Rc::clone(&changed);
        drag.connect_drag_end(move |_, _, _| {
            if start.take().is_none() {
                return;
            }
            if let Some((Some(position), size)) = weak.upgrade().map(|this| this.geometry()) {
                moved(position, size);
            }
        });
        shell.add_controller(drag);

        this.attach_resize_handles(changed);
        this
    }

    /// The eight edge and corner targets, resizing the panel live.
    fn attach_resize_handles(self: &Rc<Self>, changed: Rc<dyn Fn((i32, i32), (i32, i32))>) {
        let weak = Rc::downgrade(self);
        let limits: Rc<dyn Fn() -> Limits> = Rc::new(move || {
            weak.upgrade().map_or(IDLE_LIMITS, |this| this.limits())
        });
        let weak = Rc::downgrade(self);
        let get_start: Rc<dyn Fn() -> Option<Rect>> = Rc::new(move || {
            let this = weak.upgrade()?;
            let (x, y, width, height) = this.rect(&this.overlay);
            Some(Rect { x: x as f64, y: y as f64, width, height })
        });
        let weak = Rc::downgrade(self);
        let on_preview: Rc<dyn Fn(Rect)> = Rc::new(move |rect| {
            if let Some(this) = weak.upgrade() {
                this.set_geometry((rect.x as i32, rect.y as i32), (rect.width, rect.height));
            }
        });
        let weak = Rc::downgrade(self);
        let on_commit: Rc<dyn Fn(Rect)> = Rc::new(move |rect| {
            let Some(this) = weak.upgrade() else { return };
            this.set_geometry((rect.x as i32, rect.y as i32), (rect.width, rect.height));
            if let (Some(position), size) = this.geometry() {
                changed(position, size);
            }
        });
        crate::card_resize::attach_resize_borders_with(
            &self.shell,
            limits,
            get_start,
            Rc::new(|| {}),
            on_preview,
            on_commit,
        );
    }

    fn rect(&self, overlay: &gtk4::Overlay) -> (i32, i32, i32, i32) {
        place((overlay.width(), overlay.height()), self.size.get(), (self.top)(), self.wanted.get())
    }

    /// Where a resize may take each edge, and how big the result may be: on
    /// screen, below the top bar, and never below what the pages need.
    fn limits(&self) -> Limits {
        let (screen_width, screen_height) = (self.overlay.width(), self.overlay.height());
        let top = (self.top)();
        Limits {
            min_width: self.min_size.0,
            min_height: self.min_size.1,
            max_width: (screen_width - 2 * MARGIN).max(self.min_size.0),
            max_height: (screen_height - top - 2 * MARGIN).max(self.min_size.1),
            left: MARGIN as f64,
            top: (top + MARGIN) as f64,
            right: (screen_width - MARGIN) as f64,
            bottom: (screen_height - MARGIN) as f64,
        }
    }

    /// The panel's top-left (`None` while centered) and its chosen size.
    pub fn geometry(&self) -> (Option<(i32, i32)>, (i32, i32)) {
        (self.wanted.get(), self.size.get())
    }

    /// Put the panel's top-left at `position` at `size`, both kept on screen.
    pub fn set_geometry(&self, position: (i32, i32), size: (i32, i32)) {
        let size = fit_min(size, self.min_size);
        let (x, y, _, _) = place(
            (self.overlay.width(), self.overlay.height()),
            size,
            (self.top)(),
            Some(position),
        );
        let moved = self.wanted.replace(Some((x, y))) != Some((x, y));
        let resized = self.size.replace(size) != size;
        if moved || resized {
            self.overlay.queue_allocate();
        }
    }

    /// Put the panel's top-left at `position`, kept on screen.
    pub fn move_to(&self, position: (i32, i32)) {
        self.set_geometry(position, self.size.get());
    }
}

/// A size no smaller than the panel's pages need.
fn fit_min(size: (i32, i32), min: (i32, i32)) -> (i32, i32) {
    (size.0.max(min.0), size.1.max(min.1))
}

/// Stand-in bounds for a panel that is already gone: every resize on it is
/// denied before these are read.
const IDLE_LIMITS: Limits = Limits {
    min_width: 1,
    min_height: 1,
    max_width: 1,
    max_height: 1,
    left: 0.0,
    top: 0.0,
    right: 0.0,
    bottom: 0.0,
};

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
    fn the_settings_panel_moves_by_its_header_and_resizes_by_its_edges() {
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
        // `install` puts it in the overlay, inside its own resize shell.
        let saved: Rc<Cell<Option<((i32, i32), (i32, i32))>>> = Rc::new(Cell::new(None));
        let layout = PanelLayout {
            default_size: (400, 300),
            min_size: (200, 150),
            saved_pos: None,
            saved_size: None,
        };
        let movable = MovablePanel::install(&overlay, &panel, layout, Rc::new(|| 46), Rc::new({
            let saved = Rc::clone(&saved);
            move |position, size| saved.set(Some((position, size)))
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
        // Dragging an edge does: the size the user chose is kept, down to the
        // minimum and up to what the screen holds.
        movable.set_geometry((120, 200), (640, 400));
        let grown = bounds(&|r| r.width() == 640.0);
        assert_eq!((grown.x(), grown.y(), grown.height()), (120.0, 200.0, 400.0));
        movable.set_geometry((120, 200), (10, 10));
        let shrunk = bounds(&|r| r.width() == 200.0);
        assert_eq!(shrunk.height(), 150.0);
        // A moved panel keeps the size it was given.
        movable.move_to((300, 250));
        let kept = bounds(&|r| r.x() == 300.0);
        assert_eq!((kept.width(), kept.height()), (200.0, 150.0));
        movable.set_geometry((120, 200), (400, 300));
        bounds(&|r| r.width() == 400.0);
        // Eight resize targets, on the panel itself.
        let zones = std::iter::successors(movable.shell.first_child(), gtk4::Widget::next_sibling)
            .filter(|w| w.has_css_class("card-resize-zone"))
            .count();
        assert_eq!(zones, 8);
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
    fn the_real_settings_panel_fits_its_smallest_size() {
        crate::gtk_test::run_in_child_process("floating_panel::tests::real_size_inner");
    }

    #[test]
    fn real_size_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        // No page may need more than the smallest the panel can be dragged
        // to: it scrolls inside it instead.
        let state = Rc::new(std::cell::RefCell::new(crate::state::AppState::default()));
        let panel = crate::harness_settings::build_harness_settings_panel(
            state,
            Rc::new(|_| {}),
            Rc::new(|_| {}),
            Rc::new(|_| {}),
            Rc::new(|_| {}),
            crate::launcher_settings::ConnectionHooks::inert(),
        );
        let (width, height) = crate::harness_settings::SETTINGS_PANEL_MIN_SIZE;
        let min_width = panel.widget.measure(gtk4::Orientation::Horizontal, -1).0;
        let min_height = panel.widget.measure(gtk4::Orientation::Vertical, width).0;
        assert!(min_width <= width && min_height <= height, "needs {min_width}x{min_height}");
    }
}
