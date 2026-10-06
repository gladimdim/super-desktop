//! Opt-in frame statistics: `SUPER_DESKTOP_PROFILE_FRAMES=1`.
//!
//! Every two seconds a watched window reports how many frames it produced and
//! how long the main thread spent in them, split into the frame clock's
//! update+layout phase (tick callbacks, size allocation) and its paint phase
//! (snapshot, GSK render, buffer swap). A frame is only produced when
//! something asked for one, so an idle overlay must report nothing at all:
//! any steady frame rate without visible motion is work to remove.
//! Animations that tick every vsync (the slide) also report, per run, how
//! many vsyncs they missed. Counts and durations only — never widget text,
//! terminal output or note content.
use gtk4::gdk::FrameClock;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

const REPORT_EVERY: Duration = Duration::from_secs(2);

pub fn enabled() -> bool {
    thread_local! {
        static ENABLED: bool = std::env::var_os("SUPER_DESKTOP_PROFILE_FRAMES").as_deref()
            == Some(std::ffi::OsStr::new("1"));
    }
    ENABLED.with(|enabled| *enabled)
}

thread_local! {
    /// A user action waiting for the next painted frame (see `note_request`).
    static PENDING: Cell<Option<(&'static str, Instant)>> = const { Cell::new(None) };
}

/// Report how long after this moment the next frame of a watched window is
/// painted: the delay between an action (show, new card) and the user seeing it.
pub fn note_request(action: &'static str) {
    if enabled() {
        PENDING.with(|pending| pending.set(Some((action, Instant::now()))));
    }
}

/// Report how long a step on the GTK thread took.
pub fn note_duration(step: &str, started: Instant) {
    if enabled() {
        eprintln!(
            "SUPER DESKTOP frames: {step} took {:.2} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// Report how long after `since` a new terminal first showed content.
pub fn note_terminal_content(since: Instant) {
    if enabled() {
        eprintln!(
            "SUPER DESKTOP frames: first terminal content {:.2} ms after its emulator was created",
            since.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// Vsync accounting for one run of an animation that ticks every frame.
#[derive(Default)]
pub struct Motion {
    first_us: Option<i64>,
    last_us: Option<i64>,
    frames: u32,
    missed: u32,
    longest_gap_us: i64,
    refresh_us: i64,
}

impl Motion {
    /// Record the frame the tick callback is producing.
    pub fn frame(&mut self, clock: &FrameClock) {
        if !enabled() {
            return;
        }
        let now = clock.frame_time();
        // The compositor reports the refresh interval with a frame's
        // presentation, so read it from the last frames already shown.
        let counter = clock.frame_counter();
        let refresh = (1..=4)
            .filter_map(|back| clock.timings(counter.saturating_sub(back)))
            .map(|timings| timings.refresh_interval())
            .find(|interval| *interval > 0)
            .unwrap_or(if self.refresh_us > 0 { self.refresh_us } else { 16_667 });
        self.refresh_us = refresh;
        self.frames += 1;
        self.first_us.get_or_insert(now);
        if let Some(previous) = self.last_us.replace(now) {
            let gap = now - previous;
            self.longest_gap_us = self.longest_gap_us.max(gap);
            // One frame on time is ~1 interval apart, one skipped ~2.
            if gap * 2 > refresh * 3 {
                self.missed += ((gap + refresh / 2) / refresh - 1) as u32;
            }
        }
    }

    pub fn report(&self, name: &str) {
        let (Some(first), Some(last)) = (self.first_us, self.last_us) else {
            return;
        };
        if !enabled() {
            return;
        }
        let span = (last - first) as f64 / 1000.0;
        eprintln!(
            "SUPER DESKTOP frames [{name}]: {} frames in {span:.1} ms ({:.1}/s), missed vsyncs {}, longest gap {:.2} ms, refresh {:.2} ms",
            self.frames,
            if span > 0.0 { f64::from(self.frames - 1) * 1000.0 / span } else { 0.0 },
            self.missed,
            self.longest_gap_us as f64 / 1000.0,
            self.refresh_us as f64 / 1000.0,
        );
    }
}

#[derive(Default)]
struct Window {
    frames: u32,
    layout: Duration,
    paint: Duration,
    worst: Duration,
    started: Option<Instant>,
    laid_out: Option<Instant>,
}

impl Window {
    fn report(&mut self, name: &str, elapsed: Duration) {
        if self.frames > 0 {
            let n = f64::from(self.frames);
            eprintln!(
                "SUPER DESKTOP frames [{name}]: {} in {:.1}s ({:.1}/s), update+layout {:.2} ms, paint {:.2} ms avg, worst {:.2} ms",
                self.frames,
                elapsed.as_secs_f64(),
                n / elapsed.as_secs_f64(),
                self.layout.as_secs_f64() * 1000.0 / n,
                self.paint.as_secs_f64() * 1000.0 / n,
                self.worst.as_secs_f64() * 1000.0,
            );
        }
        *self = Window::default();
    }
}

/// Report `widget`'s window frames under `name` when profiling is enabled.
/// Follows the frame clock across unrealize/realize (a hidden layer surface
/// can come back with a new one).
pub fn watch(widget: &impl IsA<gtk4::Widget>, name: &'static str) {
    if !enabled() {
        return;
    }
    let stats = Rc::new(RefCell::new(Window::default()));
    type Connected = Option<(glib::WeakRef<FrameClock>, Vec<glib::SignalHandlerId>)>;
    let connected: Rc<RefCell<Connected>> = Rc::new(RefCell::new(None));
    let disconnect = {
        let connected = Rc::clone(&connected);
        move || {
            if let Some((clock, handlers)) = connected.borrow_mut().take() {
                if let Some(clock) = clock.upgrade() {
                    for handler in handlers {
                        clock.disconnect(handler);
                    }
                }
            }
        }
    };
    let connect = {
        let stats = Rc::clone(&stats);
        let disconnect = disconnect.clone();
        move |widget: &gtk4::Widget| {
            disconnect();
            let Some(clock) = widget.frame_clock() else {
                return;
            };
            // Connected after GTK's own surface handlers, so `layout` marks the
            // end of GTK's layout and `after-paint` the end of its paint.
            let before = Rc::clone(&stats);
            let layout = Rc::clone(&stats);
            let after = Rc::clone(&stats);
            let handlers = vec![
                clock.connect_before_paint(move |_| {
                    let mut stats = before.borrow_mut();
                    stats.started = Some(Instant::now());
                    stats.laid_out = None;
                }),
                clock.connect_layout(move |_| {
                    layout.borrow_mut().laid_out = Some(Instant::now());
                }),
                clock.connect_after_paint(move |_| {
                    let mut stats = after.borrow_mut();
                    let Some(started) = stats.started.take() else {
                        return;
                    };
                    let now = Instant::now();
                    if let Some((action, at)) = PENDING.with(Cell::take) {
                        eprintln!(
                            "SUPER DESKTOP frames: first frame painted {:.2} ms after {action}",
                            (now - at).as_secs_f64() * 1000.0
                        );
                    }
                    let laid_out = stats.laid_out.take().unwrap_or(started);
                    stats.frames += 1;
                    stats.layout += laid_out - started;
                    stats.paint += now - laid_out;
                    stats.worst = stats.worst.max(now - started);
                }),
            ];
            *connected.borrow_mut() = Some((clock.downgrade(), handlers));
        }
    };
    let widget = widget.as_ref();
    if widget.is_realized() {
        connect(widget);
    }
    widget.connect_realize(move |widget| connect(widget));
    widget.connect_unrealize(move |_| disconnect());

    let since = Cell::new(Instant::now());
    let weak = widget.downgrade();
    glib::timeout_add_local(REPORT_EVERY, move || {
        if weak.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        let now = Instant::now();
        stats.borrow_mut().report(name, now - since.replace(now));
        glib::ControlFlow::Continue
    });
}
