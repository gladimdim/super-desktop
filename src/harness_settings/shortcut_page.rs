use super::*;

/// How long a recording may stay armed before it cancels itself. The recorder
/// holds the keyboard (see `shortcut::begin_capture`), so "the user clicked
/// Record and walked away" has to end on its own.
pub(in crate::harness_settings) const RECORD_WATCHDOG: Duration = Duration::from_millis(10_000);
/// The watchdog is checked by a 250ms tick, not by a timer of its own. The
/// tick runs only while a recording is armed.
pub(in crate::harness_settings) const RECORD_TICK: Duration = Duration::from_millis(250);

/// The one way out of a recording; `None` keeps the note as it is.
pub(in crate::harness_settings) type StopRecording = Rc<dyn Fn(Option<&str>)>;
/// Commits a captured combination together with its physical (X11) keycode.
pub(in crate::harness_settings) type CommitShortcut = Rc<dyn Fn(&str, u32)>;


/// Settings → Keyboard shortcut: the overlay's own show / hide combination.
/// `wire_shortcut_recorder` makes it record.
pub(in crate::harness_settings) struct ShortcutPage {
    pub(in crate::harness_settings) root: Box,
    pub(in crate::harness_settings) combo_label: Label,
    pub(in crate::harness_settings) btn_record: Button,
    pub(in crate::harness_settings) note: Label,
    pub(in crate::harness_settings) hint: Label,
}

pub(in crate::harness_settings) fn shortcut_page() -> ShortcutPage {
    let root = settings_page_root();
    let (_, body) = section_card(&root, "", "Show / hide shortcut");
    let combo_label = Label::new(None);
    combo_label.add_css_class("shortcut-combo");
    combo_label.set_valign(Align::Center);
    combo_label.set_xalign(0.0);
    combo_label.set_hexpand(true);
    combo_label.set_selectable(true);
    combo_label.set_tooltip_text(Some("Written to ~/.config/hypr/bindings.lua"));

    let btn_record = Button::with_label("⏺ Record");
    btn_record.set_tooltip_text(Some("Press the key combination you want to use"));
    btn_record.add_css_class("launcher-btn");
    btn_record.add_css_class("launcher-btn-primary");
    btn_record.set_valign(Align::Center);

    let combo_row = Box::new(Orientation::Horizontal, 10);
    combo_row.add_css_class("shortcut-row");
    combo_row.append(&combo_label);
    combo_row.append(&btn_record);
    body.append(&combo_row);

    let note = Label::new(None);
    note.add_css_class("launcher-note");
    note.set_xalign(0.0);
    note.set_wrap(true);
    note.set_visible(false);
    body.append(&note);
    let shortcut_hint = hint("");
    body.append(&shortcut_hint);
    ShortcutPage { root, combo_label, btn_record, note, hint: shortcut_hint }
}

