use super::*;


/// Whether a card's terminal widget draws.
///
/// Two independent reasons stop it: the overlay is unmapped (the whole-window
/// hide), or the cards painted above it hide it completely (see
/// `overlap_ghost::hidden_indexes`). It draws only while neither holds, so a
/// show cannot bring back a buried terminal, and an uncover cannot draw into a
/// hidden overlay.
///
/// Stopping only hides the widget. The emulator, its PTY and the tmux client
/// stay; it keeps reading output and is not reallocated while hidden, so tmux
/// sees no resize, and it paints its current screen as soon as it draws again.
///
/// While the overlay slides, the terminal is also replaced by a still image of
/// its screen (`frozen`, see [`MiniTerminalCard::freeze_for_slide`]).
pub struct VteDrawing {
    pub(in crate::mini_terminal) mapped: Cell<bool>,
    pub(in crate::mini_terminal) covered: Cell<bool>,
    pub(in crate::mini_terminal) frozen: Cell<bool>,
}

impl VteDrawing {
    pub fn new() -> Self {
        Self {
            mapped: Cell::new(true),
            covered: Cell::new(false),
            frozen: Cell::new(false),
        }
    }

    /// The overlay was mapped or unmapped.
    pub fn set_mapped(&self, mapped: bool, term: Option<&VteTerminal>) {
        self.mapped.set(mapped);
        self.apply(term);
    }

    /// The cards above hide this terminal completely, or no longer do.
    pub fn set_covered(&self, covered: bool, term: Option<&VteTerminal>) {
        self.covered.set(covered);
        self.apply(term);
    }

    /// A still image stands in for the terminal (`frame`), or no longer does.
    pub fn set_frozen(&self, frozen: bool, term: Option<&VteTerminal>, frame: &gtk4::Picture) {
        self.frozen.set(frozen);
        // The image first, so the box never lays out with neither child.
        frame.set_visible(frozen);
        self.apply(term);
    }

    pub fn frozen(&self) -> bool {
        self.frozen.get()
    }

    pub fn drawing(&self) -> bool {
        self.mapped.get() && !self.covered.get() && !self.frozen.get()
    }

    /// Bring the widget in line with both reasons. The widget is compared, not
    /// the last value set: a terminal re-created by a restore starts visible.
    /// Its own flag (`get_visible`), not `is_visible`, which is also false
    /// while the overlay window is hidden and would leave the flag set then.
    pub(in crate::mini_terminal) fn apply(&self, term: Option<&VteTerminal>) {
        let Some(term) = term else {
            return;
        };
        let drawing = self.drawing();
        if term.get_visible() != drawing {
            term.set_visible(drawing);
        }
    }
}


/// Apply the active Omarchy theme and a font size to a VTE terminal.
///
/// One source of truth for terminal rendering: a local card and the remote live
/// view must paint the host's bytes identically.
pub fn apply_vte_theme(term: &VteTerminal, font_size: f64) {
    let theme = crate::theme::current_theme();
    term.set_font(Some(&vte_font(&theme.font_family, font_size)));
    apply_vte_colors(term, &theme);
}

/// A terminal font: the theme's family at a size in points. Fractional sizes
/// are supported because a remote card fits the host's grid into the viewer's
/// own screen.
pub fn vte_font(family: &str, font_size: f64) -> gtk4::pango::FontDescription {
    let mut font = gtk4::pango::FontDescription::from_string(family);
    font.set_size((font_size * f64::from(gtk4::pango::SCALE)).round() as i32);
    font
}

/// A card's terminal font size: one point larger while it is expanded.
pub(in crate::mini_terminal) fn card_font_size(expanded: bool) -> f64 {
    if expanded { 11.0 } else { 10.0 }
}

/// Put the theme's font back on a card's terminal at its form's size.
pub(in crate::mini_terminal) fn set_card_font(term: &VteTerminal, expanded: bool) {
    let theme = crate::theme::current_theme();
    term.set_font(Some(&vte_font(&theme.font_family, card_font_size(expanded))));
}

/// A theme's colors on a VTE terminal: foreground, background, palette and
/// cursor.
pub fn apply_vte_colors(term: &VteTerminal, theme: &crate::theme::OmarchyTheme) {
    let fg = gdk::RGBA::parse(&theme.foreground).ok();
    let bg = gdk::RGBA::parse(&theme.darker_background).ok();
    let palette = theme.get_ansi_palette();
    let palette_refs: Vec<&gdk::RGBA> = palette.iter().collect();
    term.set_colors(fg.as_ref(), bg.as_ref(), &palette_refs);

    if let Ok(cursor) = gdk::RGBA::parse(&theme.accent) {
        term.set_color_cursor(Some(&cursor));
    }
}

