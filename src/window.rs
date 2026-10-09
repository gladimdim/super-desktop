mod cli_preferences;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Button, EventControllerFocus, EventControllerKey,
    EventControllerMotion, Fixed, Image, Label, Orientation, Overlay, Popover, PositionType,
};
use crate::desktop_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::mini_terminal::{
    clamp_card_size, displayed_pos, expanded_rect, set_displayed_pos, HoverRaiseLock,
    MiniTerminalCard, NEW_TERM_HEIGHT, NEW_TERM_WIDTH,
};
use crate::overlap_ghost::GhostLayer;
use crate::state::{AppState, NoteData, TerminalData, TopBarSize};
use crate::sticky_note::StickyNote;
use crate::tmux::create_session;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Trajectory {
    sx: f64,
    sy: f64,
    tx: f64,
    ty: f64,
}

/// Critically-damped spring (rad/s). With the launch impulse the cards are
/// within 1% of the target after about 6.5/ω, and `spring_settled` reports
/// rest after about 9/ω.
/// Appear is a touch slower so high-refresh screens get more in-between
/// frames: 99% at ~300ms (ω=15 took ~420ms, and ~570ms to settle).
const SLIDE_OMEGA_IN: f64 = 22.0;
/// Hide is stiffer so a reverse during appear still clears the screen
/// promptly: 99% off-screen at ~230ms, before `HIDE_FALLBACK` unmaps.
const SLIDE_OMEGA_OUT: f64 = 28.0;
/// Initial speed (progress / second) when starting from rest. A spring at v=0
/// eases in; this impulse makes the first frames shoot in from the edge.
const SLIDE_LAUNCH_IN: f64 = 5.5;
const SLIDE_LAUNCH_OUT: f64 = 7.5;
const SLIDE_SETTLE_X: f64 = 0.0015;
const SLIDE_SETTLE_V: f64 = 0.025;
/// Extra pixels past the HUD's own height so it is fully off-screen at progress 0.
const HUD_OFFSCREEN_PAD: f64 = 40.0;
/// Fallback HUD height before GTK has allocated it, so the first frame still
/// starts above the screen instead of at rest.
const HUD_MIN_HEIGHT: i32 = 56;
/// Hide must not wait forever on Hyprland vsync. A local LLM (LM Studio /
/// llama.cpp) can fill the GPU so frame callbacks never run, which used to
/// leave the overlay mapped after Toggle. The hide spring is 99% off-screen
/// at ~230ms; the last fraction of a percent would take another ~100ms to
/// report rest, so this unmaps on time either way.
pub(crate) const HIDE_FALLBACK: Duration = Duration::from_millis(280);

thread_local! {
    /// From the start of a hide until the next show, the overlay's keyboard
    /// and pointer belong to the desktop again (see `release_input`).
    static INPUT_RELEASED: Cell<bool> = const { Cell::new(false) };
}

/// Set the overlay's layer-shell keyboard interactivity.
///
/// Every overlay caller goes through here: releasing input makes the keyboard
/// and pointer leave the surface, and the focus and pointer handlers that
/// fire for that must not take the keyboard back while the overlay slides out.
pub(crate) fn set_overlay_keyboard_mode(window: &impl IsA<gtk4::Window>, mode: KeyboardMode) {
    if overlay_keyboard_mode_allowed(mode, INPUT_RELEASED.with(Cell::get)) {
        window.set_keyboard_mode(mode);
    }
}

fn overlay_keyboard_mode_allowed(mode: KeyboardMode, input_released: bool) -> bool {
    mode == KeyboardMode::None || !input_released
}

fn top_bar_height(size: TopBarSize) -> i32 {
    match size {
        TopBarSize::Small => 36,
        TopBarSize::Medium => 46,
        TopBarSize::Large => HUD_MIN_HEIGHT,
    }
}

/// One bar style for both the local dock and the remote one, so switching PCs
/// never changes the size of the toolbar the user is reading.
pub fn paint_top_bar_size(bar: &impl IsA<gtk4::Widget>, size: TopBarSize) {
    for class in ["hud-size-small", "hud-size-medium", "hud-size-large"] {
        bar.remove_css_class(class);
    }
    bar.add_css_class(match size {
        TopBarSize::Small => "hud-size-small",
        TopBarSize::Medium => "hud-size-medium",
        TopBarSize::Large => "hud-size-large",
    });
    bar.set_height_request(top_bar_height(size));
}

/// The controls on the right are outside every scrolling viewport. The left
/// and launcher groups share the remaining space and scroll when necessary.
pub fn top_bar_content(
    left: &impl IsA<gtk4::Widget>,
    launchers: &impl IsA<gtk4::Widget>,
    right: &impl IsA<gtk4::Widget>,
) -> gtk4::Box {
    let scroll_group = |child: &gtk4::Widget| {
        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_policy(gtk4::PolicyType::External, gtk4::PolicyType::Never);
        scroll.set_propagate_natural_width(true);
        scroll.set_child(Some(child));
        scroll
    };
    let row = gtk4::CenterBox::new();
    row.set_halign(Align::Fill);
    row.set_valign(Align::Fill);
    row.set_start_widget(Some(&scroll_group(left.as_ref())));
    row.set_center_widget(Some(&scroll_group(launchers.as_ref())));
    // Fixed allocates children at their natural size. Do not let long folder
    // names or many launchers inflate the dock beyond its requested width.
    let viewport = gtk4::ScrolledWindow::new();
    viewport.set_policy(gtk4::PolicyType::External, gtk4::PolicyType::Never);
    viewport.set_hexpand(true);
    viewport.set_vexpand(true);
    viewport.set_child(Some(&row));
    let content = gtk4::Box::new(Orientation::Horizontal, 10);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.append(&viewport);
    content.append(right);
    content
}

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

fn fit_top_bar(hud: &gtk4::Box, width: i32, brand: &Label, hint: &Label) {
    // An unmapped window has no allocation yet. Keep the initial monitor size
    // until the compositor supplies a positive logical width.
    if width <= 0 {
        return;
    }
    if hud.width_request() != width {
        hud.set_width_request(width);
    }
    // Hide's tooltip retains the shortcut when its optional label is hidden.
    hint.set_visible(width >= 1200);
    brand.set_visible(width >= 1000);
}

fn bind_top_bar_width(
    hud: &gtk4::Box,
    window: &impl IsA<gtk4::Window>,
    brand: &Label,
    hint: &Label,
    on_allocate: Rc<dyn Fn(i32, i32)>,
) {
    // The compositor's allocation is authoritative, never the canvas extents.
    //
    // Event-driven, not a tick callback: a tick callback keeps the frame clock
    // running at the display's refresh rate for as long as the overlay lives,
    // only to compare two integers. The frame clock's `layout` and
    // `after-paint` signals fire only on frames GTK produces anyway — every
    // configure (output, scale or window size change) is such a frame — and
    // never while the window is unmapped. `layout` runs after the window's
    // own allocation (its surface connected first), so the bar usually
    // follows within the same frame; `after-paint` is the safety net for a
    // frame whose layout ran before the new size was allocated.
    let fit: Rc<dyn Fn()> = {
        let window = window.as_ref().downgrade();
        let hud = hud.downgrade();
        let brand = brand.clone();
        let hint = hint.clone();
        Rc::new(move || {
            if let (Some(window), Some(hud)) = (window.upgrade(), hud.upgrade()) {
                fit_top_bar(&hud, window.width(), &brand, &hint);
                if window.width() > 0 && window.height() > 0 {
                    on_allocate(window.width(), window.height());
                }
            }
        })
    };
    type Connected = Option<(glib::WeakRef<gtk4::gdk::FrameClock>, Vec<glib::SignalHandlerId>)>;
    let connected: Rc<RefCell<Connected>> = Rc::new(RefCell::new(None));
    let disconnect: Rc<dyn Fn()> = {
        let connected = Rc::clone(&connected);
        Rc::new(move || {
            if let Some((clock, handlers)) = connected.borrow_mut().take() {
                if let Some(clock) = clock.upgrade() {
                    for handler in handlers {
                        clock.disconnect(handler);
                    }
                }
            }
        })
    };
    let connect: Rc<dyn Fn(&gtk4::Box)> = {
        let fit = Rc::clone(&fit);
        let disconnect = Rc::clone(&disconnect);
        Rc::new(move |hud: &gtk4::Box| {
            disconnect();
            let Some(clock) = hud.frame_clock() else {
                return;
            };
            let on_layout = Rc::clone(&fit);
            let on_paint = Rc::clone(&fit);
            let handlers = vec![
                clock.connect_layout(move |_| on_layout()),
                clock.connect_after_paint(move |_| on_paint()),
            ];
            *connected.borrow_mut() = Some((clock.downgrade(), handlers));
            fit();
        })
    };
    if hud.is_realized() {
        connect(hud);
    }
    // A hidden layer surface may be unrealized and come back with a new
    // frame clock: follow it.
    hud.connect_realize(move |hud| connect(hud));
    hud.connect_unrealize(move |_| disconnect());
}

/// Bidirectional slide: `progress` 0 = off-screen edge, 1 = resting on canvas.
/// Motion is a critically damped spring sampled on the GTK frame clock (the
/// Wayland surface's vsync: 60–240 Hz). Reversing only changes the target;
/// position and velocity stay, so there is no jump.
struct SlideAnim {
    /// Bumped when a new tick callback is armed so a stale one exits.
    gen: Cell<u64>,
    progress: Cell<f64>,
    velocity: Cell<f64>,
    appear: Cell<bool>,
    running: Cell<bool>,
    /// Previous `FrameClock::frame_time` (µs). 0 = first frame of this run.
    last_us: Cell<i64>,
    /// Opt-in vsync accounting for the current run (`frame_profile`).
    motion: RefCell<crate::frame_profile::Motion>,
    /// Some cards still slide with a live terminal: replace it with a still
    /// image once it has been laid out (`MiniTerminalCard::freeze_for_slide`).
    freeze_pending: Cell<bool>,
    /// The workspace size the paths in `anim_trajectories` were built for.
    /// The tick rebuilds them when the window's real size differs.
    built_for: Cell<(i32, i32)>,
}