/// The shortcut recorder.
///
/// Recording seizes the keyboard for a few seconds (`shortcut::begin_capture`
/// parks Hyprland in a throw-away submap so no global bind can eat the key),
/// so every exit — Esc, the button turning into Cancel, the watchdog, the
/// panel being closed, the panel being reopened — funnels through
/// `stop_recording`, which is also the only thing that releases the guard.
/// Returns `stop_recording` and the painter of the combo and Record button.
pub(in crate::harness_settings) fn wire_shortcut_recorder(
    panel: &Box,
    page: &ShortcutPage,
    state: &Rc<RefCell<AppState>>,
    on_shortcut_change: &Rc<dyn Fn(String)>,
) -> (StopRecording, Rc<dyn Fn()>) {
    // `armed` holds the keymap guard for exactly as long as the listener is
    // live: dropping it is what gives the user their global shortcuts back.
    let recording = Rc::new(Cell::new(false));
    let armed: Rc<RefCell<Option<CaptureGuard>>> = Rc::new(RefCell::new(None));

    // Paint the combo and the Record/Cancel button from `recording`.
    let paint_recorder: Rc<dyn Fn()> = {
        let recording = Rc::clone(&recording);
        let state = Rc::clone(state);
        let combo_label = page.combo_label.clone();
        let btn_record = page.btn_record.clone();
        let shortcut_hint = page.hint.clone();
        Rc::new(move || {
            if recording.get() {
                combo_label.set_text("Press your combination…");
                combo_label.add_css_class("shortcut-recording");
                btn_record.set_label("✕ Cancel");
                btn_record.remove_css_class("launcher-btn-primary");
                btn_record.add_css_class("launcher-btn-danger");
                shortcut_hint.set_text(
                    "Listening — global shortcuts are paused until you press one. \
                     Esc keeps the current combination.",
                );
            } else {
                combo_label.set_text(&crate::shortcut::current_combo(
                    state.borrow().toggle_shortcut.as_deref(),
                ));
                combo_label.remove_css_class("shortcut-recording");
                btn_record.set_label("⏺ Record");
                btn_record.remove_css_class("launcher-btn-danger");
                btn_record.add_css_class("launcher-btn-primary");
                shortcut_hint.set_text(
                    "Click Record, then press the combination you want — SUPER/CTRL/ALT \
                     plus a key, or F1-F12 on their own. Global shortcuts pause while \
                     recording, so any combination can be captured.",
                );
            }
        })
    };

    // The one way out of a recording. `message` is the outcome to show (None
    // keeps whatever is already there) — every cancel path calls this.
    let stop_recording: StopRecording = {
        let recording = Rc::clone(&recording);
        let armed = Rc::clone(&armed);
        let paint = Rc::clone(&paint_recorder);
        let shortcut_note = page.note.clone();
        Rc::new(move |message: Option<&str>| {
            // Release first: this is what un-pauses the user's shortcuts.
            if let Some(guard) = armed.borrow_mut().take() {
                guard.end();
            }
            let was_recording = recording.replace(false);
            if let Some(message) = message {
                shortcut_note.remove_css_class("launcher-note-error");
                shortcut_note.set_text(message);
                shortcut_note.set_visible(true);
            } else {
                shortcut_note.set_visible(false);
            }
            if was_recording {
                paint();
            }
        })
    };

    // A captured combination: leave the recording, then write it to Hyprland.
    let commit_shortcut: CommitShortcut = {
        let state = Rc::clone(state);
        let on_shortcut_change = Rc::clone(on_shortcut_change);
        let stop_recording = Rc::clone(&stop_recording);
        let paint = Rc::clone(&paint_recorder);
        let shortcut_note = page.note.clone();
        Rc::new(move |combo: &str, keycode: u32| {
            stop_recording(None);
            let (message, failed) = match crate::shortcut::apply_combo(combo, Some(keycode)) {
                Ok(applied) => {
                    {
                        let mut s = state.borrow_mut();
                        s.toggle_shortcut = Some(applied.combo.clone());
                    }
                    let snapshot = state.borrow().clone();
                    crate::state::save_state_async(snapshot);
                    on_shortcut_change(applied.combo.clone());
                    match (applied.warning, applied.conflict) {
                        (Some(warning), _) => (format!("⚠ {warning}"), true),
                        (None, Some(other)) => (
                            format!("● {combo} toggles SUPER DESKTOP — it used to run “{other}”."),
                            false,
                        ),
                        (None, None) => (format!("● {combo} toggles SUPER DESKTOP."), false),
                    }
                }
                Err(e) => (format!("⚠ {e}"), true),
            };
            // Restyle from scratch: an outcome shown after an earlier failure
            // must not inherit the red left over from it.
            shortcut_note.remove_css_class("launcher-note-error");
            if failed {
                shortcut_note.add_css_class("launcher-note-error");
            }
            shortcut_note.set_text(&message);
            shortcut_note.set_visible(true);
            paint();
        })
    };

    let recorder = Recorder { recording, armed, stop: Rc::clone(&stop_recording), paint: Rc::clone(&paint_recorder) };
    arm_shortcut_recorder(panel, page, &recorder, commit_shortcut);
    (stop_recording, paint_recorder)
}

/// A shortcut recording's state and its two ways to repaint or end it.
pub(in crate::harness_settings) struct Recorder {
    pub(in crate::harness_settings) recording: Rc<Cell<bool>>,
    pub(in crate::harness_settings) armed: Rc<RefCell<Option<CaptureGuard>>>,
    pub(in crate::harness_settings) stop: StopRecording,
    pub(in crate::harness_settings) paint: Rc<dyn Fn()>,
}