/// Show `frame`, a texture of `term`'s screen rendered here once, in place of
/// the terminal (see `MiniTerminalCard::freeze_for_slide`). `false` when the
/// terminal has no layout yet.
pub(in crate::mini_terminal) fn freeze_terminal(term: Option<&VteTerminal>, drawing: &VteDrawing, frame: &gtk4::Picture) -> bool {
    if drawing.frozen() {
        return true;
    }
    let Some(term) = term else {
        return true;
    };
    if !term.get_visible() {
        // Not drawing (unmapped or covered): nothing to stand in for.
        return true;
    }
    if !term.is_drawable() || term.width() <= 0 || term.height() <= 0 {
        return false;
    }
    let (Some(renderer), Some(parent)) =
        (term.native().and_then(|native| native.renderer()), term.parent())
    else {
        return true;
    };
    // The terminal's own border box, so the image is drawn 1:1.
    let (Some(bounds), Some(origin)) = (
        term.compute_bounds(term),
        term.compute_point(&parent, &gtk4::graphene::Point::zero()),
    ) else {
        return false;
    };
    let (width, height) = (bounds.width(), bounds.height());
    let scale = term.scale_factor() as f32;
    let snapshot = gtk4::Snapshot::new();
    snapshot.scale(scale, scale);
    snapshot.translate(&gtk4::graphene::Point::new(-origin.x(), -origin.y()));
    // Through the parent, which renders the terminal's current screen. A
    // `WidgetPaintable` only replays the node of the last painted frame, and
    // GTK drops that node as soon as new output queues a redraw: a busy
    // terminal gave an empty image on almost every try.
    let started = std::time::Instant::now();
    parent.snapshot_child(term, &snapshot);
    let Some(node) = snapshot.to_node() else {
        return false;
    };
    crate::frame_profile::note_duration("slide freeze: terminal snapshot", started);
    // The border box (VTE's CSS padding lies outside its content origin):
    // the still image takes the whole allocation the terminal leaves.
    let viewport = gtk4::graphene::Rect::new(bounds.x() * scale, bounds.y() * scale, width * scale, height * scale);
    let started = std::time::Instant::now();
    let texture = renderer.render_texture(&node, Some(&viewport));
    crate::frame_profile::note_duration("slide freeze: render to image", started);
    frame.set_paintable(Some(&texture));
    drawing.set_frozen(true, Some(term), frame);
    true
}

pub(in crate::mini_terminal) fn thaw_terminal(term: Option<&VteTerminal>, drawing: &VteDrawing, frame: &gtk4::Picture) {
    if !drawing.frozen() {
        return;
    }
    drawing.set_frozen(false, term, frame);
    frame.set_paintable(None::<&gtk4::gdk::Paintable>);
}

/// A card's size, expanded and iconified state: an image taken at another
/// look would be stretched into this one.
pub(in crate::mini_terminal) type SlideLook = (i32, i32, bool, bool);

/// Tracks whether a slide's still image still shows its terminal: taken of
/// this emulator, at this look, with no output processed since.
#[derive(Default)]
pub struct SlideFrameWatch {
    pub(in crate::mini_terminal) source: RefCell<Option<SlideFrameSource>>,
}

pub(in crate::mini_terminal) struct SlideFrameSource {
    pub(in crate::mini_terminal) term: glib::WeakRef<VteTerminal>,
    pub(in crate::mini_terminal) handler: glib::SignalHandlerId,
    pub(in crate::mini_terminal) changed: Rc<Cell<bool>>,
    pub(in crate::mini_terminal) look: SlideLook,
}

impl SlideFrameWatch {
    /// An image of `term` was just taken.
    pub(in crate::mini_terminal) fn watch(&self, term: &VteTerminal, look: SlideLook) {
        self.forget();
        let changed = Rc::new(Cell::new(false));
        let flag = Rc::clone(&changed);
        let handler = term.connect_contents_changed(move |_| flag.set(true));
        *self.source.borrow_mut() = Some(SlideFrameSource { term: term.downgrade(), handler, changed, look });
    }

    pub(in crate::mini_terminal) fn forget(&self) {
        let source = self.source.borrow_mut().take();
        if let Some(source) = source {
            if let Some(term) = source.term.upgrade() {
                term.disconnect(source.handler);
            }
        }
    }

    pub(in crate::mini_terminal) fn still_shows(&self, term: Option<&VteTerminal>, look: SlideLook) -> bool {
        self.source.borrow().as_ref().is_some_and(|source| {
            !source.changed.get()
                && source.look == look
                && term.is_some_and(|term| source.term.upgrade().as_ref() == Some(term))
        })
    }
}

