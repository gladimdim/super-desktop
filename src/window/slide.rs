use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::window) struct Trajectory {
    pub(in crate::window) sx: f64,
    pub(in crate::window) sy: f64,
    pub(in crate::window) tx: f64,
    pub(in crate::window) ty: f64,
}

/// Critically-damped spring (rad/s). With the launch impulse the cards are
/// within 1% of the target after about 6.5/ω, and `spring_settled` reports
/// rest after about 9/ω.
/// A toggle must feel done within 100ms: the slide-in is 99% there at
/// ~92ms on a 240 Hz panel (~100ms at 60 Hz) and reports rest at ~150ms
/// (ω=22 took ~300ms, and ~400ms to settle).
pub(in crate::window) const SLIDE_OMEGA_IN: f64 = 70.0;
/// Hide is stiffer so a reverse during appear still clears the screen
/// promptly: 99% off-screen at ~83ms, before `HIDE_FALLBACK` unmaps.
pub(in crate::window) const SLIDE_OMEGA_OUT: f64 = 80.0;
/// Initial speed (progress / second) when starting from rest. A spring at v=0
/// eases in; this impulse makes the first frames shoot in from the edge.
/// About a quarter of ω, as before: fast off the edge, no overshoot.
pub(in crate::window) const SLIDE_LAUNCH_IN: f64 = 17.5;
pub(in crate::window) const SLIDE_LAUNCH_OUT: f64 = 21.5;
pub(in crate::window) const SLIDE_SETTLE_X: f64 = 0.0015;
pub(in crate::window) const SLIDE_SETTLE_V: f64 = 0.025;
/// Extra pixels past the HUD's own height so it is fully off-screen at progress 0.
pub(in crate::window) const HUD_OFFSCREEN_PAD: f64 = 40.0;
/// Fallback HUD height before GTK has allocated it, so the first frame still
/// starts above the screen instead of at rest.
pub(in crate::window) const HUD_MIN_HEIGHT: i32 = 56;

/// Bidirectional slide: `progress` 0 = off-screen edge, 1 = resting on canvas.
/// Motion is a critically damped spring sampled on the GTK frame clock (the
/// Wayland surface's vsync: 60–240 Hz). Reversing only changes the target;
/// position and velocity stay, so there is no jump.
pub(in crate::window) struct SlideAnim {
    /// Bumped when a new tick callback is armed so a stale one exits.
    pub(in crate::window) gen: Cell<u64>,
    pub(in crate::window) progress: Cell<f64>,
    pub(in crate::window) velocity: Cell<f64>,
    pub(in crate::window) appear: Cell<bool>,
    pub(in crate::window) running: Cell<bool>,
    /// Previous `FrameClock::frame_time` (µs). 0 = first frame of this run.
    pub(in crate::window) last_us: Cell<i64>,
    /// Opt-in vsync accounting for the current run (`frame_profile`).
    pub(in crate::window) motion: RefCell<crate::frame_profile::Motion>,
    /// Some cards still slide with a live terminal: replace it with a still
    /// image once it has been laid out (`MiniTerminalCard::freeze_for_slide`).
    pub(in crate::window) freeze_pending: Cell<bool>,
    /// The workspace size the paths in `anim_trajectories` were built for.
    /// The tick rebuilds them when the window's real size differs.
    pub(in crate::window) built_for: Cell<(i32, i32)>,
}

impl SlideAnim {
    pub(in crate::window) fn new() -> Self {
        Self {
            gen: Cell::new(0),
            progress: Cell::new(0.0),
            velocity: Cell::new(0.0),
            appear: Cell::new(true),
            running: Cell::new(false),
            last_us: Cell::new(0),
            motion: RefCell::default(),
            freeze_pending: Cell::new(false),
            built_for: Cell::new((0, 0)),
        }
    }
}

/// Exact step of a critically damped spring (`ζ = 1`) toward `target`.
///
/// Analytical, not Euler: a missed vsync (large `dt`) cannot overshoot or
/// explode, and reversing mid-flight only changes `target`.
pub(in crate::window) fn spring_step(x: f64, v: f64, target: f64, dt: f64, omega: f64) -> (f64, f64) {
    if dt <= 0.0 || omega <= 0.0 {
        return (x, v);
    }
    let a = x - target;
    let b = v + omega * a;
    let e = (-omega * dt).exp();
    let a2 = (a + b * dt) * e;
    let v2 = (b - omega * (a + b * dt)) * e;
    (target + a2, v2)
}