/// Starting a recording: the Record button, the key listener on `panel`, and
/// the watchdog that ends a recording nobody finishes.
pub(in crate::harness_settings) fn arm_shortcut_recorder(panel: &Box, page: &ShortcutPage, recorder: &Recorder, commit_shortcut: CommitShortcut) {
    // Two ways out of a recording nobody finishes: the panel/overlay going
    // away, and the watchdog. Both matter because the recorder holds the
    // keyboard — a stuck recording is a desktop with no working shortcuts.
    // Every recording starts its own tick, which ends with that recording (or
    // when a newer one replaces it), so none runs while nothing is recorded.
    let watch_recording: Rc<dyn Fn()> = {
        let recording = Rc::clone(&recorder.recording);
        let stop_recording = Rc::clone(&recorder.stop);
        let panel = panel.downgrade();
        let generation = Rc::new(Cell::new(0u64));
        Rc::new(move || {
            generation.set(generation.get().wrapping_add(1));
            let ticket = generation.get();
            let generation = Rc::clone(&generation);
            let recording = Rc::clone(&recording);
            let stop_recording = Rc::clone(&stop_recording);
            let panel = panel.clone();
            let mut armed_ticks = 0u32;
            let max_ticks = (RECORD_WATCHDOG.as_millis() / RECORD_TICK.as_millis()).max(1) as u32;
            glib::timeout_add_local(RECORD_TICK, move || {
                let Some(panel) = panel.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if !recording.get() || generation.get() != ticket {
                    return glib::ControlFlow::Break;
                }
                armed_ticks += 1;
                // `is_mapped`, not `is_visible`: hiding the whole overlay unmaps the
                // window without ever touching the panel's own visibility flag.
                if !panel.is_mapped() {
                    stop_recording(Some("Recording cancelled — the panel was closed."));
                    return glib::ControlFlow::Break;
                }
                if armed_ticks >= max_ticks {
                    stop_recording(Some("No combination captured — try again."));
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            });
        })
    };

    let start_recording: Rc<dyn Fn()> = {
        let recording = Rc::clone(&recorder.recording);
        let armed = Rc::clone(&recorder.armed);
        let watch_recording = Rc::clone(&watch_recording);
        let paint = Rc::clone(&recorder.paint);
        let shortcut_note = page.note.clone();
        let btn_record = page.btn_record.clone();
        Rc::new(move || {
            if recording.get() {
                return;
            }
            // The keys have to reach THIS surface: clicking Record focuses the
            // button, and with it the panel the capture controller sits on.
            btn_record.grab_focus();
            let guard = crate::shortcut::begin_capture();
            // Without the guard Hyprland keeps handling the shortcuts it owns
            // before they ever reach this window, so say so instead of letting
            // the user press a taken combination and watch nothing happen.
            let unguarded = !guard.armed();
            *armed.borrow_mut() = Some(guard);
            recording.set(true);
            watch_recording();
            shortcut_note.set_visible(false);
            if unguarded {
                shortcut_note.add_css_class("launcher-note-error");
                shortcut_note.set_text(
                    "Could not pause Hyprland's own shortcuts — a combination that is \
                     already bound will not reach this window.",
                );
                shortcut_note.set_visible(true);
            }
            paint();
        })
    };

    // The listener. Capture phase, on the whole panel: it must see the keys
    // before the widget the user last clicked, and before the window's own Esc
    // handler — while recording, Esc cancels the recording, it does not hide
    // the overlay.
    let key_ctrl = EventControllerKey::new();
    key_ctrl.set_propagation_phase(PropagationPhase::Capture);
    {
        let recording = Rc::clone(&recorder.recording);
        let stop_recording = Rc::clone(&recorder.stop);
        let shortcut_note = page.note.clone();
        key_ctrl.connect_key_pressed(move |_, key, keycode, mods| {
            if !recording.get() {
                return glib::Propagation::Proceed;
            }
            if key == gdk::Key::Escape {
                stop_recording(Some("Recording cancelled — the shortcut is unchanged."));
                return glib::Propagation::Stop;
            }
            match crate::shortcut::interpret(key, mods) {
                Capture::Combo(combo) => {
                    commit_shortcut(&combo, keycode);
                    glib::Propagation::Stop
                }
                Capture::NeedsModifier => {
                    shortcut_note.add_css_class("launcher-note-error");
                    shortcut_note.set_text(
                        "Hold SUPER, CTRL or ALT with the key — or use F1-F12 on their own.",
                    );
                    shortcut_note.set_visible(true);
                    glib::Propagation::Stop
                }
                Capture::Waiting => glib::Propagation::Stop,
            }
        });
    }
    panel.add_controller(key_ctrl);

    page.btn_record.connect_clicked({
        let recording = Rc::clone(&recorder.recording);
        let stop_recording = Rc::clone(&recorder.stop);
        move |_| {
            if recording.get() {
                stop_recording(Some("Recording cancelled — the shortcut is unchanged."));
            } else {
                start_recording();
            }
        }
    });
}