pub(in crate::mini_terminal) fn remove_vte(vte: &Rc<RefCell<Option<VteTerminal>>>, preview_box: &gtk4::Box) {
    let term = vte.borrow_mut().take();
    if let Some(term) = term {
        preview_box.remove(&term);
        crate::desktop_shell::terminal_removed(preview_box);
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::mini_terminal) fn spawn_vte(
    vte: &Rc<RefCell<Option<VteTerminal>>>,
    preview_box: &gtk4::Box,
    data: &Rc<RefCell<TerminalData>>,
    is_expanded: bool,
    on_toggle: &Rc<dyn Fn(&TerminalData)>,
    expanded_ref: &Rc<RefCell<bool>>,
    session_task: &Arc<crate::session_task::SessionTask>,
    inventory: Option<Arc<crate::tmux::SessionInventory>>,
    hover_lock: HoverRaiseLock,
    activity: &Rc<CardActivity>,
    remote: Option<Rc<RemoteSession>>,
    fit: &Rc<Cell<(f64, f64, f64)>>,
    font_fit: &RefCell<Option<crate::card_source::FontFit>>,
    prepare: bool,
) {
    if session_task.is_closed() { return; }
    remove_vte(vte, preview_box);

    let term = VteTerminal::new();
    if crate::frame_profile::enabled() {
        let created = Instant::now();
        let reported = Cell::new(false);
        term.connect_contents_changed(move |_| {
            if !reported.replace(true) {
                crate::frame_profile::note_terminal_content(created);
            }
        });
    }
    term.set_hexpand(true);
    term.set_vexpand(true);
    term.set_input_enabled(true);
    term.set_scroll_on_keystroke(true);
    term.set_scroll_on_output(true);
    term.set_scrollback_lines(5000);
    term.add_css_class("term-vte");
    term.set_can_focus(true);
    term.set_focusable(true);
    let shell_card = crate::shell_title::is_regular(&data.borrow().agent_type);
    crate::terminal_clipboard::install(&term, !shell_card);
    crate::terminal_links::install(&term);

    apply_vte_theme(&term, card_font_size(is_expanded));
    // The font of a remote card follows the host's own grid instead of this
    // machine's theme: the emulator must line up with the columns and rows the
    // host decided. See below, where the grid is known.

    let term_click = GestureClick::new();
    let term_weak = term.downgrade();
    let click_lock = hover_lock.clone();
    let click_session = data.borrow().session_name.clone();
    let click_activity = Rc::clone(activity);
    term_click.connect_pressed(move |_, _, _, _| {
        click_lock.on_click(&click_session);
        // Clicking a terminal is how the user says "I am working here": keep
        // the outlines of the cards it covers off the screen while that lasts
        // (see `CardActivity`), even if the pointer wanders off to type.
        click_activity.note_activity(Instant::now());
        if let Some(t) = term_weak.upgrade() {
            t.grab_focus();
        }
    });
    term.add_controller(term_click);

    // Keystrokes are how the overlay knows the user is working in *this* card
    // (see `CardActivity`): a terminal taking input keeps the dotted outlines
    // of the cards it covers off the screen until the typing stops. Capture
    // phase, and `Proceed`, so the terminal still receives every key.
    let keys_activity = Rc::clone(activity);
    let keys = EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    keys.connect_key_pressed(move |_, _, _, _| {
        keys_activity.note_activity(Instant::now());
        glib::Propagation::Proceed
    });
    term.add_controller(keys);

    let session = data.borrow().session_name.clone();
    let term_hover = gtk4::EventControllerMotion::new();
    let term_weak = term.downgrade();
    let hover_lock_vte = hover_lock.clone();
    let hover_session = session.clone();
    term_hover.connect_enter(move |_, x, y| {
        if let Some(t) = term_weak.upgrade() {
            if !hover_lock_vte.allows_hover_at(&hover_session, &t, x, y) { return; }
            if crate::sticky_note::note_has_focus(&t) { return; }
            if !t.has_focus() {
                t.grab_focus();
            }
        }
    });
    term.add_controller(term_hover);
    if let Some(session) = remote {
        // A remote card has no local process at all. The host's bytes are fed
        // into this emulator, and every committed keystroke goes to the host
        // session rather than to a PTY here.
        term.set_scrollback_lines(crate::card_source::REMOTE_SCROLLBACK);
        let input_session = Rc::clone(&session);
        crate::card_source::connect_host_input(&term, move |bytes| {
            input_session.input(bytes);
        });
        // Return never arrives as a commit on this PTY-less emulator: the input
        // method and the window's activate-default binding consume it, while
        // letters still come through `commit`. Catch it on the way in and send
        // the carriage return the host shell treats as "run this".
        let return_session = Rc::clone(&session);
        let keys = EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, keyval, _, state| {
            if !crate::card_source::is_submit_key(keyval, state) {
                return glib::Propagation::Proceed;
            }
            return_session.input(b"\r");
            glib::Propagation::Stop
        });
        term.add_controller(keys);
        // Size the font for the host's grid once this card has a body: until
        // then the theme font stands in and the view refits after layout.
        crate::card_source::pack_remote_emulator(&term);
        preview_box.append(&term);
        let (body_width, body_height, scale) = fit.get();
        if body_width > 1.0 && body_height > 1.0 {
            crate::card_source::fit_font(
                &term,
                body_width,
                body_height,
                session.grid(),
                scale,
                font_fit,
            );
        }
        *vte.borrow_mut() = Some(term);
        session.attach();
        return;
    }

    // Observe bytes VTE sends, not key labels or the response painted on screen.
    let agent_type = data.borrow().agent_type.clone();
    let prompt_session = session.clone();
    let typed_agent = agent_type.clone();
    let prompt_input = RefCell::new(crate::prompt_history::InputTracker::default());
    crate::card_source::connect_host_input(&term, move |bytes| {
        for prompt in prompt_input.borrow_mut().feed(bytes) {
            crate::prompt_history::record_typed(&typed_agent, &prompt_session, &prompt);
        }
    });

    let cmd = data.borrow().command.clone();
    let agent_session_id = data.borrow().agent_session_id.clone();
    // The folder this card was created in (the top bar's field at that time),
    // not the field's current value: every harness scopes its resume and its
    // history to the cwd, so a card restored elsewhere would come back
    // attached to a different project. Cards from before this existed have
    // `None` and keep the old behaviour ($HOME).
    let workspace_dir = data.borrow().workspace_dir.clone();

    // Display the widget immediately, but do not block GTK on tmux, filesystem
    // probes, or agent resume lookup. A weak widget + identity check prevents
    // a late completion attaching a removed/replaced VTE after minimize/close.
    let weak_term = term.downgrade();
    let weak_slot = Rc::downgrade(vte);
    let task = Arc::clone(session_task);
    glib::MainContext::default().spawn_local(async move {
        if task.is_closed() { return; }
        let prepare_task = Arc::clone(&task);
        let prepare_session = session.clone();
        let prepared = gtk4::gio::spawn_blocking(move || {
            let cwd = crate::tmux::resolve_workspace_dir(workspace_dir.as_deref());
            let ready = prepare_task.prepare(|| if prepare { ensure_session_with_inventory(
                &prepare_session, &agent_type, Some(&cmd), agent_session_id.as_deref(), Some(&cwd),
                inventory.as_deref(),
            ) });
            (ready, cwd, tmux_bin())
        }).await;
        let Ok((true, cwd, tmux)) = prepared else { return; };
        crate::startup::mark("terminal session prepared");
        if task.is_closed() { return; }
        let (Some(term), Some(slot)) = (weak_term.upgrade(), weak_slot.upgrade()) else { return; };
        if slot.borrow().as_ref() != Some(&term) { return; }
        // CLI mode changes attach only by exact saved name. attach-session
        // cannot create or respawn a missing session.
        let target = if prepare { session.clone() } else { format!("={session}") };
        let argv = [tmux.as_str(), "-2", "attach-session", "-t", target.as_str()];

        let mut env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
        // Attaching from inside a tmux client nests sessions and routes input
        // to the OUTER session. Unset these for launches from a terminal too.
        env_map.remove("TMUX");
        env_map.remove("TMUX_PANE");
        env_map.insert("TERM".to_string(), "xterm-256color".to_string());
        env_map.insert("COLORTERM".to_string(), "truecolor".to_string());
        let env: Vec<String> = env_map
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let env_refs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();

        term.spawn_async(
            PtyFlags::DEFAULT,
            Some(cwd.as_str()),
            &argv,
            &env_refs,
            glib::SpawnFlags::DEFAULT,
            || {},
            -1,
            None::<&gtk4::gio::Cancellable>,
            |result| {
                if let Err(err) = result {
                    eprintln!("SUPER DESKTOP: failed to attach tmux in overlay: {err}");
                } else {
                    crate::startup::mark("terminal attach process spawned");
                }
            },
        );
    });

    let expand_time = std::time::Instant::now();
    let on_toggle_child = Rc::clone(on_toggle);
    let data_child = Rc::clone(data);
    let expanded_child = Rc::clone(expanded_ref);
    term.connect_child_exited(move |_, status| {
        if *expanded_child.borrow() && expand_time.elapsed() > std::time::Duration::from_millis(1500) && status == 0 {
            on_toggle_child(&data_child.borrow());
        }
    });

    preview_box.append(&term);
    *vte.borrow_mut() = Some(term);
}

