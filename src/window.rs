mod cli_preferences;
pub(in crate::window) use gtk4::gdk;
pub(in crate::window) use gtk4::glib;
pub(in crate::window) use gtk4::prelude::*;
pub(in crate::window) use gtk4::{
    Align, Application, ApplicationWindow, Button, EventControllerFocus, EventControllerKey,
    EventControllerMotion, Fixed, Image, Label, Orientation, Overlay, Popover, PositionType,
};
pub(in crate::window) use crate::desktop_shell::{Edge, KeyboardMode, Layer, LayerShell};
pub(in crate::window) use std::cell::{Cell, RefCell};
pub(in crate::window) use std::collections::HashMap;
pub(in crate::window) use std::rc::Rc;
pub(in crate::window) use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(in crate::window) use crate::mini_terminal::{
    clamp_card_size, displayed_pos, expanded_rect, set_displayed_pos, HoverRaiseLock,
    MiniTerminalCard, NEW_TERM_HEIGHT, NEW_TERM_WIDTH,
};
pub(in crate::window) use crate::overlap_ghost::GhostLayer;
pub(in crate::window) use crate::state::{AppState, NoteData, TerminalData, TopBarSize};
pub(in crate::window) use crate::sticky_note::StickyNote;
pub(in crate::window) use crate::tmux::create_session;

mod cards;
mod cli;
mod input_focus;
mod lifecycle;
mod plugin_hooks;
mod slide;
mod top_bar;
#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use top_bar::{
    paint_top_bar_size, top_bar_content, watch_frames, TopBarContent, TopBarFit, Trim, BRAND,
    BRAND_ICON,
};
pub(in crate::window) use top_bar::{bind_top_bar_width, paint_shortcut_hints, top_bar_height};
#[cfg(test)]
pub(in crate::window) use top_bar::fit_top_bar;

pub(crate) use input_focus::{set_overlay_keyboard_mode, HIDE_FALLBACK};
pub(in crate::window) use input_focus::{
    install_focus_guard, overlay_owns_input, reclaim_keyboard, FOCUS_GUARD, INPUT_RELEASED,
};
#[cfg(test)]
pub(in crate::window) use input_focus::overlay_keyboard_mode_allowed;

pub(in crate::window) use slide::{
    canvas_cards, SlideAnim, Trajectory, HUD_MIN_HEIGHT,
};
#[cfg(test)]
pub(in crate::window) use slide::{
    card_slide_offscreen, hud_slide_pose, paint_slide_widget, spring_settled, spring_step,
    SLIDE_LAUNCH_IN, SLIDE_LAUNCH_OUT, SLIDE_OMEGA_IN, SLIDE_OMEGA_OUT,
};
#[allow(unused_imports)]
pub(in crate::window) use slide::{
    hud_slide_pose as _, SLIDE_LAUNCH_IN as _, SLIDE_LAUNCH_OUT as _,
};

pub(in crate::window) use plugin_hooks::install_plugin_hooks;

pub(crate) use cards::raise_canvas_child;
pub(in crate::window) use cards::{
    apply_terminal_expand, clamp_card_origin, close_session_card, live_workspace_size, persist,
    persist_if, place_at_rest, place_expanded, raise_notes_on_canvas, set_card_expanded,
};
#[cfg(test)]
pub(in crate::window) use cards::{first_free_icon_slot, IconArea, ICON_ROW_GAP};

/// Cards may extend past the output while being dragged, restored from a
/// larger display, or animated off screen. Their extents must never become
/// the layer-shell window's minimum size.
pub(crate) fn desktop_overlay(workspace: &impl IsA<gtk4::Widget>) -> Overlay {
    let root = Overlay::new();
    root.set_child(Some(&gtk4::Box::new(Orientation::Vertical, 0)));
    workspace.set_halign(Align::Fill);
    workspace.set_valign(Align::Fill);
    root.add_overlay(workspace);
    root.set_measure_overlay(workspace, false);
    root.set_clip_overlay(workspace, true);
    root
}


pub struct SuperDesktopWindow {
    pub window: ApplicationWindow,
    pub(in crate::window) canvas: Fixed,
    pub(in crate::window) machine_view: Rc<crate::machine_selector::MachineView>,
    pub(in crate::window) pairing_wizard: Rc<crate::peer_pairing_ui::PairingWizard>,
    /// Where pairing requests are approved or rejected. Floats above every
    /// other panel; opened by the bridge, the notification and Review buttons.
    pub(in crate::window) pairing_requests: Rc<crate::pairing_request_ui::PairingRequestPanel>,
    pub(in crate::window) ghost_box: gtk4::Box,
    pub(in crate::window) ghost_label: Label,
    pub(in crate::window) state: Rc<RefCell<AppState>>,
    /// Cards live behind `Rc` so callers can snapshot the list and drop the
    /// `RefCell` borrow before touching GTK.
    ///
    /// GTK delivers signals (pointer/focus enter, unmap, child-exit, ...)
    /// synchronously from inside our own `remove`/`expand`/`focus` calls, and
    /// those signals re-enter our handlers. Any borrow of these lists that is
    /// still alive across such a call makes the re-entrant handler panic with
    /// "RefCell already borrowed"; release builds use `panic = "abort"`, so
    /// that panic kills the whole daemon.
    pub(in crate::window) note_cards: Rc<RefCell<Vec<Rc<StickyNote>>>>,
    pub(in crate::window) terminal_cards: Rc<RefCell<Vec<Rc<MiniTerminalCard>>>>,
    /// Dotted outlines drawn over terminals that another card hides. Shared with
    /// the cards' callbacks, which have no `Rc<Self>` to reach the window.
    pub(in crate::window) ghosts: Rc<GhostLayer>,
    pub(in crate::window) hud: gtk4::Box,
    pub(in crate::window) screen_width: i32,
    pub(in crate::window) screen_height: i32,
    pub(in crate::window) drag_pending: Rc<RefCell<HashMap<gtk4::Widget, (f64, f64)>>>,
    pub(in crate::window) drag_tick_active: Rc<RefCell<bool>>,
    pub(in crate::window) anim_trajectories: Rc<RefCell<HashMap<gtk4::Widget, Trajectory>>>,
    pub(in crate::window) slide: Rc<SlideAnim>,
    /// Called when a hide slide reaches the edge, unless a show reversed it.
    pub(in crate::window) on_slide_hidden: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    /// Toolbar brand icons as (image widget, agent key) for theme-aware refresh.
    pub(in crate::window) brand_images: Rc<RefCell<Vec<(Image, String)>>>,
    /// Rebuilds the ⚙ settings panel (detection + brand logos for the new
    /// light/dark mode) after a theme switch.
    pub(in crate::window) settings_refresh: Rc<dyn Fn()>,
    pub(in crate::window) cli_harness_bar: Rc<RefCell<Option<Rc<crate::harness_bar::HarnessBar>>>>,
    /// Panels inside `root_overlay` that take input over the workspace.
    pub(in crate::window) overlay_panels: Vec<gtk4::Widget>,
    /// The workspace field's ▾ list. A popover is its own Wayland surface, so
    /// hiding the window does not dismiss it: it must be popped down
    /// explicitly, or it stays on screen over an empty desktop.
    pub(in crate::window) ws_popover: Popover,
    /// The local folder field, so a viewer's folder change shows up in it.
    pub(in crate::window) ws_bar: crate::workspace_bar::WorkspaceBar,
    /// Bumped by `show_again`; a slide-out that finishes afterwards must not
    /// unmap the window again (hide → show inside the 140ms animation).
    pub(in crate::window) show_token: std::cell::Cell<u64>,
    /// Pauses the local cards' tmux clients while the overlay stays hidden.
    pub(in crate::window) hidden_pause: Rc<crate::hidden_pause::Controller>,
    /// After a new harness is spawned, hover-raise on other cards is ignored
    /// until this hold expires so the pointer path cannot bury the new card.
    pub(in crate::window) hover_raise_lock: HoverRaiseLock,
    pub(in crate::window) terminal_picker: RefCell<Option<Rc<crate::terminal_picker::Picker>>>,
    /// Keeps the ⚙ Settings card's placement alive: the overlay asks it where
    /// the card goes and how big it is, and stretches the card over the whole
    /// screen once it is dropped.
    pub(in crate::window) settings_layout: RefCell<Option<Rc<crate::floating_panel::MovablePanel>>>,
    /// The welcome tour: opened once by the first show after a fresh install,
    /// and again from ⚙ Settings or `super-desktop tour`.
    pub(in crate::window) welcome_tour: Rc<crate::welcome_tour::WelcomeTour>,
    pub(in crate::window) command_palette: Rc<crate::command_palette::CommandPalette>,
    pub(in crate::window) hidden_notify_gen: Rc<Cell<u64>>,
    pub(in crate::window) open_settings_action: RefCell<Option<Rc<dyn Fn()>>>,
    pub(in crate::window) hide_overlay_action: RefCell<Option<Rc<dyn Fn()>>>,
    /// This window, for the slide's frame tick, which rebuilds the paths once
    /// the window has its real size.
    pub(in crate::window) this: std::cell::OnceCell<std::rc::Weak<Self>>,
}

