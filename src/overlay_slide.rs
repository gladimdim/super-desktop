//! The show/hide slide for what the overlay places itself instead of the
//! card canvas: floating panels (Settings, Files, plugin panels), plugin
//! docks and a plugin's full-screen surface. They leave and come back by the
//! nearest left or right edge, on the same spring and the same rule as the
//! cards (`window.rs` paints both). At rest the overlay places them as usual.
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};

thread_local! {
    /// 0: off screen, 1: at rest (the spring overshoots a little past it).
    static FACTOR: Cell<f64> = const { Cell::new(1.0) };
    static OVERLAYS: RefCell<Vec<gtk4::glib::WeakRef<gtk4::Overlay>>> = const { RefCell::new(Vec::new()) };
}

/// Where the slide is now.
pub fn factor() -> f64 {
    FACTOR.with(Cell::get)
}

/// Whether the overlay places its children as usual (the slide is at rest).
pub fn at_rest() -> bool {
    factor() == 1.0
}

/// Moves everything that slides to `factor` (see [`factor`]).
pub fn paint(factor: f64) {
    if FACTOR.with(|cell| cell.replace(factor)) == factor {
        return;
    }
    OVERLAYS.with(|overlays| {
        overlays.borrow_mut().retain(|weak| weak.upgrade().is_some());
        for overlay in overlays.borrow().iter().filter_map(|weak| weak.upgrade()) {
            overlay.queue_allocate();
        }
    });
}

/// An overlay whose children follow the slide: it lays them out again at
/// every step.
pub fn register(overlay: &gtk4::Overlay) {
    OVERLAYS.with(|overlays| {
        let mut overlays = overlays.borrow_mut();
        if !overlays.iter().any(|weak| weak.upgrade().as_ref() == Some(overlay)) {
            overlays.push(overlay.downgrade());
        }
    });
}

/// Just beyond the nearer left or right edge of a `screen_width` screen for
/// something `width` wide resting at `x`. Never up or down.
pub fn offscreen_x(x: f64, width: f64, screen_width: f64) -> f64 {
    if x + width * 0.5 < screen_width * 0.5 {
        -width - 40.0
    } else {
        screen_width + 40.0
    }
}

/// Where something resting at `x` is drawn now.
pub fn shifted_x(x: i32, width: i32, screen_width: i32) -> i32 {
    let rest = f64::from(x);
    let away = offscreen_x(rest, f64::from(width), f64::from(screen_width));
    (away + (rest - away) * factor()).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_slide_leaves_by_the_nearer_edge() {
        assert_eq!(offscreen_x(10.0, 300.0, 1000.0), -340.0);
        assert_eq!(offscreen_x(690.0, 300.0, 1000.0), 1040.0);
        // The middle goes right, as a full-screen surface does.
        assert_eq!(offscreen_x(0.0, 1000.0, 1000.0), 1040.0);
        paint(0.0);
        assert_eq!(shifted_x(10, 300, 1000), -340);
        assert!(!at_rest());
        paint(0.5);
        assert_eq!(shifted_x(690, 300, 1000), 865);
        paint(1.0);
        assert!(at_rest());
        assert_eq!(shifted_x(690, 300, 1000), 690);
    }
}
