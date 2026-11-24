use super::*;

/// Settings → Top bar: the dock's scale. Returns the page and what repaints
/// the size buttons from the state.
pub(in crate::harness_settings) fn top_bar_page(state: &Rc<RefCell<AppState>>, on_top_bar_size_change: &Rc<dyn Fn(TopBarSize)>) -> (Box, Rc<dyn Fn()>) {
    let root = settings_page_root();
    let (_, body) = section_card(&root, "", "Top bar");
    let size_label = Label::new(Some("Size"));
    size_label.add_css_class("launcher-status-text");
    size_label.set_xalign(0.0);
    body.append(&size_label);

    let size_actions = Box::new(Orientation::Horizontal, 8);
    size_actions.add_css_class("launcher-actions");
    let btn_small = Button::with_label("Small");
    let btn_medium = Button::with_label("Medium");
    let btn_large = Button::with_label("Large");
    for button in [&btn_small, &btn_medium, &btn_large] {
        button.add_css_class("launcher-btn");
        button.add_css_class("top-bar-size");
        size_actions.append(button);
    }
    body.append(&size_actions);

    let size_buttons = Rc::new(vec![
        (TopBarSize::Small, btn_small),
        (TopBarSize::Medium, btn_medium),
        (TopBarSize::Large, btn_large),
    ]);
    let paint_size: Rc<dyn Fn()> = {
        let state = Rc::clone(state);
        let size_buttons = Rc::clone(&size_buttons);
        Rc::new(move || {
            let selected = state.borrow().top_bar_size;
            for (size, button) in size_buttons.iter() {
                paint_size_button(button, *size == selected);
            }
        })
    };
    for (size, button) in size_buttons.iter() {
        let size = *size;
        let state = Rc::clone(state);
        let paint_size = Rc::clone(&paint_size);
        let on_top_bar_size_change = Rc::clone(on_top_bar_size_change);
        button.connect_clicked(move |_| {
            state.borrow_mut().top_bar_size = size;
            paint_size();
            on_top_bar_size_change(size);
        });
    }
    paint_size();
    (root, paint_size)
}

/// Settings → SD Editor: the built-in editor's top-bar button, on or off,
/// and a way to open it when the button is hidden.
pub(in crate::harness_settings) fn editor_page(state: &Rc<RefCell<AppState>>) -> Box {
    let root = settings_page_root();
    let (_, body) = section_card(&root, "", "SD Editor");
    body.append(&hint("A text editor card on the desktop. It opens on the folder chosen in the top bar: \
        its files in a tree on the left, open files in tabs on the right, with syntax colors, find and replace, \
        Save and Save as. Images, sound and video open in previews."));
    let row = Box::new(Orientation::Horizontal, 12);
    let label = Label::new(Some("Show the SD Editor button (a lightning bolt with a pencil) in the top bar"));
    label.set_hexpand(true);
    label.set_xalign(0.0);
    label.set_wrap(true);
    let toggle = gtk4::Switch::new();
    toggle.add_css_class("editor-button-toggle");
    toggle.set_valign(Align::Center);
    toggle.set_active(!state.borrow().editor_button_hidden);
    row.append(&label);
    row.append(&toggle);
    body.append(&row);
    toggle.connect_active_notify({
        let state = Rc::clone(state);
        move |toggle| {
            let shown = toggle.is_active();
            state.borrow_mut().editor_button_hidden = !shown;
            crate::state::save_state_async(state.borrow().clone());
            crate::editor::set_button_shown(shown);
        }
    });
    let actions = Box::new(Orientation::Horizontal, 8);
    actions.add_css_class("launcher-actions");
    let open = Button::with_label("Open SD Editor");
    open.add_css_class("launcher-btn");
    open.set_tooltip_text(Some("Open SD Editor on the folder chosen in the top bar"));
    actions.append(&open);
    body.append(&actions);
    let status = hint("");
    status.set_visible(false);
    body.append(&status);
    open.connect_clicked(move |_| {
        if let Err(e) = crate::editor::open_here() {
            status.set_text(&e);
            status.set_visible(true);
        }
    });
    root
}

