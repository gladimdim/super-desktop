use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Button, EventControllerFocus, EventControllerKey,
    EventControllerMotion, Fixed, Image, Label, Orientation, Overlay, Popover, PositionType,
    Separator,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::mini_terminal::{
    clamp_card_size, displayed_pos, expanded_rect, set_displayed_pos, MiniTerminalCard,
    NEW_TERM_HEIGHT, NEW_TERM_WIDTH,
};
use crate::state::{load_state, AppState, NoteData, TerminalData};
use crate::sticky_note::StickyNote;
use crate::tag::DEFAULT_TERMINAL_TAG;
use crate::tmux::{create_session, kill_session};

#[derive(Clone, Copy)]
struct Trajectory {
    sx: f64,
    sy: f64,
    tx: f64,
    ty: f64,
}

/// Critically-damped spring (rad/s). Settle time is about 4/ω.
/// Appear is a touch slower so high-refresh screens get more in-between frames.
const SLIDE_OMEGA_IN: f64 = 15.0;
/// Hide is stiffer so a reverse during appear still clears the screen promptly.
const SLIDE_OMEGA_OUT: f64 = 20.0;
/// Initial speed (progress / second) when starting from rest. A spring at v=0
/// eases in; this impulse makes the first frames shoot in from the edge.
const SLIDE_LAUNCH_IN: f64 = 5.5;
const SLIDE_LAUNCH_OUT: f64 = 7.5;
const SLIDE_SETTLE_X: f64 = 0.0015;
const SLIDE_SETTLE_V: f64 = 0.025;
/// Resting gap between the top of the overlay and the HUD pill.
const HUD_REST_MARGIN: i32 = 18;
/// Extra pixels past the HUD's own height so it is fully off-screen at progress 0.
const HUD_OFFSCREEN_PAD: f64 = 40.0;
/// Fallback HUD height before GTK has allocated it, so the first frame still
/// starts above the screen instead of at rest.
const HUD_MIN_HEIGHT: i32 = 56;

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

/// Sets the HUD counts label only when the text actually changed.
/// Unconditional set_label() queues a relayout of the HUD bar.
fn set_counts_label(badge: &Label, notes: usize, terms: usize) {
    let text = format!("{} Notes • {} Terminals", notes, terms);
    if badge.label().as_str() != text.as_str() {
        badge.set_label(&text);
    }
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
    hud: gtk4::Box,
    hud_badge: Label,
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
    /// Floating panels (📱 launcher, ⚙ settings) inside `root_overlay`; hidden
    /// with the window so they cannot reappear on the next show.
    overlay_panels: Vec<gtk4::Widget>,
    /// The workspace field's ▾ list. A popover is its own Wayland surface, so
    /// hiding the window does not dismiss it: it must be popped down
    /// explicitly, or it stays on screen over an empty desktop.
    ws_popover: Popover,
    /// Bumped by `show_again`; a slide-out that finishes afterwards must not
    /// unmap the window again (hide → show inside the 140ms animation).
    show_token: std::cell::Cell<u64>,
}

