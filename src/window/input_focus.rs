use super::*;

/// Hide must not wait forever on Hyprland vsync. A local LLM (LM Studio /
/// llama.cpp) can fill the GPU so frame callbacks never run, which used to
/// leave the overlay mapped after Toggle. The hide spring is 99% off-screen
/// at ~83ms and reports rest at ~130ms, so this unmaps on time either way.
pub(crate) const HIDE_FALLBACK: Duration = Duration::from_millis(150);

thread_local! {
    /// From the start of a hide until the next show, the overlay's keyboard
    /// and pointer belong to the desktop again (see `release_input`).
    pub(in crate::window) static INPUT_RELEASED: Cell<bool> = const { Cell::new(false) };
    /// Counts keyboard-mode changes, so the focus guard only undoes its own
    /// `Exclusive` (see `reclaim_keyboard`).
    pub(in crate::window) static KEYBOARD_MODE_CHANGES: Cell<u64> = const { Cell::new(0) };
    /// Pairs background windows opening with the overlay losing the keyboard.
    pub(in crate::window) static FOCUS_GUARD: RefCell<crate::focus_guard::FocusGuard> =
        RefCell::new(crate::focus_guard::FocusGuard::default());
}

/// How long the focus guard holds the keyboard `Exclusive` before it goes
/// back to on demand. Hyprland moves the keyboard as it applies the commit;
/// on demand then keeps it until something else takes it.
pub(in crate::window) const RECLAIM_HOLD: Duration = Duration::from_millis(200);

/// Set the overlay's layer-shell keyboard interactivity.
///
/// Every overlay caller goes through here: releasing input makes the keyboard
/// and pointer leave the surface, and the focus and pointer handlers that
/// fire for that must not take the keyboard back while the overlay slides out.
pub(crate) fn set_overlay_keyboard_mode(window: &impl IsA<gtk4::Window>, mode: KeyboardMode) {
    if overlay_keyboard_mode_allowed(mode, INPUT_RELEASED.with(Cell::get)) {
        KEYBOARD_MODE_CHANGES.with(|changes| changes.set(changes.get().wrapping_add(1)));
        window.set_keyboard_mode(mode);
    }
}

/// The overlay is on screen and owns its input (not hidden, not sliding out).
pub(in crate::window) fn overlay_owns_input(window: &impl IsA<gtk4::Window>) -> bool {
    window.as_ref().is_visible() && !INPUT_RELEASED.with(Cell::get)
}

/// Take the keyboard back from a window that opened under the overlay.
///
/// `Exclusive` makes Hyprland give the layer the keyboard; going back to on
/// demand afterwards keeps it there while desktop keys still work once the
/// user moves elsewhere. An `Exclusive` overlay (an expanded card, the folder
/// field) never loses the keyboard this way, and is left alone.
pub(in crate::window) fn reclaim_keyboard(window: &ApplicationWindow) {
    if !overlay_owns_input(window) || window.keyboard_mode() != KeyboardMode::OnDemand {
        return;
    }
    set_overlay_keyboard_mode(window, KeyboardMode::Exclusive);
    let ours = KEYBOARD_MODE_CHANGES.with(Cell::get);
    let window = window.clone();
    glib::timeout_add_local_once(RECLAIM_HOLD, move || {
        // Anything that set a mode since (a card expanding, the folder field,
        // a hide) decides the mode now.
        if KEYBOARD_MODE_CHANGES.with(Cell::get) == ours
            && window.keyboard_mode() == KeyboardMode::Exclusive
        {
            set_overlay_keyboard_mode(&window, KeyboardMode::OnDemand);
        }
    });
}

/// Follow the overlay's keyboard for the focus guard (`focus_guard`).
pub(in crate::window) fn install_focus_guard(window: &ApplicationWindow) {
    // Passive: it only answers whether the pointer is over the overlay.
    let pointer = EventControllerMotion::new();
    window.add_controller(pointer.clone());
    window.connect_is_active_notify(move |window| {
        if window.is_active() || !overlay_owns_input(window) {
            FOCUS_GUARD.with(|guard| guard.borrow_mut().reset());
            return;
        }
        let pointer_left = !pointer.contains_pointer();
        let reclaim = FOCUS_GUARD.with(|guard| {
            guard.borrow_mut().keyboard_lost(std::time::Instant::now(), pointer_left)
        });
        if reclaim {
            reclaim_keyboard(window);
        }
    });
}

pub(in crate::window) fn overlay_keyboard_mode_allowed(mode: KeyboardMode, input_released: bool) -> bool {
    mode == KeyboardMode::None || !input_released
}