/// Use compositor-allocated logical pixels; monitor zero is only a startup fallback.
pub(in crate::window) fn allocated_workspace_size(window: &impl IsA<gtk4::Window>, fallback: (i32, i32)) -> (i32, i32) {
    let window = window.as_ref();
    if window.width() > 0 && window.height() > 0 {
        (window.width(), window.height())
    } else {
        fallback
    }
}

/// The window's size, once the compositor has given it one. GTK clears it when
/// the window is hidden, so a shown-again overlay has none until its first frame
/// is laid out.
pub(in crate::window) fn allocated_size(window: &impl IsA<gtk4::Window>) -> Option<(i32, i32)> {
    let window = window.as_ref();
    (window.width() > 0 && window.height() > 0).then(|| (window.width(), window.height()))
}

/// The size to build slide paths for while the window has no size yet: the
/// widest and tallest monitor. A card parked past that right edge is off
/// screen on whichever monitor the compositor picks; monitor 0 alone (the
/// startup fallback) can be a laptop panel narrower than the screen the
/// overlay opens on, which started right-hand cards in the middle of it.
pub(in crate::window) fn provisional_slide_size(fallback: (i32, i32)) -> (i32, i32) {
    let Some(display) = gdk::Display::default() else { return fallback };
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_then(|m| m.downcast::<gdk::Monitor>().ok()))
        .map(|m| m.geometry())
        .fold(fallback, |(w, h), geo| (w.max(geo.width()), h.max(geo.height())))
}

/// How one CLI handler words its refusals of the card a request names. The
/// checks are shared (`SuperDesktopWindow::guarded_card`); the wording, which
/// scripts may read, stays each handler's own.

impl SuperDesktopWindow {
    pub(in crate::window) fn screen_width(&self) -> i32 {
        allocated_workspace_size(&self.window, (self.screen_width, self.screen_height)).0
    }

    pub(in crate::window) fn screen_height(&self) -> i32 {
        allocated_workspace_size(&self.window, (self.screen_width, self.screen_height)).1
    }