pub(in crate::window) fn spring_settled(x: f64, v: f64, target: f64) -> bool {
    (x - target).abs() < SLIDE_SETTLE_X && v.abs() < SLIDE_SETTLE_V
}


impl SuperDesktopWindow {
    pub fn start_slide_in(&self) {
        // Outlines are placed from rest geometry: a ghost must not hang in
        // mid-air while the cards are still on their way in.
        self.ghosts.suspend();
        self.ensure_slide_trajectories(!self.slide.running.get());
        self.add_remote_slide_targets();
        if !self.slide.running.get() {
            // Idle and hidden: park cards at the edge so the appear starts
            // off-screen. Mid-flight reverse must not do this — it would jump.
            self.slide.progress.set(0.0);
            self.slide.velocity.set(SLIDE_LAUNCH_IN);
            self.paint_slide(0.0);
        }
        self.slide.appear.set(true);
        // Terminals that slide in live were just shown again and have no
        // layout yet: the slide's tick freezes them from its second frame.
        self.slide.freeze_pending.set(true);
        self.canvas.add_css_class("sliding");
        self.ensure_slide_tick();
    }

    pub fn start_slide_out<F: Fn() + 'static>(&self, on_finish: F) {
        if let Some(picker) = self.terminal_picker.borrow().as_ref() { picker.cancel(); }
        self.machine_view.dismiss();
        self.release_input();
        // Outlines describe rest positions, so they go away with the cards.
        self.ghosts.suspend();
        // Live terminals slide out with their content, so hide reads as one
        // motion instead of a blank frame followed by an unmap. Their GPU
        // surfaces are dropped in `hide_now`, at the unmap itself: with VRAM
        // exhausted (a 27B local model on a 16 GB card) moving those buffers can
        // stall the compositor, and `HIDE_FALLBACK` unmaps regardless. tmux
        // clients stay attached for the next show.
        // From rest, start from where the cards are now: a card moved, resized,
        // arranged or opened since the show must not jump back to its old spot
        // and leave by the side that was nearer then. Only a reversed slide-in
        // keeps its paths.
        self.ensure_slide_trajectories(!self.slide.running.get());
        // A remote PC's cards are created while that PC is viewed, so they are
        // added to the slide at each slide start rather than at window build.
        self.add_remote_slide_targets();
        *self.on_slide_hidden.borrow_mut() = Some(Rc::new(on_finish));
        if !self.slide.running.get() {
            // Idle and shown: start from rest with an outward impulse.
            // Mid-flight reverse keeps current progress *and* velocity.
            self.slide.progress.set(1.0);
            self.slide.velocity.set(-SLIDE_LAUNCH_OUT);
        }
        self.slide.appear.set(false);
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        self.slide.freeze_pending.set(!freeze_slide_cards(&cards));
        self.canvas.add_css_class("sliding");
        self.ensure_slide_tick();
    }

    /// Add the remote PC's cards to the current slide.
    ///
    /// They are pruned when their card is gone, and their rest pose is inside
    /// the fitted canvas, so the off-screen pose is the nearest edge of the
    /// viewer's real screen — the same rule local cards use.
    pub(in crate::window) fn add_remote_slide_targets(&self) {
        let mut trajectories = self.anim_trajectories.borrow_mut();
        trajectories.retain(|widget, _| widget.parent().is_some());
        let (sw, _) = self.slide_size();
        for (widget, x, y, width, offset) in self.machine_view.slide_cards() {
            let (sx, sy) = card_slide_offscreen(x + offset, y, width, sw as f64);
            trajectories.insert(
                widget,
                Trajectory {
                    sx: sx - offset,
                    sy,
                    tx: x,
                    ty: y,
                },
            );
        }
    }

    /// Fill rest/edge positions. `reset` rebuilds them (fresh show). A reverse
    /// keeps the existing pair so the cards stay on the same path.
    pub(in crate::window) fn ensure_slide_trajectories(&self, reset: bool) {
        let mut trajs = self.anim_trajectories.borrow_mut();
        if !reset && !trajs.is_empty() {
            return;
        }
        trajs.clear();
        let (sw, sh) = self.slide_size();
        self.slide.built_for.set((sw, sh));

        let notes: Vec<Rc<StickyNote>> = self.note_cards.borrow().clone();
        let terms: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();

        for note in notes.iter() {
            let tx = note.data.borrow().x as f64;
            let ty = note.data.borrow().y as f64;
            let w = note.data.borrow().width as f64;
            let (sx, sy) = card_slide_offscreen(tx, ty, w, sw as f64);
            trajs.insert(note.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
        }
        for term in terms.iter() {
            let (tx, ty, w, _) = terminal_slide_geom(term, sw, sh);
            let (sx, sy) = card_slide_offscreen(tx, ty, w, sw as f64);
            trajs.insert(term.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
        }
        for card in canvas_cards() {
            let g = card.geometry();
            let (tx, ty) = (f64::from(g.x), f64::from(g.y));
            let (sx, sy) = card_slide_offscreen(tx, ty, f64::from(g.width), sw as f64);
            trajs.insert(card.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
        }

        let (tx, ty, sx, sy) = hud_slide_pose(hud_measured_height(&self.hud));
        trajs.insert(self.hud.clone().upcast(), Trajectory { sx, sy, tx, ty });
        raise_canvas_child(&self.canvas, &self.hud);
    }

    /// The workspace size the slide paths are built for. A window shown again
    /// has no size until its first frame is laid out (GTK clears it on hide),
    /// so until then the paths are provisional and the tick rebuilds them.
    pub(in crate::window) fn slide_size(&self) -> (i32, i32) {
        allocated_size(&self.window)
            .unwrap_or_else(|| provisional_slide_size((self.screen_width, self.screen_height)))
    }

    /// The paths of every sliding widget, rebuilt for the current size.
    pub(in crate::window) fn rebuild_slide_frames(&self) -> Vec<(gtk4::Widget, Trajectory)> {
        self.ensure_slide_trajectories(true);
        self.add_remote_slide_targets();
        self.slide_frames()
    }

    pub(in crate::window) fn slide_frames(&self) -> Vec<(gtk4::Widget, Trajectory)> {
        self.anim_trajectories
            .borrow()
            .iter()
            .map(|(w, t)| (w.clone(), *t))
            .collect()
    }

    pub(in crate::window) fn paint_slide(&self, progress: f64) {
        let trajs = self.anim_trajectories.borrow();
        for (widget, traj) in trajs.iter() {
            paint_slide_widget(widget, *traj, progress);
        }
        // Panels, plugin docks and full screen, which the overlay places.
        crate::overlay_slide::paint(progress);
    }

    pub(in crate::window) fn ensure_slide_tick(&self) {
        if self.slide.running.get() {
            // Already in flight: the next vsync reads the flipped target and
            // the spring turns around from the current velocity.
            return;
        }
        self.slide.running.set(true);
        self.slide.last_us.set(0);
        self.slide.motion.take();
        let gen = self.slide.gen.get().wrapping_add(1);
        self.slide.gen.set(gen);

        let frames = RefCell::new(self.slide_frames());
        let this = self.this.get().cloned();
        let slide = Rc::clone(&self.slide);
        let canvas = self.canvas.clone();
        let on_hidden = Rc::clone(&self.on_slide_hidden);
        let ghosts = Rc::clone(&self.ghosts);
        // The cards that slide: one created meanwhile is not frozen.
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();

        // Tick the window, not the canvas: the layer-shell surface owns the
        // GDK frame clock, which Hyprland drives at the monitor refresh rate.
        // `add_tick_callback` is vsync; a glib timeout would cap us at 10–16ms.
        self.window.add_tick_callback(move |window, clock| {
            if slide.gen.get() != gen {
                return glib::ControlFlow::Break;
            }
            // A show starts before the window has a size: its first frame used
            // provisional paths that keep every card off screen. Rebuild them
            // for the real size as soon as it is known (and after an output
            // change mid-slide), so each card enters from its own nearest edge.
            if allocated_size(window).is_some_and(|size| size != slide.built_for.get()) {
                if let Some(win) = this.as_ref().and_then(std::rc::Weak::upgrade) {
                    *frames.borrow_mut() = win.rebuild_slide_frames();
                }
            }
            slide.motion.borrow_mut().frame(clock);
            let now = clock.frame_time();
            let prev = slide.last_us.get();
            slide.last_us.set(now);
            // First frame: paint the current pose, do not fake a 1/240s step
            // that would hitch on a 60 Hz panel or skip on a 240 Hz one.
            let dt = if prev == 0 {
                0.0
            } else {
                ((now - prev) as f64 / 1_000_000.0).clamp(0.0, 0.05)
            };
            if prev != 0 && slide.freeze_pending.get() {
                slide.freeze_pending.set(!freeze_slide_cards(&cards));
            }
            let appear = slide.appear.get();
            let target = if appear { 1.0 } else { 0.0 };
            let omega = if appear { SLIDE_OMEGA_IN } else { SLIDE_OMEGA_OUT };
            let (progress, velocity) = spring_step(
                slide.progress.get(),
                slide.velocity.get(),
                target,
                dt,
                omega,
            );
            if spring_settled(progress, velocity, target) {
                slide.progress.set(target);
                slide.velocity.set(0.0);
                slide.running.set(false);
                for (widget, traj) in frames.borrow().iter() {
                    paint_slide_widget(widget, *traj, target);
                }
                crate::overlay_slide::paint(target);
                canvas.remove_css_class("sliding");
                slide.freeze_pending.set(false);
                thaw_slide_cards(&cards);
                slide.motion.take().report(if appear { "slide in" } else { "slide out" });
                if appear {
                    // The cards are at rest again: the buried ones can have
                    // their outlines back.
                    ghosts.resume();
                } else if let Some(cb) = on_hidden.borrow().clone() {
                    cb();
                }
                return glib::ControlFlow::Break;
            }
            slide.progress.set(progress);
            slide.velocity.set(velocity);
            for (widget, traj) in frames.borrow().iter() {
                paint_slide_widget(widget, *traj, progress);
            }
            crate::overlay_slide::paint(progress);
            glib::ControlFlow::Continue
        });
    }

}

/// Freeze every card's terminal for the slide; `false` while one still needs
/// a layout first (see `MiniTerminalCard::freeze_for_slide`). Takes a snapshot
/// of the card list: hiding a widget can emit signals that re-enter it.
pub(in crate::window) fn freeze_slide_cards(cards: &[Rc<MiniTerminalCard>]) -> bool {
    let started = std::time::Instant::now();
    let done = cards.iter().fold(true, |done, card| card.freeze_for_slide() && done);
    crate::frame_profile::note_duration("slide freeze", started);
    done
}

pub(in crate::window) fn thaw_slide_cards(cards: &[Rc<MiniTerminalCard>]) {
    for card in cards {
        card.thaw_after_slide();
    }
}

/// Move one sliding widget between its off-screen and rest poses.
///
/// A card lives either in the local canvas or in a remote PC's fitted canvas,
/// so the widget moves inside whichever `Fixed` holds it.
/// The cards on the canvas that are not terminals or notes: plugin cards and
/// SD Editor cards. They arrange, slide and follow the display the same way.
pub(in crate::window) fn canvas_cards() -> Vec<Rc<crate::plugin_ui::cards::PluginCard>> {
    let mut cards = crate::plugin_ui::slots::get().map(|s| s.cards()).unwrap_or_default();
    cards.extend(crate::editor::cards());
    cards
}

pub(in crate::window) fn paint_slide_widget(widget: &gtk4::Widget, traj: Trajectory, factor: f64) {
    let Some(parent) = widget
        .parent()
        .and_then(|parent| parent.downcast::<Fixed>().ok())
    else {
        return;
    };
    let cx = traj.sx + (traj.tx - traj.sx) * factor;
    let cy = traj.sy + (traj.ty - traj.sy) * factor;
    parent.move_(widget, cx, cy);
}

/// Off-screen pose for a card or icon: nearest of the left or right edge,
/// same Y. Corner (top-left, bottom-right, …) only decides which side is
/// nearer — cards never slide up or down.
pub(in crate::window) fn card_slide_offscreen(tx: f64, ty: f64, w: f64, screen_w: f64) -> (f64, f64) {
    (crate::overlay_slide::offscreen_x(tx, w, screen_w), ty)
}


pub(in crate::window) fn hud_measured_height(hud: &gtk4::Box) -> f64 {
    let (_, nat_h, _, _) = hud.measure(Orientation::Vertical, -1);
    hud.height().max(nat_h).max(HUD_MIN_HEIGHT) as f64
}

/// Rest pose (flush with the top-left edge) and off-screen pose (same X, above
/// the overlay) for the toolbar as a single translated widget.
pub(in crate::window) fn hud_slide_pose(hud_h: f64) -> (f64, f64, f64, f64) {
    let tx = 0.0;
    let ty = 0.0;
    let sy = -hud_h - HUD_OFFSCREEN_PAD;
    (tx, ty, tx, sy)
}

pub(in crate::window) fn terminal_slide_geom(term: &MiniTerminalCard, sw: i32, sh: i32) -> (f64, f64, f64, f64) {
    let rect = term.canvas_rect(sw, sh);
    (rect.x, rect.y, rect.width as f64, rect.height as f64)
}