impl SuperDesktopWindow {
    /// `hot_inside` is the daemon's shared "pointer is in the top-left corner
    /// zone" flag: this window is full-screen and therefore sees every pointer
    /// move while it is visible, which is the half of the hot-corner gesture the
    /// corner surface cannot provide once the overlay covers it (see
    /// `crate::hotcorner`).
    pub fn new<FClose: Fn() + 'static>(
        app: &Application,
        on_request_close: FClose,
        hot_inside: Rc<Cell<bool>>,
    ) -> Rc<Self> {
        let window = ApplicationWindow::new(app);

        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("super-desktop"));

        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }

        window.set_keyboard_mode(KeyboardMode::OnDemand);
        window.add_css_class("super-desktop-window");

        let mut screen_width = 2560;
        let mut screen_height = 1600;

        if let Some(display) = gdk::Display::default() {
            let monitors = display.monitors();
            if let Some(mon) = monitors.item(0).and_then(|m| m.downcast::<gdk::Monitor>().ok()) {
                let geo = mon.geometry();
                screen_width = geo.width().max(1920);
                screen_height = geo.height().max(1080);
            }
        }

        let root_overlay = Overlay::new();
        let canvas = Fixed::new();
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        root_overlay.set_child(Some(&canvas));

        // Launcher connection hover card: floats centered above notes and
        // terminals, toggled by the 📱 HUD button (never a separate window).
        let launcher_panel = crate::launcher_settings::build_launcher_panel();
        launcher_panel.widget.set_visible(false);
        launcher_panel.widget.set_halign(Align::Center);
        launcher_panel.widget.set_valign(Align::Center);
        root_overlay.add_overlay(&launcher_panel.widget);

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

        let state = Rc::new(RefCell::new(load_state()));
        let note_cards: Rc<RefCell<Vec<Rc<StickyNote>>>> = Rc::new(RefCell::new(Vec::new()));
        let terminal_cards: Rc<RefCell<Vec<Rc<MiniTerminalCard>>>> =
            Rc::new(RefCell::new(Vec::new()));

        // Top-bar harness launch buttons, keyed by agent type: the ⚙ settings
        // panel shows/hides them, so every button is built once and visibility
        // is just `set_visible` (no HUD rebuild).
        let harness_buttons: Rc<RefCell<Vec<(String, Button)>>> =
            Rc::new(RefCell::new(Vec::new()));

        // HUD pieces the ⚙ panel writes to when the shortcut changes. They are
        // built here (and appended to `hud` further down) so the panel's
        // callback can capture them.
        let on_close_rc = Rc::new(on_request_close);
        let btn_close = Button::with_label("✕ Hide");
        btn_close.add_css_class("hud-button");
        btn_close.add_css_class("hud-button-danger");
        let hint = Label::new(None);
        hint.add_css_class("hud-shortcut");
        paint_shortcut_hints(&hint, &btn_close, &state.borrow());

        let settings_panel = crate::harness_settings::build_harness_settings_panel(
            Rc::clone(&state),
            Rc::new({
                let state = Rc::clone(&state);
                let harness_buttons = Rc::clone(&harness_buttons);
                move |keys: Vec<String>| {
                    let snapshot = {
                        let mut s = state.borrow_mut();
                        s.visible_harnesses = Some(keys.clone());
                        s.clone()
                    };
                    crate::state::save_state_async(snapshot);
                    for (key, btn) in harness_buttons.borrow().iter() {
                        btn.set_visible(keys.iter().any(|k| k == key));
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
        );
        settings_panel.widget.set_visible(false);
        settings_panel.widget.set_halign(Align::Center);
        settings_panel.widget.set_valign(Align::Center);

        let hud = gtk4::Box::new(Orientation::Horizontal, 10);
        hud.add_css_class("hud-bar");

        let brand = Label::new(Some("⚡ SUPER DESKTOP"));
        brand.add_css_class("hud-title");
        hud.append(&brand);

        // Workspace folder: the directory new harness cards start in. It sits
        // right after the brand so the folder in use is the first thing read
        // when a card is launched (see workspace_bar for why `~` is a bad
        // default once several agents run at once).
        let workspace_bar = crate::workspace_bar::build_workspace_bar(
            Rc::clone(&state),
            Rc::new(crate::state::save_state_async),
        );
        hud.append(&workspace_bar.widget);

        let hud_badge = Label::new(Some("0 Notes • 0 Agents"));
        hud_badge.add_css_class("hud-badge");
        hud.append(&hud_badge);

        let sep1 = Separator::new(Orientation::Vertical);
        hud.append(&sep1);

        let drag_pending = Rc::new(RefCell::new(HashMap::new()));
        let drag_tick_active = Rc::new(RefCell::new(false));
        let anim_trajectories = Rc::new(RefCell::new(HashMap::new()));
        let slide = Rc::new(SlideAnim::new());
        let on_slide_hidden: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let brand_images: Rc<RefCell<Vec<(Image, String)>>> = Rc::new(RefCell::new(Vec::new()));

        let win_rc = Rc::new(Self {
            window,
            canvas,
            ghost_box,
            ghost_label,
            state,
            note_cards,
            terminal_cards,
            hud: hud.clone(),
            hud_badge,
            screen_width,
            screen_height,
            drag_pending,
            drag_tick_active,
            anim_trajectories,
            slide,
            on_slide_hidden,
            brand_images: Rc::clone(&brand_images),
            settings_refresh: Rc::clone(&settings_panel.refresh),
            overlay_panels: vec![
                launcher_panel.widget.clone(),
                settings_panel.widget.clone(),
            ],
            ws_popover: workspace_bar.popover.clone(),
            show_token: std::cell::Cell::new(0),
        });

        // The overlay is OnDemand so an unfocused HUD does not eat desktop
        // keys. GtkEntry on a layer-shell surface only receives those keys
        // when the surface is Exclusive, so flip for as long as the folder
        // field holds focus (same as an expanded terminal card).
        {
            let focus = EventControllerFocus::new();
            let win = win_rc.window.clone();
            let terms = Rc::clone(&win_rc.terminal_cards);
            focus.connect_enter(move |_| {
                win.set_keyboard_mode(KeyboardMode::Exclusive);
            });
            let win = win_rc.window.clone();
            focus.connect_leave(move |_| {
                let any_expanded = terms.borrow().iter().any(|t| t.is_expanded());
                win.set_keyboard_mode(if any_expanded {
                    KeyboardMode::Exclusive
                } else {
                    KeyboardMode::OnDemand
                });
            });
            workspace_bar.entry.add_controller(focus);
        }

        // + Note Button
        let btn_note = Button::with_label("📝 + Note");
        btn_note.set_tooltip_text(Some("Create Sticky Note"));
        btn_note.add_css_class("hud-button");
        let win_w = Rc::downgrade(&win_rc);
        btn_note.connect_clicked(move |_| {
            if let Some(w) = win_w.upgrade() {
                w.create_new_note(None, None, "");
            }
        });
        hud.append(&btn_note);

        // Agents (company logo + name; emoji label if the SVG is missing).
        // Driven by `HARNESS_KEYS` so the launch buttons and the ⚙ settings
        // panel can never disagree about what this app can run; the visible
        // subset comes from the stored selection ∩ what is installed here.
        let detected = crate::tmux::detect_harnesses();
        let visible_keys = crate::harness_settings::resolve_visible(
            win_rc.state.borrow().visible_harnesses.as_deref(),
            &detected,
        );

        let light_theme = crate::theme::current_theme().mode == "light";
        for agent_key in crate::tmux::HARNESS_KEYS.iter().copied() {
            let (name, emoji) = harness_label(agent_key);
            let btn = Button::new();
            btn.add_css_class("hud-button");
            if let Some(logo) = crate::brand::logo_path(agent_key, light_theme) {
                let row = gtk4::Box::new(Orientation::Horizontal, 6);
                let img = Image::from_file(&logo);
                img.set_pixel_size(crate::brand::BRAND_ICON_SIZE);
                row.append(&img);
                row.append(&Label::new(Some(name)));
                btn.set_child(Some(&row));
                brand_images.borrow_mut().push((img, agent_key.to_string()));
            } else {
                btn.set_label(&format!("{emoji} {name}"));
            }
            let tooltip = match agent_key {
                "antigravity" => "Launch Antigravity CLI (--dangerously-skip-permissions)",
                "claude" => "Launch Claude Code (--dangerously-skip-permissions)",
                "codex" => "Launch OpenAI Codex (--dangerously-bypass-approvals-and-sandbox)",
                "opencode" => "Launch OpenCode (--auto)",
                "grok" => "Launch Grok CLI (--dangerously-skip-permissions)",
                "reasonix" => "Launch Reasonix (reasonix code, else npx -y reasonix code)",
                "aider" => "Launch Aider (--yes-always)",
                _ => "Launch Terminal Shell",
            };
            let has_usage = crate::usage::usage_id_for_agent(agent_key).is_some();
            if !has_usage {
                // Usage buttons render their launch hint inside the hover
                // card instead, so the native tooltip never double-renders
                // on top of it.
                btn.set_tooltip_text(Some(tooltip));
            }
            let win_w = Rc::downgrade(&win_rc);
            let a_key = agent_key.to_string();
            btn.connect_clicked(move |_| {
                if let Some(w) = win_w.upgrade() {
                    w.create_new_terminal(&a_key, None, None, None);
                }
            });

            // Hover usage card: Omarchy quota/tokens in a popover that hangs
            // flush under the provider button, headed by the button itself.
            if let Some(usage_id) = crate::usage::usage_id_for_agent(agent_key) {
                let pop = Popover::new();
                pop.add_css_class("usage-pop");
                pop.set_position(PositionType::Bottom);
                pop.set_has_arrow(false);
                pop.set_offset(0, 4);
                pop.set_autohide(false);
                pop.set_can_focus(false);
                pop.set_parent(&btn);

                let motion = EventControllerMotion::new();
                let pop_enter = pop.clone();
                let card = crate::usage::UsageCardInfo {
                    usage_id,
                    agent_key,
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
                btn.add_controller(motion);

                // Don't leave a stale hover card behind after launching.
                let pop_click = pop.clone();
                btn.connect_clicked(move |_| {
                    pop_click.popdown();
                });
            }
            btn.set_visible(visible_keys.iter().any(|k| k == agent_key));
            hud.append(&btn);
            harness_buttons
                .borrow_mut()
                .push((agent_key.to_string(), btn));
        }

        let sep2 = Separator::new(Orientation::Vertical);
        hud.append(&sep2);

        // Arrange
        let btn_arrange = Button::with_label("✨ Arrange");
        btn_arrange.set_tooltip_text(Some("Organize notes left, terminals right"));
        btn_arrange.add_css_class("hud-button");
        let win_w = Rc::downgrade(&win_rc);
        btn_arrange.connect_clicked(move |_| {
            if let Some(w) = win_w.upgrade() {
                w.auto_arrange();
            }
        });
        hud.append(&btn_arrange);

        // Launcher connection settings (IP / port / PIN for the Android app)
        let btn_launcher = Button::with_label("📱 Launcher");
        btn_launcher.set_tooltip_text(Some("Launcher connection: bridge status, IPs, PIN"));
        btn_launcher.add_css_class("hud-button");
        let panel_w = launcher_panel.widget.clone();
        let panel_refresh = Rc::clone(&launcher_panel.refresh);
        btn_launcher.connect_clicked(move |_| {
            let show = !panel_w.is_visible();
            panel_w.set_visible(show);
            if show {
                panel_refresh();
            }
        });
        hud.append(&btn_launcher);

        // Harness settings: which launch buttons the top bar shows
        let btn_settings = Button::with_label("⚙ Settings");
        btn_settings.set_tooltip_text(Some("Choose which harnesses appear in the top bar"));
        btn_settings.add_css_class("hud-button");
        let settings_w = settings_panel.widget.clone();
        let settings_refresh = Rc::clone(&settings_panel.refresh);
        btn_settings.connect_clicked(move |_| {
            let show = !settings_w.is_visible();
            settings_w.set_visible(show);
            if show {
                // Re-detect here: a harness installed while the app runs
                // shows up the next time the panel is opened.
                settings_refresh();
            }
        });
        hud.append(&btn_settings);

        // Close (its tooltip already names the current shortcut — see
        // `paint_shortcut_hints`).
        let on_close_btn = Rc::clone(&on_close_rc);
        btn_close.connect_clicked(move |_| {
            on_close_btn();
        });
        hud.append(&btn_close);

        hud.append(&hint);

        // On the canvas, not an Overlay child: Overlay+margin_top reallocates
        // the pill every frame and squashes the harness buttons. Fixed.move_
        // translates the whole bar as one widget, same as the cards.
        hud.set_hexpand(false);
        hud.set_vexpand(false);
        hud.set_halign(Align::Start);
        hud.set_valign(Align::Start);
        let (hud_w, _) = hud_measured_size(&hud);
        let hud_x = (win_rc.screen_width as f64 - hud_w) * 0.5;
        win_rc.canvas.put(&hud, hud_x, HUD_REST_MARGIN as f64);
        raise_canvas_child(&win_rc.canvas, &hud);

        {
            let canvas_hud = win_rc.canvas.clone();
            let slide_hud = Rc::clone(&win_rc.slide);
            let sw = win_rc.screen_width;
            hud.connect_notify_local(Some("width"), move |h, _| {
                if slide_hud.running.get() || slide_hud.progress.get() < 0.5 {
                    return;
                }
                let (w, _) = hud_measured_size(h);
                canvas_hud.move_(h, (sw as f64 - w) * 0.5, HUD_REST_MARGIN as f64);
            });
        }

        // Added after the HUD so the settings card floats above it.
        root_overlay.add_overlay(&settings_panel.widget);

        // Esc key
        let key_ctrl = EventControllerKey::new();
        let on_close_key = Rc::clone(&on_close_rc);
        let win_w = Rc::downgrade(&win_rc);
        let ws_popover = workspace_bar.popover.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, state| {
            if key == gdk::Key::Escape {
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
                    w.create_new_note(None, None, "");
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

        // Periodic status refresh
        let win_w = Rc::downgrade(&win_rc);
        glib::timeout_add_local(std::time::Duration::from_millis(1000), move || {
            if let Some(w) = win_w.upgrade() {
                // Off screen there is nothing to update: per-card status probes
                // are pure `tmux` processes, so skip them while hidden.
                if w.window.is_visible() {
                    w.periodic_refresh();
                }
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });

        win_rc
    }

    fn load_items(&self) {
        let terminals: Vec<TerminalData> = self.state.borrow().terminals.clone();
        for term_data in terminals {
            self.spawn_terminal_widget(term_data, false);
        }

        let notes: Vec<NoteData> = self.state.borrow().notes.clone();
        for note_data in notes {
            self.spawn_note_widget(note_data, false);
        }

        self.raise_all_notes();
        raise_canvas_child(&self.canvas, &self.hud);
        self.update_counts();
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
        self.update_counts();
    }

    fn spawn_note_widget(&self, note_data: NoteData, save: bool) {
        let canvas = self.canvas.clone();
        let state = Rc::clone(&self.state);
        let note_cards = Rc::clone(&self.note_cards);
        let hud_badge = self.hud_badge.clone();
        let term_len = self.terminal_cards.borrow().len();

        let drag_pending_update = Rc::clone(&self.drag_pending);
        let drag_tick_active = Rc::clone(&self.drag_tick_active);
        let sw = self.screen_width;
        let sh = self.screen_height;

        let canvas_for_tick = canvas.clone();
        let on_drag_update = move |widget: gtk4::Widget, x: f64, y: f64| {
            // Perf: .dragging disables hover transitions/shadows (see CSS)
            // so the note paints cheaply while it moves at 120Hz.
            if !widget.has_css_class("dragging") {
                widget.add_css_class("dragging");
            }
            let cx = x.clamp(10.0, (sw - 80) as f64);
            let cy = y.clamp(70.0, (sh - 60) as f64);
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
        let on_drag_end = move |widget: gtk4::Widget, data: &NoteData| {
            widget.remove_css_class("dragging");
            drag_pending_note_end.borrow_mut().remove(&widget);
            let mut final_data = data.clone();
            final_data.x = final_data.x.clamp(10, sw - 80);
            final_data.y = final_data.y.clamp(70, sh - 60);
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
        let hud_del = hud_badge.clone();

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
            let n = note_cards_del.borrow().len();
            set_counts_label(&hud_del, n, term_len);
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

        let note = StickyNote::new(
            note_data,
            on_drag_update,
            on_drag_end,
            on_delete,
            on_change,
            on_raise,
        );
        canvas.put(&note.container, x as f64, y as f64);
        note_cards.borrow_mut().push(Rc::new(note));
        raise_canvas_child(&canvas, &self.hud);
    }

    pub fn create_new_terminal(&self, agent_type: &str, cmd: Option<&str>, x: Option<i32>, y: Option<i32>) {
        // The folder from the top bar field: this card's harness starts there,
        // and keeps it for its whole life (see TerminalData::workspace_dir).
        let workspace_dir = crate::state::effective_workspace_dir(&self.state.borrow());
        let (sess, cmd_run) = create_session(agent_type, cmd, Some(&workspace_dir));
        let idx = self.terminal_cards.borrow().len();

        // Default size for a new harness: 640x480, clamped to the screen.
        let (def_w, def_h) =
            clamp_card_size(NEW_TERM_WIDTH, NEW_TERM_HEIGHT, self.screen_width, self.screen_height);
        // Center on screen; cascade slightly so stacked harnesses don't overlap exactly.
        let cascade = (idx as i32 % 5) * 32;
        let cx = ((self.screen_width - def_w) / 2 + cascade)
            .clamp(10, (self.screen_width - def_w - 10).max(10));
        let cy = ((self.screen_height - def_h) / 2 + cascade)
            .clamp(70, (self.screen_height - def_h - 10).max(70));

        let nx = x.unwrap_or(cx);
        let ny = y.unwrap_or(cy);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let data = TerminalData {
            id: sess.clone(),
            session_name: sess,
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
            tag: DEFAULT_TERMINAL_TAG,
            agent_session_id: None,
            workspace_dir: Some(workspace_dir),
        };

        self.spawn_terminal_widget(data, true);
        self.update_counts();
    }

    fn spawn_terminal_widget(&self, term_data: TerminalData, save: bool) {
        let canvas = self.canvas.clone();
        let state = Rc::clone(&self.state);
        let term_cards = Rc::clone(&self.terminal_cards);
        let hud_badge = self.hud_badge.clone();
        let notes_len = self.note_cards.borrow().len();

        let drag_pending_update = Rc::clone(&self.drag_pending);
        let drag_tick_active = Rc::clone(&self.drag_tick_active);
        let sw = self.screen_width;
        let sh = self.screen_height;

        let canvas_for_tick = canvas.clone();
        let on_drag_update = move |widget: gtk4::Widget, x: f64, y: f64| {
            // Perf: .dragging disables hover transitions/shadows (see CSS)
            // so the card paints cheaply while it moves at 120Hz.
            if !widget.has_css_class("dragging") {
                widget.add_css_class("dragging");
            }
            let cx = x.clamp(10.0, (sw - 80) as f64);
            let cy = y.clamp(70.0, (sh - 60) as f64);
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

        let canvas_term_end = canvas.clone();
        let drag_pending_term_end = Rc::clone(&self.drag_pending);
        let state_end = Rc::clone(&state);
        let on_drag_end = move |widget: gtk4::Widget, data: &TerminalData| {
            widget.remove_css_class("dragging");
            drag_pending_term_end.borrow_mut().remove(&widget);
            let mut final_data = data.clone();
            // The icon and the expanded card have separate remembered spots;
            // clamp and snap to whichever one this card is currently in.
            if final_data.iconified {
                final_data.icon_x = Some(final_data.icon_x.unwrap_or(final_data.x).clamp(10, sw - 80));
                final_data.icon_y = Some(final_data.icon_y.unwrap_or(final_data.y).clamp(70, sh - 60));
            } else {
                final_data.x = final_data.x.clamp(10, sw - 80);
                final_data.y = final_data.y.clamp(70, sh - 60);
            }
            let (px, py) = displayed_pos(&final_data);
            canvas_term_end.move_(&widget, px, py);
            let mut s = state_end.borrow_mut();
            if let Some(t) = s.terminals.iter_mut().find(|t| t.session_name == final_data.session_name) {
                *t = final_data;
            } else {
                s.terminals.push(final_data);
            }
            // Perf: keep the drag-end frame inside the 8.3ms 120Hz budget.
            let snapshot = s.clone();
            drop(s);
            crate::state::save_state_async(snapshot);
        };

        let terminal_cards_toggle = Rc::clone(&term_cards);
        let canvas_toggle = canvas.clone();
        let window_toggle = self.window.clone();
        let on_double_click = move |data: &TerminalData| {
            apply_terminal_expand(
                &terminal_cards_toggle,
                &canvas_toggle,
                &window_toggle,
                sw,
                sh,
                &data.session_name,
            );
        };

        let canvas_del = canvas.clone();
        let state_del = Rc::clone(&state);
        let term_cards_del = Rc::clone(&term_cards);
        let hud_del = hud_badge.clone();

        let on_close = move |sess: String| {
            kill_session(&sess);
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

            canvas_del.remove(&card.container);
            let mut s = state_del.borrow_mut();
            s.terminals.retain(|t| t.session_name != sess);
            let snapshot = s.clone();
            drop(s);
            crate::state::save_state_async(snapshot);
            let n = term_cards_del.borrow().len();
            set_counts_label(&hud_del, notes_len, n);
        };

        // Restored cards reappear wherever their current mode lives: an
        // iconified card belongs at its remembered icon spot, not at the
        // expanded card position.
        let (x, y) = displayed_pos(&term_data);

        if save {
            self.state.borrow_mut().terminals.push(term_data.clone());
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
            let Ok(mut cards) = term_cards_raise.try_borrow_mut() else {
                return;
            };
            if let Some(pos) = cards.iter().position(|c| c.data.borrow().session_name == sess_name) {
                let card = cards.remove(pos);
                cards.push(card);
            }
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
            sw,
            sh,
        );
        canvas.put(&card.container, x, y);
        term_cards.borrow_mut().push(Rc::new(card));
        raise_canvas_child(&canvas, &self.hud);
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
        self.window.set_keyboard_mode(KeyboardMode::OnDemand);

        let start_y = 110.0;
        let gap = 20.0;

        // Notes left
        let mut col_x = 60.0;
        let mut curr_y = start_y;
        for note in notes.iter() {
            let w = note.data.borrow().width as f64;
            let h = note.data.borrow().height as f64;
            if curr_y + h > (self.screen_height - 60) as f64 {
                col_x += w + gap;
                curr_y = start_y;
            }
            note.data.borrow_mut().x = col_x as i32;
            note.data.borrow_mut().y = curr_y as i32;
            self.canvas.move_(&note.container, col_x, curr_y);
            curr_y += h + gap;
        }

        // Terminals right
        let mut col_right = (self.screen_width - 30) as f64;
        let mut curr_y = start_y;
        let mut col_width = 0.0;
        for term in terms.iter() {
            let w = term.data.borrow().width as f64;
            let h = term.data.borrow().height as f64;
            if curr_y + h > (self.screen_height - 60) as f64 && curr_y > start_y {
                col_right -= col_width + gap;
                curr_y = start_y;
                col_width = 0.0;
            }
            let col_x = col_right - w;
            col_width = col_width.max(w);
            // Arrange writes the spot of whichever form is on screen (icons and
            // cards keep separate positions).
            set_displayed_pos(&mut term.data.borrow_mut(), col_x as i32, curr_y as i32);
            self.canvas.move_(&term.container, col_x, curr_y);
            curr_y += h + gap;
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
    }

    pub fn start_slide_in(&self) {
        self.ensure_slide_trajectories(!self.slide.running.get());
        if !self.slide.running.get() {
            // Idle and hidden: park cards at the edge so the appear starts
            // off-screen. Mid-flight reverse must not do this — it would jump.
            self.slide.progress.set(0.0);
            self.slide.velocity.set(SLIDE_LAUNCH_IN);
            self.paint_slide(0.0);
        }
        self.slide.appear.set(true);
        self.canvas.add_css_class("sliding");
        self.ensure_slide_tick();
    }

    pub fn start_slide_out<F: Fn() + 'static>(&self, on_finish: F) {
        self.ensure_slide_trajectories(false);
        *self.on_slide_hidden.borrow_mut() = Some(Rc::new(on_finish));
        if !self.slide.running.get() {
            // Idle and shown: start from rest with an outward impulse.
            // Mid-flight reverse keeps current progress *and* velocity.
            self.slide.progress.set(1.0);
            self.slide.velocity.set(-SLIDE_LAUNCH_OUT);
        }
        self.slide.appear.set(false);
        self.canvas.add_css_class("sliding");
        self.ensure_slide_tick();
    }

    /// Fill rest/edge positions. `reset` rebuilds them (fresh show). A reverse
    /// keeps the existing pair so the cards stay on the same path.
    fn ensure_slide_trajectories(&self, reset: bool) {
        let mut trajs = self.anim_trajectories.borrow_mut();
        if !reset && !trajs.is_empty() {
            return;
        }
        trajs.clear();

        let notes: Vec<Rc<StickyNote>> = self.note_cards.borrow().clone();
        let terms: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();

        for note in notes.iter() {
            let tx = note.data.borrow().x as f64;
            let ty = note.data.borrow().y as f64;
            let w = note.data.borrow().width as f64;
            let (sx, sy) = card_slide_offscreen(tx, ty, w, self.screen_width as f64);
            trajs.insert(note.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
        }
        for term in terms.iter() {
            let (tx, ty, w, _) = terminal_slide_geom(term, self.screen_width, self.screen_height);
            let (sx, sy) = card_slide_offscreen(tx, ty, w, self.screen_width as f64);
            trajs.insert(term.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
        }

        let (hw, hh) = hud_measured_size(&self.hud);
        let (tx, ty, sx, sy) = hud_slide_pose(hw, hh, self.screen_width as f64);
        trajs.insert(self.hud.clone().upcast(), Trajectory { sx, sy, tx, ty });
        raise_canvas_child(&self.canvas, &self.hud);
    }

    fn paint_slide(&self, progress: f64) {
        let trajs = self.anim_trajectories.borrow();
        for (widget, traj) in trajs.iter() {
            paint_slide_widget(&self.canvas, widget, *traj, progress);
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
        let gen = self.slide.gen.get().wrapping_add(1);
        self.slide.gen.set(gen);

        let frames: Vec<(gtk4::Widget, Trajectory)> = self
            .anim_trajectories
            .borrow()
            .iter()
            .map(|(w, t)| (w.clone(), *t))
            .collect();
        let slide = Rc::clone(&self.slide);
        let canvas = self.canvas.clone();
        let on_hidden = Rc::clone(&self.on_slide_hidden);

        // Tick the window, not the canvas: the layer-shell surface owns the
        // GDK frame clock, which Hyprland drives at the monitor refresh rate.
        // `add_tick_callback` is vsync; a glib timeout would cap us at 10–16ms.
        self.window.add_tick_callback(move |_, clock| {
            if slide.gen.get() != gen {
                return glib::ControlFlow::Break;
            }
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
                for (widget, traj) in frames.iter() {
                    paint_slide_widget(&canvas, widget, *traj, target);
                }
                canvas.remove_css_class("sliding");
                if !appear {
                    if let Some(cb) = on_hidden.borrow().clone() {
                        cb();
                    }
                }
                return glib::ControlFlow::Break;
            }
            slide.progress.set(progress);
            slide.velocity.set(velocity);
            for (widget, traj) in frames.iter() {
                paint_slide_widget(&canvas, widget, *traj, progress);
            }
            glib::ControlFlow::Continue
        });
    }

    fn update_counts(&self) {
        let n_notes = self.note_cards.borrow().len();
        let n_terms = self.terminal_cards.borrow().len();
        set_counts_label(&self.hud_badge, n_notes, n_terms);
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
        self.show_token.set(self.show_token.get().wrapping_add(1));
        self.window.present();
        self.window.set_visible(true);
        // Re-assert keyboard interactivity: typing in a card flips it to
        // Exclusive (see `apply_terminal_expand`), and a re-mapped layer surface
        // must not come back without it.
        self.window.set_keyboard_mode(KeyboardMode::OnDemand);
        self.start_slide_in();
        // Card statuses went stale while off screen (the periodic refresh is
        // paused then); this refreshes them on worker threads.
        self.periodic_refresh();
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
        // Floating panels must not come back with the window.
        for panel in &self.overlay_panels {
            panel.set_visible(false);
        }
        // …and neither may the workspace list, which is not part of the
        // overlay's widget tree (it is its own popup surface).
        self.ws_popover.popdown();
        self.window.set_visible(false);
    }

    fn periodic_refresh(&self) {
        if crate::theme::check_theme_changed() {
            self.reload_theme();
        }
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        for card in cards.iter() {
            card.refresh_status();
        }
        self.update_counts();
    }
}

/// Short top-bar label + emoji fallback for a harness key.
fn harness_label(key: &str) -> (&'static str, &'static str) {
    match key {
        "antigravity" => ("Antigravity", "🌌"),
        "claude" => ("Claude", "⚡"),
        "codex" => ("Codex", "🤖"),
        "opencode" => ("OpenCode", "🔮"),
        "grok" => ("Grok", "🚀"),
        "reasonix" => ("Reasonix", "🧭"),
        "aider" => ("Aider", "🧠"),
        _ => ("Shell", "💻"),
    }
}

fn raise_notes_on_canvas(canvas: &gtk4::Fixed, notes: &[Rc<crate::sticky_note::StickyNote>]) {
    for note in notes {
        if let Some(last) = canvas.last_child() {
            if &last != &note.container {
                note.container.insert_after(canvas, Some(&last));
            }
        }
    }
}

fn paint_slide_widget(canvas: &Fixed, widget: &gtk4::Widget, traj: Trajectory, factor: f64) {
    if widget.parent().is_some() {
        let cx = traj.sx + (traj.tx - traj.sx) * factor;
        let cy = traj.sy + (traj.ty - traj.sy) * factor;
        canvas.move_(widget, cx, cy);
    }
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

fn raise_canvas_child(canvas: &Fixed, widget: &impl gtk4::glib::object::IsA<gtk4::Widget>) {
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

/// Rest pose (centred under the top) and off-screen pose (same X, above the
/// overlay) for the toolbar as a single translated widget.
fn hud_slide_pose(hud_w: f64, hud_h: f64, screen_w: f64) -> (f64, f64, f64, f64) {
    let tx = ((screen_w - hud_w) * 0.5).max(0.0);
    let ty = HUD_REST_MARGIN as f64;
    let sy = -hud_h - HUD_OFFSCREEN_PAD;
    (tx, ty, tx, sy)
}

fn terminal_slide_geom(term: &MiniTerminalCard, sw: i32, sh: i32) -> (f64, f64, f64, f64) {
    let (w, h) = term.size(sw, sh);
    if term.is_expanded() {
        let (x, y, _, _) = expanded_rect(sw, sh);
        (x, y, w, h)
    } else {
        let (px, py) = displayed_pos(&term.data.borrow());
        (px, py, w, h)
    }
}

fn apply_terminal_expand(
    terminal_cards: &Rc<RefCell<Vec<Rc<MiniTerminalCard>>>>,
    canvas: &gtk4::Fixed,
    window: &ApplicationWindow,
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

    window.set_keyboard_mode(if will_expand {
        KeyboardMode::Exclusive
    } else {
        KeyboardMode::OnDemand
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_hud_slides_straight_up_from_its_rest_pose() {
        let (tx, ty, sx, sy) = hud_slide_pose(400.0, 48.0, 2560.0);
        assert_eq!(sx, tx, "toolbar must not drift sideways");
        assert_eq!(ty, HUD_REST_MARGIN as f64);
        assert!(sy < 0.0 && sy <= -48.0, "hidden toolbar sits fully above the overlay, sy={sy}");
        assert!((tx - (2560.0 - 400.0) * 0.5).abs() < 0.01);
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
        let _ = gtk4::init();
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

        let note1 = StickyNote::new(note1_data, |_, _, _| {}, |_, _| {}, |_| {}, |_| {}, |_| {});
        let note2 = StickyNote::new(note2_data, |_, _, _| {}, |_, _| {}, |_| {}, |_| {}, |_| {});

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