impl SlideAnim {
    fn new() -> Self {
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
fn spring_step(x: f64, v: f64, target: f64, dt: f64, omega: f64) -> (f64, f64) {
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

fn spring_settled(x: f64, v: f64, target: f64) -> bool {
    (x - target).abs() < SLIDE_SETTLE_X && v.abs() < SLIDE_SETTLE_V
}

/// Show the stored toggle shortcut in the HUD: the trailing hint label and the
/// Hide button's tooltip. Called once at build time and again whenever the ⚙
/// Settings panel records a new combination.
fn paint_shortcut_hints(hint: &Label, btn_close: &Button, state: &AppState) {
    let combo = crate::shortcut::current_combo(state.toggle_shortcut.as_deref());
    hint.set_text(&format!("[{combo}]"));
    btn_close.set_tooltip_text(Some(&format!("Hide Super Desktop [{combo} or Esc]")));
}

pub struct SuperDesktopWindow {
    pub window: ApplicationWindow,
    canvas: Fixed,
    machine_view: Rc<crate::machine_selector::MachineView>,
    pairing_wizard: Rc<crate::peer_pairing_ui::PairingWizard>,
    /// Where pairing requests are approved or rejected. Floats above every
    /// other panel; opened by the bridge, the notification and Review buttons.
    pairing_requests: Rc<crate::pairing_request_ui::PairingRequestPanel>,
    ghost_box: gtk4::Box,
    ghost_label: Label,
    state: Rc<RefCell<AppState>>,
    /// Cards live behind `Rc` so callers can snapshot the list and drop the
    /// `RefCell` borrow before touching GTK.
    ///
    /// GTK delivers signals (pointer/focus enter, unmap, child-exit, ...)
    /// synchronously from inside our own `remove`/`expand`/`focus` calls, and
    /// those signals re-enter our handlers. Any borrow of these lists that is
    /// still alive across such a call makes the re-entrant handler panic with
    /// "RefCell already borrowed"; release builds use `panic = "abort"`, so
    /// that panic kills the whole daemon.
    note_cards: Rc<RefCell<Vec<Rc<StickyNote>>>>,
    terminal_cards: Rc<RefCell<Vec<Rc<MiniTerminalCard>>>>,
    /// Dotted outlines drawn over terminals that another card hides. Shared with
    /// the cards' callbacks, which have no `Rc<Self>` to reach the window.
    ghosts: Rc<GhostLayer>,
    hud: gtk4::Box,
    screen_width: i32,
    screen_height: i32,
    drag_pending: Rc<RefCell<HashMap<gtk4::Widget, (f64, f64)>>>,
    drag_tick_active: Rc<RefCell<bool>>,
    anim_trajectories: Rc<RefCell<HashMap<gtk4::Widget, Trajectory>>>,
    slide: Rc<SlideAnim>,
    /// Called when a hide slide reaches the edge, unless a show reversed it.
    on_slide_hidden: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    /// Toolbar brand icons as (image widget, agent key) for theme-aware refresh.
    brand_images: Rc<RefCell<Vec<(Image, String)>>>,
    /// Rebuilds the ⚙ settings panel (detection + brand logos for the new
    /// light/dark mode) after a theme switch.
    settings_refresh: Rc<dyn Fn()>,
    cli_harness_bar: Rc<RefCell<Option<Rc<crate::harness_bar::HarnessBar>>>>,
    /// Floating panels (the ⚙ settings card) inside `root_overlay`; hidden with
    /// the window so they cannot reappear on the next show.
    overlay_panels: Vec<gtk4::Widget>,
    /// The workspace field's ▾ list. A popover is its own Wayland surface, so
    /// hiding the window does not dismiss it: it must be popped down
    /// explicitly, or it stays on screen over an empty desktop.
    ws_popover: Popover,
    /// The local folder field, so a viewer's folder change shows up in it.
    ws_bar: crate::workspace_bar::WorkspaceBar,
    /// Bumped by `show_again`; a slide-out that finishes afterwards must not
    /// unmap the window again (hide → show inside the 140ms animation).
    show_token: std::cell::Cell<u64>,
    /// Pauses the local cards' tmux clients while the overlay stays hidden.
    hidden_pause: Rc<crate::hidden_pause::Controller>,
    /// After a new harness is spawned, hover-raise on other cards is ignored
    /// until this hold expires so the pointer path cannot bury the new card.
    hover_raise_lock: HoverRaiseLock,
    terminal_picker: RefCell<Option<Rc<crate::terminal_picker::Picker>>>,
    /// Keeps the ⚙ Settings card's placement alive: the overlay asks it where
    /// the card goes and how big it is, and stretches the card over the whole
    /// screen once it is dropped.
    settings_layout: RefCell<Option<Rc<crate::floating_panel::MovablePanel>>>,
    /// The welcome tour: opened once by the first show after a fresh install,
    /// and again from ⚙ Settings or `super-desktop tour`.
    welcome_tour: Rc<crate::welcome_tour::WelcomeTour>,
    /// This window, for the slide's frame tick, which rebuilds the paths once
    /// the window has its real size.
    this: std::cell::OnceCell<std::rc::Weak<Self>>,
}

/// Use compositor-allocated logical pixels; monitor zero is only a startup fallback.
fn allocated_workspace_size(window: &impl IsA<gtk4::Window>, fallback: (i32, i32)) -> (i32, i32) {
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
fn allocated_size(window: &impl IsA<gtk4::Window>) -> Option<(i32, i32)> {
    let window = window.as_ref();
    (window.width() > 0 && window.height() > 0).then(|| (window.width(), window.height()))
}

/// The size to build slide paths for while the window has no size yet: the
/// widest and tallest monitor. A card parked past that right edge is off
/// screen on whichever monitor the compositor picks; monitor 0 alone (the
/// startup fallback) can be a laptop panel narrower than the screen the
/// overlay opens on, which started right-hand cards in the middle of it.
fn provisional_slide_size(fallback: (i32, i32)) -> (i32, i32) {
    let Some(display) = gdk::Display::default() else { return fallback };
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_then(|m| m.downcast::<gdk::Monitor>().ok()))
        .map(|m| m.geometry())
        .fold(fallback, |(w, h), geo| (w.max(geo.width()), h.max(geo.height())))
}

impl SuperDesktopWindow {
    fn screen_width(&self) -> i32 {
        allocated_workspace_size(&self.window, (self.screen_width, self.screen_height)).0
    }

    fn screen_height(&self) -> i32 {
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
                    let snapshot = {
                        let mut s = state.borrow_mut();
                        s.visible_harnesses = Some(keys.clone());
                        s.clone()
                    };
                    crate::state::save_state_async(snapshot.clone());
                    // The same call a remote view makes with a host's list: the
                    // bar only ever reflects what its owner says is offered.
                    if let Some(bar) = bar_slot.borrow().as_ref() {
                        bar.apply(&crate::harness_bar::HarnessState {
                            keys,
                            custom: snapshot.custom_harnesses.iter().map(Into::into).collect(),
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
                    let snapshot = {
                        let mut s = state.borrow_mut();
                        s.toggle_shortcut = Some(combo);
                        s.clone()
                    };
                    paint_shortcut_hints(&hint, &btn_close, &snapshot);
                    crate::state::save_state_async(snapshot);
                }
            }),
            Rc::new({
                let state = Rc::clone(&state);
                let hud_for_settings = Rc::clone(&hud_for_settings);
                let machine_for_settings = Rc::clone(&machine_for_settings);
                move |size: TopBarSize| {
                    let snapshot = {
                        let mut s = state.borrow_mut();
                        s.top_bar_size = size;
                        s.clone()
                    };
                    if let Some(hud) = hud_for_settings.borrow().as_ref() {
                        paint_top_bar_size(hud, size);
                    }
                    if let Some(view) = machine_for_settings.borrow().as_ref() {
                        view.paint_top_bar_size(size);
                    }
                    crate::state::save_state_async(snapshot);
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
        hud_left.append(&machine_view.local_button);

        let brand = Label::new(Some("⚡ SUPER DESKTOP"));
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
            overlay_panels: vec![settings_panel.widget.clone(), welcome_tour.widget.clone().upcast()],
            ws_popover: workspace_bar.popover.clone(),
            ws_bar: workspace_bar.clone(),
            show_token: std::cell::Cell::new(0),
            hidden_pause: crate::hidden_pause::Controller::new(),
            hover_raise_lock: HoverRaiseLock::new(),
            terminal_picker: RefCell::new(None),
            settings_layout: RefCell::new(None),
            welcome_tour: Rc::clone(&welcome_tour),
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

        // Harnesses (company logo + name; emoji label if the SVG is missing).
        // The bar is the same widget a remote PC's workspace shows: it is told
        // which keys this machine offers and what a click means, and everything
        // else about it — order, labels, logos, tooltips, the launch line — is
        // shared code. Driven by `HARNESS_KEYS` so the launch buttons and the ⚙
        // settings panel can never disagree about what this app can run; the
        // visible subset comes from the stored selection ∩ what is installed.
        let detected = crate::tmux::detect_harnesses();
        let visible_keys = crate::harness_settings::visible_keys(&win_rc.state.borrow(), &detected);
        let light_theme = crate::theme::current_theme().mode == "light";
        let harness_bar = crate::harness_bar::HarnessBar::new(
            Rc::new({
                let win_w = Rc::downgrade(&win_rc);
                move |key: &str| {
                    if let Some(w) = win_w.upgrade() {
                        w.launch_new_terminal(key);
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
                motion.connect_enter(move |_, _, _| {
                    // Rebuilt on every hover so numbers are fresh from disk.
                    let content = crate::usage::build_usage_content(&card);
                    pop_enter.set_child(Some(&content));
                    pop_enter.popup();
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
        *harness_bar_for_settings.borrow_mut() = Some(Rc::clone(&harness_bar));
        // The same brand logos the remote bar draws, so a light/dark switch
        // swaps both from the one list.
        brand_images
            .borrow_mut()
            .extend(harness_bar.brand_images());
        hud.append(&top_bar_content(&hud_left, &harness_bar.group, &hud_right));

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

        // Close (its tooltip already names the current shortcut — see
        // `paint_shortcut_hints`).
        let on_close_btn = Rc::clone(&on_close_rc);
        btn_close.connect_clicked(move |_| {
            on_close_btn();
        });
        hud_right.append(&btn_close);

        hud_right.append(&hint);

        // Layer-shell chooses the output. Its allocated logical width is the
        // authority, including after a monitor or scale change.
        bind_top_bar_width(&hud, &win_rc.window, &brand, &hint, Rc::new({
            let owner = Rc::downgrade(&win_rc);
            let previous = Cell::new((0, 0));
            move |width, height| {
                if previous.replace((width, height)) == (width, height) {
                    return;
                }
                if let Some(owner) = owner.upgrade() {
                    let cards = owner.terminal_cards.borrow().clone();
                    for card in cards {
                        card.set_workspace_size(width, height);
                    }
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

        // Added after the HUD so the settings card floats above it. It moves
        // by its header and resizes by its edges like a terminal card, and
        // never over the bar. Where and how big the user left it is kept.
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
            },
            Rc::new({
                let state = Rc::clone(&win_rc.state);
                move || top_bar_height(state.borrow().top_bar_size)
            }),
            Rc::new({
                let state = Rc::clone(&win_rc.state);
                move |position, size| {
                    let snapshot = {
                        let mut s = state.borrow_mut();
                        s.settings_panel_pos = Some(position);
                        s.settings_panel_size = Some(size);
                        s.clone()
                    };
                    crate::state::save_state_async(snapshot);
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

        // Esc key
        let key_ctrl = EventControllerKey::new();
        let on_close_key = Rc::clone(&on_close_rc);
        let win_w = Rc::downgrade(&win_rc);
        let ws_popover = workspace_bar.popover.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, state| {
            if key == gdk::Key::Escape {
                if let Some(w) = win_w.upgrade() {
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

    fn load_items(&self) {
        let mut terminals: Vec<TerminalData> = self.state.borrow().terminals.clone();
        let order = self.state.borrow().terminal_order.clone();
        terminals.sort_by_key(|t| order.iter().position(|id| id == &t.id).unwrap_or(usize::MAX));
        let inventory = std::sync::Arc::new(crate::tmux::SessionInventory::default());
        // One state.json read for the whole batch, not one per card.
        let custom = (!terminals.is_empty())
            .then(|| Rc::new(crate::state::load_state().custom_harnesses));
        for term_data in terminals {
            self.spawn_terminal_widget(
                term_data,
                false,
                Some(std::sync::Arc::clone(&inventory)),
                custom.clone(),
            );
        }

        let notes: Vec<NoteData> = self.state.borrow().notes.clone();
        for note_data in notes {
            self.spawn_note_widget(note_data, false);
        }

        self.raise_all_notes();
        raise_canvas_child(&self.canvas, &self.hud);
    }

    pub fn create_new_note(&self, x: Option<i32>, y: Option<i32>, text: &str) {
        let idx = self.note_cards.borrow().len();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();

        let nx = x.unwrap_or(80 + (idx as i32 % 4) * 50);
        let ny = y.unwrap_or(140 + (idx as i32 % 3) * 60);

        let data = NoteData {
            id: format!("note_{}", now),
            text: if text.is_empty() { "New sticky note...".to_string() } else { text.to_string() },
            x: nx,
            y: ny,
            width: 260,
            height: 200,
            color: "omarchy".to_string(),
            updated_at: now as f64 / 1000.0,
            tag: 0,
        };

        self.spawn_note_widget(data, true);
    }

    fn spawn_note_widget(&self, note_data: NoteData, save: bool) {
        let canvas = self.canvas.clone();
        let state = Rc::clone(&self.state);
        let note_cards = Rc::clone(&self.note_cards);

        let drag_pending_update = Rc::clone(&self.drag_pending);
        let drag_tick_active = Rc::clone(&self.drag_tick_active);
        let sw = self.screen_width();
        let sh = self.screen_height();

        let canvas_for_tick = canvas.clone();
        let drag_window = self.window.downgrade();
        let on_drag_update = move |widget: gtk4::Widget, x: f64, y: f64| {
            let (sw, sh) = drag_window.upgrade().map(|w| allocated_workspace_size(&w, (sw, sh))).unwrap_or((sw, sh));
            // Perf: .dragging disables hover transitions/shadows (see CSS)
            // so the note paints cheaply while it moves at 120Hz.
            if !widget.has_css_class("dragging") {
                widget.add_css_class("dragging");
            }
            let cx = x.clamp(10.0, (sw - 80).max(10) as f64);
            let cy = y.clamp(70.0, (sh - 60).max(70) as f64);
            drag_pending_update.borrow_mut().insert(widget, (cx, cy));

            if !*drag_tick_active.borrow() {
                *drag_tick_active.borrow_mut() = true;
                let dp = Rc::clone(&drag_pending_update);
                let dta = Rc::clone(&drag_tick_active);
                let c = canvas_for_tick.clone();

                canvas_for_tick.add_tick_callback(move |_, _| {
                    if dp.borrow().is_empty() {
                        *dta.borrow_mut() = false;
                        return glib::ControlFlow::Break;
                    }
                    let items: Vec<(gtk4::Widget, (f64, f64))> = dp.borrow_mut().drain().collect();
                    for (w, (px, py)) in items {
                        c.move_(&w, px, py);
                    }
                    glib::ControlFlow::Continue
                });
            }
        };

        let canvas_note_end = canvas.clone();
        let drag_pending_note_end = Rc::clone(&self.drag_pending);
        let state_end = Rc::clone(&state);
        let drag_window = self.window.downgrade();
        let on_drag_end = move |widget: gtk4::Widget, data: &NoteData| {
            let (sw, sh) = drag_window.upgrade().map(|w| allocated_workspace_size(&w, (sw, sh))).unwrap_or((sw, sh));
            widget.remove_css_class("dragging");
            drag_pending_note_end.borrow_mut().remove(&widget);
            let mut final_data = data.clone();
            final_data.x = final_data.x.clamp(10, (sw - 80).max(10));
            final_data.y = final_data.y.clamp(70, (sh - 60).max(70));
            canvas_note_end.move_(&widget, final_data.x as f64, final_data.y as f64);
            let mut s = state_end.borrow_mut();
            if let Some(n) = s.notes.iter_mut().find(|n| n.id == final_data.id) {
                *n = final_data;
            } else {
                s.notes.push(final_data);
            }
            // Perf: serialize + write the JSON off the main thread so the
            // drag-end frame completes within the 8.3ms 120Hz budget.
            let snapshot = s.clone();
            drop(s);
            crate::state::save_state_async(snapshot);
        };

        let canvas_del = canvas.clone();
        let state_del = Rc::clone(&state);
        let note_cards_del = Rc::clone(&note_cards);

        let on_delete = move |id: String| {
            // Take the note out of the shared list and drop the borrow BEFORE
            // touching GTK: `canvas.remove` unmaps the note subtree, which
            // emits pointer/focus signals that re-enter the raise handler.
            let note = {
                let mut cards = note_cards_del.borrow_mut();
                cards
                    .iter()
                    .position(|c| c.data.borrow().id == id)
                    .map(|pos| cards.remove(pos))
            };
            let Some(note) = note else { return };

            canvas_del.remove(&note.container);
            let mut s = state_del.borrow_mut();
            s.notes.retain(|n| n.id != id);
            let snapshot = s.clone();
            drop(s);
            crate::state::save_state_async(snapshot);
        };

        let state_change = Rc::clone(&state);
        let on_change = move |data: &NoteData| {
            let mut s = state_change.borrow_mut();
            if let Some(n) = s.notes.iter_mut().find(|n| n.id == data.id) {
                *n = data.clone();
            }
            // Perf: typing already debounces 300ms; the remaining JSON
            // serialize + file write goes to a worker thread.
            let snapshot = s.clone();
            drop(s);
            crate::state::save_state_async(snapshot);
        };

        let canvas_raise = canvas.clone();
        let hud_raise = self.hud.clone();
        let note_cards_raise = Rc::clone(&note_cards);
        let note_id = note_data.id.clone();
        let on_raise = move |widget: gtk4::Widget| {
            if let Some(last) = canvas_raise.last_child() {
                if &last != &widget {
                    widget.insert_after(&canvas_raise, Some(&last));
                }
            }
            raise_canvas_child(&canvas_raise, &hud_raise);
            // GTK can call this while another handler is still holding the
            // list borrow (e.g. during a widget removal). The z-order above
            // already happened, so just skip the bookkeeping instead of
            // panicking on an active borrow.
            let Ok(mut cards) = note_cards_raise.try_borrow_mut() else {
                return;
            };
            if let Some(pos) = cards.iter().position(|c| c.data.borrow().id == note_id) {
                let note = cards.remove(pos);
                cards.push(note);
            }
        };

        let x = note_data.x;
        let y = note_data.y;

        if save {
            self.state.borrow_mut().notes.push(note_data.clone());
            crate::state::save_state_async(self.state.borrow().clone());
        }

        let ghost = self.ghost_box.clone();
        let ghost_lbl = self.ghost_label.clone();
        let canvas_ghost = canvas.clone();
        let ghost_last: Rc<RefCell<Option<(f64, f64, i32, i32)>>> =
            Rc::new(RefCell::new(None));
        let ghost_last_show = Rc::clone(&ghost_last);
        let on_resize_ghost = move |x: f64, y: f64, w: i32, h: i32| {
            let qx = x.round();
            let qy = y.round();
            if *ghost_last_show.borrow() == Some((qx, qy, w, h)) {
                return;
            }
            let previous = *ghost_last_show.borrow();
            *ghost_last_show.borrow_mut() = Some((qx, qy, w, h));

            if !ghost.is_visible() {
                if ghost.parent().is_some() {
                    canvas_ghost.remove(&ghost);
                }
                canvas_ghost.put(&ghost, qx, qy);
            } else if previous.map(|(x, y, _, _)| (x, y)) != Some((qx, qy)) {
                canvas_ghost.move_(&ghost, qx, qy);
            }
            ghost.set_visible(true);
            if previous.map(|(_, _, w, h)| (w, h)) != Some((w, h)) {
                ghost.set_size_request(w, h);
            }
            ghost.remove_css_class("ghost-icon");
            ghost.add_css_class("ghost-note");
            let label = format!("📝 {w} × {h} (Note)");
            if ghost_lbl.label().as_str() != label {
                ghost_lbl.set_label(&label);
            }
        };

        let ghost_end = self.ghost_box.clone();
        let ghost_last_hide = Rc::clone(&ghost_last);
        let on_resize_end = move || {
            *ghost_last_hide.borrow_mut() = None;
            ghost_end.set_visible(false);
        };

        let note = StickyNote::new(
            note_data,
            on_drag_update,
            on_drag_end,
            on_delete,
            on_change,
            on_raise,
            on_resize_ghost,
            on_resize_end,
            sw,
            sh,
        );
        canvas.put(&note.container, x as f64, y as f64);
        note_cards.borrow_mut().push(Rc::new(note));
        raise_canvas_child(&canvas, &self.hud);
    }

    pub fn create_new_terminal(&self, agent_type: &str, cmd: Option<&str>, x: Option<i32>, y: Option<i32>) -> String {
        self.create_new_terminal_in(agent_type, cmd, x, y, None)
    }

    pub fn workspace_choices(&self) -> serde_json::Value {
        let state = self.state.borrow();
        serde_json::json!({"workspace": crate::state::effective_workspace_dir(&state),
            "recentDirectories": state.recent_dirs,
            "usedDirectories": state.used_dirs})
    }

    /// Create a card and its tmux session; the session exists on return, so
    /// callers can report it (IPC, a viewer's request).
    pub fn create_new_terminal_in(&self, agent_type: &str, cmd: Option<&str>, x: Option<i32>, y: Option<i32>, directory: Option<&str>) -> String {
        self.new_terminal_card(agent_type, cmd, x, y, directory, true)
    }

    /// The top bar's launch: the card is on screen at the next frame and its
    /// session is started by the card's first attach, on a worker. Creating it
    /// here held GTK for one tmux process (several, before they were chained).
    pub fn launch_new_terminal(&self, agent_type: &str) {
        self.new_terminal_card(agent_type, None, None, None, None, false);
    }

    fn new_terminal_card(&self, agent_type: &str, cmd: Option<&str>, x: Option<i32>, y: Option<i32>, directory: Option<&str>, wait_for_session: bool) -> String {
        crate::frame_profile::note_request("new terminal");
        // The folder from the top bar field: this card's harness starts there,
        // and keeps it for its whole life (see TerminalData::workspace_dir).
        let workspace_dir = directory.map(str::to_owned)
            .unwrap_or_else(|| crate::state::effective_workspace_dir(&self.state.borrow()));
        crate::state::remember_workspace_dir(&mut self.state.borrow_mut(), &workspace_dir);
        // Same folder, same label colour (see `folder_colors`).
        let tag = {
            let mut state = self.state.borrow_mut();
            let tag = crate::folder_colors::tag_for_new_card(&state, &workspace_dir);
            crate::folder_colors::remember(&mut state, &workspace_dir, tag);
            tag
        };
        let custom_command = self.state.borrow().custom_harnesses.iter()
            .find(|item| item.id == agent_type && item.validate().is_ok())
            .map(|item| item.command());
        let command = custom_command.as_deref().or(cmd);
        // Default size for a new harness: 640x480, clamped to the screen.
        let (def_w, def_h) =
            clamp_card_size(NEW_TERM_WIDTH, NEW_TERM_HEIGHT, self.screen_width(), self.screen_height());
        // A card too narrow to attach has no worker to start its session.
        let (sess, cmd_run, inventory) = if wait_for_session || def_w < crate::mini_terminal::MIN_CARD_WIDTH {
            let (sess, cmd_run) = create_session(agent_type, command, Some(&workspace_dir));
            (sess, cmd_run, None)
        } else {
            let cmd_run = crate::tmux::resolve_command(agent_type, command);
            let sess = super_desktop::session_id::candidate();
            let fresh = crate::tmux::SessionInventory::fresh(&sess, agent_type, &cmd_run, &workspace_dir);
            (sess, cmd_run, Some(std::sync::Arc::new(fresh)))
        };
        let idx = self.terminal_cards.borrow().len();

        // Center on screen; cascade slightly so stacked harnesses don't overlap exactly.
        let cascade = (idx as i32 % 5) * 32;
        let cx = ((self.screen_width() - def_w) / 2 + cascade)
            .clamp(10, (self.screen_width() - def_w - 10).max(10));
        let cy = ((self.screen_height() - def_h) / 2 + cascade)
            .clamp(70, (self.screen_height() - def_h - 10).max(70));

        let nx = x.unwrap_or(cx);
        let ny = y.unwrap_or(cy);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let data = TerminalData {
            id: sess.clone(),
            session_name: sess.clone(),
            agent_type: agent_type.to_string(),
            command: cmd_run,
            x: nx,
            y: ny,
            width: def_w,
            height: def_h,
            restored_width: def_w,
            restored_height: def_h,
            iconified: false,
            icon_x: None,
            icon_y: None,
            created_at: now,
            tag,
            agent_session_id: None,
            workspace_dir: Some(workspace_dir),
        };

        self.spawn_terminal_widget(data, true, inventory, None);
        sess
    }

    /// Adopt an already launched CLI session without mapping or focusing the overlay.
    pub(crate) fn adopt_cli_terminal(&self, mut data: TerminalData) -> Result<(), ()> {
        {
            let state = self.state.borrow();
            if state.terminals.len() >= crate::workspace_model::MAX_DESKTOP_CARDS
                || state
                    .terminals
                    .iter()
                    .any(|t| t.id == data.id || t.session_name == data.session_name)
            {
                return Err(());
            }
        }
        let (width, height) = clamp_card_size(
            NEW_TERM_WIDTH,
            NEW_TERM_HEIGHT,
            self.screen_width(),
            self.screen_height(),
        );
        let cascade = (self.terminal_cards.borrow().len() as i32 % 5) * 32;
        data.x = ((self.screen_width() - width) / 2 + cascade)
            .clamp(10, (self.screen_width() - width - 10).max(10));
        data.y = ((self.screen_height() - height) / 2 + cascade)
            .clamp(70, (self.screen_height() - height - 10).max(70));
        data.width = width;
        data.height = height;
        data.restored_width = width;
        data.restored_height = height;
        {
            let mut state = self.state.borrow_mut();
            if let Some(directory) = &data.workspace_dir {
                crate::state::remember_workspace_dir(&mut state, directory);
                data.tag = crate::folder_colors::tag_for_new_card(&state, directory);
                crate::folder_colors::remember(&mut state, directory, data.tag);
            }
            state.terminals.push(data.clone());
            crate::state::normalize_terminal_order(&mut state);
            crate::state::save_state_async(state.clone());
        }
        let inventory = std::sync::Arc::new(crate::tmux::SessionInventory::cli_created(
            &data.session_name,
        ));
        self.spawn_terminal_widget(data, false, Some(inventory), None);
        Ok(())
    }

    fn spawn_terminal_widget(
        &self,
        term_data: TerminalData,
        save: bool,
        startup_inventory: Option<std::sync::Arc<crate::tmux::SessionInventory>>,
        custom_harnesses: Option<Rc<Vec<crate::custom_harness::CustomHarness>>>,
    ) {
        let canvas = self.canvas.clone();
        let state = Rc::clone(&self.state);
        let term_cards = Rc::clone(&self.terminal_cards);
        // This card's callbacks have no `Rc<Self>`, so they hold the ghost
        // layer directly and redraw the buried-card outlines themselves.
        let ghosts = Rc::clone(&self.ghosts);

        let drag_pending_update = Rc::clone(&self.drag_pending);
        let drag_tick_active = Rc::clone(&self.drag_tick_active);
        let sw = self.screen_width();
        let sh = self.screen_height();

        let canvas_for_tick = canvas.clone();
        let ghosts_drag = Rc::clone(&ghosts);
        let drag_window = self.window.downgrade();
        let on_drag_update = move |widget: gtk4::Widget, x: f64, y: f64| {
            let (sw, sh) = drag_window.upgrade().map(|w| allocated_workspace_size(&w, (sw, sh))).unwrap_or((sw, sh));
            // Perf: .dragging disables hover transitions/shadows (see CSS)
            // so the card paints cheaply while it moves at 120Hz.
            if !widget.has_css_class("dragging") {
                widget.add_css_class("dragging");
            }
            let cx = x.clamp(10.0, (sw - 80).max(10) as f64);
            let cy = y.clamp(70.0, (sh - 60).max(70) as f64);
            drag_pending_update.borrow_mut().insert(widget, (cx, cy));

            if !*drag_tick_active.borrow() {
                *drag_tick_active.borrow_mut() = true;
                let dp = Rc::clone(&drag_pending_update);
                let dta = Rc::clone(&drag_tick_active);
                let c = canvas_for_tick.clone();
                let ghosts_tick = Rc::clone(&ghosts_drag);

                canvas_for_tick.add_tick_callback(move |_, _| {
                    if dp.borrow().is_empty() {
                        *dta.borrow_mut() = false;
                        return glib::ControlFlow::Break;
                    }
                    let items: Vec<(gtk4::Widget, (f64, f64))> = dp.borrow_mut().drain().collect();
                    for (w, (px, py)) in items {
                        c.move_(&w, px, py);
                    }
                    // A card dragged over another one hides it: keep that
                    // card's ghost outline following the drag.
                    ghosts_tick.refresh();
                    glib::ControlFlow::Continue
                });
            }
        };

        let canvas_term_end = canvas.clone();
        let drag_pending_term_end = Rc::clone(&self.drag_pending);
        let state_end = Rc::clone(&state);
        let ghosts_end = Rc::clone(&ghosts);
        let drag_window = self.window.downgrade();
        let on_drag_end = move |widget: gtk4::Widget, data: &TerminalData| {
            let (sw, sh) = drag_window.upgrade().map(|w| allocated_workspace_size(&w, (sw, sh))).unwrap_or((sw, sh));
            widget.remove_css_class("dragging");
            drag_pending_term_end.borrow_mut().remove(&widget);
            let mut final_data = data.clone();
            // The icon and the expanded card have separate remembered spots;
            // clamp and snap to whichever one this card is currently in.
            if final_data.iconified {
                final_data.icon_x = Some(final_data.icon_x.unwrap_or(final_data.x).clamp(10, (sw - 80).max(10)));
                final_data.icon_y = Some(final_data.icon_y.unwrap_or(final_data.y).clamp(70, (sh - 60).max(70)));
            } else {
                final_data.x = final_data.x.clamp(10, (sw - 80).max(10));
                final_data.y = final_data.y.clamp(70, (sh - 60).max(70));
            }
            let (px, py) = displayed_pos(&final_data);
            canvas_term_end.move_(&widget, px, py);
            let mut s = state_end.borrow_mut();
            // A colour picked on the card's dot becomes its folder's colour.
            let previous = s.terminals.iter().find(|t| t.session_name == final_data.session_name).cloned();
            crate::folder_colors::note_card_saved(&mut s, previous.as_ref(), &final_data);
            if let Some(t) = s.terminals.iter_mut().find(|t| t.session_name == final_data.session_name) {
                *t = final_data;
            } else {
                s.terminals.push(final_data);
            }
            // Perf: keep the drag-end frame inside the 8.3ms 120Hz budget.
            let snapshot = s.clone();
            drop(s);
            crate::state::save_state_async(snapshot);
            // A card that just landed on another one buries it (and an iconify
            // or restore commit arrives through this same callback).
            ghosts_end.refresh();
        };

        let terminal_cards_toggle = Rc::clone(&term_cards);
        let canvas_toggle = canvas.clone();
        let window_toggle = self.window.clone();
        let ghosts_toggle = Rc::clone(&ghosts);
        let on_double_click = move |data: &TerminalData| {
            apply_terminal_expand(
                &terminal_cards_toggle,
                &canvas_toggle,
                &window_toggle,
                &ghosts_toggle,
                sw,
                sh,
                &data.session_name,
            );
        };

        let canvas_del = canvas.clone();
        let state_del = Rc::clone(&state);
        let term_cards_del = Rc::clone(&term_cards);
        let ghosts_del = Rc::clone(&ghosts);

        let on_close = move |sess: String| {
            // Pull the card out of the shared list and drop the borrow BEFORE
            // touching GTK. `canvas.remove` unparents the whole card subtree
            // (VTE included), and that unmap emits pointer/focus-enter signals
            // which synchronously re-enter the raise handler of the remaining
            // cards. Holding the list borrow across it aborted the daemon with
            // "RefCell already borrowed" (see the field docs above).
            let card = {
                let mut cards = term_cards_del.borrow_mut();
                cards
                    .iter()
                    .position(|c| c.data.borrow().session_name == sess)
                    .map(|pos| cards.remove(pos))
            };
            let Some(card) = card else { return };

            card.close_session();
            canvas_del.remove(&card.container);
            crate::desktop_shell::terminal_removed(&canvas_del);
            let mut s = state_del.borrow_mut();
            s.terminals.retain(|t| t.session_name != sess);
            crate::state::normalize_terminal_order(&mut s);
            let snapshot = s.clone();
            drop(s);
            crate::state::save_state_async(snapshot);
            // The closed card's outline is dropped and the cards it used to
            // hide become visible again.
            ghosts_del.refresh();
        };

        // Restored cards reappear wherever their current mode lives: an
        // iconified card belongs at its remembered icon spot, not at the
        // expanded card position.
        let (x, y) = displayed_pos(&term_data);

        if save {
            self.state.borrow_mut().terminals.push(term_data.clone());
            crate::state::normalize_terminal_order(&mut self.state.borrow_mut());
            crate::state::save_state_async(self.state.borrow().clone());
        }

        let ghost = self.ghost_box.clone();
        let ghost_lbl = self.ghost_label.clone();
        let canvas_ghost = canvas.clone();
        // Perf: resize motion events arrive far faster than the 120Hz frame
        // clock. Coalesce them here: skip no-op updates and only touch
        // size-request / label / CSS classes when that aspect changed.
        // Each of those calls queues a relayout/restyle otherwise.
        let ghost_last: Rc<RefCell<Option<(f64, f64, i32, i32, bool)>>> =
            Rc::new(RefCell::new(None));
        let ghost_last_show = Rc::clone(&ghost_last);
        let on_resize_ghost = move |x: f64, y: f64, w: i32, h: i32, is_icon: bool| {
            let qx = x.round();
            let qy = y.round();
            if let Some((lx, ly, lw, lh, li)) = *ghost_last_show.borrow() {
                if lx == qx && ly == qy && lw == w && lh == h && li == is_icon {
                    return;
                }
            }
            let prev = ghost_last_show.borrow().clone();
            let pos_changed = prev.map(|(lx, ly, _, _, _)| lx != qx || ly != qy).unwrap_or(true);
            let size_changed = prev.map(|(_, _, lw, lh, _)| lw != w || lh != h).unwrap_or(true);
            let mode_changed = prev.map(|(_, _, _, _, li)| li != is_icon).unwrap_or(true);
            *ghost_last_show.borrow_mut() = Some((qx, qy, w, h, is_icon));

            if !ghost.is_visible() {
                if ghost.parent().is_some() {
                    canvas_ghost.remove(&ghost);
                }
                canvas_ghost.put(&ghost, qx, qy);
            } else if pos_changed {
                canvas_ghost.move_(&ghost, qx, qy);
            }
            ghost.set_visible(true);
            if size_changed {
                ghost.set_size_request(w, h);
            }
            if mode_changed {
                if is_icon {
                    ghost.add_css_class("ghost-icon");
                } else {
                    ghost.remove_css_class("ghost-icon");
                }
            }
            ghost.remove_css_class("ghost-note");
            let text = if is_icon {
                "🗕 128 × 128 (Icon)".to_string()
            } else {
                format!("💻 {w} × {h} (Terminal)")
            };
            if ghost_lbl.label().as_str() != text.as_str() {
                ghost_lbl.set_label(&text);
            }
        };

        let ghost_end = self.ghost_box.clone();
        let ghost_last_hide = Rc::clone(&ghost_last);
        let on_resize_end = move || {
            *ghost_last_hide.borrow_mut() = None;
            if ghost_end.is_visible() {
                ghost_end.set_visible(false);
            }
        };

        let canvas_raise = canvas.clone();
        let hud_raise = self.hud.clone();
        let term_cards_raise = Rc::clone(&term_cards);
        let sess_name = term_data.session_name.clone();
        let card_id_raise = term_data.id.clone();
        let state_raise = Rc::clone(&state);
        let ghosts_raise = Rc::clone(&ghosts);
        let on_raise = move |widget: gtk4::Widget| {
            if let Some(last) = canvas_raise.last_child() {
                if &last != &widget {
                    widget.insert_after(&canvas_raise, Some(&last));
                }
            }
            raise_canvas_child(&canvas_raise, &hud_raise);
            // GTK can call this while another handler is still holding the
            // list borrow (e.g. during a widget removal). The z-order above
            // already happened, so just skip the bookkeeping instead of
            // panicking on an active borrow. The ghosts are recomputed below,
            // where the lists are free again.
            let Ok(mut cards) = term_cards_raise.try_borrow_mut() else {
                return;
            };
            if let Some(pos) = cards.iter().position(|c| c.data.borrow().session_name == sess_name) {
                let card = cards.remove(pos);
                cards.push(card);
            }
            drop(cards);
            let mut state = state_raise.borrow_mut();
            let persisted = state.terminal_order.last() != Some(&card_id_raise)
                && state.terminals.iter().any(|t| t.id == card_id_raise);
            if persisted {
                state.terminal_order.retain(|id| id != &card_id_raise);
                state.terminal_order.push(card_id_raise.clone());
                let snapshot = state.clone();
                drop(state);
                crate::state::save_state_async(snapshot);
            } else {
                drop(state);
            }
            // A raise changes which card covers which: the buried ones may now
            // need an outline (or lost the one they had).
            ghosts_raise.refresh();
        };

        let state_sess = Rc::clone(&state);
        let on_session_persist = move |updated: &TerminalData| {
            let mut s = state_sess.borrow_mut();
            if let Some(slot) = s
                .terminals
                .iter_mut()
                .find(|t| t.session_name == updated.session_name)
            {
                // Only the agent mapping changed here; keep geometry from the
                // live card to avoid clobbering an in-progress drag/resize.
                slot.agent_session_id = updated.agent_session_id.clone();
                let snapshot = s.clone();
                drop(s);
                crate::state::save_state_async(snapshot);
            }
        };

        let hover_lock = self.hover_raise_lock.clone();
        if save {
            // Hold hover-raise on every other card so moving the pointer
            // toward this newly opened harness cannot bury it.
            hover_lock.lock(&term_data.session_name);
        }
        let ghosts_interaction = Rc::clone(&ghosts);
        let on_interaction = move || ghosts_interaction.refresh();
        let card = MiniTerminalCard::new(
            term_data,
            on_drag_update,
            on_drag_end,
            on_double_click,
            on_close,
            on_resize_ghost,
            on_resize_end,
            on_raise,
            on_session_persist,
            on_interaction,
            sw,
            sh,
            startup_inventory,
            custom_harnesses,
            hover_lock,
            // The local workspace's own session: this machine's tmux, attached
            // by the card's emulator.
            crate::card_source::CardSource::Local,
        );
        let card = Rc::new(card);
        card.keyboard_digit.set(crate::terminal_picker::next_digit(
            term_cards.borrow().iter().filter_map(|card| card.keyboard_digit.get())
        ));
        canvas.put(&card.container, x, y);
        if save {
            card.focus_terminal();
        }
        term_cards.borrow_mut().push(Rc::clone(&card));
        raise_canvas_child(&canvas, &self.hud);
        // A new card can land on top of an existing one (they cascade 32px):
        // its outline shows up right away while the user works in neither.
        ghosts.refresh();
    }

    pub fn auto_arrange(&self) {
        // Snapshot the cards (cheap `Rc` clones) and drop both list borrows
        // before moving anything: `collapse`/`move_` emit signals that
        // re-enter the raise handlers.
        let terms: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        let notes: Vec<Rc<StickyNote>> = self.note_cards.borrow().clone();

        for term in terms.iter() {
            if term.is_expanded() {
                term.collapse();
                let (px, py) = displayed_pos(&term.data.borrow());
                self.canvas.move_(&term.container, px, py);
            }
        }
        set_overlay_keyboard_mode(&self.window, KeyboardMode::OnDemand);

        let margin = 12;
        let gap = 12;
        let top = self.hud.height().max(top_bar_height(self.state.borrow().top_bar_size)) + gap;
        let width = (self.screen_width() - 2 * margin).max(1);
        let height = (self.screen_height() - top - margin).max(1);
        // Keep notes in their own lane; terminal geometry never depends on
        // saved card extents or a previously connected output.
        let note_width = if notes.is_empty() { 0 } else {
            notes.iter().map(|n| n.data.borrow().width).max().unwrap_or(0)
                .min(width / 3)
        };
        let note_area = crate::card_resize::Rect {
            x: f64::from(margin), y: f64::from(top), width: note_width.max(1), height,
        };
        for (note, rect) in notes.iter().zip(crate::arrange::terminal_grid(note_area, notes.len())) {
            let mut data = note.data.borrow_mut();
            data.x = rect.x as i32;
            data.y = rect.y as i32;
            data.width = rect.width;
            data.height = rect.height;
            drop(data);
            note.container.set_size_request(rect.width, rect.height);
            self.canvas.move_(&note.container, rect.x, rect.y);
        }
        let left = margin + if notes.is_empty() { 0 } else { note_width + gap };
        let area = crate::card_resize::Rect {
            x: f64::from(left), y: f64::from(top),
            width: (self.screen_width() - margin - left).max(1), height,
        };
        let icons: Vec<_> = terms.iter().filter(|term| term.is_compact()).collect();
        let terminals: Vec<_> = terms.iter().filter(|term| !term.is_compact()).collect();
        let icon_side = icons.iter().map(|term| {
            let data = term.data.borrow();
            data.width.max(data.height)
        }).max().unwrap_or(0);
        let icon_columns = ((area.width + gap) / (icon_side + gap)).max(1) as usize;
        let icon_rows = icons.len().div_ceil(icon_columns);
        let icon_height = if icons.is_empty() { 0 } else {
            (icon_rows as i32 * (icon_side + gap)).min(area.height)
        };
        let terminal_area = crate::card_resize::Rect {
            height: (area.height - icon_height).max(1), ..area
        };
        for (term, rect) in terminals.iter().zip(crate::arrange::terminal_grid(terminal_area, terminals.len())) {
            term.apply_geometry(rect);
            self.canvas.move_(&term.container, rect.x, rect.y);
        }
        for (index, term) in icons.iter().enumerate() {
            let x = area.x + ((index % icon_columns) as i32 * (icon_side + gap)) as f64;
            let y = area.y + (area.height - icon_height + (index / icon_columns) as i32 * (icon_side + gap)) as f64;
            // Icons retain their own size and independent restore position.
            set_displayed_pos(&mut term.data.borrow_mut(), x as i32, y as i32);
            self.canvas.move_(&term.container, x, y);
        }

        let mut s = self.state.borrow_mut();
        for note in notes.iter() {
            if let Some(n) = s.notes.iter_mut().find(|n| n.id == note.data.borrow().id) {
                *n = note.data.borrow().clone();
            }
        }
        for term in terms.iter() {
            if let Some(t) = s.terminals.iter_mut().find(|t| t.session_name == term.data.borrow().session_name) {
                *t = term.data.borrow().clone();
            }
        }
        let snapshot = s.clone();
        drop(s);
        crate::state::save_state_async(snapshot);
        // Refresh outlines after every card has its final geometry.
        self.ghosts.refresh();
    }

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
    fn add_remote_slide_targets(&self) {
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
    fn ensure_slide_trajectories(&self, reset: bool) {
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

        let (hw, hh) = hud_measured_size(&self.hud);
        let (tx, ty, sx, sy) = hud_slide_pose(hw, hh, sw as f64);
        trajs.insert(self.hud.clone().upcast(), Trajectory { sx, sy, tx, ty });
        raise_canvas_child(&self.canvas, &self.hud);
    }

    /// The workspace size the slide paths are built for. A window shown again
    /// has no size until its first frame is laid out (GTK clears it on hide),
    /// so until then the paths are provisional and the tick rebuilds them.
    fn slide_size(&self) -> (i32, i32) {
        allocated_size(&self.window)
            .unwrap_or_else(|| provisional_slide_size((self.screen_width, self.screen_height)))
    }

    /// The paths of every sliding widget, rebuilt for the current size.
    fn rebuild_slide_frames(&self) -> Vec<(gtk4::Widget, Trajectory)> {
        self.ensure_slide_trajectories(true);
        self.add_remote_slide_targets();
        self.slide_frames()
    }

    fn slide_frames(&self) -> Vec<(gtk4::Widget, Trajectory)> {
        self.anim_trajectories
            .borrow()
            .iter()
            .map(|(w, t)| (w.clone(), *t))
            .collect()
    }

    fn paint_slide(&self, progress: f64) {
        let trajs = self.anim_trajectories.borrow();
        for (widget, traj) in trajs.iter() {
            paint_slide_widget(widget, *traj, progress);
        }
    }

    fn ensure_slide_tick(&self) {
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
            glib::ControlFlow::Continue
        });
    }

    /// Read current local layout without presenting the window or probing tmux.
    /// This remains the local workspace even when a remote view is added later.
    pub fn desktop_snapshot(&self, model: &crate::workspace_model::LocalWorkspace)
        -> Result<crate::desktop_protocol::LocalWorkspaceSnapshot, &'static str>
    {
        let presentation = self.terminal_cards.borrow().iter().map(|card| {
            (card.data.borrow().id.clone(), card.desktop_presentation())
        }).collect();
        // Export the same logical canvas used by local placement/animation.
        // Do not export animated widget coordinates during slide-in/out.
        let canvas = crate::desktop_protocol::Canvas {
            x: 0, y: 0, width: self.screen_width() as u32, height: self.screen_height() as u32,
            scale: self.window.scale_factor() as f64,
            top_inset: top_bar_height(self.state.borrow().top_bar_size) as u32,
        };
        model.snapshot(canvas, &presentation)
    }

    pub(crate) fn cli_file_target(&self,model:&crate::workspace_model::LocalWorkspace,request:&crate::control::Request)->Result<crate::state::TerminalData,crate::control::Reply> {
        use crate::control::{Command,Reply};
        let fail=|code,message|Reply::failure(&request.request_id,code,message);
        let id=match &request.command {Command::Files {id,..}|Command::FilesEdit {id,..}=>id,_=>return Err(fail("invalid_request","Expected terminal files request."))};
        let snapshot=self.desktop_snapshot(model).map_err(|_|fail("unavailable","Workspace unavailable."))?;
        let matches:Vec<_>=snapshot.cards.iter().filter(|c|c.card_id==*id).collect();
        if matches.len()!=1{return Err(fail("not_found","No unique local terminal card has that ID."));}
        let card=matches[0];
        if let Command::FilesEdit {expect_epoch,expect_revision,..}=&request.command {
            if expect_epoch!=&snapshot.epoch || expect_revision!=&crate::control_geometry::revision(&snapshot,card){return Err(fail("conflict","Card or display changed; read terminal geometry again."));}
        }
        let widget=self.any_terminal_card(id).map_err(|_|fail("not_found","Local terminal widget missing."))?;
        let data=widget.data.borrow().clone();
        if snapshot.cards.iter().filter(|c|c.session_name==data.session_name).count()!=1 || data.session_name!=card.session_name || widget.cli_session_task().is_closed(){return Err(fail("conflict","Terminal identity is ambiguous or closing."));}
        Ok(data)
    }

    /// Read actual note buffers, including edits waiting for autosave.
    fn cli_live_state(&self) -> AppState {
        let mut state = self.state.borrow().clone();
        state.notes = self.note_cards.borrow().iter().map(|note| note.live_data()).collect();
        state
    }

    fn cli_layout_ready(&self, layout: &crate::control::Layout) -> Result<(), &'static str> {
        if self.slide.running.get() {return Err("Workspace animation is in progress.");}
        for item in &layout.items {
            if item.kind=="note" {
                let cards:Vec<_>=self.note_cards.borrow().iter().filter(|n|n.data.borrow().id==item.id).cloned().collect();
                if cards.len()!=1 || cards[0].text_view.has_focus() || cards[0].container.has_css_class("dragging") || cards[0].container.has_css_class("resizing") {return Err("A note is missing, ambiguous or being edited.");}
            } else {
                let cards:Vec<_>=self.terminal_cards.borrow().iter().filter(|n|n.data.borrow().id==item.id).cloned().collect();
                if cards.len()!=1 || cards[0].is_being_dragged() || cards[0].container.has_css_class("term-resizing") {return Err("A terminal is missing, ambiguous or being moved.");}
            }
        }
        Ok(())
    }

    pub(crate) fn cli_workspace(&self, model: &crate::workspace_model::LocalWorkspace,
        request: &crate::control::Request) -> crate::control::Reply
    {
        if let Some(reply)=self.cli_preferences(model,request) { return reply; }
        use crate::control::{Command, Reply, WorkspaceEdit as Edit, WorkspaceQuery as Query};
        use crate::control_workspace as workspace;
        use serde_json::json;
        let fail = |code, message| Reply::failure(&request.request_id, code, message);
        let snapshot = match self.desktop_snapshot(model) {
            Ok(s) => s, Err(_) => return fail("unavailable", "Local workspace is unavailable."),
        };
        let state = self.cli_live_state();
        let envelope = |state: &AppState| json!({"epoch":snapshot.epoch,"revision":workspace::revision(state,&snapshot),
            "revisionScope":"workspace","units":"logical-pixels","canvas":snapshot.canvas});
        if let Command::Workspace { query } = &request.command {
            let mut data = envelope(&state);
            match query {
                Query::Inspect => { data["workspace"] = json!(crate::state::effective_workspace_dir(&state));
                    data["notesCount"] = json!(state.notes.len()); data["terminalsCount"] = json!(state.terminals.len()); },
                Query::Folders => data["folders"] = self.workspace_choices(),
                Query::Layout => data["layout"] = json!(crate::control_workspace::export(&state,&snapshot)),
                Query::ValidateLayout {layout} => {
                    if let Err(message)=crate::control_workspace::validate_layout(layout,&state,&snapshot) { return fail("invalid_arguments",message); }
                    if let Err(message)=self.cli_layout_ready(layout) { return fail("conflict",message); }
                    data["valid"]=json!(true); data["layout"]=json!(layout);
                },
                Query::Notes => data["notes"] = json!(state.notes.iter().map(|n| workspace::note(n,false)).collect::<Vec<_>>()),
                Query::Note { id } => {
                    let matches:Vec<_> = state.notes.iter().filter(|n| n.id == *id).collect();
                    if matches.len()!=1 { return fail("not_found","No unique local note has that ID."); }
                    data["note"] = workspace::note(matches[0],true);
                },
            }
            return Reply::success(&request.request_id,data);
        }
        if let Err(reply) = workspace::check(request,&state,&snapshot,&snapshot.epoch) { return reply; }
        if self.slide.running.get() { return fail("conflict","A workspace animation is in progress."); }
        let Command::WorkspaceEdit {edit,..} = &request.command else { unreachable!() };
        let mut result = json!({});
        if matches!(edit,Edit::Layout {..}|Edit::Arrange) {
            let layout=match edit {
                Edit::Layout {layout} => layout.clone(),
                _ => match workspace::arrange(&state,&snapshot) {Ok(v)=>v,Err(message)=>return fail("out_of_bounds",message)},
            };
            if let Err(message)=workspace::validate_layout(&layout,&state,&snapshot) { return fail("invalid_arguments",message); }
            if let Err(message)=self.cli_layout_ready(&layout) { return fail("conflict",message); }
            for item in &layout.items {
                if item.kind=="note" {
                    let note=self.note_cards.borrow().iter().find(|n| n.data.borrow().id==item.id).cloned().unwrap();
                    {let mut data=note.data.borrow_mut();data.x=item.x;data.y=item.y;data.width=item.width;data.height=item.height;}
                    note.container.set_size_request(item.width,item.height);
                    self.canvas.move_(&note.container,item.x as f64,item.y as f64);
                    if let Some(data)=self.state.borrow_mut().notes.iter_mut().find(|n|n.id==item.id) {*data=note.live_data();}
                } else {
                    let card=self.any_terminal_card(&item.id).unwrap();
                    let applied=if item.mode=="minimized" {self.move_terminal_card(&item.id,item.x,item.y).is_ok()} else {
                        card.apply_geometry(crate::card_resize::Rect {x:item.x as f64,y:item.y as f64,width:item.width,height:item.height})
                    };
                    if !applied {return Reply::unknown(&request.request_id);}
                }
            }
            self.ghosts.refresh();
            result["layout"]=json!(layout);
        } else if let Edit::Folder { path } = edit {
            if !std::path::Path::new(path).is_absolute() || path.len()>4096 || path.chars().any(char::is_control) {
                return fail("invalid_arguments","Use an absolute existing folder path without control characters.");
            }
            let Some(directory) = crate::state::clean_dir(path) else { return fail("invalid_arguments","Folder does not exist."); };
            { let mut state = self.state.borrow_mut(); state.workspace_dir=Some(directory.clone()); crate::state::remember_workspace_dir(&mut state,&directory); }
            self.ws_bar.show_folder(&directory);
            result["workspace"] = json!(directory);
        } else {
            let (id, mut updated, old) = match edit {
                Edit::NoteCreate { text,x,y,width,height,tag } => {
                    if state.notes.len()>=256 { return fail("limit_exceeded","At most 256 notes can be created through the CLI."); }
                    let id=format!("note_cli_{}",request.request_id);
                    if state.notes.iter().any(|n| n.id==id) { return fail("conflict","Reserved note ID already exists."); }
                    (id.clone(),Some(NoteData {id,text:text.clone(),x:*x,y:*y,width:*width,height:*height,tag:*tag,
                        color:"omarchy".into(),updated_at:0.0}),None)
                },
                Edit::NoteUpdate {id,..} | Edit::NoteDelete {id} | Edit::NoteMove {id,..} | Edit::NoteResize {id,..} | Edit::NoteTag {id,..} => {
                    let cards:Vec<_>=self.note_cards.borrow().iter().filter(|n| n.data.borrow().id==*id).cloned().collect();
                    if cards.len()!=1 { return fail("not_found","No unique local note has that ID."); }
                    let card=Rc::clone(&cards[0]);
                    if card.text_view.has_focus() || card.container.has_css_class("dragging") || card.container.has_css_class("resizing") {
                        return fail("conflict","The note is being edited or moved on the desktop.");
                    }
                    let mut note=card.live_data();
                    match edit {
                        Edit::NoteUpdate {text,..} => note.text=text.clone(),
                        Edit::NoteMove {x,y,..} => {note.x=*x; note.y=*y;},
                        Edit::NoteResize {width,height,..} => {note.width=*width;note.height=*height;},
                        Edit::NoteTag {tag,..} => note.tag=*tag,
                        _ => {}
                    }
                    (id.clone(), if matches!(edit,Edit::NoteDelete {..}) {None} else {Some(note)},Some(card))
                },
                _ => unreachable!()
            };
            if let Some(note) = &mut updated {
                if let Err(message)=workspace::validate_note(note,&snapshot.canvas,true) {
                    return fail("invalid_arguments",message);
                }
                note.updated_at=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
            }
            if let Some(old)=old {
                old.cancel_pending_save();
                self.note_cards.borrow_mut().retain(|n| n.data.borrow().id!=id);
                self.drag_pending.borrow_mut().remove(old.container.upcast_ref::<gtk4::Widget>());
                self.canvas.remove(&old.container);
            }
            self.state.borrow_mut().notes.retain(|n| n.id!=id);
            if let Some(note)=updated {
                result["note"] = workspace::note(&note,false);
                self.state.borrow_mut().notes.push(note.clone());
                self.spawn_note_widget(note,false);
            }
            result["id"] = json!(id);
            result["deleted"] = json!(matches!(edit,Edit::NoteDelete {..}));
        }
        crate::state::save_state_async(self.state.borrow().clone());
        let mut data=envelope(&self.cli_live_state());
        data["result"]=result; data["outcome"]=json!("applied");
        Reply::success(&request.request_id,data)
    }

    fn cli_card_action(&self,model:&crate::workspace_model::LocalWorkspace,request:&crate::control::Request)->crate::control::Reply {
        use crate::control::{CardAction,Command,Reply};
        let fail=|code,message|Reply::failure(&request.request_id,code,message);
        let Command::CardAction {id,action,expect_epoch,expect_revision}=&request.command else {return fail("invalid_request","Expected a card action.");};
        let snapshot=match self.desktop_snapshot(model){Ok(v)=>v,Err(_)=>return fail("unavailable","Workspace unavailable.")};
        let Some(current)=snapshot.cards.iter().find(|c|c.card_id==*id)else{return fail("not_found","No local card has that ID.");};
        if expect_epoch!=&snapshot.epoch||expect_revision!=&crate::control_geometry::revision(&snapshot,current){return fail("conflict","Card or display changed; read terminal geometry again.");}
        let card=match self.any_terminal_card(id){Ok(c)=>c,Err(_)=>return fail("not_found","Card widget is unavailable.")};
        if card.is_being_dragged()||card.container.has_css_class("term-resizing")||self.slide.running.get(){return fail("conflict","A local gesture or animation is in progress.");}
        if snapshot.cards.iter().filter(|c|c.card_id==*id||c.session_name==current.session_name).count()!=1{return fail("conflict","Card identity is ambiguous.");}
        match action {
            CardAction::Raise=>card.cli_raise(),
            CardAction::Focus=>{
                if !self.window.is_visible()||self.machine_view.is_remote()||current.layout.iconified && !current.expanded||self.overlay_panels.iter().any(|p|p.is_visible())||self.pairing_requests.is_open()||self.pairing_wizard.is_open(){return fail("invalid_state","Focus requires a visible local workspace, restored card and closed dialogs. Use show and terminal restore first.");}
                if !card.cli_focus(){return Reply::unknown(&request.request_id);}
            },
            CardAction::Tag {value}=>{
                if *value>8{return fail("invalid_arguments","Tag must be 0..8.");}
                let before=card.data.borrow().clone();card.cli_set_tag(*value);let after=card.data.borrow().clone();
                let mut state=self.state.borrow_mut();crate::folder_colors::note_card_saved(&mut state,Some(&before),&after);
                if let Some(saved)=state.terminals.iter_mut().find(|c|c.id==*id){saved.tag=*value;}
                let saved=state.clone();drop(state);crate::state::save_state_async(saved);
            },
        }
        let after=match self.desktop_snapshot(model){Ok(v)=>v,Err(_)=>return Reply::unknown(&request.request_id)};
        let Some(current)=after.cards.iter().find(|c|c.card_id==*id)else{return Reply::unknown(&request.request_id)};
        let mut data=crate::control_geometry::describe(&after,current);
        data["tag"]=serde_json::json!(card.data.borrow().tag);data["action"]=serde_json::json!(action);data["outcome"]=serde_json::json!("applied");
        data["compositorFocusObserved"]=serde_json::json!(false);
        Reply::success(&request.request_id,data)
    }

    /// Local CLI geometry uses the live card and output on this GTK turn.
    /// It never sends a bridge command, starts a session, or presents the overlay.
    pub fn cli_geometry(&self, model: &crate::workspace_model::LocalWorkspace, request: &crate::control::Request) -> crate::control::Reply {
        if matches!(request.command,crate::control::Command::CardAction {..}) {return self.cli_card_action(model,request);}
        use crate::control::{Command, Reply};
        use crate::control_geometry as geometry;
        let id = &request.request_id;
        let card_id = geometry::card_id(&request.command);
        let snapshot = match self.desktop_snapshot(model) {
            Ok(snapshot) => snapshot,
            Err(_) => return Reply::failure(id, "unavailable", "Local geometry is unavailable."),
        };
        let Some(current) = snapshot.cards.iter().find(|card| card.card_id == card_id) else {
            return Reply::failure(id, "not_found", "No local terminal card has that ID.");
        };
        if matches!(request.command, Command::Geometry { .. }) {
            return Reply::success(id, geometry::describe(&snapshot, current));
        }
        let mode_change = if matches!(request.command, Command::Mode { .. }) {
            match geometry::prepare_mode(request, &snapshot, current) {
                Ok(changed) => Some(changed),
                Err(reply) => return reply,
            }
        } else { None };
        let prepared = if mode_change.is_none() {
            match geometry::prepare(request, &snapshot, current) {
                Ok(prepared) => Some(prepared),
                Err(reply) => return reply,
            }
        } else { None };
        let card = match self.any_terminal_card(card_id) {
            Ok(card) => card,
            Err(_) => return Reply::failure(id, "not_found", "No local terminal widget has that ID."),
        };
        if snapshot.cards.iter().filter(|other| other.session_name == current.session_name).count() != 1
            || self.terminal_cards.borrow().iter().filter(|other| other.data.borrow().id == card_id).count() != 1
            || card.data.borrow().session_name != current.session_name
        {
            return Reply::failure(id, "conflict", "The card-to-session mapping is ambiguous or changed.");
        }
        if card.is_being_dragged() || card.container.has_css_class("term-resizing") {
            return Reply::failure(id, "conflict", "A local geometry gesture is in progress.");
        }
        let applied = match request.command {
            Command::Mode { action, .. } => {
                if mode_change == Some(false) { true } else {
                    let changed = card.cli_set_mode(action, self.screen_width(), self.screen_height());
                    if changed {
                        let rect = card.canvas_rect(self.screen_width(), self.screen_height());
                        // Minimize/restore already position and persist through
                        // the card's save callback, including display clamping.
                        match action {
                            crate::control::ModeAction::Expand => {
                                self.canvas.remove(&card.container);
                                self.canvas.put(&card.container, rect.x, rect.y);
                            }
                            crate::control::ModeAction::Collapse => self.canvas.move_(&card.container, rect.x, rect.y),
                            _ => {}
                        }
                        self.ghosts.refresh();
                    }
                    changed
                }
            }
            Command::Move { .. } => {
                let rect = prepared.as_ref().unwrap().rect;
                self.move_terminal_card(card_id, rect.x as i32, rect.y as i32).is_ok()
            }
            Command::Resize { .. } => card.apply_geometry(prepared.as_ref().unwrap().rect),
            _ => false,
        };
        if !applied {
            return Reply::unknown(id);
        }
        let after = match self.desktop_snapshot(model) {
            Ok(snapshot) => snapshot,
            Err(_) => return Reply::unknown(id),
        };
        let Some(current) = after.cards.iter().find(|card| card.card_id == card_id) else {
            return Reply::unknown(id);
        };
        let mut data = geometry::describe(&after, current);
        if let Some(prepared) = prepared {
            data["requested"] = prepared.requested;
            data["clamped"] = serde_json::json!(prepared.clamped);
        } else if let Command::Mode { action, .. } = &request.command {
            data["requested"] = serde_json::json!({"action":action});
            data["changed"] = serde_json::json!(mode_change.unwrap());
            data["attachmentObserved"] = serde_json::json!(false);
        }
        data["outcome"] = serde_json::json!("applied");
        Reply::success(id, data)
    }

    /// The close worker owns destruction; GTK only validates and removes the
    /// exact card. This path deliberately does not call close_session().
    pub(crate) fn cli_close(&self, model: &crate::workspace_model::LocalWorkspace,
        request: &crate::control::Request, action: crate::control_close::Action) -> crate::control_close::UiResult
    {
        use crate::control::{Command, Reply};
        use crate::control_close::{Action, Target};
        let fail = |code, message| Reply::failure(&request.request_id, code, message);
        let (id, expect_epoch, expect_revision) = match &request.command {
            Command::Forget {id,expect_epoch,expect_revision} | Command::Relaunch {id,expect_epoch,expect_revision,..} | Command::Viewport {
                id,
                expect_epoch: Some(expect_epoch),
                expect_revision: Some(expect_revision),
                ..
            }
            | Command::Attach {
                id,
                expect_epoch,
                expect_revision,
                ..
            }
            | Command::Close {
                id,
                expect_epoch,
                expect_revision,
                ..
            }
            | Command::Input {
                id,
                expect_epoch,
                expect_revision,
                ..
            } => (id, expect_epoch, expect_revision),
            _ => {
                return Err(fail(
                    "invalid_request",
                    "Expected a guarded terminal operation.",
                ))
            }
        };
        let snapshot = self.desktop_snapshot(model).map_err(|_| fail("unavailable", "Local workspace is unavailable."))?;
        let current = snapshot.cards.iter().find(|card| card.card_id == *id)
            .ok_or_else(|| fail("not_found", "No local terminal card has that ID."))?;
        if expect_epoch != &snapshot.epoch || expect_revision != &crate::control_geometry::revision(&snapshot, current) {
            return Err(fail("conflict", "The card changed or the daemon restarted. Read terminal geometry again."));
        }
        let card = self.any_terminal_card(id).map_err(|_| fail("not_found", "No local terminal widget has that ID."))?;
        let data = card.data.borrow().clone();
        if snapshot.cards.iter().filter(|other| other.card_id == *id).count() != 1
            || snapshot.cards.iter().filter(|other| other.session_name == data.session_name).count() != 1
            || current.session_name != data.session_name || card.cli_session_task().is_closed()
        {
            return Err(fail("conflict", "The card-to-session mapping changed, is ambiguous, or is closing."));
        }
        if card.is_being_dragged() || card.container.has_css_class("term-resizing") {
            return Err(fail("conflict", "A local geometry gesture is in progress; the card was not closed."));
        }
        match action {
            Action::Inspect => Ok(Some(Target { data, task: card.cli_session_task() })),
            Action::Remove(expected) => {
                if !matches!(request.command, Command::Close { .. } | Command::Forget {..} | Command::Relaunch {..}) {
                    return Err(fail("invalid_request", "Only close may remove a card."));
                }
                if data.id != expected.data.id || data.session_name != expected.data.session_name
                    || data.created_at != expected.data.created_at
                    || data.command != expected.data.command || data.workspace_dir != expected.data.workspace_dir
                    || !std::sync::Arc::ptr_eq(&card.cli_session_task(), &expected.task) {
                    return Err(fail("conflict", "The terminal card was replaced before close."));
                }
                card.detach_for_cli_close();
                self.terminal_cards.borrow_mut().retain(|other| other.data.borrow().id != *id);
                self.drag_pending.borrow_mut().remove(card.container.upcast_ref::<gtk4::Widget>());
                self.canvas.remove(&card.container);
                let snapshot = {
                    let mut state = self.state.borrow_mut();
                    state.terminals.retain(|other| other.id != *id);
                    crate::state::normalize_terminal_order(&mut state);
                    state.clone()
                };
                crate::state::save_state_async(snapshot);
                self.ghosts.refresh();
                Ok(None)
            }
        }
    }

    pub fn item_counts(&self) -> (usize, usize) {
        (self.note_cards.borrow().len(), self.terminal_cards.borrow().len())
    }

    pub fn close_terminal(&self, sess: &str) -> bool {
        let card = {
            let mut cards = self.terminal_cards.borrow_mut();
            cards
                .iter()
                .position(|c| c.data.borrow().session_name == sess)
                .map(|pos| cards.remove(pos))
        };
        let Some(card) = card else { return false };

        card.close_session();
        self.canvas.remove(&card.container);
        crate::desktop_shell::terminal_removed(&self.canvas);
        let mut s = self.state.borrow_mut();
        s.terminals.retain(|t| t.session_name != sess);
        crate::state::normalize_terminal_order(&mut s);
        let snapshot = s.clone();
        drop(s);
        crate::state::save_state_async(snapshot);
        self.ghosts.refresh();
        true
    }

    /// Move one local card to a host-pixel origin and raise it.
    ///
    /// Used when a remote viewer drops the card. The icon and the open card
    /// keep separate spots; which one moves is the card's own state, not the
    /// viewer's. Expanded cards stay put: their on-screen rectangle is
    /// transient and must not overwrite the saved origin.
    pub fn move_terminal_card(&self, card_id: &str, x: i32, y: i32) -> Result<(), &'static str> {
        let card = self.terminal_card(card_id)?;
        let (x, y) = crate::remote_workspace::clamp_card_origin(
            self.screen_width().max(0) as u32,
            self.screen_height().max(0) as u32,
            x,
            y,
        );
        crate::mini_terminal::set_displayed_pos(&mut card.data.borrow_mut(), x, y);
        let (px, py) = crate::mini_terminal::displayed_pos(&card.data.borrow());
        self.canvas.move_(&card.container, px, py);
        let widget = card.container.upcast_ref::<gtk4::Widget>();
        if let Some(last) = self.canvas.last_child() {
            if &last != widget {
                widget.insert_after(&self.canvas, Some(&last));
            }
        }
        raise_canvas_child(&self.canvas, &self.hud);
        {
            let mut list = self.terminal_cards.borrow_mut();
            if let Some(pos) = list.iter().position(|c| c.data.borrow().id == card_id) {
                let raised = list.remove(pos);
                list.push(raised);
            }
        }
        let data = card.data.borrow().clone();
        let snapshot = {
            let mut state = self.state.borrow_mut();
            let Some(saved) = state.terminals.iter_mut().find(|t| t.id == card_id) else {
                return Err("unknown_card");
            };
            saved.x = data.x;
            saved.y = data.y;
            saved.icon_x = data.icon_x;
            saved.icon_y = data.icon_y;
            state.terminal_order.retain(|id| id != card_id);
            state.terminal_order.push(card_id.to_string());
            state.clone()
        };
        crate::state::save_state_async(snapshot);
        self.ghosts.refresh();
        Ok(())
    }

    /// Apply one validated workspace command to this window's own cards.
    ///
    /// Identity, epoch, revision and bounds are already checked by the bridge
    /// and the owner IPC; what is left here are the card's own state rules, and
    /// the same actions its local buttons and gestures run. An expanded card
    /// refuses layout commands because its rectangle is transient, and an
    /// iconified card keeps its square: only its icon spot moves.
    ///
    /// Returns the card a create made, so the caller can report the published
    /// id and revision for it.
    pub fn apply_workspace_command(
        &self,
        command: &crate::desktop_protocol::WorkspaceCommand,
    ) -> Result<Option<String>, &'static str> {
        use crate::desktop_protocol::WorkspaceCommand as Command;
        match command {
            Command::SetLayout { card_id, layout, .. } => {
                self.set_terminal_card_layout(card_id, layout).map(|()| None)
            }
            Command::CloseTerminal { card_id, .. } => {
                self.close_terminal_card(card_id).map(|()| None)
            }
            Command::CreateTerminal {
                agent_type,
                workspace,
            } => self.create_terminal_card(agent_type, workspace).map(Some),
            Command::SetExpanded {
                card_id, expanded, ..
            } => self
                .set_terminal_card_expanded(card_id, *expanded)
                .map(|()| None),
            Command::SetWorkspace { workspace, .. } => {
                self.set_workspace_folder(workspace).map(|()| None)
            }
        }
    }

    /// Create one harness card at a viewer's request, on this machine.
    ///
    /// The viewer can only name a harness this host offers and the folder this
    /// host published (both are checked before this runs), so a create can
    /// never choose a command, a flag or an arbitrary path. The card is built
    /// exactly like a local launch, and without forcing this overlay to show.
    fn create_terminal_card(
        &self,
        agent_type: &str,
        workspace: &str,
    ) -> Result<String, &'static str> {
        if !crate::tmux::HARNESS_KEYS.contains(&agent_type)
            && !self.state.borrow().custom_harnesses.iter().any(|item| item.id == agent_type && item.validate().is_ok()) {
            return Err("unsupported_harness");
        }
        let Some(directory) = crate::state::clean_dir(workspace) else {
            return Err("invalid_workspace");
        };
        let session = self.create_new_terminal_in(agent_type, None, None, None, Some(&directory));
        if session.is_empty() || !crate::tmux::session_alive(&session) {
            return Err("terminal_unavailable");
        }
        Ok(session)
    }

    /// Put this machine's workspace folder where a viewer asked for it.
    ///
    /// The folder has to be one this host itself offers: the same list the
    /// snapshot publishes is the menu and the permission, so a viewer can never
    /// point this machine at a path it did not offer. The change is this
    /// machine's own, visible and persisted like typing it here.
    fn set_workspace_folder(&self, workspace: &str) -> Result<(), &'static str> {
        let known = {
            let state = self.state.borrow();
            crate::workspace_model::offered_folders(&state, &crate::state::effective_workspace_dir(&state))
        };
        if !known.iter().any(|folder| folder == workspace) {
            return Err("invalid_workspace");
        }
        let Some(directory) = crate::state::clean_dir(workspace) else {
            return Err("invalid_workspace");
        };
        let snapshot = {
            let mut state = self.state.borrow_mut();
            state.workspace_dir = Some(directory.clone());
            crate::state::remember_workspace_dir(&mut state, &directory);
            state.clone()
        };
        self.ws_bar.show_folder(&directory);
        crate::state::save_state_async(snapshot);
        Ok(())
    }

    /// Expand or collapse one card at a viewer's request.
    ///
    /// The card's rectangle, its VTE and the other cards' outlines end up
    /// exactly as they would after a local double-click on its header. The
    /// host's own layer-shell keyboard mode is left alone: a viewer may look
    /// into this card, but it does not take this machine's keyboard.
    fn set_terminal_card_expanded(
        &self,
        card_id: &str,
        expanded: bool,
    ) -> Result<(), &'static str> {
        // An expanded card accepts this one: collapsing is how it stops being
        // expanded, and `SetLayout` is the command that stays refused.
        let card = self.any_terminal_card(card_id)?;
        if card.is_expanded() == expanded {
            return Ok(());
        }
        if expanded {
            // At most one card is expanded, exactly like a local expand.
            let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
            for other in cards.iter() {
                if other.is_expanded() && other.data.borrow().id != card.data.borrow().id {
                    other.collapse();
                    let (px, py) = displayed_pos(&other.data.borrow());
                    self.canvas.move_(&other.container, px, py);
                }
            }
            card.expand(self.screen_width(), self.screen_height());
            let (x, y, _, _) = expanded_rect(self.screen_width(), self.screen_height());
            self.canvas.remove(&card.container);
            self.canvas.put(&card.container, x, y);
        } else {
            card.collapse();
            let (px, py) = displayed_pos(&card.data.borrow());
            self.canvas.move_(&card.container, px, py);
        }
        // Cards the expanded one covers lose their outlines, and a collapse
        // sets them free again.
        self.ghosts.refresh();
        Ok(())
    }

    /// Move, resize and iconify one card to a viewer's request, clamped to this
    /// machine's own screen and persisted like the matching local gesture.
    fn set_terminal_card_layout(
        &self,
        card_id: &str,
        layout: &crate::desktop_protocol::CardLayout,
    ) -> Result<(), &'static str> {
        let card = self.terminal_card(card_id)?;
        if card.data.borrow().iconified != layout.iconified
            && !card.set_iconified(layout.iconified)
        {
            return Err("terminal_unavailable");
        }
        // A resize only applies to a card the host shows at its own size: an
        // iconified card is a fixed square, and the local UI cannot resize one
        // either. Clamping both sides is what keeps a move from silently
        // resizing a card the host's screen no longer fits.
        let (width, height) = clamp_card_size(
            layout.width as i32,
            layout.height as i32,
            self.screen_width(),
            self.screen_height(),
        );
        let current = {
            let data = card.data.borrow();
            (
                clamp_card_size(
                    data.width,
                    data.height,
                    self.screen_width(),
                    self.screen_height(),
                ),
                data.iconified,
            )
        };
        if !current.1 && (width, height) != current.0 {
            card.apply_geometry(crate::card_resize::Rect {
                x: layout.x as f64,
                y: layout.y as f64,
                width,
                height,
            });
        }
        // The card keeps two spots: which one this command addresses is the
        // mode it is in, exactly as the viewer drew it.
        let (x, y) = if layout.iconified {
            (
                layout.icon_x.unwrap_or(layout.x),
                layout.icon_y.unwrap_or(layout.y),
            )
        } else {
            (layout.x, layout.y)
        };
        self.move_terminal_card(card_id, x, y)
    }

    /// Close one card at a viewer's request. The host owns the lifecycle: the
    /// card, its widget and its session go together, exactly like the local
    /// close button. Closing writes no geometry, so it is allowed on an
    /// expanded card — a close must win over a layout gesture.
    fn close_terminal_card(&self, card_id: &str) -> Result<(), &'static str> {
        let card = self.any_terminal_card(card_id)?;
        let session = card.data.borrow().session_name.clone();
        if self.close_terminal(&session) {
            Ok(())
        } else {
            Err("unknown_card")
        }
    }

    /// Find one card by the desktop snapshot's own id, refusing the ones whose
    /// rectangle is transient. Every layout command starts here, so identity
    /// and the expanded-card rule cannot drift between them.
    fn terminal_card(&self, card_id: &str) -> Result<Rc<MiniTerminalCard>, &'static str> {
        let card = self.any_terminal_card(card_id)?;
        if card.is_expanded() {
            return Err("terminal_expanded");
        }
        Ok(card)
    }

    /// Identity check only; the caller decides what an expanded card may take.
    fn any_terminal_card(&self, card_id: &str) -> Result<Rc<MiniTerminalCard>, &'static str> {
        if !crate::desktop_protocol::valid_card_id(card_id) {
            return Err("unknown_card");
        }
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        cards
            .into_iter()
            .find(|card| card.data.borrow().id == card_id)
            .ok_or("unknown_card")
    }

    pub fn raise_all_notes(&self) {
        let notes: Vec<Rc<StickyNote>> = self.note_cards.borrow().clone();
        raise_notes_on_canvas(&self.canvas, &notes);
        raise_canvas_child(&self.canvas, &self.hud);
    }

    pub fn reload_theme(&self) {
        let theme = crate::styles::reload_styles();
        // Snapshot before restyling: `apply_theme` touches VTE/fonts, which
        // can emit signals back into our handlers.
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        for card in cards.iter() {
            card.apply_theme(&theme);
        }
        // Remote consoles too: their colors are set only when an emulator is
        // built and here, never again while its font is fitted.
        self.machine_view.apply_theme(&theme);
        // Swap monochrome toolbar logos for the new mode (light/dark).
        let light_theme = theme.mode == "light";
        for (img, agent) in self.brand_images.borrow().iter() {
            if let Some(logo) = crate::brand::logo_path(agent, light_theme) {
                img.set_from_file(Some(logo));
            }
        }
        // The settings card keeps its own logo images: re-detect + re-render
        // them for the new mode (it re-reads the stored selection too).
        (self.settings_refresh)();
    }

    /// Re-map a window that was hidden with `hide_now`.
    ///
    /// Every card, VTE and `tmux attach` client is still alive, so the overlay
    /// is back on screen within a frame — instead of the full rebuild (state
    /// load, panels, one `tmux` exec per card, ~30 forks) that used to sit
    /// between the shortcut and the overlay appearing.
    pub fn show_again(&self) {
        crate::frame_profile::note_request("show");
        // First: paused tmux clients redraw within a few ms of this, before
        // the slide-in brings their cards on screen.
        self.hidden_pause.on_shown();
        self.show_token.set(self.show_token.get().wrapping_add(1));
        self.reclaim_input();
        // Before drawing is re-enabled, so a terminal whose image is stale
        // comes back live rather than for one frame as an image.
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        for card in &cards {
            card.keep_slide_frame_for_show();
        }
        self.set_terminal_gpu_mapped(true);
        crate::desktop_shell::before_present(&self.window);
        self.window.present();
        self.window.set_visible(true);
        // Re-assert keyboard interactivity: typing in a card flips it to
        // Exclusive (see `apply_terminal_expand`), and a re-mapped layer surface
        // must not come back without it.
        set_overlay_keyboard_mode(&self.window, KeyboardMode::OnDemand);
        self.start_slide_in();
        // Card statuses went stale while off screen (the periodic refresh is
        // paused then); this refreshes them on worker threads.
        self.periodic_refresh();
        // A fresh install's first show introduces the app, once.
        let first_show = !self.state.borrow().welcome_tour_seen;
        if first_show {
            self.open_welcome_tour();
        }
    }

    /// Open the welcome tour over the local workspace and remember it was
    /// shown, so it never opens by itself again.
    pub fn open_welcome_tour(&self) {
        // Settings (where a replay comes from) closes; the tour opens below.
        for panel in &self.overlay_panels {
            panel.set_visible(false);
        }
        self.pairing_wizard.close();
        let (combo, snapshot) = {
            let mut state = self.state.borrow_mut();
            let combo = crate::shortcut::current_combo(state.toggle_shortcut.as_deref());
            let changed = !state.welcome_tour_seen;
            state.welcome_tour_seen = true;
            (combo, changed.then(|| state.clone()))
        };
        if let Some(snapshot) = snapshot {
            crate::state::save_state_async(snapshot);
        }
        self.welcome_tour.open(&combo);
    }

    /// Put waiting pairing requests in front of the user: the approval panel.
    pub fn review_pairing_requests(&self) -> bool {
        self.pairing_requests.open();
        true
    }

    /// Token to hand back to `hide_if_unchanged` when a slide-out starts.
    pub fn current_show_token(&self) -> u64 {
        self.show_token.get()
    }

    /// Hide once the slide-out finished — unless the overlay was shown again
    /// while it was animating, in which case the unmap would undo that show.
    pub fn hide_if_unchanged(&self, token: u64) {
        if self.show_token.get() == token {
            self.hide_now();
        }
    }

    /// Unmap the window but keep the whole widget tree alive for the next show.
    fn hide_now(&self) {
        // Outlines are hints on a visible desk: they do not survive the unmap.
        self.ghosts.suspend();
        // An unmap does not always deliver a pointer leave, and a card that
        // still believed the pointer was on it would keep suppressing the
        // outlines of the cards it covers after the next show.
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        for card in cards.iter() {
            card.forget_pointer();
        }
        // Stop a vsync tick that may never fire (GPU stall) from later
        // painting or calling `on_slide_hidden` after we already unmapped.
        if self.slide.running.get() {
            self.slide.motion.take().report("slide out, cut short by the unmap");
        }
        self.slide.running.set(false);
        self.slide.gen.set(self.slide.gen.get().wrapping_add(1));
        // Drop live terminal surfaces with the unmap: nothing composites a GPU
        // terminal buffer while the overlay is hidden, and `show_again`
        // re-enables drawing for the next show. The slide-out's still images
        // stay: the next slide-in reuses those that still show their terminal
        // (`MiniTerminalCard::keep_slide_frame_for_show`).
        self.set_terminal_gpu_mapped(false);
        self.slide.freeze_pending.set(false);
        // Floating panels must not come back with the window.
        for panel in &self.overlay_panels {
            panel.set_visible(false);
        }
        self.pairing_wizard.close();
        self.pairing_requests.close();
        // …and neither may the workspace list, which is not part of the
        // overlay's widget tree (it is its own popup surface).
        self.ws_popover.popdown();
        self.window.set_visible(false);
        // Remote consoles are only released once the overlay is gone: stopping
        // them earlier blanked the view before it could animate out. Their
        // cards keep the last frame, so showing again reconnects immediately.
        self.machine_view.suspend_streams();
        self.arm_hidden_pause();
    }

    /// The overlay is off screen (unmapped, or built but never shown): pause
    /// the local cards' tmux clients if it stays that way (`hidden_pause`).
    pub fn arm_hidden_pause(&self) {
        let cards = Rc::clone(&self.terminal_cards);
        self.hidden_pause.on_hidden(move || {
            let cards: Vec<Rc<MiniTerminalCard>> = cards.borrow().clone();
            cards.iter().filter_map(|card| card.attach_target()).collect()
        });
    }

    /// The daemon is about to exit: wake any paused tmux client first.
    pub fn resume_hidden_clients(&self) {
        self.hidden_pause.resume_before_exit();
    }

    /// Give the keyboard and pointer back to the desktop as the hide starts.
    ///
    /// The unmap waits for the slide-out (up to `HIDE_FALLBACK`), and until
    /// then a full-screen surface that still takes input swallows the keys
    /// and clicks meant for the window underneath (with a card expanded, the
    /// keyboard is even `Exclusive`). The window-system adapter lets clicks
    /// through; `reclaim_input` undoes both on the next show.
    fn release_input(&self) {
        INPUT_RELEASED.with(|released| released.set(true));
        set_overlay_keyboard_mode(&self.window, KeyboardMode::None);
        crate::desktop_shell::set_pointer_input(&self.window, false);
    }

    fn reclaim_input(&self) {
        INPUT_RELEASED.with(|released| released.set(false));
        crate::desktop_shell::set_pointer_input(&self.window, true);
    }

    /// Hide VTE widgets so hide/unmap does not composite live GPU terminals.
    ///
    /// Mapping again only brings back the terminals no card hides: the overlap
    /// layer pauses the buried ones (see `MiniTerminalCard::set_vte_covered`).
    /// A show starts a slide, which suspends that layer and so lets them all
    /// draw until the cards are at rest.
    fn set_terminal_gpu_mapped(&self, mapped: bool) {
        // Snapshot: hiding a widget can emit signals that re-enter the card
        // list (see `terminal_cards`).
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        for card in cards.iter() {
            card.set_vte_drawing(mapped);
        }
    }

    fn periodic_refresh(&self) {
        if crate::theme::check_theme_changed() {
            self.reload_theme();
        }
        // Safety net for the overlap ghosts: the pointer, focus, drag and resize
        // callbacks cover the interactive cases, and this catches the rest. It
        // also picks the outlines back up when the settle of the slide-in was
        // never reported (a stalled compositor skips that frame tick), so an
        // idle desk always ends up with the ghosts it should have.
        if !self.slide.running.get() {
            self.ghosts.resume();
        }
        // One worker and one `tmux list-panes -a` for every card, instead of
        // several tmux processes per card per second.
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        crate::mini_terminal::run_status_refresh(
            cards.iter().filter_map(|card| card.prepare_status_refresh()).collect(),
        );
    }
}

/// Short top-bar label + emoji fallback for a harness key.
fn raise_notes_on_canvas(canvas: &gtk4::Fixed, notes: &[Rc<crate::sticky_note::StickyNote>]) {
    for note in notes {
        if let Some(last) = canvas.last_child() {
            if &last != &note.container {
                note.container.insert_after(canvas, Some(&last));
            }
        }
    }
}

/// Freeze every card's terminal for the slide; `false` while one still needs
/// a layout first (see `MiniTerminalCard::freeze_for_slide`). Takes a snapshot
/// of the card list: hiding a widget can emit signals that re-enter it.
fn freeze_slide_cards(cards: &[Rc<MiniTerminalCard>]) -> bool {
    let started = std::time::Instant::now();
    let done = cards.iter().fold(true, |done, card| card.freeze_for_slide() && done);
    crate::frame_profile::note_duration("slide freeze", started);
    done
}

fn thaw_slide_cards(cards: &[Rc<MiniTerminalCard>]) {
    for card in cards {
        card.thaw_after_slide();
    }
}

/// Move one sliding widget between its off-screen and rest poses.
///
/// A card lives either in the local canvas or in a remote PC's fitted canvas,
/// so the widget moves inside whichever `Fixed` holds it.
fn paint_slide_widget(widget: &gtk4::Widget, traj: Trajectory, factor: f64) {
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
fn card_slide_offscreen(tx: f64, ty: f64, w: f64, screen_w: f64) -> (f64, f64) {
    let mid = tx + w * 0.5;
    let sx = if mid < screen_w * 0.5 {
        -w - 40.0
    } else {
        screen_w + 40.0
    };
    (sx, ty)
}

pub(crate) fn raise_canvas_child(canvas: &Fixed, widget: &impl gtk4::glib::object::IsA<gtk4::Widget>) {
    let widget = widget.as_ref();
    if let Some(last) = canvas.last_child() {
        if &last != widget {
            widget.insert_after(canvas, Some(&last));
        }
    }
}

fn hud_measured_size(hud: &gtk4::Box) -> (f64, f64) {
    let (_, nat_w, _, _) = hud.measure(Orientation::Horizontal, -1);
    let (_, nat_h, _, _) = hud.measure(Orientation::Vertical, -1);
    let w = hud.width().max(nat_w).max(1) as f64;
    let h = hud.height().max(nat_h).max(HUD_MIN_HEIGHT) as f64;
    (w, h)
}

/// Rest pose (flush with the top-left edge) and off-screen pose (same X, above
/// the overlay) for the toolbar as a single translated widget.
fn hud_slide_pose(_hud_w: f64, hud_h: f64, _screen_w: f64) -> (f64, f64, f64, f64) {
    let tx = 0.0;
    let ty = 0.0;
    let sy = -hud_h - HUD_OFFSCREEN_PAD;
    (tx, ty, tx, sy)
}

fn terminal_slide_geom(term: &MiniTerminalCard, sw: i32, sh: i32) -> (f64, f64, f64, f64) {
    let rect = term.canvas_rect(sw, sh);
    (rect.x, rect.y, rect.width as f64, rect.height as f64)
}

fn apply_terminal_expand(
    terminal_cards: &Rc<RefCell<Vec<Rc<MiniTerminalCard>>>>,
    canvas: &gtk4::Fixed,
    window: &ApplicationWindow,
    ghosts: &Rc<GhostLayer>,
    sw: i32,
    sh: i32,
    session_name: &str,
) {
    // Snapshot the list (cheap `Rc` clones) and release the borrow before the
    // expand/collapse/focus/unparent calls below: those emit pointer- and
    // focus-enter signals that re-enter the raise handlers, which would panic
    // if a list borrow were still alive.
    let cards: Vec<Rc<MiniTerminalCard>> = terminal_cards.borrow().clone();
    let currently_expanded = cards
        .iter()
        .find(|c| c.data.borrow().session_name == session_name)
        .map(|c| c.is_expanded())
        .unwrap_or(false);
    let will_expand = !currently_expanded;

    for card in cards.iter() {
        let sess = card.data.borrow().session_name.clone();
        if sess == session_name {
            if will_expand {
                card.expand(sw, sh);
                let (x, y, _, _) = expanded_rect(sw, sh);
                canvas.remove(&card.container);
                canvas.put(&card.container, x, y);
                card.focus_terminal();
            } else {
                // Collapsing returns the card to the spot of its current form
                // (the icon spot when it is minimized).
                card.collapse();
                let (px, py) = displayed_pos(&card.data.borrow());
                canvas.move_(&card.container, px, py);
            }
        } else if card.is_expanded() {
            card.collapse();
            let (px, py) = displayed_pos(&card.data.borrow());
            canvas.move_(&card.container, px, py);
        }
    }

    set_overlay_keyboard_mode(window, if will_expand {
        KeyboardMode::Exclusive
    } else {
        KeyboardMode::OnDemand
    });
    // Expanding buries everything under the card that grew; collapsing sets
    // the stacked cards free again.
    ghosts.refresh();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_notes_preserve_live_text_and_guard_edits() {
        crate::gtk_test::run_in_child_process("window::tests::cli_notes_inner");
    }

    #[test]
    fn cli_notes_inner() {
        if !crate::gtk_test::is_child() { return; }
        use crate::control::{Command, Request, WorkspaceQuery as Query, WorkspaceEdit as Edit};
        let root=std::env::temp_dir().join(format!("sd-notes-window-{}",std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for name in ["HOME","XDG_CONFIG_HOME","XDG_STATE_HOME","XDG_CACHE_HOME","TMUX_TMPDIR"] { std::env::set_var(name,&root); }
        std::env::remove_var("TMUX"); std::env::remove_var("TMUX_PANE");
        gtk4::init().unwrap();
        let app=gtk4::Application::new(Some("com.superdesktop.CliNotesTest"),gtk4::gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        let mut state=AppState::default(); state.notes.clear();
        let model=crate::workspace_model::LocalWorkspace::new(state);
        let window=SuperDesktopWindow::new(&app,||{},Rc::new(crate::hotcorner::Zone::default()),model.state());
        let read=|query| window.cli_workspace(&model,&Request {control_version:1,request_id:"read".into(),command:Command::Workspace {query}});
        let prepare=|edit,id:&str| {
            let data=read(Query::Inspect).data.unwrap();
            Request {control_version:1,request_id:id.into(),command:Command::WorkspaceEdit {edit,
                expect_epoch:data["epoch"].as_str().unwrap().into(),expect_revision:data["revision"].as_str().unwrap().into()}}
        };
        let create=prepare(Edit::NoteCreate {text:"one\nПривіт\t$(literal)".into(),x:80,y:140,width:260,height:200,tag:3},"create");
        let journal=root.join("receipts");
        let apply=|request:&Request| crate::control_geometry::dispatch(&journal,request,std::time::Instant::now()+Duration::from_secs(3),|r|Ok(window.cli_workspace(&model,r)));
        assert!(apply(&create).ok);
        assert!(apply(&create).ok,"duplicate returns receipt");
        assert_eq!(window.note_cards.borrow().len(),1);
        assert!(!read(Query::Notes).data.unwrap()["notes"][0].as_object().unwrap().contains_key("text"));
        let note_id="note_cli_create".to_string();
        assert_eq!(read(Query::Note {id:note_id.clone()}).data.unwrap()["note"]["text"],"one\nПривіт\t$(literal)");
        let stale=prepare(Edit::NoteUpdate {id:note_id.clone(),text:"overwrite".into()},"stale");
        let old=Rc::clone(&window.note_cards.borrow()[0]);
        old.text_view.buffer().set_text("unsaved desktop edit");
        assert_eq!(window.cli_workspace(&model,&stale).exit_code(),5,"pending autosave must invalidate revision");
        assert_eq!(read(Query::Note {id:note_id.clone()}).data.unwrap()["note"]["text"],"unsaved desktop edit");
        let update=prepare(Edit::NoteUpdate {id:note_id.clone(),text:"replacement\n".into()},"update");
        old.container.add_css_class("resizing");
        assert_eq!(window.cli_workspace(&model,&update).exit_code(),5);
        old.container.remove_css_class("resizing");
        assert!(apply(&update).ok);
        // Keep the retired widget alive past its former debounce deadline.
        let deadline=std::time::Instant::now()+Duration::from_millis(400);
        while std::time::Instant::now()<deadline { while glib::MainContext::default().iteration(false) {} std::thread::sleep(Duration::from_millis(10)); }
        assert_eq!(model.state().borrow().notes[0].text,"replacement\n");
        for (edit,id) in [(Edit::NoteMove {id:note_id.clone(),x:100,y:160},"move"),
            (Edit::NoteResize {id:note_id.clone(),width:300,height:220},"resize"),
            (Edit::NoteTag {id:note_id.clone(),tag:8},"tag")] {
            let reply=apply(&prepare(edit,id)); assert!(reply.ok,"{reply:?}");
        }
        let bad=prepare(Edit::NoteMove {id:note_id.clone(),x:i32::MAX,y:160},"bad");
        assert_eq!(apply(&bad).exit_code(),2);
        assert_eq!(model.state().borrow().notes[0].x,100);
        assert_eq!(model.state().borrow().notes[0].tag,8);
        let exported=read(Query::Layout).data.unwrap()["layout"].clone();
        let mut layout:crate::control::Layout=serde_json::from_value(exported).unwrap();
        layout.items[0].x=120;
        assert!(read(Query::ValidateLayout {layout:layout.clone()}).ok);
        layout.items.push(crate::control::LayoutItem {kind:"note".into(),id:"missing".into(),mode:"normal".into(),x:20,y:140,width:260,height:200});
        assert!(!apply(&prepare(Edit::Layout {layout:layout.clone()},"invalid-layout")).ok);
        assert_eq!(model.state().borrow().notes[0].x,100,"invalid later item must not move earlier item");
        layout.items.pop();
        assert!(apply(&prepare(Edit::Layout {layout},"layout")).ok);
        assert_eq!(model.state().borrow().notes[0].x,120);
        assert!(apply(&prepare(Edit::Arrange,"arrange")).ok);
        assert_eq!(model.state().borrow().notes[0].x,10);
        assert!(!window.window.is_visible());
        crate::state::flush_state_saves_checked().unwrap();
        assert_eq!(crate::state::load_state().notes[0].text,"replacement\n");
        assert!(apply(&prepare(Edit::NoteDelete {id:note_id.clone()},"delete")).ok);
        assert_eq!(read(Query::Note {id:note_id}).exit_code(),3);
        assert_eq!(window.note_cards.borrow().len(),0);
        assert!(crate::state::load_state().notes.is_empty());
        use crate::control::{PreferencesQuery as PQ,PreferencesEdit as PE,LauncherSpec};
        let pref_read=|query| window.cli_workspace(&model,&Request {control_version:1,request_id:"prefs-read".into(),command:Command::Preferences {query}});
        let pref_request=|edit,id:&str| {let data=read(Query::Inspect).data.unwrap();Request {control_version:1,request_id:id.into(),command:Command::PreferencesEdit {edit,expect_epoch:data["epoch"].as_str().unwrap().into(),expect_revision:data["revision"].as_str().unwrap().into()}}};
        assert!(pref_read(PQ::Settings {key:None}).ok);
        assert_eq!(pref_read(PQ::Settings {key:Some("unlisted".into())}).exit_code(),3);
        let secret="PRIVATE_ARGUMENT_LITERAL";
        let args=pref_request(PE::HarnessArgs {id:"claude".into(),arguments:Some(vec![secret.into()])},"args-set");
        assert!(apply(&args).ok);
        assert_eq!(crate::launch_args::effective("claude"),[secret]);
        assert_eq!(pref_read(PQ::HarnessArgs {id:"claude".into()}).data.unwrap()["arguments"][0],secret);
        assert!(!std::fs::read_to_string(journal.join("args-set.json")).unwrap().contains(secret));
        assert!(apply(&pref_request(PE::HarnessArgs {id:"claude".into(),arguments:None},"args-reset")).ok);
        assert_eq!(crate::launch_args::effective("claude"),crate::launch_args::builtin("claude"));
        assert_eq!(apply(&pref_request(PE::Setting {key:"toolbarSize".into(),value:Some(serde_json::json!("giant"))},"invalid-size")).exit_code(),2);
        for size in ["small","medium","large"] {
            assert!(apply(&pref_request(PE::Setting {key:"toolbarSize".into(),value:Some(serde_json::json!(size))},size)).ok);
            assert_eq!(pref_read(PQ::Settings {key:Some("toolbarSize".into())}).data.unwrap()["setting"]["value"],size);
            assert_eq!(window.hud.height_request(),top_bar_height(model.state().borrow().top_bar_size));
        }
        for (key,value) in [("settingsPanelPosition",serde_json::json!([30,100])),("settingsPanelSize",serde_json::json!([700,650]))] {
            assert!(apply(&pref_request(PE::Setting {key:key.into(),value:Some(value)},key)).ok);
        }
        assert_eq!(window.settings_layout.borrow().as_ref().unwrap().geometry(),(Some((30,100)),(700,650)));
        assert!(apply(&pref_request(PE::Setting {key:"settingsPanelPosition".into(),value:None},"position-reset")).ok);
        assert_eq!(window.settings_layout.borrow().as_ref().unwrap().geometry().0,None);
        let custom=LauncherSpec {id:"custom-cli".into(),name:"CLI test".into(),icon:"🤖".into(),executable:"/bin/true".into(),arguments:vec![secret.into()]};
        assert!(apply(&pref_request(PE::CustomPut {launcher:custom,create:true},"custom-add")).ok);
        assert_eq!(pref_read(PQ::Custom {id:"custom-cli".into()}).data.unwrap()["launcher"]["arguments"][0],secret);
        assert!(apply(&pref_request(PE::Visibility {keys:Some(vec!["custom-cli".into()])},"visible")).ok);
        assert_eq!(model.state().borrow().visible_harnesses,Some(vec!["custom-cli".to_string()]));
        assert!(apply(&pref_request(PE::CustomRemove {id:"custom-cli".into()},"custom-remove")).ok);
        assert!(model.state().borrow().custom_harnesses.is_empty());
        assert_eq!(model.state().borrow().visible_harnesses,Some(vec![]));
        assert_eq!(pref_read(PQ::Custom {id:"custom-cli".into()}).exit_code(),3);
        assert!(!window.window.is_visible());
        let mut shortcut=Request {control_version:1,request_id:"shortcut-state".into(),command:Command::Shortcut {combo:"SUPER + F8".into(),preview:None,expect_epoch:None,expect_revision:None}};
        let preview=window.cli_shortcut(&model,&shortcut,false).data.unwrap();
        if let Command::Shortcut {preview:p,expect_epoch,expect_revision,..}=&mut shortcut.command {*p=Some("a".repeat(64));*expect_epoch=Some(preview["epoch"].as_str().unwrap().into());*expect_revision=Some(preview["revision"].as_str().unwrap().into());}
        assert!(window.cli_shortcut(&model,&shortcut,false).ok);assert!(window.cli_shortcut(&model,&shortcut,true).ok);
        assert_eq!(model.state().borrow().toggle_shortcut.as_deref(),Some("SUPER + F8"));
        assert_eq!(window.cli_shortcut(&model,&shortcut,false).exit_code(),5,"old workspace revision must fail after shortcut state changes");
        window.window.close(); drop(old);
        crate::state::flush_state_saves_checked().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cli_mode_handler_checks_live_widgets_and_persists_layout() {
        crate::gtk_test::run_in_child_process("window::tests::cli_mode_handler_inner");
    }

    #[test]
    fn cli_mode_handler_inner() {
        if !crate::gtk_test::is_child() { return; }
        use crate::control::{Command, ModeAction, Request};
        let root = std::env::temp_dir().join(format!("sd-mode-window-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
            std::env::set_var(name, &root);
        }
        std::env::remove_var("TMUX");
        std::env::remove_var("TMUX_PANE");
        gtk4::init().unwrap();
        let app = gtk4::Application::new(Some("com.superdesktop.CliModeTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        let mut state = AppState::default();
        state.terminals.push(serde_json::from_value(serde_json::json!({
            "id":"sd_term_mode", "session_name":"sd_term_mode", "agent_type":"shell", "command":"/bin/false",
            "x":100, "y":200, "width":128, "height":128, "restored_width":480, "restored_height":320,
            "iconified":true, "icon_x":300, "icon_y":400, "created_at":0.0
        })).unwrap());
        let model = crate::workspace_model::LocalWorkspace::new(state);
        let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
        let card = window.any_terminal_card("sd_term_mode").unwrap();
        let request = |action| {
            let snapshot = window.desktop_snapshot(&model).unwrap();
            Request { control_version: 1, request_id: "mode-test".into(), command: Command::Mode {
                id: "sd_term_mode".into(), action, expect_epoch: snapshot.epoch.clone(),
                expect_revision: crate::control_geometry::revision(&snapshot, &snapshot.cards[0]),
            }}
        };
        let restore = request(ModeAction::Restore);
        card.container.add_css_class("term-resizing");
        assert_eq!(window.cli_geometry(&model, &restore).exit_code(), 5);
        card.container.remove_css_class("term-resizing");
        let reply = window.cli_geometry(&model, &restore);
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.data.as_ref().unwrap()["mode"], "normal");
        assert_eq!(reply.data.as_ref().unwrap()["changed"], true);
        assert_eq!(model.state().borrow().terminals[0].width, 480);
        assert_eq!(window.cli_geometry(&model, &restore).exit_code(), 5, "old revision must conflict");
        let noop = window.cli_geometry(&model, &request(ModeAction::Restore));
        assert_eq!(noop.data.unwrap()["changed"], false);
        for (action, mode) in [(ModeAction::Minimize, "minimized"), (ModeAction::Expand, "expanded"), (ModeAction::Collapse, "minimized")] {
            let reply = window.cli_geometry(&model, &request(action));
            assert!(reply.ok, "{reply:?}");
            let data = reply.data.unwrap();
            assert_eq!(data["mode"], mode);
            assert_eq!(data["changed"], true);
        }
        let action=|action| {let snapshot=window.desktop_snapshot(&model).unwrap();Request {control_version:1,request_id:"card-action".into(),command:Command::CardAction {id:"sd_term_mode".into(),action,expect_epoch:snapshot.epoch.clone(),expect_revision:crate::control_geometry::revision(&snapshot,&snapshot.cards[0])}}};
        let tagged=window.cli_geometry(&model,&action(crate::control::CardAction::Tag {value:7}));assert!(tagged.ok,"{tagged:?}");
        assert_eq!(model.state().borrow().terminals[0].tag,7);
        assert!(window.cli_geometry(&model,&action(crate::control::CardAction::Raise)).ok);
        assert_eq!(window.cli_geometry(&model,&action(crate::control::CardAction::Focus)).exit_code(),6,"hidden workspace must not take focus");
        assert!(!window.window.is_visible());
        crate::state::flush_state_saves_checked().unwrap();
        let persisted = crate::state::load_state();
        assert!(persisted.terminals[0].iconified);
        assert_eq!(persisted.terminals[0].restored_width, 480);
        assert_eq!(persisted.terminals[0].icon_x, Some(300));
        window.window.close();
        crate::state::flush_state_saves_checked().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn test_card_slide_goes_to_the_nearest_left_or_right_edge() {
        let sw = 2560.0;
        // Top-left corner → left, Y unchanged.
        let (sx, sy) = card_slide_offscreen(80.0, 40.0, 128.0, sw);
        assert!(sx < 0.0, "left-half card must leave to the left, got {sx}");
        assert_eq!(sy, 40.0);
        // Bottom-right corner → right, Y unchanged.
        let (sx, sy) = card_slide_offscreen(2200.0, 1400.0, 380.0, sw);
        assert!(sx > sw, "right-half card must leave to the right, got {sx}");
        assert_eq!(sy, 1400.0);
        // Top-right icon → right, not up.
        let (sx, sy) = card_slide_offscreen(2400.0, 20.0, 128.0, sw);
        assert!(sx > sw);
        assert_eq!(sy, 20.0);
    }

    #[test]
    fn hide_fallback_does_not_wait_on_a_stuck_compositor() {
        // The fallback must fire after the cards are visually gone, and well
        // before a frozen overlay feels like a hang.
        assert!(HIDE_FALLBACK >= Duration::from_millis(200));
        assert!(HIDE_FALLBACK <= Duration::from_millis(400));
        let (x, _) = slide_pose_at(1.0, -SLIDE_LAUNCH_OUT, 0.0, SLIDE_OMEGA_OUT, HIDE_FALLBACK);
        assert!(x < 0.01, "the fallback unmaps cards still {:.1}% on screen", x * 100.0);
    }

    /// Where a slide starting at (`x`, `v`) is after `elapsed` of 60 Hz frames.
    fn slide_pose_at(x: f64, v: f64, target: f64, omega: f64, elapsed: Duration) -> (f64, f64) {
        let frames = (elapsed.as_secs_f64() * 60.0).floor() as usize;
        (0..frames).fold((x, v), |(x, v), _| spring_step(x, v, target, 1.0 / 60.0, omega))
    }

    #[test]
    fn a_hiding_overlay_cannot_take_the_keyboard_back() {
        // Released: the focus/pointer leave handlers the release itself
        // triggers must not flip the surface back to OnDemand/Exclusive.
        assert!(overlay_keyboard_mode_allowed(KeyboardMode::None, true));
        assert!(!overlay_keyboard_mode_allowed(KeyboardMode::OnDemand, true));
        assert!(!overlay_keyboard_mode_allowed(KeyboardMode::Exclusive, true));
        for mode in [KeyboardMode::None, KeyboardMode::OnDemand, KeyboardMode::Exclusive] {
            assert!(overlay_keyboard_mode_allowed(mode, false));
        }
    }

    #[test]
    fn hide_slides_cards_from_where_they_are_now() {
        crate::gtk_test::run_in_child_process("window::tests::hide_slides_cards_from_where_they_are_now_inner");
    }

    #[test]
    fn hide_slides_cards_from_where_they_are_now_inner() {
        if !crate::gtk_test::is_child() { return; }
        let root = std::env::temp_dir().join(format!("sd-slide-out-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
            std::env::set_var(name, &root);
        }
        std::env::remove_var("TMUX");
        std::env::remove_var("TMUX_PANE");
        gtk4::init().unwrap();
        let app = gtk4::Application::new(Some("com.superdesktop.SlideOutTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        let mut state = AppState::default();
        state.terminals.push(serde_json::from_value(serde_json::json!({
            "id":"sd_term_slide", "session_name":"sd_term_slide", "agent_type":"shell", "command":"/bin/false",
            "x":20, "y":200, "width":160, "height":120, "created_at":0.0
        })).unwrap());
        let model = crate::workspace_model::LocalWorkspace::new(state);
        let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
        let card = window.any_terminal_card("sd_term_slide").unwrap();
        let widget: gtk4::Widget = card.container.clone().upcast();
        let sw = window.screen_width() as f64;
        let path = || *window.anim_trajectories.borrow().get(&widget).unwrap();

        window.start_slide_in();
        assert!(path().sx < 0.0, "a left-half card enters from the left");
        // The slide-in settles; the user then drags the card to the right.
        window.slide.running.set(false);
        window.slide.gen.set(window.slide.gen.get().wrapping_add(1));
        let right = (sw - 200.0) as i32;
        card.data.borrow_mut().x = right;

        window.start_slide_out(|| {});
        let out = path();
        assert_eq!((out.tx, out.ty), (right as f64, 200.0), "hide must start where the card is");
        assert!(out.sx > sw, "a right-half card must leave to the right, got {}", out.sx);

        // Reversing a hide mid-flight keeps the same path, so nothing jumps.
        card.data.borrow_mut().x = 20;
        window.start_slide_in();
        assert_eq!(path(), out);
        window.window.close();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn show_slides_cards_in_from_the_edges_of_the_real_size() {
        crate::gtk_test::run_in_child_process("window::tests::show_slides_cards_in_from_the_edges_of_the_real_size_inner");
    }

    #[test]
    fn show_slides_cards_in_from_the_edges_of_the_real_size_inner() {
        if !crate::gtk_test::is_child() { return; }
        let root = std::env::temp_dir().join(format!("sd-slide-in-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for name in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMUX_TMPDIR"] {
            std::env::set_var(name, &root);
        }
        std::env::remove_var("TMUX");
        std::env::remove_var("TMUX_PANE");
        gtk4::init().unwrap();
        let app = gtk4::Application::new(Some("com.superdesktop.SlideInTest"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        // Right of the middle of the 900px screen the overlay opens on, left of
        // the middle of the wider monitor the paths are first built for.
        let mut state = AppState::default();
        state.terminals.push(serde_json::from_value(serde_json::json!({
            "id":"sd_term_slide_in", "session_name":"sd_term_slide_in", "agent_type":"shell", "command":"/bin/false",
            "x":380, "y":200, "width":160, "height":120, "created_at":0.0
        })).unwrap());
        let model = crate::workspace_model::LocalWorkspace::new(state);
        let window = SuperDesktopWindow::new(&app, || {}, Rc::new(crate::hotcorner::Zone::default()), model.state());
        let card = window.any_terminal_card("sd_term_slide_in").unwrap();
        let widget: gtk4::Widget = card.container.clone().upcast();
        let path = || *window.anim_trajectories.borrow().get(&widget).unwrap();
        let monitors = gdk::Display::default().unwrap().monitors();
        let widest = (0..monitors.n_items())
            .filter_map(|i| monitors.item(i).and_then(|m| m.downcast::<gdk::Monitor>().ok()))
            .map(|m| m.geometry().width())
            .max()
            .unwrap();
        assert!(widest > 2 * 460, "the test needs a monitor wider than 920px, got {widest}");

        window.window.set_default_size(900, 600);
        window.window.present();
        // Shown again, the window has no size until its first frame is laid
        // out: the first pose must be off screen on every monitor.
        assert_eq!(allocated_size(&window.window), None);
        window.start_slide_in();
        let (sw, _) = window.slide.built_for.get();
        assert!(sw >= widest, "provisional paths must cover the widest monitor, built for {sw} < {widest}");
        let first = path();
        assert!(first.sx < 0.0 || first.sx >= f64::from(widest), "first pose on screen: {}", first.sx);

        // Broadway ticks slowly; the rebuild, not the settle, is what counts.
        let rebuilt = || allocated_size(&window.window).is_some_and(|size| size == window.slide.built_for.get());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !rebuilt() && std::time::Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
        let size = allocated_size(&window.window).expect("the window was laid out");
        assert_eq!(size.0, 900, "the window must take its requested width");
        assert_eq!(window.slide.built_for.get(), size, "the paths must be rebuilt for the real size");
        let entry = path();
        assert_eq!(entry.sx, 940.0, "a right-half card must enter from the real right edge");
        assert_eq!((entry.tx, entry.ty), (380.0, 200.0));
        window.window.close();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn slides_finish_their_visible_motion_quickly() {
        // Hyprland adds nothing on top (the install rule turns layer fades
        // off), so these are the latencies the user sees on every toggle.
        let (x, _) = slide_pose_at(0.0, SLIDE_LAUNCH_IN, 1.0, SLIDE_OMEGA_IN, Duration::from_millis(320));
        assert!(x > 0.99, "slide-in only {:.1}% there after 320ms", x * 100.0);
        let (x, _) = slide_pose_at(1.0, -SLIDE_LAUNCH_OUT, 0.0, SLIDE_OMEGA_OUT, Duration::from_millis(250));
        assert!(x < 0.01, "slide-out still {:.1}% on screen after 250ms", x * 100.0);
    }

    #[test]
    fn slide_moves_a_card_inside_its_own_canvas() {
        crate::gtk_test::run_in_child_process_needing_large_screen("window::tests::slide_inside_own_canvas_inner");
    }

    #[test]
    fn workspace_bounds_follow_output_allocation() {
        crate::gtk_test::run_in_child_process("window::tests::workspace_bounds_follow_output_allocation_inner");
    }

    #[test]
    fn workspace_bounds_follow_output_allocation_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let window = gtk4::Window::new();
        window.set_resizable(false);
        let canvas = Fixed::new();
        let hud = gtk4::Box::new(Orientation::Horizontal, 0);
        canvas.put(&hud, 0.0, 0.0);
        let offscreen = Label::new(Some("saved card"));
        canvas.put(&offscreen, 4000.0, 2000.0);
        window.set_child(Some(&desktop_overlay(&canvas)));
        let observed = Rc::new(Cell::new((0, 0)));
        bind_top_bar_width(&hud, &window, &Label::new(None), &Label::new(None), Rc::new({
            let observed = Rc::clone(&observed);
            move |w, h| observed.set((w, h))
        }));
        let fallback = (320, 240);
        assert_eq!(allocated_workspace_size(&window, fallback), fallback);
        for size in [(480, 360), (960, 700), (600, 400), (960, 700), (480, 360)] {
            window.set_default_size(size.0, size.1);
            window.present();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while observed.get() != size && std::time::Instant::now() < deadline {
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(observed.get(), size, "production allocation callback must follow both dimensions");
            assert_eq!(allocated_workspace_size(&window, fallback), size);
            assert_eq!(hud.width_request(), size.0);
            let limits = crate::mini_terminal::workspace_limits(observed.get(), 1.0);
            assert_eq!(limits.right, f64::from(size.0 - 10));
            assert_eq!(limits.bottom, f64::from(size.1 - 10));
        }
        window.close();
    }

    #[test]
    fn toolbar_controls_stay_on_screen() {
        crate::gtk_test::run_in_child_process_needing_large_screen("window::tests::toolbar_controls_inner");
    }

    #[test]
    fn toolbar_controls_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let left = gtk4::Box::new(Orientation::Horizontal, 0);
        let brand = Label::new(Some("SUPER DESKTOP"));
        left.append(&brand);
        let folder = gtk4::Entry::new();
        folder.set_text(&format!("/home/user/{}", "long-project-folder/".repeat(30)));
        folder.set_width_chars(60);
        left.append(&folder);
        let launchers = gtk4::Box::new(Orientation::Horizontal, 0);
        let launcher_buttons: Vec<_> = (0..24)
            .map(|index| {
                let button = Button::with_label(&format!("Custom harness {index}"));
                launchers.append(&button);
                button
            })
            .collect();
        let right = gtk4::Box::new(Orientation::Horizontal, 10);
        right.set_valign(Align::Center);
        let actions: Vec<_> = [
            "sd-arrange-symbolic",
            "sd-gears-symbolic",
            "sd-hide-symbolic",
        ]
        .into_iter()
        .map(|icon| {
            let button = Button::from_icon_name(icon);
            button.add_css_class("hud-button");
            button.add_css_class("hud-icon-btn");
            right.append(&button);
            button
        })
        .collect();
        let hint = Label::new(Some("[SUPER + SHIFT + Q]"));
        right.append(&hint);
        let hud = gtk4::Box::new(Orientation::Horizontal, 0);
        hud.add_css_class("hud-bar");
        hud.append(&top_bar_content(&left, &launchers, &right));
        hud.set_width_request(1024);
        fit_top_bar(&hud, 0, &brand, &hint);
        assert_eq!(
            hud.width_request(),
            1024,
            "an unmapped window must not collapse the toolbar"
        );
        for count in [0, 3, 24] {
            for (index, button) in launcher_buttons.iter().enumerate() {
                button.set_visible(index < count);
            }
            for size in [TopBarSize::Small, TopBarSize::Medium, TopBarSize::Large] {
                paint_top_bar_size(&hud, size);
                // Shrink, enlarge, then shrink again, as with output/scale changes.
                // Logical widths also cover scaled outputs, e.g. 1920/1.5=1280
                // and 2560/2=1280. Cross the compact-label breakpoints both ways.
                for width in [
                    1280, 320, 800, 2560, 640, 999, 1000, 1199, 1200, 480, 3440, 1024,
                ] {
                    fit_top_bar(&hud, width, &brand, &hint);
                    assert_eq!(brand.is_visible(), width >= 1000);
                    assert_eq!(hint.is_visible(), width >= 1200);
                    let (_, natural, _, _) = hud.measure(Orientation::Horizontal, -1);
                    assert_eq!(natural, width, "contents must not inflate the dock");
                    let (_, height, _, _) = hud.measure(Orientation::Vertical, width);
                    hud.allocate(width, height, -1, None);
                    let bounds = right.compute_bounds(&hud).unwrap();
                    assert!(bounds.x() >= 0.0);
                    // GTK widget coordinates exclude the dock's CSS padding.
                    assert!(
                        (bounds.x() + bounds.width() - hud.width() as f32).abs() < 1.0,
                        "right controls must meet the display edge at {width}: {bounds:?}"
                    );
                    for button in &actions {
                        let bounds = button.compute_bounds(&hud).unwrap();
                        assert!(button.is_visible() && button.is_sensitive());
                        assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
                        assert!(bounds.x() >= 0.0 && bounds.x() + bounds.width() <= hud.width() as f32);
                        assert!(
                            bounds.y() >= 0.0 && bounds.y() + bounds.height() <= hud.height() as f32
                        );
                    }
                }
            }
        }

        // Reproduce the real failure: a saved/off-screen card enlarges Fixed's
        // minimum size, then the toolbar follows that oversized window.
        let canvas = Fixed::new();
        let offscreen = Label::new(Some("off-screen card"));
        offscreen.set_size_request(640, 480);
        canvas.put(&offscreen, 5000.0, 2000.0);
        canvas.put(&hud, 0.0, 0.0);
        let stack = gtk4::Stack::new();
        stack.add_named(&canvas, Some("local"));
        let root = desktop_overlay(&stack);
        assert!(canvas.measure(Orientation::Horizontal, -1).0 > 5000);
        assert_eq!(root.measure(Orientation::Horizontal, -1).0, 0);
        assert_eq!(root.measure(Orientation::Vertical, -1).0, 0);

        let window = gtk4::Window::new();
        window.set_default_size(1024, 600);
        window.set_resizable(false);
        window.set_child(Some(&root));
        bind_top_bar_width(&hud, &window, &brand, &hint, Rc::new(|_, _| {}));
        window.present();
        let check_mapped = |width: i32| {
            let until = std::time::Instant::now() + Duration::from_secs(3);
            let mut settled = 0;
            while std::time::Instant::now() < until {
                while glib::MainContext::default().iteration(false) {}
                let fits = right.compute_bounds(&window).is_some_and(|bounds| {
                    bounds.width() > 0.0
                        && bounds.x() >= 0.0
                        && bounds.x() + bounds.width() <= window.width() as f32
                        && hud.width_request() == window.width()
                        && window.width() == width
                });
                settled = if fits { settled + 1 } else { 0 };
                if settled >= 5 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(window.width() > 0 && window.width() < 5000);
            assert_eq!(
                window.width(),
                width,
                "the mapped window must accept the new size"
            );
            assert_eq!(hud.width_request(), window.width());
            assert_eq!(brand.is_visible(), width >= 1000);
            assert_eq!(hint.is_visible(), width >= 1200);
            let bounds = right.compute_bounds(&window).unwrap();
            assert!(
                bounds.x() >= 0.0 && bounds.x() + bounds.width() <= window.width() as f32,
                "mapped controls {bounds:?} exceed window width {}",
                window.width()
            );
            // Visible geometry alone is insufficient: every action must also be
            // hittable through the mapped window and its clipped canvas.
            for button in &actions {
                let bounds = button.compute_bounds(&window).unwrap();
                let picked = window
                    .pick(
                        bounds.x() as f64 + bounds.width() as f64 / 2.0,
                        bounds.y() as f64 + bounds.height() as f64 / 2.0,
                        gtk4::PickFlags::DEFAULT,
                    )
                    .expect("toolbar action must be hittable");
                assert!(picked == *button || picked.is_ancestor(button));
            }
        };
        for width in [1024, 640, 1600, 480, 1280] {
            window.set_default_size(width, 600);
            check_mapped(width);
        }
        // Sizing is event-driven: an idle, settled overlay must not keep the
        // frame clock running (the old per-frame tick callback did, at the
        // display's refresh rate, for the overlay's whole lifetime).
        let frames = Rc::new(Cell::new(0u32));
        let clock = window.frame_clock().expect("mapped window has a frame clock");
        let counter = Rc::clone(&frames);
        let handler = clock.connect_after_paint(move |_| counter.set(counter.get() + 1));
        let until = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < until {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(Duration::from_millis(10));
        }
        clock.disconnect(handler);
        assert!(
            frames.get() < 10,
            "an idle toolbar must not repaint every frame ({} frames in 500 ms)",
            frames.get()
        );
        // Hidden, resized while hidden, shown again: the surface may come
        // back realized with a new frame clock, which the bar must follow.
        window.set_visible(false);
        while glib::MainContext::default().iteration(false) {}
        window.set_default_size(900, 600);
        window.set_visible(true);
        check_mapped(900);
        window.set_default_size(1400, 600);
        check_mapped(1400);
        window.close();
    }

    #[test]
    fn slide_inside_own_canvas_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let app = gtk4::Application::new(
            Some("com.superdesktop.SlideTest"),
            gtk4::gio::ApplicationFlags::NON_UNIQUE,
        );
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        let window = gtk4::ApplicationWindow::new(&app);
        window.set_default_size(800, 600);
        // A remote PC's cards live in a nested canvas, not in the local one: a
        // hide must move them where they are instead of leaving them frozen.
        let outer = gtk4::Fixed::new();
        let inner = gtk4::Fixed::new();
        let card = gtk4::Label::new(Some("remote console"));
        card.set_size_request(200, 100);
        inner.put(&card, 100.0, 120.0);
        outer.put(&inner, 0.0, 0.0);
        window.set_child(Some(&outer));
        window.present();
        // A move takes effect in the next allocation, exactly as it does inside
        // the real vsync slide tick.
        let pump = || {
            let until = std::time::Instant::now() + Duration::from_millis(120);
            while std::time::Instant::now() < until {
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        pump();
        let trajectory = Trajectory {
            sx: -240.0,
            sy: 120.0,
            tx: 100.0,
            ty: 120.0,
        };
        paint_slide_widget(card.upcast_ref(), trajectory, 0.0);
        pump();
        assert_eq!(inner.child_position(&card), (-240.0, 120.0));
        paint_slide_widget(card.upcast_ref(), trajectory, 0.5);
        pump();
        assert_eq!(inner.child_position(&card), (-70.0, 120.0));
        paint_slide_widget(card.upcast_ref(), trajectory, 1.0);
        pump();
        assert_eq!(inner.child_position(&card), (100.0, 120.0));
        // A widget that is not in a Fixed is skipped, never panicked over.
        paint_slide_widget(window.upcast_ref(), trajectory, 0.0);
        window.close();
    }

    #[test]
    fn test_hud_slides_straight_up_from_its_rest_pose() {
        let (tx, ty, sx, sy) = hud_slide_pose(400.0, 48.0, 2560.0);
        assert_eq!(sx, tx, "toolbar must not drift sideways");
        assert_eq!(ty, 0.0);
        assert!(sy <= -48.0, "hidden toolbar sits fully above the overlay, sy={sy}");
        assert_eq!(tx, 0.0, "full-width toolbar is anchored to the left edge");
    }

    #[test]
    fn test_spring_reverses_without_a_position_jump() {
        let (x, v) = spring_step(0.0, SLIDE_LAUNCH_IN, 1.0, 1.0 / 240.0, SLIDE_OMEGA_IN);
        assert!(x > 0.0 && x < 1.0, "one 240 Hz frame must advance, got {x}");

        // Reverse: same pose, new target. One frame later we are still near x,
        // not teleported to 1.0 or 0.0.
        let (x2, v2) = spring_step(x, v, 0.0, 1.0 / 240.0, SLIDE_OMEGA_OUT);
        assert!(
            (x2 - x).abs() < 0.05,
            "a reverse must not jump, {x} -> {x2}"
        );
        assert!(v2 < v, "target 0 must decelerate / reverse velocity, {v} -> {v2}");

        let (frozen_x, frozen_v) = spring_step(0.4, 1.1, 1.0, 0.0, SLIDE_OMEGA_IN);
        assert_eq!((frozen_x, frozen_v), (0.4, 1.1));

        let (mut x, mut v) = (0.0, SLIDE_LAUNCH_IN);
        for _ in 0..240 {
            (x, v) = spring_step(x, v, 1.0, 1.0 / 240.0, SLIDE_OMEGA_IN);
        }
        assert!(
            spring_settled(x, v, 1.0) || (x - 1.0).abs() < 0.02,
            "must settle on-canvas in ~1s at 240 Hz, x={x} v={v}"
        );
    }

    #[test]
    fn test_focused_terminal_rendered_above_any_other_icon_and_sticky_notes() {
        crate::gtk_test::run_in_child_process("window::tests::focused_terminal_stacking_inner");
    }

    #[test]
    fn focused_terminal_stacking_inner() {
        if !crate::gtk_test::is_child() { return; }
        gtk4::init().unwrap();
        let canvas = Fixed::new();

        // Create terminal container 1 (icon/mini terminal)
        let term1 = gtk4::Box::new(Orientation::Vertical, 0);
        // Create terminal container 2 (another icon/terminal)
        let term2 = gtk4::Box::new(Orientation::Vertical, 0);
        canvas.put(&term1, 10.0, 10.0);
        canvas.put(&term2, 20.0, 20.0);

        // Create sticky notes
        let note1_data = NoteData {
            id: "n1".to_string(),
            text: "Note 1".to_string(),
            x: 50,
            y: 50,
            width: 200,
            height: 150,
            color: "omarchy".to_string(),
            updated_at: 0.0,
            tag: 0,
        };
        let note2_data = NoteData {
            id: "n2".to_string(),
            text: "Note 2".to_string(),
            x: 100,
            y: 100,
            width: 200,
            height: 150,
            color: "omarchy".to_string(),
            updated_at: 0.0,
            tag: 0,
        };

        let note1 = StickyNote::new(
            note1_data,
            |_, _, _| {},
            |_, _| {},
            |_| {},
            |_| {},
            |_| {},
            |_, _, _, _| {},
            || {},
            1920,
            1080,
        );
        let note2 = StickyNote::new(
            note2_data,
            |_, _, _| {},
            |_, _| {},
            |_| {},
            |_| {},
            |_| {},
            |_, _, _, _| {},
            || {},
            1920,
            1080,
        );

        canvas.put(&note1.container, 50.0, 50.0);
        canvas.put(&note2.container, 100.0, 100.0);

        // Initially: notes were added after term1 and term2
        assert_eq!(canvas.last_child().as_ref(), Some(note2.container.upcast_ref::<gtk4::Widget>()));

        // Focus terminal 1! Focused terminal must be rendered above any other icon AND even sticky notes!
        if let Some(last) = canvas.last_child() {
            if &last != term1.upcast_ref::<gtk4::Widget>() {
                term1.insert_after(&canvas, Some(&last));
            }
        }
        // Terminal 1 is now the last child (topmost rendered widget)
        assert_eq!(canvas.last_child().as_ref(), Some(term1.upcast_ref::<gtk4::Widget>()));

        // Now focus terminal 2 (an icon/terminal)!
        if let Some(last) = canvas.last_child() {
            if &last != term2.upcast_ref::<gtk4::Widget>() {
                term2.insert_after(&canvas, Some(&last));
            }
        }
        // Terminal 2 is now on top of terminal 1 AND on top of all sticky notes!
        assert_eq!(canvas.last_child().as_ref(), Some(term2.upcast_ref::<gtk4::Widget>()));

        // Now simulate expanding terminal 1 (remove and put to expand to overlay)
        canvas.remove(&term1);
        canvas.put(&term1, 0.0, 0.0);
        // Expanded terminal 1 is at the top of the canvas, above any other icon and even sticky notes!
        assert_eq!(canvas.last_child().as_ref(), Some(term1.upcast_ref::<gtk4::Widget>()));
    }
}
