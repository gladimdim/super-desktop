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
//! 100ms tick that runs only while those events say it has something to decide
//! (see `Zone`) — so a pointer anywhere else costs no wakeups at all.
//!
//! **Two feeds, one dwell machine.** The corner surface reports the pointer
//! while the overlay is hidden. While it is visible the overlay window is
//! full-screen and already receives every motion event, so it feeds the same
//! `Zone` (see `window.rs`). Whichever surface Hyprland happens to route the
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
use std::cell::RefCell;
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

/// How often the dwell is evaluated while it runs. A tick is two cell reads and
/// a comparison — no subprocess, no allocation — so it can afford to be this
/// frequent; `Corner` keeps it from running when nothing is being decided.
const TICK: Duration = Duration::from_millis(100);

/// Edge-triggered dwell detector: fires once per visit to the corner.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
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

    /// Whether a tick with the pointer `inside` could change anything. Outside
    /// with nothing held, or inside a visit that already fired, a tick only
    /// finds the state it leaves — so no tick has to run until the pointer
    /// flag changes.
    pub fn wants_tick(&self, inside: bool) -> bool {
        if inside {
            !self.fired
        } else {
            *self != Self::default()
        }
    }
}

/// The pointer flag, the dwell, and whether a ticker is running for them — the
/// whole decision of when to tick, apart from the timer itself.
///
/// The ticker starts when a feed reports something a tick could act on and
/// stops as soon as a tick would only repeat itself. What a tick *samples* is
/// unchanged from a free-running tick: a leave is noticed by the next tick, so
/// the leave/enter pair of a hand-over between the corner surface and the
/// overlay (one Wayland frame) still does not re-arm a visit that fired, and a
/// parked pointer still cannot toggle the overlay back and forth.
#[derive(Debug, Default)]
struct Corner {
    inside: bool,
    dwell: Dwell,
    ticking: bool,
}

impl Corner {
    /// A feed reported the pointer; `true` means a ticker has to be started.
    fn set(&mut self, inside: bool) -> bool {
        self.inside = inside;
        if self.ticking || !self.dwell.wants_tick(inside) {
            return false;
        }
        self.ticking = true;
        true
    }

    /// One tick of the running ticker; `true` on the tick that completes a
    /// dwell.
    fn tick(&mut self, elapsed: Duration) -> bool {
        self.dwell.tick(self.inside, elapsed)
    }

    /// Asked after every tick (and after the toggle it may have run, which can
    /// move the pointer flag): whether the ticker keeps going. `false` marks it
    /// stopped, so the next `set` that matters starts a new one.
    fn keep_ticking(&mut self) -> bool {
        self.ticking = self.dwell.wants_tick(self.inside);
        self.ticking
    }
}

/// "The pointer is parked in the top-left corner zone", shared by both feeds:
/// the corner surface while the overlay is hidden and the overlay window while
/// it is visible.
///
/// Reporting the pointer is also what runs the dwell. A free-running 100ms tick
/// woke the main thread ten times a second for the whole life of the daemon;
/// this one only runs from the moment the pointer enters the zone until the
/// visit fires or ends.
#[derive(Default)]
pub struct Zone {
    corner: RefCell<Corner>,
    /// Runs once per completed dwell; set by `HotCorner::spawn`.
    on_toggle: RefCell<Option<Rc<dyn Fn()>>>,
}

impl Zone {
    /// Report whether the pointer is inside the zone. Cheap when nothing
    /// changes: it is called for every pointer move over the overlay.
    pub fn set(self: &Rc<Self>, inside: bool) {
        let start = self.corner.borrow_mut().set(inside);
        if start {
            self.start_ticker();
        }
    }