/// Settings → Sleep lock: stay awake on external power.
pub(in crate::harness_settings) fn sleep_lock_page(state: &Rc<RefCell<AppState>>) -> Box {
    let root = settings_page_root();
    let (_, sleep_body) = section_card(&root, "", "Stay awake on external power");
    let sleep_row = Box::new(Orientation::Horizontal, 12);
    let sleep_label = Label::new(Some("Prevent sleep while plugged in"));
    sleep_label.set_hexpand(true);
    sleep_label.set_xalign(0.0);
    sleep_label.set_wrap(true);
    let sleep_toggle = gtk4::Switch::new();
    sleep_toggle.add_css_class("sleep-lock-toggle");
    sleep_toggle.set_valign(Align::Center);
    sleep_toggle.set_active(state.borrow().sleep_lock_on_ac);
    sleep_toggle.set_tooltip_text(Some("Keep the bridge and AI harnesses running on mains or charger power, including with the lid closed"));
    sleep_row.append(&sleep_label);
    sleep_row.append(&sleep_toggle);
    sleep_body.append(&sleep_row);
    let sleep_help = hint("Keeps this PC and its bridge awake while SUPER DESKTOP is running on mains or charger power, including with a laptop lid closed. Normal sleep behavior returns on battery. The screen can still turn off and lock. Turn this off before manually suspending.");
    #[cfg(target_os = "macos")]
    {
        sleep_toggle.set_active(false);
        sleep_toggle.set_sensitive(false);
        sleep_toggle.set_tooltip_text(Some("Stay-awake support is unavailable in this macOS build"));
        sleep_help.set_label("macOS currently manages sleep normally. Stay-awake support is unavailable in this build.");
    }
    sleep_body.append(&sleep_help);
    let sleep_status = Label::new(Some(&crate::sleep_lock::status()));
    sleep_status.set_wrap(true);
    sleep_status.set_xalign(0.0);
    sleep_status.add_css_class("sleep-lock-status");
    sleep_body.append(&sleep_status);
    sleep_toggle.connect_active_notify({
        let state = Rc::clone(state);
        move |toggle| {
            let enabled = toggle.is_active();
            state.borrow_mut().sleep_lock_on_ac = enabled;
            crate::state::save_state_async(state.borrow().clone());
            crate::sleep_lock::set_enabled(enabled);
        }
    });
    // Shown right away on map (the status is one mutex read), then every
    // second while the page stays on screen.
    let paint_status: Rc<dyn Fn()> = {
        let weak_status = sleep_status.downgrade();
        Rc::new(move || {
            let Some(label) = weak_status.upgrade() else { return; };
            let text = crate::sleep_lock::status();
            if label.text().as_str() != text { label.set_text(&text); }
        })
    };
    sleep_status.connect_map({
        let paint_status = Rc::clone(&paint_status);
        move |_| paint_status()
    });
    crate::launcher_settings::tick_while_mapped(
        &[sleep_status.clone().upcast()],
        Duration::from_secs(1),
        move || paint_status(),
    );
    root
}

/// Settings → System launch: the daemon starts at login on this PC, so a
/// headless PC answers the phone bridge without any desktop session.
pub(in crate::harness_settings) fn system_launch_page(state: &Rc<RefCell<AppState>>) -> Box {
    let root = settings_page_root();
    let (_, launch_body) = section_card(&root, "", "Daemon starts when this PC starts");
    let launch_row = Box::new(Orientation::Horizontal, 12);
    let launch_label = Label::new(Some("Start the daemon at login"));
    launch_label.set_hexpand(true);
    launch_label.set_xalign(0.0);
    launch_label.set_wrap(true);
    let launch_toggle = gtk4::Switch::new();
    launch_toggle.add_css_class("system-launch-toggle");
    launch_toggle.set_valign(Align::Center);
    launch_toggle.set_active(state.borrow().boot_autostart);
    launch_toggle.set_tooltip_text(Some(
        "Run the daemon as a private systemd user service at every login of this user, including a headless text console",
    ));
    launch_row.append(&launch_label);
    launch_row.append(&launch_toggle);
    launch_body.append(&launch_row);
    let launch_help = hint(
        "For a PC you only reach from your phone or another PC: the daemon, its cards and its phone bridge answer with no desktop. This owns one file, ~/.config/systemd/user/super-desktop-boot.service, and turning this off removes it. The service starts when this user's session starts, so the PC has to log the user in at boot (a getty autologin or a display manager). Hyprland's own autostart keeps working; only one daemon ever runs.",
    );
    #[cfg(target_os = "macos")]
    {
        launch_toggle.set_active(false);
        launch_toggle.set_sensitive(false);
        launch_toggle.set_tooltip_text(Some("System launch is unavailable in this macOS build"));
        launch_help.set_label("This build cannot manage systemd services.");
    }
    launch_body.append(&launch_help);
    let launch_status = Label::new(Some(&crate::boot_autostart::status()));
    launch_status.set_wrap(true);
    launch_status.set_xalign(0.0);
    launch_status.add_css_class("system-launch-status");
    launch_body.append(&launch_status);
    launch_toggle.connect_active_notify({
        let state = Rc::clone(state);
        move |toggle| {
            let enabled = toggle.is_active();
            state.borrow_mut().boot_autostart = enabled;
            crate::state::save_state_async(state.borrow().clone());
            crate::boot_autostart::apply(enabled);
        }
    });
    // Shown right away on map (the status is one mutex read), then every
    // second while the page stays on screen.
    let paint_launch_status: Rc<dyn Fn()> = {
        let weak_status = launch_status.downgrade();
        Rc::new(move || {
            let Some(label) = weak_status.upgrade() else { return; };
            let text = crate::boot_autostart::status();
            if label.text().as_str() != text { label.set_text(&text); }
        })
    };
    launch_status.connect_map({
        let paint_launch_status = Rc::clone(&paint_launch_status);
        move |_| paint_launch_status()
    });
    crate::launcher_settings::tick_while_mapped(
        &[launch_status.clone().upcast()],
        Duration::from_secs(1),
        move || paint_launch_status(),
    );
    root
}

