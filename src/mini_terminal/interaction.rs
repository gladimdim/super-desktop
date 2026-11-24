use super::*;

/// Newly opened harnesses ignore hover-raise on other cards for this long
/// so the pointer can travel to the new card without burying it.
pub const NEW_HARNESS_HOVER_LOCK: Duration = Duration::from_secs(4);
/// Keep a resize release over another card from immediately switching terminals.
pub(in crate::mini_terminal) const RESIZE_HOVER_LOCK: Duration = Duration::from_secs(1);

/// Shared across every terminal card. After `lock()`, pointer-enter on any
/// other card skips raise/focus until the hold expires or a click on another
/// card calls `on_click` (clicks stay an intentional switch).
#[derive(Clone)]
pub struct HoverRaiseLock {
    pub(in crate::mini_terminal) until: Rc<Cell<Option<Instant>>>,
    pub(in crate::mini_terminal) owner: Rc<RefCell<Option<String>>>,
    pub(in crate::mini_terminal) hover_owner: Rc<RefCell<Option<(String, glib::WeakRef<gtk4::Widget>)>>>,
}

impl HoverRaiseLock {
    pub fn new() -> Self {
        Self {
            until: Rc::new(Cell::new(None)),
            owner: Rc::new(RefCell::new(None)),
            hover_owner: Rc::new(RefCell::new(None)),
        }
    }

    pub fn lock(&self, session_name: &str) {
        self.lock_for(session_name, NEW_HARNESS_HOVER_LOCK);
    }

    pub fn lock_for(&self, session_name: &str, duration: Duration) {
        self.until.set(Some(Instant::now() + duration));
        *self.owner.borrow_mut() = Some(session_name.to_string());
    }

    pub fn release(&self) {
        self.until.set(None);
        self.owner.borrow_mut().take();
    }

    pub(in crate::mini_terminal) fn active_owner(&self) -> Option<String> {
        let until = self.until.get()?;
        if Instant::now() >= until {
            self.release();
            return None;
        }
        self.owner.borrow().clone()
    }

    /// True when this card may raise and take focus from a pointer enter.
    pub fn allows_hover(&self, session_name: &str) -> bool {
        match self.active_owner() {
            None => true,
            Some(owner) => owner == session_name,
        }
    }

    pub(in crate::mini_terminal) fn note_hover(&self, session: &str, widget: &impl IsA<gtk4::Widget>) {
        *self.hover_owner.borrow_mut() = Some((session.to_owned(), widget.as_ref().downgrade()));
    }

    pub(in crate::mini_terminal) fn allows_hover_at(&self, session: &str, source: &impl IsA<gtk4::Widget>, x: f64, y: f64) -> bool {
        if !self.allows_hover(session) {
            return false;
        }
        let owner = self.hover_owner.borrow();
        let Some((name, weak)) = owner.as_ref() else { return true; };
        if name == session { return true; }
        let Some(widget) = weak.upgrade().filter(|widget| widget.is_mapped()) else { return true; };
        // Translate against the live allocation, so dragging, resizing and output
        // scaling never leave a stale exclusion rectangle behind.
        let Some(point) = source.as_ref().compute_point(&widget, &gtk4::graphene::Point::new(x as f32, y as f32)) else { return true; };
        !inside_hover_margin(point.x(), point.y(), widget.width(), widget.height())
    }

    /// A click on a different card is an intentional switch: drop the hold.
    pub fn on_click(&self, session_name: &str) {
        if self.hover_owner.borrow().as_ref().is_some_and(|(owner, _)| owner != session_name) {
            self.hover_owner.borrow_mut().take();
        }
        if let Some(owner) = self.active_owner() {
            if owner != session_name {
                self.release();
            }
        }
    }
}

/// How long a card stays "in use" after the last sign of the user working in it
/// (a keystroke, a launch, an expand).
pub const ACTIVE_FOR: Duration = Duration::from_secs(3);

/// Why a card counts as the one the user is working in, plus the hook that
/// tells the overlay when that changed.
///
/// GTK keyboard focus cannot answer this question in this overlay: hovering a
/// card raises it *and* grabs focus, nothing releases that focus when the
/// pointer moves on, and a launched or expanded card holds it until something
/// else claims it. "Focused" therefore meant "active forever", and the dotted
/// outlines of the cards it covered never came back. Both signals here expire
/// on their own instead — the pointer immediately, keystrokes and deliberate
/// launches/expands after [`ACTIVE_FOR`] (the overlay's 1s refresh re-reads
/// that).
pub struct CardActivity {
    pub(in crate::mini_terminal) pointer_inside: Cell<bool>,
    pub(in crate::mini_terminal) last_activity: Cell<Option<Instant>>,
    pub(in crate::mini_terminal) on_change: Rc<dyn Fn()>,
}

impl CardActivity {
    pub fn new(on_change: Rc<dyn Fn()>) -> Self {
        Self {
            pointer_inside: Cell::new(false),
            last_activity: Cell::new(None),
            on_change,
        }
    }

    /// True while the user is working in this card.
    pub fn is_active(&self, now: Instant) -> bool {
        self.pointer_inside.get()
            || self
                .last_activity
                .get()
                .is_some_and(|at| now.saturating_duration_since(at) < ACTIVE_FOR)
    }

    /// The pointer entered or left the card. Notifies on a real change only, so
    /// a pointer that keeps crossing cards does not redraw the outlines twice.
    pub fn set_pointer_inside(&self, inside: bool) {
        if self.pointer_inside.replace(inside) != inside {
            (self.on_change)();
        }
    }