    fn start_ticker(self: &Rc<Self>) {
        let zone = Rc::downgrade(self);
        let mut last = Instant::now();
        glib::timeout_add_local(TICK, move || {
            let Some(zone) = zone.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let now = Instant::now();
            let elapsed = now.duration_since(last);
            last = now;
            // No borrow is held across the toggle: showing or hiding the
            // overlay can deliver crossing events that report back into `set`.
            let fired = zone.corner.borrow_mut().tick(elapsed);
            if fired {
                let on_toggle = zone.on_toggle.borrow().clone();
                if let Some(on_toggle) = on_toggle {
                    on_toggle();
                }
            }
            if zone.corner.borrow_mut().keep_ticking() {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
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
    /// Create and map the corner surface, and arm the dwell that watches it.
    ///
    /// `zone` is the shared "the pointer is in the corner zone" flag — the
    /// overlay window writes it too. `on_toggle` runs once per completed dwell.
    /// `None` means there is no display to draw on (see `main::show_window`):
    /// building widgets then dereferences NULL deep inside GTK.
    pub fn spawn(
        app: &Application,
        zone: Rc<Zone>,
        on_toggle: impl Fn() + 'static,
    ) -> Option<Self> {
        if gtk4::gdk::Display::default().is_none() {
            return None;
        }
        *zone.on_toggle.borrow_mut() = Some(Rc::new(on_toggle));

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
            let zone = Rc::clone(&zone);
            motion.connect_enter(move |_, _, _| zone.set(true));
        }
        {
            let zone = Rc::clone(&zone);
            motion.connect_motion(move |_, _, _| zone.set(true));
        }
        motion.connect_leave(move |_| zone.set(false));
        area.add_controller(motion);

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

    /// Drive `Corner` the way `Zone` does, on a simulated clock: pointer
    /// reports at the given milliseconds, and a tick every `TICK` for as long as
    /// the ticker runs. Returns when each dwell fired and how many ticks ran.
    fn drive(reports: &[(u64, bool)], until_ms: u64) -> (Vec<u64>, u32) {
        let tick_ms = TICK.as_millis() as u64;
        let mut corner = Corner::default();
        let mut next_tick = None;
        let mut fired = Vec::new();
        let mut ticks = 0;
        let mut reports = reports.iter().peekable();
        for now in 0..=until_ms {
            while let Some(&&(_, inside)) = reports.peek().filter(|(at, _)| *at == now) {
                reports.next();
                if corner.set(inside) {
                    assert!(next_tick.is_none(), "one ticker at a time");
                    next_tick = Some(now + tick_ms);
                }
            }
            if next_tick == Some(now) {
                ticks += 1;
                if corner.tick(TICK) {
                    fired.push(now);
                }
                next_tick = corner.keep_ticking().then_some(now + tick_ms);
            }
        }
        (fired, ticks)
    }

    #[test]
    fn test_corner_does_not_tick_while_the_pointer_is_elsewhere() {
        // A minute of pointer moves over the overlay, none in the corner.
        let moves: Vec<(u64, bool)> = (0..60_000).step_by(7).map(|at| (at, false)).collect();
        assert_eq!(drive(&moves, 60_000), (vec![], 0));
    }

    #[test]
    fn test_corner_parked_pointer_fires_once_then_stops_ticking() {
        // Parked for a minute: the dwell's 20 ticks, then none — a free-running
        // tick would have woken the daemon 600 times.
        assert_eq!(drive(&[(1_000, true)], 60_000), (vec![3_000], 20));
    }

    #[test]
    fn test_corner_flick_through_ticks_once_and_never_fires() {
        let (fired, ticks) = drive(&[(1_000, true), (1_050, false), (4_000, true), (4_020, false)], 10_000);
        assert!(fired.is_empty());
        // One tick per visit, the one that samples the leave and stops.
        assert_eq!(ticks, 2);
    }

    #[test]
    fn test_corner_hand_over_between_surfaces_does_not_re_fire() {
        // The dwell shows the overlay, which takes the corner over in one
        // Wayland frame: leave on one surface, enter on the other. The pointer
        // stays parked; the overlay must not toggle back.
        let (fired, ticks) = drive(&[(1_000, true), (3_010, false), (3_010, true)], 30_000);
        assert_eq!(fired, [3_000]);
        // The hand-over costs the one tick that samples it.
        assert_eq!(ticks, 21);
    }

    #[test]
    fn test_corner_re_arms_after_a_real_leave() {
        let (fired, ticks) = drive(&[(1_000, true), (5_000, false), (6_000, true)], 30_000);
        assert_eq!(fired, [3_000, 8_000]);
        assert_eq!(ticks, 20 + 1 + 20);
    }

    #[test]
    fn test_corner_leave_reported_during_the_toggle_keeps_the_ticker() {
        let mut corner = Corner::default();
        assert!(corner.set(true));
        for _ in 0..19 {
            assert!(!corner.tick(TICK));
            assert!(corner.keep_ticking());
        }
        assert!(corner.tick(TICK));
        // The toggle unmaps a surface, which reports a leave before the ticker
        // is asked whether to go on: that ticker samples it, no second starts.
        assert!(!corner.set(false));
        assert!(corner.keep_ticking());
        assert!(!corner.tick(TICK));
        assert!(!corner.keep_ticking());
        assert_eq!(corner.dwell, Dwell::default());
        assert!(corner.set(true), "the next visit starts a new ticker");
    }

    #[test]
    fn test_zone_on_a_real_main_loop() {
        // Own process: the timers need the default main context to themselves.
        crate::gtk_test::run_in_child_process("hotcorner::tests::zone_on_a_real_main_loop_child");
    }

    /// Only meaningful when re-run as the single test of a fresh process.
    #[test]
    fn zone_on_a_real_main_loop_child() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let context = glib::MainContext::default();
        let wakeups = std::cell::Cell::new(0u32);
        let run = |ms: u64| {
            wakeups.set(0);
            let deadline = Instant::now() + Duration::from_millis(ms);
            while Instant::now() < deadline {
                while context.iteration(false) {
                    wakeups.set(wakeups.get() + 1);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let zone = Rc::new(Zone::default());
        let toggles = Rc::new(std::cell::Cell::new(0));
        *zone.on_toggle.borrow_mut() = Some(Rc::new({
            let toggles = Rc::clone(&toggles);
            let zone = Rc::downgrade(&zone);
            move || {
                toggles.set(toggles.get() + 1);
                // Showing or hiding the overlay hands the corner to the other
                // surface from inside the toggle: leave, then enter.
                if let Some(zone) = zone.upgrade() {
                    zone.set(false);
                    zone.set(true);
                }
            }
        }));

        // Nothing reported yet, or only moves elsewhere: no timer at all.
        zone.set(false);
        run(300);
        assert_eq!((toggles.get(), wakeups.get()), (0, 0));

        zone.set(true);
        run(DWELL.as_millis() as u64 + 400);
        assert_eq!(toggles.get(), 1);
        // Parked after the toggle: the ticker has stopped.
        run(500);
        assert_eq!((toggles.get(), wakeups.get()), (1, 0));

        // Leaving costs one tick; the next visit toggles again.
        zone.set(false);
        run(300);
        assert_eq!(wakeups.get(), 1);
        zone.set(true);
        run(DWELL.as_millis() as u64 + 400);
        assert_eq!(toggles.get(), 2);
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
