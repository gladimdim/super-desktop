//! Alt-held keyboard selection, drawn above cards without changing layout.
use gtk4::{gdk, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

const HOLD_DELAY: Duration = Duration::from_millis(30);
const MAX_TARGETS: usize = 10;

pub fn next_digit(used: impl Iterator<Item = u8>) -> Option<u8> {
    let mut occupied = [false; MAX_TARGETS];
    for digit in used {
        if let Some(slot) = occupied.get_mut(digit as usize) {
            *slot = true;
        }
    }
    occupied
        .iter()
        .position(|used| !used)
        .map(|index| index as u8)
}

type Targets = Rc<dyn Fn() -> Vec<Target>>;

pub struct Target {
    pub widget: gtk4::Widget,
    pub digit: Option<u8>,
    pub activate: Rc<dyn Fn()>,
}

/// Held Alt keys are tracked by hardware keycode, never by keyval: while Alt
/// is down the layout may translate the same key to another keysym, so its
/// release need not say `Alt_L` (`grp:alts_toggle` reports `ISO_Prev_Group`).
/// Autorepeat never restarts the timer. Other Alt shortcuts cancel the
/// visual preview.
#[derive(Default)]
struct Hold {
    keys: Vec<u32>,
    cancelled: bool,
    generation: u64,
}

impl Hold {
    fn press(&mut self, keycode: u32) -> Option<u64> {
        let first = self.keys.is_empty();
        if !self.keys.contains(&keycode) {
            self.keys.push(keycode);
        }
        if !first {
            return None;
        }
        self.cancelled = false;
        self.generation += 1;
        Some(self.generation)
    }
    /// True only when this release lets go of the last held Alt key.
    fn release(&mut self, keycode: u32) -> bool {
        let before = self.keys.len();
        self.keys.retain(|held| *held != keycode);
        self.keys.len() < before && self.keys.is_empty()
    }
    fn holds(&self, keycode: u32) -> bool {
        self.keys.contains(&keycode)
    }
    fn cancel(&mut self) {
        self.cancelled = true;
        self.generation += 1;
    }
    fn ready(&self, generation: u64) -> bool {
        !self.keys.is_empty() && !self.cancelled && self.generation == generation
    }
}

fn is_alt(key: gdk::Key) -> bool {
    matches!(key, gdk::Key::Alt_L | gdk::Key::Alt_R)
}

fn digit_index(key: gdk::Key) -> Option<usize> {
    let digit = match key {
        gdk::Key::KP_0 => 0,
        gdk::Key::KP_1 => 1,
        gdk::Key::KP_2 => 2,
        gdk::Key::KP_3 => 3,
        gdk::Key::KP_4 => 4,
        gdk::Key::KP_5 => 5,
        gdk::Key::KP_6 => 6,
        gdk::Key::KP_7 => 7,
        gdk::Key::KP_8 => 8,
        gdk::Key::KP_9 => 9,
        _ => key.to_unicode()?.to_digit(10)? as usize,
    };
    (digit < MAX_TARGETS).then_some(digit)
}

pub struct Picker {
    layer: gtk4::DrawingArea,
    targets: RefCell<Vec<Target>>,
    get_targets: Targets,
    hold: RefCell<Hold>,
    showing: Cell<bool>,
    selected: Cell<Option<usize>>,
    /// Told when the preview goes up (`true`) and comes down (`false`).
    on_showing: RefCell<Option<Rc<dyn Fn(bool)>>>,
}

impl Picker {
    pub fn install(
        window: &gtk4::ApplicationWindow,
        overlay: &gtk4::Overlay,
        get_targets: Targets,
    ) -> Rc<Self> {
        let layer = gtk4::DrawingArea::new();
        layer.set_hexpand(true);
        layer.set_vexpand(true);
        layer.set_can_target(false);
        layer.set_focusable(false);
        layer.add_css_class("terminal-picker-overlay");
        layer.set_visible(false);
        overlay.add_overlay(&layer);
        overlay.set_measure_overlay(&layer, false);
        overlay.set_clip_overlay(&layer, true);
        let picker = Rc::new(Self {
            layer,
            targets: RefCell::new(Vec::new()),
            get_targets,
            hold: RefCell::new(Hold::default()),
            showing: Cell::new(false),
            selected: Cell::new(None),
            on_showing: RefCell::new(None),
        });
        let weak = Rc::downgrade(&picker);
        picker.layer.set_draw_func(move |layer, cr, _, _| {
            let Some(picker) = weak.upgrade() else {
                return;
            };
            let color = layer.color();
            let mut badges = Vec::new();
            for target in picker.targets.borrow().iter() {
                if !target.widget.is_mapped() {
                    continue;
                }
                let Some(rect) = target.widget.compute_bounds(layer) else {
                    continue;
                };
                let (x, y, w, h) = (
                    rect.x() as f64,
                    rect.y() as f64,
                    rect.width() as f64,
                    rect.height() as f64,
                );
                cr.set_source_rgba(
                    color.red() as f64,
                    color.green() as f64,
                    color.blue() as f64,
                    0.9,
                );
                cr.set_line_width(2.0);
                cr.set_dash(&[2.0, 5.0], 0.0);
                cr.rectangle(x + 1.0, y + 1.0, (w - 2.0).max(0.0), (h - 2.0).max(0.0));
                let _ = cr.stroke();
                let Some(digit) = target.digit else {
                    continue;
                };
                badges.push((digit, x + w / 2.0, y + h / 2.0));
            }
            // Draw every label after every border, so another card's outline
            // cannot cross through a digit on an overlapping terminal.
            for (digit, cx, cy) in badges {
                let text = digit.to_string();
                cr.select_font_face(
                    "sans-serif",
                    gtk4::cairo::FontSlant::Normal,
                    gtk4::cairo::FontWeight::Bold,
                );
                cr.set_font_size(48.0);
                // An opaque badge keeps a buried terminal's digit legible.
                cr.set_source_rgba(0.08, 0.08, 0.10, 0.94);
                cr.arc(cx, cy, 34.0, 0.0, std::f64::consts::TAU);
                let _ = cr.fill();
                cr.set_source_rgb(1.0, 1.0, 1.0);
                if let Ok(extents) = cr.text_extents(&text) {
                    cr.move_to(
                        cx - extents.width() / 2.0 - extents.x_bearing(),
                        cy - extents.height() / 2.0 - extents.y_bearing(),
                    );
                    let _ = cr.show_text(&text);
                }
            }
        });
        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let p = Rc::clone(&picker);
        keys.connect_key_pressed(move |_, key, keycode, modifiers| p.press(key, keycode, modifiers));
        let p = Rc::clone(&picker);
        keys.connect_key_released(move |_, _, keycode, _| p.release(keycode));
        window.add_controller(keys);
        let p = Rc::clone(&picker);
        window.connect_unmap(move |_| p.reset());
        let p = Rc::clone(&picker);
        window.connect_is_active_notify(move |window| {
            if !window.is_active() {
                p.reset();
            }
        });
        picker
    }

    /// Call `f` when the preview goes up (`true`) and when it comes down
    /// (`false`). The preview dims every card, so whatever lies under a card
    /// shows through it meanwhile.
    pub fn connect_showing(&self, f: impl Fn(bool) + 'static) {
        *self.on_showing.borrow_mut() = Some(Rc::new(f));
    }

    fn notify_showing(&self, showing: bool) {
        // Out of the borrow: the callback may touch widgets whose signals
        // come back here.
        let callback = self.on_showing.borrow().clone();
        if let Some(callback) = callback {
            callback(showing);
        }
    }

    fn press(
        self: &Rc<Self>,
        key: gdk::Key,
        keycode: u32,
        modifiers: gdk::ModifierType,
    ) -> glib::Propagation {
        let conflicting = modifiers.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::SUPER_MASK
                | gdk::ModifierType::SHIFT_MASK,
        );
        // A repeat of a held Alt key may carry another keysym (see `Hold`).
        if is_alt(key) || self.hold.borrow().holds(keycode) {
            if conflicting {
                self.cancel();
                return glib::Propagation::Proceed;
            }
            let generation = self.hold.borrow_mut().press(keycode);
            if let Some(generation) = generation {
                *self.targets.borrow_mut() = (self.get_targets)()
                    .into_iter()
                    .filter(|target| target.widget.is_mapped())
                    .collect();
                let weak = Rc::downgrade(self);
                glib::timeout_add_local_once(HOLD_DELAY, move || {
                    if let Some(picker) = weak.upgrade() {
                        if picker.hold.borrow().ready(generation) {
                            picker.show();
                        }
                    }
                });
            }
            return glib::Propagation::Proceed;
        }
        let held = !self.hold.borrow().keys.is_empty();
        if !held {
            return glib::Propagation::Proceed;
        }
        if !conflicting && modifiers.contains(gdk::ModifierType::ALT_MASK) {
            if let Some(index) = digit_index(key) {
                let action = self
                    .targets
                    .borrow()
                    .iter()
                    .find(|target| target.digit == Some(index as u8))
                    .filter(|target| target.widget.is_mapped())
                    .map(|target| Rc::clone(&target.activate));
                if let Some(action) = action {
                    self.hold.borrow_mut().cancel();
                    self.hide();
                    if self.selected.replace(Some(index)) != Some(index) {
                        action();
                    }
                    return glib::Propagation::Stop;
                }
            }
        }
        let escape = key == gdk::Key::Escape && self.showing.get();
        self.cancel();
        if escape {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }

    /// Letting go of the last held Alt key ends the preview, matched by
    /// keycode because the released keyval need not be an Alt keysym.
    fn release(&self, keycode: u32) {
        if self.hold.borrow_mut().release(keycode) {
            self.cancel();
        }
    }

    fn show(self: &Rc<Self>) {
        if self.targets.borrow().is_empty() {
            return;
        }
        self.showing.set(true);
        for target in self.targets.borrow().iter() {
            target.widget.add_css_class("terminal-picker-ghost");
        }
        self.notify_showing(true);
        self.layer.set_visible(true);
        self.layer.queue_draw();
        // Only the visible preview follows moving/resizing cards each frame.
        // The normal desktop does not retain a repaint timer. The layer is a
        // full-screen Cairo drawing, so it is repainted only on the frames
        // where an outline actually moved, not at the display rate.
        let weak = Rc::downgrade(self);
        let drawn = RefCell::new(self.target_bounds());
        self.layer.add_tick_callback(move |layer, _| {
            let Some(picker) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !picker.showing.get() {
                return glib::ControlFlow::Break;
            }
            if !picker
                .targets
                .borrow()
                .iter()
                .any(|target| target.widget.is_mapped())
            {
                picker.cancel();
                return glib::ControlFlow::Break;
            }
            let bounds = picker.target_bounds();
            if bounds != *drawn.borrow() {
                *drawn.borrow_mut() = bounds;
                layer.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }

    /// Where each target's outline goes, as the draw function computes it.
    fn target_bounds(&self) -> Vec<Option<[f32; 4]>> {
        self.targets
            .borrow()
            .iter()
            .map(|target| {
                if !target.widget.is_mapped() {
                    return None;
                }
                let rect = target.widget.compute_bounds(&self.layer)?;
                Some([rect.x(), rect.y(), rect.width(), rect.height()])
            })
            .collect()
    }

    fn hide(&self) {
        let was_showing = self.showing.replace(false);
        self.layer.set_visible(false);
        let widgets: Vec<_> = self
            .targets
            .borrow()
            .iter()
            .map(|target| target.widget.clone())
            .collect();
        for widget in widgets {
            widget.remove_css_class("terminal-picker-ghost");
        }
        // Every cancel ends up here: report the preview going away only once.
        if was_showing {
            self.notify_showing(false);
        }
    }

    pub fn cancel(&self) {
        self.hold.borrow_mut().cancel();
        self.hide();
        self.targets.borrow_mut().clear();
        self.selected.set(None);
    }

    fn reset(&self) {
        self.cancel();
        self.hold.borrow_mut().keys.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hardware keycodes (evdev + 8) as GDK reports them on Wayland.
    const LALT: u32 = 64;
    const RALT: u32 = 108;
    const KEY_0: u32 = 19;
    const KEY_3: u32 = 12;
    const KEY_F: u32 = 41;

    #[test]
    #[ignore = "uses a real Wayland layer surface and wtype keyboard events"]
    fn wayland_alt_events_reach_picker() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process("terminal_picker::tests::wayland_alt_events_inner");
            return;
        }
    }

    #[test]
    fn wayland_alt_events_inner() {
        if !crate::gtk_test::is_child() { return; }
        use gtk4_layer_shell::{LayerShell, KeyboardMode, Layer};
        gtk4::init().unwrap();
        let window = gtk4::ApplicationWindow::builder().build();
        window.init_layer_shell();
        window.set_namespace(Some("sd-picker-keyboard-test"));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        let overlay = gtk4::Overlay::new();
        let terminal = vte4::Terminal::new();
        terminal.set_size_request(400, 250);
        overlay.set_child(Some(&terminal));
        window.set_child(Some(&overlay));
        let selected = Rc::new(Cell::new(false));
        let get_targets: Targets = {
            let widget = terminal.clone();
            let selected = Rc::clone(&selected);
            Rc::new(move || {
                let selected = Rc::clone(&selected);
                vec![Target { widget: widget.clone().upcast(), digit: Some(0),
                    activate: Rc::new(move || selected.set(true)) }]
            })
        };
        let picker = Picker::install(&window, &overlay, get_targets);
        let events = Rc::new(RefCell::new(Vec::new()));
        let capture = gtk4::EventControllerLegacy::new();
        capture.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let events_log = Rc::clone(&events);
        capture.connect_event(move |_, event| {
            if let Some(key) = event.downcast_ref::<gdk::KeyEvent>() {
                events_log.borrow_mut().push((key.event_type(), key.keyval(), key.keycode(), key.modifier_state()));
            }
            glib::Propagation::Proceed
        });
        window.add_controller(capture);
        let pump = |ms| {
            let until = std::time::Instant::now() + Duration::from_millis(ms);
            while std::time::Instant::now() < until {
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        window.present();
        terminal.grab_focus();
        pump(200);
        // Hold and let go (0-300 ms), then hold again and pick 0 (700-1200 ms).
        // wtype's own keymap has no modifier map, so its Alt_L key sets no
        // modifier: `-M alt` supplies the ALT state a real keyboard reports.
        let mut keys = std::process::Command::new("wtype")
            .args(["-M", "alt", "-P", "Alt_L", "-s", "300", "-p", "Alt_L", "-m", "alt",
                "-s", "400", "-M", "alt", "-P", "Alt_L", "-s", "300", "-k", "0", "-s", "200",
                "-p", "Alt_L", "-m", "alt"])
            .spawn().unwrap();
        pump(200);
        let shown = picker.showing.get();
        pump(300);
        let hidden_on_release = !picker.showing.get();
        pump(500);
        let shown_again = picker.showing.get();
        pump(500);
        assert!(keys.wait().unwrap().success());
        window.close();
        assert!(shown, "Alt preview missing; events={:?}, selected={}", events.borrow(), selected.get());
        assert!(hidden_on_release, "releasing Alt left the digits up; events={:?}", events.borrow());
        assert!(shown_again, "second Alt hold missing; events={:?}", events.borrow());
        assert!(selected.get(), "Alt+0 did not select; events={:?}", events.borrow());
        assert!(!picker.showing.get());
    }

    #[test]
    fn mapped_picker_delay_selection_and_cleanup() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process(
                "terminal_picker::tests::mapped_picker_delay_selection_and_cleanup",
            );
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let window = gtk4::ApplicationWindow::builder()
            .default_width(500)
            .default_height(350)
            .build();
        let overlay = gtk4::Overlay::new();
        let canvas = gtk4::Fixed::new();
        overlay.set_child(Some(&canvas));
        window.set_child(Some(&overlay));
        let zero = gtk4::Entry::new();
        let three = gtk4::Entry::new();
        zero.set_size_request(200, 100);
        three.set_size_request(200, 100);
        canvas.put(&zero, 20.0, 20.0);
        canvas.put(&three, 120.0, 80.0);
        let selected = Rc::new(Cell::new(None));
        let selections = Rc::new(Cell::new(0));
        let targets: Targets = {
            let zero = zero.clone();
            let three = three.clone();
            let selected = Rc::clone(&selected);
            let selections = Rc::clone(&selections);
            Rc::new(move || {
                // Deliberately reverse order and leave numbering holes.
                [(three.clone(), 3), (zero.clone(), 0)]
                    .into_iter()
                    .map(|(widget, digit)| {
                        let selected = Rc::clone(&selected);
                        let selections = Rc::clone(&selections);
                        let focus = widget.clone();
                        Target {
                            widget: widget.upcast(),
                            digit: Some(digit),
                            activate: Rc::new(move || {
                                selected.set(Some(digit));
                                selections.set(selections.get() + 1);
                                focus.grab_focus();
                            }),
                        }
                    })
                    .collect()
            })
        };
        let picker = Picker::install(&window, &overlay, targets);
        let reports = Rc::new(RefCell::new(Vec::new()));
        picker.connect_showing({
            let reports = Rc::clone(&reports);
            move |showing| reports.borrow_mut().push(showing)
        });
        window.present();
        let pump = |ms| {
            let deadline = std::time::Instant::now() + Duration::from_millis(ms);
            while std::time::Instant::now() < deadline {
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        pump(100);
        assert!(zero.is_mapped());
        let alt = gdk::ModifierType::ALT_MASK;
        let none = gdk::ModifierType::empty();
        picker.press(gdk::Key::Alt_L, LALT, none);
        assert!(!picker.layer.is_visible(), "must wait for the hold delay");
        picker.press(gdk::Key::Alt_L, LALT, alt); // Repeat does not reset the clock.
        pump(100);
        assert!(picker.layer.is_visible());
        assert!(zero.has_css_class("terminal-picker-ghost"));
        assert!(three.has_css_class("terminal-picker-ghost"));
        assert!(!picker.layer.can_target());
        // The overlay lets every terminal draw while the cards are dimmed.
        assert_eq!(*reports.borrow(), [true]);
        if let Ok(path) = std::env::var("SUPER_DESKTOP_PICKER_PREVIEW") {
            let snapshot = gtk4::Snapshot::new();
            let paintable = gtk4::WidgetPaintable::new(Some(&overlay));
            paintable.snapshot(&snapshot, overlay.width() as f64, overlay.height() as f64);
            let node = snapshot.to_node().unwrap();
            window
                .renderer()
                .unwrap()
                .render_texture(&node, None)
                .save_to_png(path)
                .unwrap();
        }
        assert_eq!(picker.press(gdk::Key::_3, KEY_3, alt), glib::Propagation::Stop);
        assert_eq!(selected.get(), Some(3));
        assert_eq!(*reports.borrow(), [true, false]);
        let focused = gtk4::prelude::RootExt::focus(&window).unwrap();
        assert!(focused == three.clone().upcast::<gtk4::Widget>() || focused.is_ancestor(&three));
        assert!(!picker.layer.is_visible());
        assert!(!zero.has_css_class("terminal-picker-ghost"));
        assert_eq!(picker.press(gdk::Key::_3, KEY_3, alt), glib::Propagation::Stop);
        assert_eq!(
            selections.get(),
            1,
            "repeat must not leak a digit or refocus"
        );
        assert_eq!(picker.press(gdk::Key::_0, KEY_0, alt), glib::Propagation::Stop);
        assert_eq!(selected.get(), Some(0));
        picker.reset();
        picker.press(gdk::Key::Alt_L, LALT, none);
        assert_eq!(picker.press(gdk::Key::_3, KEY_3, alt), glib::Propagation::Stop);
        assert_eq!(
            selected.get(),
            Some(3),
            "quick chord skips the visual delay"
        );
        pump(100);
        assert!(
            !picker.layer.is_visible(),
            "selection invalidates the pending preview"
        );
        picker.reset();
        picker.press(gdk::Key::Alt_L, LALT, none);
        assert_eq!(picker.press(gdk::Key::f, KEY_F, alt), glib::Propagation::Proceed);
        pump(100);
        assert!(
            !picker.layer.is_visible(),
            "normal Alt shortcuts keep working"
        );
        picker.reset();
        picker.press(gdk::Key::Alt_L, LALT, none);
        picker.release(LALT);
        pump(100);
        assert!(
            !picker.layer.is_visible(),
            "short Alt tap must not flash later"
        );
        // Letting go of Alt takes the digits down. With `grp:alts_toggle` a
        // held Alt repeats and releases as ISO_Prev_Group, so only the
        // keycode identifies it.
        picker.reset();
        picker.press(gdk::Key::Alt_L, LALT, none);
        pump(100);
        assert!(picker.layer.is_visible());
        assert_eq!(
            picker.press(gdk::Key::ISO_Prev_Group, LALT, alt),
            glib::Propagation::Proceed
        );
        assert!(picker.layer.is_visible(), "a translated repeat is still Alt");
        picker.release(KEY_3);
        assert!(picker.layer.is_visible(), "another key's release is ignored");
        picker.release(LALT);
        assert!(!picker.layer.is_visible(), "releasing Alt hides the digits");
        assert!(!zero.has_css_class("terminal-picker-ghost"));
        assert!(!three.has_css_class("terminal-picker-ghost"));
        pump(100);
        assert!(!picker.layer.is_visible(), "a release is final");
        // With both Alt keys down, the digits stay until the last one is up.
        picker.press(gdk::Key::Alt_L, LALT, none);
        picker.press(gdk::Key::Alt_R, RALT, alt);
        pump(100);
        assert!(picker.layer.is_visible());
        picker.release(LALT);
        assert!(picker.layer.is_visible());
        picker.release(RALT);
        assert!(!picker.layer.is_visible());
        picker.press(gdk::Key::Alt_L, LALT, none);
        pump(100);
        window.set_visible(false);
        assert!(!zero.has_css_class("terminal-picker-ghost"));
        assert!(!picker.layer.is_visible());
        assert!(picker.hold.borrow().keys.is_empty());
        // Every preview was reported once up and once down, however many
        // cancels followed it.
        let reports = reports.borrow();
        assert!(reports.len() > 2 && reports.len() % 2 == 0, "{reports:?}");
        assert!(
            reports.chunks(2).all(|pair| pair == [true, false]),
            "{reports:?}"
        );
        window.close();
    }

    #[test]
    fn hold_delay_repeat_release_and_cancel() {
        assert_eq!(HOLD_DELAY, Duration::from_millis(30));
        let mut hold = Hold::default();
        let first = hold.press(LALT).unwrap();
        assert!(hold.ready(first));
        assert_eq!(hold.press(LALT), None);
        assert_eq!(hold.press(RALT), None);
        assert!(!hold.release(KEY_3), "only a held Alt keycode releases");
        assert!(!hold.release(LALT));
        assert!(hold.ready(first));
        assert!(hold.release(RALT));
        assert!(!hold.ready(first));
        assert!(!hold.release(RALT), "a second release is not another end");
        let next = hold.press(LALT).unwrap();
        assert!(!hold.ready(first));
        hold.cancel();
        assert!(!hold.ready(next));
    }

    #[test]
    fn assignments_survive_reordering_and_reuse_only_free_slots() {
        assert_eq!(next_digit(std::iter::empty()), Some(0));
        assert_eq!(next_digit([2, 0, 1].into_iter()), Some(3));
        assert_eq!(next_digit((0..10).filter(|digit| *digit != 3)), Some(3));
        assert_eq!(next_digit(0..10), None);
    }

    #[test]
    fn digits_are_zero_based_and_capped_at_ten() {
        for (key, index) in [
            (gdk::Key::_0, 0),
            (gdk::Key::_3, 3),
            (gdk::Key::_9, 9),
            (gdk::Key::KP_3, 3),
        ] {
            assert_eq!(digit_index(key), Some(index));
        }
        assert_eq!(digit_index(gdk::Key::KP_0), Some(0));
        assert_eq!(digit_index(gdk::Key::a), None);
    }
}