    /// The user did something with this card: a keystroke meant for it, a
    /// launch, an expand. It is active now, and stays so while that keeps
    /// happening.
    ///
    /// Notifies when this is what made it active again (the first sign after a
    /// pause), not on every repeat: the outlines are already in the right state.
    pub fn note_activity(&self, now: Instant) {
        if !self.is_active(now) {
            (self.on_change)();
        }
        self.last_activity.set(Some(now));
    }
}


/// Where a dragged card's origin is: under the pointer at the offset it was
/// grabbed by, or the drag's start moved by the drag's offset when the event
/// has no position.
pub(crate) fn dragged_origin(
    gesture: &GestureDrag,
    grab_offset: Option<(f64, f64)>,
    start: (f64, f64),
    (offset_x, offset_y): (f64, f64),
) -> (f64, f64) {
    match (grab_offset, gesture.current_event().and_then(|e| e.position())) {
        (Some((gx, gy)), Some((mx, my))) => (mx - gx, my - gy),
        _ => (start.0 + offset_x, start.1 + offset_y),
    }
}

/// Where a card's widget sits on its canvas now, rounded to whole pixels.
pub(crate) fn placed_origin(widget: &gtk4::Widget) -> Option<(i32, i32)> {
    let canvas = widget.parent()?.downcast::<gtk4::Fixed>().ok()?;
    // Read the move's translation: `child_position` reports 0,0 on GTK 4.24.
    let (x, y) = canvas.child_transform(widget).map_or((0.0, 0.0), |t| t.to_translate());
    Some((x.round() as i32, y.round() as i32))
}

/// What a card's move drags act on: its saved and drawn position, and the
/// overlay's drag callbacks.
#[derive(Clone)]
pub(in crate::mini_terminal) struct MoveDrag {
    pub(in crate::mini_terminal) root: glib::WeakRef<Overlay>,
    pub(in crate::mini_terminal) data: Rc<RefCell<TerminalData>>,
    pub(in crate::mini_terminal) expanded: Rc<RefCell<bool>>,
    pub(in crate::mini_terminal) visual_pos: Rc<RefCell<(f64, f64)>>,
    pub(in crate::mini_terminal) on_update: Rc<dyn Fn(gtk4::Widget, f64, f64)>,
    pub(in crate::mini_terminal) on_end: Rc<dyn Fn(gtk4::Widget, &TerminalData)>,
    pub(in crate::mini_terminal) on_raise: Rc<dyn Fn(gtk4::Widget)>,
}

impl MoveDrag {
    /// Move the card by dragging `source`, while the card is iconified
    /// (`iconified_only`) or while it is not.
    pub(in crate::mini_terminal) fn attach(&self, source: &impl IsA<gtk4::Widget>, iconified_only: bool) {
        let drag = GestureDrag::new();
        let start_pos = Rc::new(Cell::new((0.0, 0.0)));
        let grab_offset: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

        let this = self.clone();
        let start_pos_begin = Rc::clone(&start_pos);
        let grab_offset_begin = Rc::clone(&grab_offset);
        drag.connect_drag_begin(move |gesture, _, _| {
            if this.data.borrow().iconified != iconified_only {
                return;
            }
            if let Some(r) = this.root.upgrade() {
                (this.on_raise)(r.upcast());
            }
            let (init_x, init_y) = if *this.expanded.borrow() {
                *this.visual_pos.borrow()
            } else {
                // Drag from wherever the card is actually drawn (icon spot when
                // minimized), otherwise the icon would jump on first motion.
                displayed_pos(&this.data.borrow())
            };
            start_pos_begin.set((init_x, init_y));
            grab_offset_begin.set(
                gesture
                    .current_event()
                    .and_then(|e| e.position())
                    .map(|(mx, my)| (mx - init_x, my - init_y)),
            );
        });

        let this = self.clone();
        let start_pos_update = Rc::clone(&start_pos);
        let grab_offset_update = Rc::clone(&grab_offset);
        drag.connect_drag_update(move |gesture, offset_x, offset_y| {
            if this.data.borrow().iconified != iconified_only || *this.expanded.borrow() {
                return;
            }
            if offset_x.abs() > 2.0 || offset_y.abs() > 2.0 {
                gesture.set_state(gtk4::EventSequenceState::Claimed);
            }
            if let Some(c) = this.root.upgrade() {
                let (nx, ny) = dragged_origin(
                    gesture,
                    grab_offset_update.get(),
                    start_pos_update.get(),
                    (offset_x, offset_y),
                );
                *this.visual_pos.borrow_mut() = (nx, ny);
                set_displayed_pos(&mut this.data.borrow_mut(), nx.round() as i32, ny.round() as i32);
                (this.on_update)(c.upcast(), nx, ny);
            }
        });

        let this = self.clone();
        drag.connect_drag_end(move |gesture, offset_x, offset_y| {
            if this.data.borrow().iconified != iconified_only || *this.expanded.borrow() {
                return;
            }
            if let Some(c) = this.root.upgrade() {
                let (nx, ny) =
                    dragged_origin(gesture, grab_offset.get(), start_pos.get(), (offset_x, offset_y));
                let (nx, ny) = (nx.round() as i32, ny.round() as i32);
                set_displayed_pos(&mut this.data.borrow_mut(), nx, ny);
                *this.visual_pos.borrow_mut() = (nx as f64, ny as f64);
                (this.on_end)(c.clone().upcast(), &this.data.borrow());
                // The drop handler may have clamped the card below the top bar:
                // keep where it put the card, or a later slide-in restores the
                // unclamped spot.
                if let Some((x, y)) = placed_origin(c.upcast_ref()) {
                    set_displayed_pos(&mut this.data.borrow_mut(), x, y);
                    *this.visual_pos.borrow_mut() = (f64::from(x), f64::from(y));
                }
            }
        });

        source.add_controller(drag);
    }
}

