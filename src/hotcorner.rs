//! 🎯 Top-left hot corner: park the pointer there for two seconds and the
//! overlay toggles.
//!
//! **Why a surface and not a poll.** While the overlay is hidden there is
//! nothing of ours under the pointer, so the only free signal is pointer
//! motion. Polling `hyprctl cursorpos` was measured at ~41ms of CPU per call on
//! this machine, so even 5Hz would cost a permanent ~20% of a core — against
//! every cheap-when-idle decision in this daemon (10ms pipe tick, no `tmux`
//! probes while hidden). Instead the daemon keeps one invisible 8×8 layer
//! surface in the corner whose only job is to receive motion events, and a
//! 100ms tick that reads one flag cell.
//!
//! **Two feeds, one dwell machine.** The corner surface reports the pointer
//! while the overlay is hidden. While it is visible the overlay window is
//! full-screen and already receives every motion event, so it feeds the same
//! flag (see `window.rs`). Whichever surface Hyprland happens to route the
//! corner to, both say the same thing — so the gesture works in both
//! directions: hidden → show, visible → hide, with no dependency on layer
//! stacking order.
//!
//! **Cost of the zone.** An input region is all-or-nothing, so the surface
//! swallows clicks inside its 8×8 px. That is why it is that small and why it
//! sits in the extreme corner, where a click is deliberate. Pointer *grabs*
//! still work: dragging a window's corner toward (0,0) routes motion to the
//! grabbing client, not to us.
//!
//! It paints nothing: `styles.rs` gives it a transparent background, and its
//! namespace deliberately does not contain `super-desktop`, because
//! `install.sh` matches that string to turn on blur for the overlay — a blurred
//! transparent rect would be a visible smudge.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, DrawingArea, EventControllerMotion};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Namespace of the corner surface. Deliberately not `super-desktop…`: the blur
/// layer rule `install.sh` writes matches that string.
pub const NAMESPACE: &str = "sd-hotcorner";

/// Side of the corner zone in logical pixels. Big enough to be hit by shoving
/// the pointer into the corner (where it clamps), small enough that a click
/// inside it is something the user meant.
pub const CORNER_PX: f64 = 8.0;

/// How long the pointer must stay inside the zone before it counts.
pub const DWELL: Duration = Duration::from_secs(2);

/// How often the dwell is evaluated. A tick is two cell reads and a comparison
/// — no subprocess, no allocation — so it can afford to be this frequent.
const TICK: Duration = Duration::from_millis(100);

/// Edge-triggered dwell detector: fires once per visit to the corner.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Dwell {
    /// Time the pointer has spent inside since it last entered.
    held: Duration,
    /// Whether this visit already fired, so a parked pointer cannot toggle the
    /// overlay back and forth.
    fired: bool,
}

impl Dwell {
    /// Feed the pointer state and the time since the previous feed; returns
    /// `true` on the single tick that completes a dwell.
    ///
    /// Leaving the zone resets the detector, which is what re-arms it for the
    /// next visit — including the visit that begins right after a fire.
    pub fn tick(&mut self, inside: bool, elapsed: Duration) -> bool {
        if !inside {
            *self = Self::default();
            return false;
        }
        self.held += elapsed;
        if !self.fired && self.held >= DWELL {
            self.fired = true;
            return true;
        }
        false
    }
}

/// The daemon's corner surface. Dropping it unmaps the zone again.
pub struct HotCorner {
    /// Dropping this would destroy the window and unmap the zone, which is the
    /// whole gesture — holding it is the point, so it is never read.
    #[allow(dead_code)]
    window: ApplicationWindow,
}