    /// `hot_inside` is the daemon's shared "pointer is in the top-left corner
    /// zone" flag: this window is full-screen and therefore sees every pointer
    /// move while it is visible, which is the half of the hot-corner gesture the
    /// corner surface cannot provide once the overlay covers it (see
    /// `crate::hotcorner`).
    pub fn new<FClose: Fn() + 'static>(
        app: &Application,
        on_request_close: FClose,
        hot_inside: Rc<crate::hotcorner::Zone>,
        state: Rc<RefCell<AppState>>,
    ) -> Rc<Self> {
        crate::startup::mark("overlay construction started");
        let window = ApplicationWindow::new(app);

        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("super-desktop"));

        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }

        // A new overlay owns its input until its first hide.
        INPUT_RELEASED.with(|released| released.set(false));
        set_overlay_keyboard_mode(&window, KeyboardMode::OnDemand);
        install_focus_guard(&window);
        window.add_css_class("super-desktop-window");
        crate::frame_profile::watch(&window, "overlay");

        let mut screen_width = 2560;
        let mut screen_height = 1600;

        if let Some(display) = gdk::Display::default() {
            let monitors = display.monitors();
            if let Some(mon) = monitors.item(0).and_then(|m| m.downcast::<gdk::Monitor>().ok()) {
                let geo = mon.geometry();
                screen_width = geo.width().max(1);
                screen_height = geo.height().max(1);
            }
        }

        let canvas = Fixed::new();
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);

        let ghost_box = gtk4::Box::new(Orientation::Vertical, 0);
        ghost_box.add_css_class("term-resize-ghost");
        ghost_box.set_can_target(false);
        ghost_box.set_visible(false);

        let ghost_label = Label::new(None);
        ghost_label.add_css_class("term-ghost-label");
        ghost_label.set_halign(Align::Center);
        ghost_label.set_valign(Align::Center);
        ghost_label.set_vexpand(true);
        ghost_label.set_hexpand(true);
        ghost_box.append(&ghost_label);

        canvas.put(&ghost_box, 0.0, 0.0);

        let note_cards: Rc<RefCell<Vec<Rc<StickyNote>>>> = Rc::new(RefCell::new(Vec::new()));
        let terminal_cards: Rc<RefCell<Vec<Rc<MiniTerminalCard>>>> =
            Rc::new(RefCell::new(Vec::new()));

        // HUD pieces the ⚙ panel writes to when the shortcut changes. They are
        // built here (and appended to `hud` further down) so the panel's
        // callback can capture them.
        let on_close_rc = Rc::new(on_request_close);
        // Hide — icon-only symbolic SVG; `.hud-button-danger` recolors it red.
        let btn_close = Button::from_icon_name("sd-hide-symbolic");
        btn_close.update_property(&[gtk4::accessible::Property::Label("Hide Super Desktop")]);
        btn_close.add_css_class("hud-button");
        btn_close.add_css_class("hud-button-danger");
        btn_close.add_css_class("hud-icon-btn");
        if let Some(img) = btn_close.child().and_downcast::<Image>() {
            img.set_pixel_size(20);
        }
        let hint = Label::new(None);
        hint.add_css_class("hud-shortcut");
        paint_shortcut_hints(&hint, &btn_close, &state.borrow());

        // The settings panel is built before the dock. This holder lets its
        // size buttons repaint the live dock once construction has finished.
        let hud_for_settings: Rc<RefCell<Option<gtk4::Box>>> = Rc::new(RefCell::new(None));
        // The harness bar is built with the dock, after this panel; its size
        // buttons and its harness toggles reach it through this holder.
        let harness_bar_for_settings: Rc<RefCell<Option<Rc<crate::harness_bar::HarnessBar>>>> =
            Rc::new(RefCell::new(None));
        // The remote workspace has its own bar, which has to follow the same
        // choice; it is built after this panel, hence another holder.
        let machine_for_settings: Rc<
            RefCell<Option<Rc<crate::machine_selector::MachineView>>>,
        > = Rc::new(RefCell::new(None));

        let pairing_requests = crate::pairing_request_ui::PairingRequestPanel::new();
        let pairing_target: Rc<RefCell<Option<std::rc::Weak<crate::machine_selector::MachineView>>>> =
            Rc::new(RefCell::new(None));
        let pairing_wizard = crate::peer_pairing_ui::PairingWizard::new(
            &window,
            Rc::new({
                let pairing_target = Rc::clone(&pairing_target);
                move |peer| {
                    if let Some(view) = pairing_target.borrow().as_ref().and_then(std::rc::Weak::upgrade) {
                        view.select_saved_peer(peer);
                    }
                }
            }),
            pairing_requests.hooks(),
        );
        // Settings → Add a device → View another PC hands over to the wizard;
        // the settings card is built lazily, so it is hidden through this slot.
        let settings_for_wizard: Rc<RefCell<Option<gtk4::Widget>>> = Rc::new(RefCell::new(None));
        let connection_hooks = crate::launcher_settings::ConnectionHooks {
            requests: pairing_requests.hooks(),
            connect_to_pc: Rc::new({
                let wizard = Rc::clone(&pairing_wizard);
                let settings = Rc::clone(&settings_for_wizard);
                move || {
                    if let Some(settings) = settings.borrow().as_ref() {
                        settings.set_visible(false);
                    }
                    wizard.open_connect();
                }
            }),
        };

        let settings_panel = crate::harness_settings::build_lazy_harness_settings_panel(
            Rc::clone(&state),
            Rc::new({
                let state = Rc::clone(&state);
                let bar_slot = Rc::clone(&harness_bar_for_settings);
                move |keys: Vec<String>| {
                    persist(&state, |s| s.visible_harnesses = Some(keys.clone()));
                    // The same call a remote view makes with a host's list: the
                    // bar only ever reflects what its owner says is offered.
                    if let Some(bar) = bar_slot.borrow().as_ref() {
                        bar.apply(&crate::harness_bar::HarnessState {
                            keys,
                            custom: state.borrow().custom_harnesses.iter().map(Into::into).collect(),
                            ready: true,
                        });
                    }
                }
            }),
            Rc::new({
                let state = Rc::clone(&state);
                let bar_slot = Rc::clone(&harness_bar_for_settings);
                move |keys: Vec<String>| {
                    if let Some(bar) = bar_slot.borrow().as_ref() {
                        bar.apply(&crate::harness_bar::HarnessState {
                            keys,
                            custom: state.borrow().custom_harnesses.iter().map(Into::into).collect(),
                            ready: true,
                        });
                    }
                }
            }),
            Rc::new({
                // The panel has already written the binding into
                // bindings.lua and reloaded Hyprland; all that is left is to
                // remember the choice and to stop the HUD advertising the old
                // combination.
                let state = Rc::clone(&state);
                let hint = hint.clone();
                let btn_close = btn_close.clone();
                move |combo: String| {
                    persist(&state, |s| s.toggle_shortcut = Some(combo));
                    paint_shortcut_hints(&hint, &btn_close, &state.borrow());
                }
            }),
            Rc::new({
                let state = Rc::clone(&state);
                let hud_for_settings = Rc::clone(&hud_for_settings);
                let machine_for_settings = Rc::clone(&machine_for_settings);
                move |size: TopBarSize| {
                    persist(&state, |s| s.top_bar_size = size);
                    if let Some(hud) = hud_for_settings.borrow().as_ref() {
                        paint_top_bar_size(hud, size);
                    }
                    if let Some(view) = machine_for_settings.borrow().as_ref() {
                        view.paint_top_bar_size(size);
                    }
                    crate::plugin_ui::slots::set_top_inset(top_bar_height(size));
                }
            }),
            connection_hooks,
        );
        *settings_for_wizard.borrow_mut() = Some(settings_panel.widget.clone());
        settings_panel.widget.set_visible(false);

        let hud = gtk4::Box::new(Orientation::Horizontal, 0);
        hud.add_css_class("hud-bar");
        paint_top_bar_size(&hud, state.borrow().top_bar_size);
        hud.set_width_request(screen_width);
        *hud_for_settings.borrow_mut() = Some(hud.clone());

        let hud_left = gtk4::Box::new(Orientation::Horizontal, 10);
        hud_left.set_valign(Align::Center);
        hud_left.set_halign(Align::Start);

        let machine_view = crate::machine_selector::MachineView::new(
            &canvas,
            Rc::new({
                let window = window.clone();
                let settings = settings_panel.widget.clone();
                let wizard = Rc::clone(&pairing_wizard);
                move || {
                    settings.set_visible(false);
                    wizard.close();
                    vte4::GtkWindowExt::set_focus(&window, None::<&gtk4::Widget>);
                    set_overlay_keyboard_mode(&window, KeyboardMode::OnDemand);
                }
            }),
            on_close_rc.clone(),
        );
        *pairing_target.borrow_mut() = Some(Rc::downgrade(&machine_view));
        machine_view.set_add_pc_action(Rc::new({
            let wizard = Rc::clone(&pairing_wizard);
            let settings = settings_panel.widget.clone();
            let window = window.clone();
            move || {
                if !window.is_visible() { return; }
                settings.set_visible(false);
                wizard.open();
            }
        }));
        machine_view.bind_keyboard(&window);
        machine_view.paint_top_bar_size(state.borrow().top_bar_size);
        *machine_for_settings.borrow_mut() = Some(Rc::clone(&machine_view));
        let root_overlay = desktop_overlay(&machine_view.stack);
        // Plugin docks: first overlays, so Settings and dialogs float above.
        crate::plugin_ui::apply_theme(&crate::theme::current_theme());
        let plugin_slots = crate::plugin_ui::slots::Slots::install(&root_overlay, top_bar_height(state.borrow().top_bar_size));
        hud_left.append(&machine_view.local_button);

        let brand = Label::new(Some(BRAND));
        brand.add_css_class("hud-title");
        hud_left.append(&brand);

        // Workspace folder: the directory new harness cards start in. It sits
        // right after the brand so the folder in use is the first thing read
        // when a card is launched (see workspace_bar for why `~` is a bad
        // default once several agents run at once).
        let workspace_bar = crate::workspace_bar::build_workspace_bar(
            Rc::clone(&state),
            Rc::new(crate::state::save_state_async),
        );
        hud_left.append(&workspace_bar.widget);
        {
            let popover = workspace_bar.popover.clone();
            workspace_bar.widget.connect_unmap(move |_| popover.popdown());
        }
        let hud_right = gtk4::Box::new(Orientation::Horizontal, 10);
        hud_right.set_valign(Align::Center);
        hud_right.set_halign(Align::End);

        let drag_pending = Rc::new(RefCell::new(HashMap::new()));
        let drag_tick_active = Rc::new(RefCell::new(false));
        let anim_trajectories = Rc::new(RefCell::new(HashMap::new()));
        let slide = Rc::new(SlideAnim::new());
        let on_slide_hidden: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let brand_images: Rc<RefCell<Vec<(Image, String)>>> = Rc::new(RefCell::new(Vec::new()));
        // Built before the cards (they are loaded further down) because every
        // card callback needs a handle on it.
        let ghosts = GhostLayer::new(
            &canvas,
            &hud,
            Rc::clone(&terminal_cards),
            screen_width,
            screen_height,
        );

        // The remote launch bar shows the same brand logos, so a light/dark
        // switch has to swap those images as well as the local ones.
        brand_images
            .borrow_mut()
            .extend(machine_view.brand_images());

        let welcome_tour = crate::welcome_tour::WelcomeTour::new(
            &crate::shortcut::current_combo(state.borrow().toggle_shortcut.as_deref()),
            crate::welcome_tour::TourHooks {
                top: Rc::new({
                    let state = Rc::clone(&state);
                    move || top_bar_height(state.borrow().top_bar_size)
                }),
                hide_overlay: Rc::new({
                    let on_close = Rc::clone(&on_close_rc);
                    move || on_close()
                }),
            },
        );

        let command_palette = crate::command_palette::CommandPalette::new();

        let win_rc = Rc::new(Self {
            window,
            canvas,
            machine_view,
            pairing_wizard: Rc::clone(&pairing_wizard),
            pairing_requests: Rc::clone(&pairing_requests),
            ghost_box,
            ghost_label,
            state,
            note_cards,
            terminal_cards,
            ghosts,
            hud: hud.clone(),
            screen_width,
            screen_height,
            drag_pending,
            drag_tick_active,
            anim_trajectories,
            slide,
            on_slide_hidden,
            brand_images: Rc::clone(&brand_images),
            settings_refresh: Rc::clone(&settings_panel.refresh),
            cli_harness_bar: Rc::clone(&harness_bar_for_settings),
            overlay_panels: vec![
                settings_panel.widget.clone(),
                welcome_tour.widget.clone().upcast(),
                command_palette.root.clone().upcast(),
            ],
            ws_popover: workspace_bar.popover.clone(),
            ws_bar: workspace_bar.clone(),
            show_token: std::cell::Cell::new(0),
            hidden_pause: crate::hidden_pause::Controller::new(),
            hover_raise_lock: HoverRaiseLock::new(),
            terminal_picker: RefCell::new(None),
            settings_layout: RefCell::new(None),
            welcome_tour: Rc::clone(&welcome_tour),
            command_palette: Rc::clone(&command_palette),
            hidden_notify_gen: Rc::new(Cell::new(0)),
            open_settings_action: RefCell::new(None),
            hide_overlay_action: RefCell::new(None),
            this: std::cell::OnceCell::new(),
        });
        let _ = win_rc.this.set(Rc::downgrade(&win_rc));

        // The overlay is OnDemand so an unfocused HUD does not eat desktop
        // keys. GtkEntry on a layer-shell surface only receives those keys
        // when the surface is Exclusive, so flip for as long as the folder
        // field holds focus (same as an expanded terminal card).
        {
            let focus = EventControllerFocus::new();
            let win = win_rc.window.clone();
            focus.connect_enter(move |_| {
                set_overlay_keyboard_mode(&win, KeyboardMode::Exclusive);
            });
            let win = win_rc.window.clone();
            let terms = Rc::clone(&win_rc.terminal_cards);
            let popover = workspace_bar.popover.clone();
            focus.connect_leave(move |controller| {
                // A newly mapped autocomplete popup can produce a transient
                // focus-leave notification. Keep the layer keyboard-enabled
                // long enough for `popup_for_entry` to restore the GtkText
                // delegate. A real click elsewhere remains unfocused after
                // the grace period, closes the list and returns to OnDemand.
                if popover.is_visible() && !popover.is_autohide() {
                    set_overlay_keyboard_mode(&win, KeyboardMode::Exclusive);
                    let controller = controller.clone();
                    let win = win.clone();
                    let terms = Rc::clone(&terms);
                    let popover = popover.clone();
                    glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
                        if controller.contains_focus() {
                            return;
                        }
                        popover.popdown();
                        let any_expanded = terms.borrow().iter().any(|t| t.is_expanded());
                        set_overlay_keyboard_mode(&win, if any_expanded {
                            KeyboardMode::Exclusive
                        } else {
                            KeyboardMode::OnDemand
                        });
                    });
                    return;
                }
                let any_expanded = terms.borrow().iter().any(|t| t.is_expanded());
                set_overlay_keyboard_mode(&win, if any_expanded {
                    KeyboardMode::Exclusive
                } else {
                    KeyboardMode::OnDemand
                });
            });
            workspace_bar.entry.add_controller(focus);
        }

        // Arm keyboard interactivity before the mouse button goes down over
        // the entry. Switching a layer-shell surface from OnDemand to
        // Exclusive in the entry's focus callback happens during that same
        // press and can cancel GTK's built-in drag-selection gesture. Pointer
        // enter runs first, so normal click/drag, double-click (word) and
        // triple-click (all) selection reach GtkEntry intact.
        {
            let motion = EventControllerMotion::new();
            let win = win_rc.window.clone();
            motion.connect_enter(move |_, _, _| {
                set_overlay_keyboard_mode(&win, KeyboardMode::Exclusive);
            });
            let win = win_rc.window.clone();
            let entry = workspace_bar.entry.clone();
            let terms = Rc::clone(&win_rc.terminal_cards);
            motion.connect_leave(move |_| {
                if !entry.has_focus() {
                    let any_expanded = terms.borrow().iter().any(|t| t.is_expanded());
                    set_overlay_keyboard_mode(&win, if any_expanded {
                        KeyboardMode::Exclusive
                    } else {
                        KeyboardMode::OnDemand
                    });
                }
            });
            workspace_bar.entry.add_controller(motion);
        }

        // + Note Button
        let btn_note = Button::with_label("📝 +");
        btn_note.set_tooltip_text(Some("Create Sticky Note"));
        btn_note.add_css_class("hud-button");
        btn_note.add_css_class("hud-action-primary");
        let win_w = Rc::downgrade(&win_rc);
        btn_note.connect_clicked(move |_| {
            if let Some(w) = win_w.upgrade() {
                w.create_new_note(None, None, "");
            }
        });
        hud_left.append(&btn_note);

        // ✎ SD Editor, on the same folder new cards start in (Settings → SD
        // Editor turns it off).
        let btn_editor = crate::editor::toolbar_button(
            Rc::new({
                let state = Rc::clone(&win_rc.state);
                move || std::path::PathBuf::from(crate::state::effective_workspace_dir(&state.borrow()))
            }),
            !win_rc.state.borrow().editor_button_hidden,
        );
        hud_left.append(&btn_editor);

        // Harnesses (company logo + name; emoji label if the SVG is missing).
        // The bar is the same widget a remote PC's workspace shows: it is told
        // which keys this machine offers and what a click means, and everything
        // else about it — order, labels, logos, tooltips, the launch line — is
        // shared code. Driven by `HARNESS_KEYS` so the launch buttons and the ⚙
        // settings panel can never disagree about what this app can run; the
        // visible subset comes from the stored selection ∩ what is installed.
        let detected = crate::tmux::detect_harnesses();
        let visible_keys = crate::harness_settings::visible_keys(&win_rc.state.borrow(), &detected);
        install_plugin_hooks(&win_rc, on_close_rc.clone(), detected.clone());
        let light_theme = crate::theme::current_theme().mode == "light";
        let harness_bar = crate::harness_bar::HarnessBar::new(
            Rc::new({
                let win_w = Rc::downgrade(&win_rc);
                move |key: &str| {
                    if let Some(w) = win_w.upgrade() {
                        // A folder typed without Enter is the one meant.
                        if w.ws_bar.commit_typed() {
                            w.launch_new_terminal(key);
                        }
                    }
                }
            }),
            Rc::new(move |button: &Button, key: &str| {
                // The usage card is this PC's own provider state, so only the
                // local bar has one: a remote host's usage is not this
                // machine's business. Returns whether it claimed the button's
                // hover, because the native tooltip would double-render on top.
                let Some(key) = crate::tmux::HARNESS_KEYS.iter().copied().find(|k| *k == key)
                else {
                    return false;
                };
                let Some(usage_id) = crate::usage::usage_id_for_agent(key) else {
                    return false;
                };
                let (name, emoji) = crate::harness_bar::harness_label(key);
                let pop = Popover::new();
                pop.add_css_class("usage-pop");
                pop.set_position(PositionType::Bottom);
                pop.set_has_arrow(false);
                pop.set_offset(0, 4);
                pop.set_autohide(false);
                pop.set_can_focus(false);
                pop.set_parent(button);

                let motion = EventControllerMotion::new();
                let pop_enter = pop.clone();
                let card = crate::usage::UsageCardInfo {
                    usage_id,
                    agent_key: key,
                    display_name: name,
                    emoji,
                    light_theme,
                };
                let hovered = button.downgrade();
                motion.connect_enter(move |_, _, _| {
                    // Not while the button is dragged to another place.
                    if hovered.upgrade().is_some_and(|b| b.has_css_class(crate::toolbar_reorder::CLASS)) {
                        return;
                    }
                    // Rebuilt on every hover so numbers are fresh from disk.
                    let content = crate::usage::build_usage_content(&card);
                    pop_enter.set_child(Some(&content));
                    pop_enter.popup();
                    if card.usage_id == "agy" {
                        let pop_refresh = pop_enter.clone();
                        let card_refresh = card.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let updated =
                                gtk4::gio::spawn_blocking(crate::antigravity_usage::refresh_if_stale)
                                    .await;
                            if updated.ok() == Some(true) && pop_refresh.is_visible() {
                                let fresh = crate::usage::build_usage_content(&card_refresh);
                                pop_refresh.set_child(Some(&fresh));
                            }
                        });
                    }
                });
                let pop_leave = pop.clone();
                motion.connect_leave(move |_| {
                    pop_leave.popdown();
                });
                button.add_controller(motion);

                // Don't leave a stale hover card behind after launching.
                let pop_click = pop.clone();
                button.connect_clicked(move |_| {
                    pop_click.popdown();
                });
                true
            }),
        );
        harness_bar.apply(&crate::harness_bar::HarnessState {
            keys: visible_keys,
            custom: win_rc.state.borrow().custom_harnesses.iter().map(Into::into).collect(),
            ready: true,
        });
        // The launchers stay in the order the user dragged them to.
        harness_bar.set_order(&win_rc.state.borrow().launcher_order);
        harness_bar.enable_reorder(Rc::new({
            let state = Rc::clone(&win_rc.state);
            move |order: Vec<String>| {
                persist_if(&state, |s| {
                    let changed = s.launcher_order != order;
                    s.launcher_order = order;
                    changed
                });
            }
        }));
        *harness_bar_for_settings.borrow_mut() = Some(Rc::clone(&harness_bar));
        // The same brand logos the remote bar draws, so a light/dark switch
        // swaps both from the one list.
        brand_images
            .borrow_mut()
            .extend(harness_bar.brand_images());
        // Plugin toolbar items and ✨ Create Plugin scroll with the launchers;
        // the actions on the right never move.
        let launchers = gtk4::Box::new(Orientation::Horizontal, 10);
        launchers.append(&harness_bar.group);
        launchers.append(plugin_slots.toolbar());
        // Plugin toolbar items stay in the order the user dragged them to.
        plugin_slots.set_toolbar_order(win_rc.state.borrow().toolbar_item_order.clone(), Rc::new({
            let state = Rc::clone(&win_rc.state);
            move |order: Vec<String>| {
                persist_if(&state, |s| {
                    let changed = s.toolbar_item_order != order;
                    s.toolbar_item_order = order;
                    changed
                });
            }
        }));
        // A plugin that launches the agents itself ("replacesLaunchers") hides
        // these buttons while it runs; they come back when it is turned off,
        // removed or fails.
        harness_bar.group.set_visible(!plugin_slots.launchers_replaced());
        plugin_slots.on_change(Rc::new({
            let group = harness_bar.group.downgrade();
            move || {
                if let Some(group) = group.upgrade() {
                    group.set_visible(!crate::plugin_ui::slots::launchers_replaced());
                }
            }
        }));
        // Terminal cards show the card buttons of the plugins running now.
        let refresh_card_buttons: Rc<dyn Fn()> = Rc::new({
            let cards = Rc::downgrade(&win_rc.terminal_cards);
            move || {
                let Some(cards) = cards.upgrade() else { return };
                let specs = crate::plugin_ui::card_buttons::current();
                let cards: Vec<Rc<MiniTerminalCard>> = cards.borrow().clone();
                for card in cards {
                    card.set_plugin_buttons(&specs);
                }
            }
        });
        refresh_card_buttons();
        plugin_slots.on_change(refresh_card_buttons);
        let top_bar = top_bar_content(&hud_left, &launchers, &hud_right);
        hud.append(&top_bar.widget);

        // Arrange — icon-only symbolic SVG (themeable via `.hud-icon-btn`).
        let btn_arrange = Button::from_icon_name("sd-arrange-symbolic");
        btn_arrange.update_property(&[gtk4::accessible::Property::Label(
            "Arrange notes and terminals",
        )]);
        btn_arrange.set_tooltip_text(Some("Fit terminals to the display; organize notes left"));
        btn_arrange.add_css_class("hud-button");
        btn_arrange.add_css_class("hud-icon-btn");
        if let Some(img) = btn_arrange.child().and_downcast::<Image>() {
            img.set_pixel_size(20);
        }
        let win_w = Rc::downgrade(&win_rc);
        btn_arrange.connect_clicked(move |_| {
            if let Some(w) = win_w.upgrade() {
                w.auto_arrange();
            }
        });
        hud_right.append(&btn_arrange);

        // The one settings entry point: a bare gears icon (no label), opening
        // the ⚙ card — shortcut, top-bar harnesses, and the 📱 launcher page.
        let btn_settings = Button::from_icon_name("sd-gears-symbolic");
        btn_settings.update_property(&[gtk4::accessible::Property::Label("Settings")]);
        btn_settings.set_tooltip_text(Some("Settings: connections, shortcuts and top bar"));
        btn_settings.add_css_class("hud-button");
        btn_settings.add_css_class("hud-gear");
        if let Some(img) = btn_settings.child().and_downcast::<Image>() {
            img.set_pixel_size(20);
        }
        let settings_w = settings_panel.widget.clone();
        let settings_refresh = Rc::clone(&settings_panel.refresh);
        let wizard_for_settings = Rc::clone(&win_rc.pairing_wizard);
        let tour_for_settings = Rc::clone(&win_rc.welcome_tour);
        btn_settings.connect_clicked(move |_| {
            let show = !settings_w.is_visible();
            if show {
                wizard_for_settings.close();
                tour_for_settings.close();
            }
            settings_w.set_visible(show);
            if show {
                // Re-detect here: a harness installed while the app runs
                // shows up the next time the panel is opened.
                settings_refresh();
            }
        });
        hud_right.append(&btn_settings);
        {
            let btn = btn_settings.clone();
            *win_rc.open_settings_action.borrow_mut() = Some(Rc::new(move || {
                btn.emit_clicked();
            }));
            let close_act: Rc<dyn Fn()> = Rc::clone(&on_close_rc) as Rc<dyn Fn()>;
            *win_rc.hide_overlay_action.borrow_mut() = Some(close_act);
        }

        // Close (its tooltip already names the current shortcut — see
        // `paint_shortcut_hints`).
        let on_close_btn = Rc::clone(&on_close_rc);
        btn_close.connect_clicked(move |_| {
            on_close_btn();
        });
        hud_right.append(&btn_close);

        hud_right.append(&hint);

        // Layer-shell chooses the output. Its allocated logical width is the
        // authority, including after a monitor or scale change. What does not
        // fit it gives up the shortcut hint, then the brand's and the
        // launchers' names.
        let top_bar_fit = TopBarFit::new(&hud, top_bar, vec![
            Trim::Hint(hint.clone()),
            Trim::Icons {
                brand: brand.clone(),
                launchers: Rc::new({
                    let bar = Rc::downgrade(&harness_bar);
                    move |compact| {
                        if let Some(bar) = bar.upgrade() {
                            bar.set_compact(compact);
                        }
                    }
                }),
            },
        ]);
        bind_top_bar_width(&hud, &win_rc.window, top_bar_fit, Rc::new({
            let owner = Rc::downgrade(&win_rc);
            let previous = Cell::new((0, 0));
            move |width, height| {
                if previous.replace((width, height)) == (width, height) {
                    return;
                }
                if let Some(owner) = owner.upgrade() {
                    owner.follow_workspace_size((width, height));
                }
            }
        }));

        // On the canvas, not an Overlay child: Fixed.move_ translates the
        // full-width dock as one widget, same as the cards.
        hud.set_hexpand(false);
        hud.set_vexpand(false);
        hud.set_halign(Align::Start);
        hud.set_valign(Align::Start);
        win_rc.canvas.put(&hud, 0.0, 0.0);
        raise_canvas_child(&win_rc.canvas, &hud);

        // Plugin cards share the canvas, its drag handling and its edges.
        {
            let fallback = (win_rc.screen_width, win_rc.screen_height);
            let window = win_rc.window.downgrade();
            plugin_slots.set_canvas_host(crate::plugin_ui::cards::CanvasHost {
                canvas: win_rc.canvas.clone(),
                raise: Rc::new({
                    let canvas = win_rc.canvas.clone();
                    let hud = hud.clone();
                    move |widget: &gtk4::Widget| {
                        raise_canvas_child(&canvas, widget);
                        raise_canvas_child(&canvas, &hud);
                    }
                }),
                drag: Rc::new(win_rc.card_drag_updater(|| {})),
                drag_end: Rc::new({
                    let canvas = win_rc.canvas.clone();
                    let pending = Rc::clone(&win_rc.drag_pending);
                    let window = window.clone();
                    move |widget: &gtk4::Widget, x: i32, y: i32| {
                        widget.remove_css_class("dragging");
                        pending.borrow_mut().remove(widget);
                        let (x, y) = clamp_card_origin(x, y, live_workspace_size(&window, fallback));
                        canvas.move_(widget, f64::from(x), f64::from(y));
                        (x, y)
                    }
                }),
                size: Rc::new(move || live_workspace_size(&window, fallback)),
                top: Rc::new({
                    let state = Rc::clone(&win_rc.state);
                    move || top_bar_height(state.borrow().top_bar_size)
                }),
            });
            // SD Editor cards share that canvas; the ones open at the last
            // stop come back.
            if let Some(host) = plugin_slots.canvas_host() {
                crate::editor::set_host(host);
                crate::editor::restore();
            }
        }

        // Added after the HUD so the settings card floats above it. It moves
        // by its header and resizes by its edges like a terminal card, and
        // never over the bar. Where and how big the user left it is kept, but
        // on a screen narrower than 1200 it is at most 80% of the width.
        let (saved_pos, saved_size) = {
            let state = win_rc.state.borrow();
            (state.settings_panel_pos, state.settings_panel_size)
        };
        *win_rc.settings_layout.borrow_mut() = Some(crate::floating_panel::MovablePanel::install(
            &root_overlay,
            &settings_panel.widget,
            crate::floating_panel::PanelLayout {
                default_size: crate::harness_settings::SETTINGS_PANEL_DEFAULT_SIZE,
                min_size: crate::harness_settings::SETTINGS_PANEL_MIN_SIZE,
                saved_pos,
                saved_size,
                small_screen_share: Some(crate::harness_settings::SETTINGS_PANEL_SMALL_SCREEN_SHARE),
            },
            Rc::new({
                let state = Rc::clone(&win_rc.state);
                move || top_bar_height(state.borrow().top_bar_size)
            }),
            Rc::new({
                let state = Rc::clone(&win_rc.state);
                move |position, size| {
                    persist(&state, |s| {
                        s.settings_panel_pos = Some(position);
                        s.settings_panel_size = Some(size);
                    });
                }
            }),
        ));
        // Over the cards and Settings, under the pairing dialogs.
        root_overlay.add_overlay(&win_rc.welcome_tour.widget);
        crate::welcome_tour::set_replay(Rc::new({
            let owner = Rc::downgrade(&win_rc);
            move || {
                if let Some(owner) = owner.upgrade() {
                    owner.open_welcome_tour();
                }
            }
        }));
        root_overlay.add_overlay(&pairing_wizard.widget);
        // Files & links panels open as their own floating cards, under the
        // pairing dialogs.
        crate::asset_view::set_host(
            &root_overlay,
            Rc::new({
                let state = Rc::clone(&win_rc.state);
                move || top_bar_height(state.borrow().top_bar_size)
            }),
            &pairing_wizard.widget,
        );
        // Last: a request is decided above whatever else is open.
        root_overlay.add_overlay(&pairing_requests.widget);
        root_overlay.add_overlay(&win_rc.command_palette.root);
        root_overlay.set_measure_overlay(&win_rc.command_palette.root, false);

        let picker_window = Rc::downgrade(&win_rc);
        let picker = crate::terminal_picker::Picker::install(&win_rc.window, &root_overlay, Rc::new(move || {
            let Some(window) = picker_window.upgrade() else { return Vec::new(); };
            if window.overlay_panels.iter().any(|panel| panel.is_visible())
                || window.pairing_requests.is_open() || window.pairing_wizard.is_open() {
                return Vec::new();
            }
            let cards = if window.machine_view.is_remote() {
                window.machine_view.keyboard_cards()
            } else {
                window.terminal_cards.borrow().clone()
            };
            cards.into_iter().map(|card| crate::terminal_picker::Target {
                widget: card.container.clone().upcast(),
                digit: card.keyboard_digit.get(),
                activate: Rc::new(move || card.select_with_keyboard()),
            }).collect()
        }));
        // The preview dims every card, so the terminals under other cards show
        // through while it is up: none of them may be paused meanwhile.
        let ghosts_picker = Rc::clone(&win_rc.ghosts);
        picker.connect_showing(move |showing| ghosts_picker.keep_all_drawing(showing));
        *win_rc.terminal_picker.borrow_mut() = Some(picker);

        // Plugin shortcuts come before the cards: a terminal card with the
        // keyboard must not swallow them. Plugins may only claim combinations
        // with Super or two modifiers (see `manifest::parse_keys`).
        let plugin_keys = EventControllerKey::new();
        plugin_keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        plugin_keys.connect_key_pressed(|_, key, _, state| {
            if crate::plugin_ui::slots::get().is_some_and(|slots| slots.handle_key(key, state)) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        win_rc.window.add_controller(plugin_keys);

        // Global Command Palette, card triage, and focused-card shortcuts (Capture phase).
        let nav_keys = EventControllerKey::new();
        nav_keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let nav_w = Rc::downgrade(&win_rc);
        nav_keys.connect_key_pressed(move |_, key, keycode, state| {
            let Some(w) = nav_w.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if w.command_palette.is_open() {
                return glib::Propagation::Proceed;
            }
            let alt = state.contains(gdk::ModifierType::ALT_MASK);
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
            let modal_open = w.overlay_panels.iter().any(|p| p.is_visible())
                || w.pairing_requests.is_open()
                || w.pairing_wizard.is_open();

            if (alt && !ctrl && key == gdk::Key::space)
                || (ctrl && shift && !alt && matches!(key, gdk::Key::k | gdk::Key::K))
            {
                if let Some(picker) = w.terminal_picker.borrow().as_ref() {
                    picker.cancel();
                }
                w.open_command_palette();
                return glib::Propagation::Stop;
            }
            if modal_open {
                return glib::Propagation::Proceed;
            }
            if alt && !ctrl && matches!(key, gdk::Key::w | gdk::Key::W) {
                if let Some(picker) = w.terminal_picker.borrow().as_ref() {
                    picker.cancel();
                }
                w.jump_to_attention_card();
                return glib::Propagation::Stop;
            }
            if alt && !ctrl && matches!(key, gdk::Key::bracketleft | gdk::Key::bracketright) {
                if let Some(picker) = w.terminal_picker.borrow().as_ref() {
                    picker.cancel();
                }
                w.cycle_local_cards(key == gdk::Key::bracketright);
                return glib::Propagation::Stop;
            }
            if ctrl && shift && !alt {
                if let Some(card) = w.focused_local_card() {
                    let display = gtk4::prelude::WidgetExt::display(&w.window);
                    match crate::terminal_clipboard::layout_shortcut_key(&display, key, keycode) {
                        gdk::Key::f | gdk::Key::F => {
                            card.toggle_expand();
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::m | gdk::Key::M => {
                            card.toggle_iconify();
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::p | gdk::Key::P => {
                            card.open_prompt_history();
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::o | gdk::Key::O => {
                            card.open_files();
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::a | gdk::Key::A => {
                            card.open_attachments();
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::slash | gdk::Key::question => {
                            w.open_slash_palette_for(&card);
                            return glib::Propagation::Stop;
                        }
                        _ => {}
                    }
                }
            }
            glib::Propagation::Proceed
        });
        win_rc.window.add_controller(nav_keys);

        // Esc key
        let key_ctrl = EventControllerKey::new();
        let on_close_key = Rc::clone(&on_close_rc);
        let win_w = Rc::downgrade(&win_rc);
        let ws_popover = workspace_bar.popover.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, state| {
            if key == gdk::Key::Escape {
                if let Some(w) = win_w.upgrade() {
                    if w.command_palette.is_open() {
                        w.command_palette.close();
                        return glib::Propagation::Stop;
                    }
                    if w.pairing_requests.is_open() {
                        w.pairing_requests.close();
                        return glib::Propagation::Stop;
                    }
                    if w.pairing_wizard.is_open() {
                        w.pairing_wizard.close();
                        return glib::Propagation::Stop;
                    }
                    if w.welcome_tour.is_open() {
                        w.welcome_tour.close();
                        return glib::Propagation::Stop;
                    }
                }
                // A plugin's full-screen surface is the innermost thing.
                if crate::plugin_ui::slots::get().is_some_and(|slots| slots.close_fullscreen()) {
                    return glib::Propagation::Stop;
                }
                if win_w.upgrade().is_some_and(|w| w.machine_view.dismiss_if_open()) {
                    return glib::Propagation::Stop;
                }
                // An open folder list is the innermost thing Esc can dismiss:
                // closing the whole overlay here would lose the typed path.
                if ws_popover.is_visible() {
                    ws_popover.popdown();
                    return glib::Propagation::Stop;
                }
                if let Some(w) = win_w.upgrade() {
                    if let Some(focused) = gtk4::prelude::RootExt::focus(&w.window) {
                        let type_name = focused.type_().name();
                        if type_name.contains("Terminal") || focused.has_css_class("term-vte") {
                            return glib::Propagation::Proceed;
                        }
                    }
                }
                on_close_key();
                return glib::Propagation::Stop;
            } else if state.contains(gdk::ModifierType::CONTROL_MASK) && (key == gdk::Key::k || key == gdk::Key::K) {
                if let Some(w) = win_w.upgrade() {
                    if let Some(focused) = gtk4::prelude::RootExt::focus(&w.window) {
                        let type_name = focused.type_().name();
                        if type_name.contains("Terminal") || focused.has_css_class("term-vte") {
                            return glib::Propagation::Proceed;
                        }
                    }
                    w.open_command_palette();
                }
                return glib::Propagation::Stop;
            } else if state.contains(gdk::ModifierType::CONTROL_MASK) && (key == gdk::Key::n || key == gdk::Key::N) {
                if let Some(w) = win_w.upgrade() {
                    if let Some(focused) = gtk4::prelude::RootExt::focus(&w.window) {
                        let type_name = focused.type_().name();
                        if type_name.contains("Terminal") || focused.has_css_class("term-vte") {
                            return glib::Propagation::Proceed;
                        }
                    }
                    if !w.machine_view.is_remote() {
                        w.create_new_note(None, None, "");
                    }
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        win_rc.window.add_controller(key_ctrl);

        // Hot corner, visible half: feed the shared flag from pointer moves over
        // the overlay itself. Passive — an `EventControllerMotion` observes and
        // never consumes, so the drag/resize controllers are unaffected.
        let corner_motion = EventControllerMotion::new();
        // Capture phase, not bubbling: the overlay is a full screen of widgets
        // (canvas, notes, terminal cards), each with its own controllers, and a
        // bubbling controller only ever sees events whose target bubbles up to
        // it. Capturing runs this one before any child, so the flag follows the
        // pointer wherever it is over the overlay — which is what makes the
        // visible -> hidden half of the gesture work.
        corner_motion.set_propagation_phase(gtk4::PropagationPhase::Capture);
        {
            let hot_inside = Rc::clone(&hot_inside);
            corner_motion.connect_enter(move |_, x, y| {
                hot_inside.set(x < crate::hotcorner::CORNER_PX && y < crate::hotcorner::CORNER_PX);
            });
        }
        {
            let hot_inside = Rc::clone(&hot_inside);
            corner_motion.connect_motion(move |_, x, y| {
                let inside = x < crate::hotcorner::CORNER_PX && y < crate::hotcorner::CORNER_PX;
                hot_inside.set(inside);
            });
        }
        win_rc.window.add_controller(corner_motion);

        win_rc.window.set_child(Some(&root_overlay));
        // Nothing in the HUD takes focus by itself. Without this GTK focuses the
        // first focusable widget on every show — now the workspace field — so
        // opening the overlay would put the caret in the folder path and let
        // stray keystrokes edit it. Focus follows what the user clicks.
        vte4::GtkWindowExt::set_focus(&win_rc.window, None::<&gtk4::Widget>);
        win_rc.load_items();

        // Periodic status refresh. Off screen there is nothing to update:
        // per-card status probes are pure `tmux` processes, so the timer only
        // exists while the window is mapped — a hidden daemon gets no wakeup
        // from it. `show_again` refreshes at once, so the first tick one
        // second after the map is not a stale second.
        let win_w = Rc::downgrade(&win_rc);
        crate::launcher_settings::tick_while_mapped(
            &[win_rc.window.clone().upcast()],
            std::time::Duration::from_millis(1000),
            move || {
                if let Some(w) = win_w.upgrade() {
                    w.periodic_refresh();
                }
            },
        );

        crate::startup::mark("overlay constructed (terminal preparation queued)");
        win_rc.window.add_tick_callback(|_, _| {
            crate::startup::mark("first overlay frame-clock tick (not presentation time)");
            glib::ControlFlow::Break
        });
        win_rc
    }

    pub(in crate::window) fn focused_local_card(&self) -> Option<Rc<MiniTerminalCard>> {
        let cards = self.terminal_cards.borrow().clone();
        if let Some(focused) = gtk4::prelude::RootExt::focus(&self.window) {
            if let Some(card) = cards
                .iter()
                .find(|card| {
                    let root: &gtk4::Widget = card.container.upcast_ref();
                    &focused == root || focused.is_ancestor(root)
                })
                .cloned()
            {
                return Some(card);
            }
        }
        cards.into_iter().find(|card| card.is_expanded())
    }

    fn focused_card_index(&self, cards: &[Rc<MiniTerminalCard>]) -> Option<usize> {
        let focused = self.focused_local_card()?;
        cards.iter().position(|c| Rc::ptr_eq(c, &focused))
    }

    pub(in crate::window) fn jump_to_attention_card(&self) {
        let cards = self.terminal_cards.borrow().clone();
        let statuses: Vec<String> = cards.iter().map(|c| c.status_name()).collect();
        let status_refs: Vec<&str> = statuses.iter().map(String::as_str).collect();
        let cur = self.focused_card_index(&cards);
        if let Some(idx) = crate::command_palette::next_attention_index(&status_refs, cur) {
            cards[idx].select_with_keyboard();
        }
    }

    pub(in crate::window) fn cycle_local_cards(&self, forward: bool) {
        let cards = self.terminal_cards.borrow().clone();
        let cur = self.focused_card_index(&cards);
        if let Some(idx) = crate::command_palette::cycle_mru_index(cards.len(), cur, forward) {
            cards[idx].select_with_keyboard();
        }
    }

    pub(in crate::window) fn open_slash_palette_for(&self, card: &Rc<MiniTerminalCard>) {
        let (session, agent, title) = {
            let d = card.data.borrow();
            (d.session_name.clone(), d.agent_type.clone(), card.title_text())
        };
        let cmds = crate::slash_commands::commands_for(&agent);
        if cmds.is_empty() {
            self.open_command_palette();
            return;
        }
        let items = cmds
            .iter()
            .map(|cmd| {
                let s = session.clone();
                let a = agent.clone();
                let c = *cmd;
                let card_w = Rc::downgrade(card);
                crate::command_palette::PaletteItem {
                    title: c.command.trim_end().to_string(),
                    subtitle: c.description.to_string(),
                    badge: if c.submit { "RUN" } else { "DRAFT" }.into(),
                    keywords: format!("{} {}", agent, c.description),
                    action: Rc::new(move || {
                        crate::slash_commands::send(&s, &a, &c);
                        if let Some(card) = card_w.upgrade() {
                            card.focus_terminal();
                        }
                    }),
                }
            })
            .collect();
        let card_focus = Rc::downgrade(card);
        set_overlay_keyboard_mode(&self.window, KeyboardMode::Exclusive);
        self.command_palette.open(
            &format!("Slash Commands · {title}"),
            items,
            Some(Rc::new(move || {
                if let Some(c) = card_focus.upgrade() {
                    c.focus_terminal();
                }
            })),
        );
    }

    pub(in crate::window) fn open_command_palette(&self) {
        let mut items: Vec<crate::command_palette::PaletteItem> = Vec::new();
        let focused = self.focused_local_card();

        // 1. Focused card actions & slash commands first
        if let Some(card) = &focused {
            let (session, agent, title) = {
                let d = card.data.borrow();
                (d.session_name.clone(), d.agent_type.clone(), card.title_text())
            };
            let card_w = Rc::downgrade(card);
            items.push(crate::command_palette::PaletteItem {
                title: if card.is_expanded() { "Collapse card" } else { "Expand card" }.into(),
                subtitle: title.clone(),
                badge: "Ctrl+Shift+F".into(),
                keywords: "toggle fullscreen".into(),
                action: Rc::new({
                    let c = card_w.clone();
                    move || { if let Some(c) = c.upgrade() { c.toggle_expand(); } }
                }),
            });
            items.push(crate::command_palette::PaletteItem {
                title: if card.is_compact() { "Restore card" } else { "Minimize card" }.into(),
                subtitle: title.clone(),
                badge: "Ctrl+Shift+M".into(),
                keywords: "iconify compact".into(),
                action: Rc::new({
                    let c = card_w.clone();
                    move || { if let Some(c) = c.upgrade() { c.toggle_iconify(); } }
                }),
            });
            items.push(crate::command_palette::PaletteItem {
                title: "Prompt history".into(),
                subtitle: title.clone(),
                badge: "Ctrl+Shift+P".into(),
                keywords: "prompts log".into(),
                action: Rc::new({
                    let c = card_w.clone();
                    move || { if let Some(c) = c.upgrade() { c.open_prompt_history(); } }
                }),
            });
            items.push(crate::command_palette::PaletteItem {
                title: "Files & links".into(),
                subtitle: title.clone(),
                badge: "Ctrl+Shift+O".into(),
                keywords: "assets drawer".into(),
                action: Rc::new({
                    let c = card_w.clone();
                    move || { if let Some(c) = c.upgrade() { c.open_files(); } }
                }),
            });
            items.push(crate::command_palette::PaletteItem {
                title: "Attach files".into(),
                subtitle: title.clone(),
                badge: "Ctrl+Shift+A".into(),
                keywords: "upload insert".into(),
                action: Rc::new({
                    let c = card_w.clone();
                    move || { if let Some(c) = c.upgrade() { c.open_attachments(); } }
                }),
            });
            items.push(crate::command_palette::PaletteItem {
                title: "Restart fresh".into(),
                subtitle: title.clone(),
                badge: "SESSION".into(),
                keywords: "restart clean new".into(),
                action: Rc::new({
                    let c = card_w.clone();
                    move || { if let Some(c) = c.upgrade() { c.restart(false); } }
                }),
            });
            items.push(crate::command_palette::PaletteItem {
                title: "Resume conversation".into(),
                subtitle: title.clone(),
                badge: "SESSION".into(),
                keywords: "restart continue resume".into(),
                action: Rc::new({
                    let c = card_w;
                    move || { if let Some(c) = c.upgrade() { c.restart(true); } }
                }),
            });
            for cmd in crate::slash_commands::commands_for(&agent) {
                let s = session.clone();
                let a = agent.clone();
                let c = *cmd;
                let cw = Rc::downgrade(card);
                items.push(crate::command_palette::PaletteItem {
                    title: format!("{} · {}", c.command.trim_end(), title),
                    subtitle: c.description.to_string(),
                    badge: if c.submit { "SLASH" } else { "DRAFT" }.into(),
                    keywords: format!("{} {}", agent, c.description),
                    action: Rc::new(move || {
                        crate::slash_commands::send(&s, &a, &c);
                        if let Some(c) = cw.upgrade() {
                            c.focus_terminal();
                        }
                    }),
                });
            }
        }

        // 2. Switch to any open local or remote card
        let active_cards = if self.machine_view.is_remote() {
            self.machine_view.keyboard_cards()
        } else {
            self.terminal_cards.borrow().clone()
        };
        for card in active_cards {
            let cw = Rc::downgrade(&card);
            let status = card.status_name();
            let title = card.title_text();
            let agent = card.data.borrow().agent_type.clone();
            items.push(crate::command_palette::PaletteItem {
                title: format!("Focus: {title}"),
                subtitle: agent.clone(),
                badge: status,
                keywords: format!("card switch {agent} {title}"),
                action: Rc::new(move || {
                    if let Some(c) = cw.upgrade() {
                        c.select_with_keyboard();
                    }
                }),
            });
        }

        // 3. Workspace & launcher actions
        if let Some(w) = self.this.get().cloned() {
            items.push(crate::command_palette::PaletteItem {
                title: "Jump to waiting / error card".into(),
                subtitle: "Focus next card needing attention".into(),
                badge: "Alt+W".into(),
                keywords: "triage waiting error".into(),
                action: Rc::new({
                    let w = w.clone();
                    move || { if let Some(w) = w.upgrade() { w.jump_to_attention_card(); } }
                }),
            });
            items.push(crate::command_palette::PaletteItem {
                title: "Arrange cards".into(),
                subtitle: "Tile terminals and organize notes".into(),
                badge: "LAYOUT".into(),
                keywords: "tile grid arrange".into(),
                action: Rc::new({
                    let w = w.clone();
                    move || { if let Some(w) = w.upgrade() { w.auto_arrange(); } }
                }),
            });
            items.push(crate::command_palette::PaletteItem {
                title: "New sticky note".into(),
                subtitle: "Create a desktop sticky note".into(),
                badge: "Ctrl+N".into(),
                keywords: "note sticky".into(),
                action: Rc::new({
                    let w = w.clone();
                    move || {
                        if let Some(w) = w.upgrade() {
                            if !w.machine_view.is_remote() {
                                w.create_new_note(None, None, "");
                            }
                        }
                    }
                }),
            });
            for info in crate::tmux::detect_harnesses() {
                let key = info.key.to_string();
                let (label, icon) = crate::harness_bar::harness_label(&key);
                items.push(crate::command_palette::PaletteItem {
                    title: format!("Launch {icon} {label}"),
                    subtitle: format!("Start a new {label} terminal card"),
                    badge: "NEW".into(),
                    keywords: format!("launch spawn {key} {label}"),
                    action: Rc::new({
                        let w = w.clone();
                        move || {
                            if let Some(w) = w.upgrade() {
                                if w.ws_bar.commit_typed() {
                                    w.launch_new_terminal(&key);
                                }
                            }
                        }
                    }),
                });
            }
        }
        if let Some(act) = self.open_settings_action.borrow().clone() {
            items.push(crate::command_palette::PaletteItem {
                title: "Open Settings".into(),
                subtitle: "Launchers, connections, and shortcuts".into(),
                badge: "PREFS".into(),
                keywords: "settings config preferences".into(),
                action: act,
            });
        }
        if let Some(act) = self.hide_overlay_action.borrow().clone() {
            items.push(crate::command_palette::PaletteItem {
                title: "Hide overlay".into(),
                subtitle: "Slide SUPER DESKTOP off screen".into(),
                badge: "Esc".into(),
                keywords: "hide close dismiss".into(),
                action: act,
            });
        }

        let card_focus = focused.as_ref().map(Rc::downgrade);
        let win_weak = self.window.downgrade();
        set_overlay_keyboard_mode(&self.window, KeyboardMode::Exclusive);
        self.command_palette.open(
            "Command Palette",
            items,
            Some(Rc::new(move || {
                if let Some(c) = card_focus.as_ref().and_then(|c| c.upgrade()) {
                    c.focus_terminal();
                    return;
                }
                if let Some(win) = win_weak.upgrade() {
                    set_overlay_keyboard_mode(&win, KeyboardMode::OnDemand);
                }
            })),
        );
    }
}
