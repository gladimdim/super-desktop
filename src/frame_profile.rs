//! Opt-in frame statistics: `SUPER_DESKTOP_PROFILE_FRAMES=1`.
//!
//! Every two seconds a watched window reports how many frames it produced and
//! how long the main thread spent in them, split into the frame clock's
//! update+layout phase (tick callbacks, size allocation) and its paint phase
//! (snapshot, GSK render, buffer swap). A frame is only produced when
//! something asked for one, so an idle overlay must report nothing at all:
//! any steady frame rate without visible motion is work to remove. Counts and
//! durations only — never widget text, terminal output or note content.
use gtk4::gdk::FrameClock;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

const REPORT_EVERY: Duration = Duration::from_secs(2);

pub fn enabled() -> bool {
    std::env::var_os("SUPER_DESKTOP_PROFILE_FRAMES").as_deref()
        == Some(std::ffi::OsStr::new("1"))
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