impl HotCorner {
    /// Create and map the corner surface, and start the ticker that watches it.
    ///
    /// `inside` is the shared "the pointer is in the corner zone" flag — the
    /// overlay window writes it too. `on_toggle` runs once per completed dwell.
    /// `None` means there is no display to draw on (see `main::show_window`):
    /// building widgets then dereferences NULL deep inside GTK.
    pub fn spawn(
        app: &Application,
        inside: Rc<Cell<bool>>,
        on_toggle: impl Fn() + 'static,
    ) -> Option<Self> {
        if gtk4::gdk::Display::default().is_none() {
            return None;
        }

        let window = ApplicationWindow::new(app);
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(NAMESPACE));
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Left, true);
        // 0, not the default: an exclusive zone would shove every other window
        // away from the corner.
        window.set_exclusive_zone(0);
        // None, not OnDemand: the gesture is a pointer gesture and must never
        // take the keyboard.
        window.set_keyboard_mode(KeyboardMode::None);
        window.add_css_class("sd-hot-corner");
        // Both, and on the window: `set_default_size` alone left the surface at
        // GTK's 200x200 window default (measured with `hyprctl layers`), which
        // would swallow clicks far beyond the corner.
        window.set_default_size(CORNER_PX as i32, CORNER_PX as i32);
        window.set_size_request(CORNER_PX as i32, CORNER_PX as i32);

        // A child widget, not the bare window: GTK picks the event target by
        // widget, and a window with no child has nothing to target.
        let area = DrawingArea::new();
        area.set_content_width(CORNER_PX as i32);
        area.set_content_height(CORNER_PX as i32);
        window.set_child(Some(&area));

        let motion = EventControllerMotion::new();
        {
            let inside = Rc::clone(&inside);
            motion.connect_enter(move |_, _, _| inside.set(true));
        }
        {
            let inside = Rc::clone(&inside);
            motion.connect_motion(move |_, _, _| inside.set(true));
        }
        {
            let inside = Rc::clone(&inside);
            motion.connect_leave(move |_| inside.set(false));
        }
        area.add_controller(motion);

        let mut dwell = Dwell::default();
        let mut last = Instant::now();
        glib::timeout_add_local(TICK, move || {
            let now = Instant::now();
            let elapsed = now.duration_since(last);
            last = now;
            if dwell.tick(inside.get(), elapsed) {
                on_toggle();
            }
            glib::ControlFlow::Continue
        });

        window.present();
        Some(Self { window })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(dwell: &mut Dwell, inside: bool, ms: u64) -> bool {
        dwell.tick(inside, Duration::from_millis(ms))
    }

    #[test]
    fn test_dwell_needs_two_seconds_inside_and_fires_once_per_visit() {
        let mut dwell = Dwell::default();
        // 19 × 100ms = 1.9s: still short of the dwell.
        for _ in 0..19 {
            assert!(!tick(&mut dwell, true, 100));
        }
        assert!(tick(&mut dwell, true, 100), "the 2s tick must fire");
        // A pointer parked in the corner must not keep toggling.
        for _ in 0..100 {
            assert!(!tick(&mut dwell, true, 100));
        }
    }

    #[test]
    fn test_dwell_re_arms_after_the_pointer_leaves() {
        let mut dwell = Dwell::default();
        for _ in 0..19 {
            tick(&mut dwell, true, 100);
        }
        assert!(tick(&mut dwell, true, 100));
        assert!(!tick(&mut dwell, false, 100), "leaving must not fire");
        // The next visit earns the full dwell again…
        for _ in 0..19 {
            assert!(!tick(&mut dwell, true, 100));
        }
        assert!(tick(&mut dwell, true, 100), "second visit must fire too");
        // …and a single tick outside is enough to re-arm.
        assert!(!tick(&mut dwell, false, 100));
        for _ in 0..19 {
            assert!(!tick(&mut dwell, true, 100));
        }
        assert!(tick(&mut dwell, true, 100));
    }

    #[test]
    fn test_dwell_ignores_a_flick_through_the_corner() {
        let mut dwell = Dwell::default();
        // Through the corner in 100ms, twice: neither visit is a gesture.
        assert!(!tick(&mut dwell, true, 100));
        assert!(!tick(&mut dwell, false, 100));
        assert!(!tick(&mut dwell, true, 100));
        assert!(!tick(&mut dwell, false, 100));
        // A parked pointer does fire, even if the ticks are coarse.
        assert!(!tick(&mut dwell, true, 1000));
        assert!(tick(&mut dwell, true, 1000));
    }

    #[test]
    fn test_dwell_spends_its_time_inside_only() {
        let mut dwell = Dwell::default();
        // 5s outside, then 1s inside: inside time is what counts.
        for _ in 0..50 {
            assert!(!tick(&mut dwell, false, 100));
        }
        for _ in 0..10 {
            assert!(!tick(&mut dwell, true, 100));
        }
        assert_eq!(
            dwell,
            Dwell {
                held: Duration::from_millis(1000),
                fired: false
            }
        );
    }
}
