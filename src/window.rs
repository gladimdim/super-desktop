use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Button, EventControllerKey, EventControllerMotion,
    Fixed, GestureClick, Image, Label, Orientation, Overlay, Popover, PositionType, Separator,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::mini_terminal::{
    clamp_card_size, expanded_rect, MiniTerminalCard, NEW_TERM_HEIGHT, NEW_TERM_WIDTH,
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

/// Sets the HUD counts label only when the text actually changed.
/// Unconditional set_label() queues a relayout of the HUD bar.
fn set_counts_label(badge: &Label, notes: usize, terms: usize) {
    let text = format!("{} Notes • {} Terminals", notes, terms);
    if badge.label().as_str() != text.as_str() {
        badge.set_label(&text);
    }
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
    hud_badge: Label,
    screen_width: i32,
    screen_height: i32,
    drag_pending: Rc<RefCell<HashMap<gtk4::Widget, (f64, f64)>>>,
    drag_tick_active: Rc<RefCell<bool>>,
    anim_trajectories: Rc<RefCell<HashMap<gtk4::Widget, Trajectory>>>,
    animating: Rc<RefCell<bool>>,
    /// Toolbar brand icons as (image widget, agent key) for theme-aware refresh.
    brand_images: Rc<RefCell<Vec<(Image, String)>>>,
}

impl SuperDesktopWindow {
    pub fn new<FClose: Fn() + 'static>(app: &Application, on_request_close: FClose) -> Rc<Self> {
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

        let hud = gtk4::Box::new(Orientation::Horizontal, 10);
        hud.add_css_class("hud-bar");

        let brand = Label::new(Some("⚡ SUPER DESKTOP"));
        brand.add_css_class("hud-title");
        hud.append(&brand);

        let hud_badge = Label::new(Some("0 Notes • 0 Agents"));
        hud_badge.add_css_class("hud-badge");
        hud.append(&hud_badge);

        let sep1 = Separator::new(Orientation::Vertical);
        hud.append(&sep1);

        let drag_pending = Rc::new(RefCell::new(HashMap::new()));
        let drag_tick_active = Rc::new(RefCell::new(false));
        let anim_trajectories = Rc::new(RefCell::new(HashMap::new()));
        let animating = Rc::new(RefCell::new(false));
        let brand_images: Rc<RefCell<Vec<(Image, String)>>> = Rc::new(RefCell::new(Vec::new()));

        let win_rc = Rc::new(Self {
            window,
            canvas,
            ghost_box,
            ghost_label,
            state,
            note_cards,
            terminal_cards,
            hud_badge,
            screen_width,
            screen_height,
            drag_pending,
            drag_tick_active,
            anim_trajectories,
            animating,
            brand_images: Rc::clone(&brand_images),
        });

        // + Note Button
        let btn_note = Button::with_label("📝 + Note");
        btn_note.set_tooltip_text(Some("Create Sticky Note (or double-click background)"));
        btn_note.add_css_class("hud-button");
        let win_w = Rc::downgrade(&win_rc);
        btn_note.connect_clicked(move |_| {
            if let Some(w) = win_w.upgrade() {
                w.create_new_note(None, None, "");
            }
        });
        hud.append(&btn_note);

        // Agents (company logo + name; emoji label if the SVG is missing)
        let agents = [
            ("antigravity", "Antigravity", "🌌"),
            ("claude", "Claude", "⚡"),
            ("codex", "Codex", "🤖"),
            ("opencode", "OpenCode", "🔮"),
            ("grok", "Grok", "🚀"),
            ("reasonix", "Reasonix", "🧭"),
            ("shell", "Shell", "💻"),
        ];

        let light_theme = crate::theme::current_theme().mode == "light";
        for (agent_key, name, emoji) in agents {
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
                "shell" => "Launch Terminal Shell",
                _ => "Launch Terminal",
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
            hud.append(&btn);
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

        // Close
        let btn_close = Button::with_label("✕ Hide");
        btn_close.set_tooltip_text(Some("Hide Super Desktop [SUPER + SHIFT + Q or Esc]"));
        btn_close.add_css_class("hud-button");
        btn_close.add_css_class("hud-button-danger");
        let on_close_rc = Rc::new(on_request_close);
        let on_close_btn = Rc::clone(&on_close_rc);
        btn_close.connect_clicked(move |_| {
            on_close_btn();
        });
        hud.append(&btn_close);

        let hint = Label::new(Some("[SUPER + SHIFT + Q]"));
        hint.add_css_class("hud-shortcut");
        hud.append(&hint);

        hud.set_halign(Align::Center);
        hud.set_valign(Align::Start);
        hud.set_margin_top(18);
        root_overlay.add_overlay(&hud);

        // Backdrop double-click
        let click = GestureClick::new();
        let win_w = Rc::downgrade(&win_rc);
        click.connect_released(move |_, n_press, x, y| {
            if n_press == 2 {
                if let Some(w) = win_w.upgrade() {
                    w.create_new_note(Some(x as i32), Some(y as i32), "");
                }
            }
        });
        win_rc.canvas.add_controller(click);

        // Esc key
        let key_ctrl = EventControllerKey::new();
        let on_close_key = Rc::clone(&on_close_rc);
        let win_w = Rc::downgrade(&win_rc);
        key_ctrl.connect_key_pressed(move |_, key, _, state| {
            if key == gdk::Key::Escape {
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

        win_rc.window.set_child(Some(&root_overlay));
        win_rc.load_items();

        // Periodic status refresh
        let win_w = Rc::downgrade(&win_rc);
        glib::timeout_add_local(std::time::Duration::from_millis(1000), move || {
            if let Some(w) = win_w.upgrade() {
                w.periodic_refresh();
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
        let note_cards_raise = Rc::clone(&note_cards);
        let note_id = note_data.id.clone();
        let on_raise = move |widget: gtk4::Widget| {
            if let Some(last) = canvas_raise.last_child() {
                if &last != &widget {
                    widget.insert_after(&canvas_raise, Some(&last));
                }
            }
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
    }

    pub fn create_new_terminal(&self, agent_type: &str, cmd: Option<&str>, x: Option<i32>, y: Option<i32>) {
        let (sess, cmd_run) = create_session(agent_type, cmd);
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
            created_at: now,
            tag: DEFAULT_TERMINAL_TAG,
            agent_session_id: None,
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
            final_data.x = final_data.x.clamp(10, sw - 80);
            final_data.y = final_data.y.clamp(70, sh - 60);
            canvas_term_end.move_(&widget, final_data.x as f64, final_data.y as f64);
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

        let x = term_data.x;
        let y = term_data.y;

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
        let term_cards_raise = Rc::clone(&term_cards);
        let sess_name = term_data.session_name.clone();
        let on_raise = move |widget: gtk4::Widget| {
            if let Some(last) = canvas_raise.last_child() {
                if &last != &widget {
                    widget.insert_after(&canvas_raise, Some(&last));
                }
            }
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
        canvas.put(&card.container, x as f64, y as f64);
        term_cards.borrow_mut().push(Rc::new(card));
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
                self.canvas.move_(
                    &term.container,
                    term.data.borrow().x as f64,
                    term.data.borrow().y as f64,
                );
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
            term.data.borrow_mut().x = col_x as i32;
            term.data.borrow_mut().y = curr_y as i32;
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
        let mut trajs = self.anim_trajectories.borrow_mut();
        trajs.clear();

        // Snapshot the cards so the list borrows are released before the
        // `move_` calls below (which can re-enter the raise handlers).
        let notes: Vec<Rc<StickyNote>> = self.note_cards.borrow().clone();
        let terms: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();

        for note in notes.iter() {
            let tx = note.data.borrow().x as f64;
            let ty = note.data.borrow().y as f64;
            let w = note.data.borrow().width as f64;
            let h = note.data.borrow().height as f64;
            let (sx, sy) = self.calc_edge_start(tx, ty, w, h);
            self.canvas.move_(&note.container, sx, sy);
            trajs.insert(note.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
        }

        for term in terms.iter() {
            let (tx, ty, w, h) = terminal_slide_geom(term, self.screen_width, self.screen_height);
            let (sx, sy) = self.calc_edge_start(tx, ty, w, h);
            self.canvas.move_(&term.container, sx, sy);
            trajs.insert(term.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
        }

        *self.animating.borrow_mut() = true;
        let start_time = Instant::now();
        let duration = 0.26;
        // Perf: snapshot to a Vec once. Iterating the HashMap + comparing
        // against the canvas (two ref/unref pairs per widget per frame)
        // showed up in profiles at 120Hz; a plain parent check is enough
        // since these widgets are only ever parented to the canvas.
        let frames: Vec<(gtk4::Widget, Trajectory)> = trajs
            .iter()
            .map(|(w, t)| (w.clone(), *t))
            .collect();
        drop(trajs);
        let anim_rc = Rc::clone(&self.animating);
        let canvas = self.canvas.clone();

        self.canvas.add_tick_callback(move |_, _| {
            let elapsed = start_time.elapsed().as_secs_f64();
            let progress = (elapsed / duration).min(1.0);
            let factor = 1.0 - (1.0 - progress).powi(3);

            for (widget, traj) in frames.iter() {
                if widget.parent().is_some() {
                    let cx = traj.sx + (traj.tx - traj.sx) * factor;
                    let cy = traj.sy + (traj.ty - traj.sy) * factor;
                    canvas.move_(widget, cx, cy);
                }
            }

            if progress >= 1.0 {
                *anim_rc.borrow_mut() = false;
                for (widget, traj) in frames.iter() {
                    if widget.parent().is_some() {
                        canvas.move_(widget, traj.tx, traj.ty);
                    }
                }
                return glib::ControlFlow::Break;
            }

            glib::ControlFlow::Continue
        });
    }

    pub fn start_slide_out<F: Fn() + 'static>(&self, on_finish: F) {
        let mut trajs = self.anim_trajectories.borrow_mut();
        if trajs.is_empty() {
            // Snapshot both lists so the borrows are gone while the
            // trajectories are computed (see `start_slide_in`).
            let notes: Vec<Rc<StickyNote>> = self.note_cards.borrow().clone();
            let terms: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();

            for note in notes.iter() {
                let tx = note.data.borrow().x as f64;
                let ty = note.data.borrow().y as f64;
                let w = note.data.borrow().width as f64;
                let h = note.data.borrow().height as f64;
                let (sx, sy) = self.calc_edge_start(tx, ty, w, h);
                trajs.insert(note.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
            }
            for term in terms.iter() {
                let (tx, ty, w, h) = terminal_slide_geom(term, self.screen_width, self.screen_height);
                let (sx, sy) = self.calc_edge_start(tx, ty, w, h);
                trajs.insert(term.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
            }
        }

        *self.animating.borrow_mut() = true;
        let start_time = Instant::now();
        let duration = 0.14;
        // Perf: same snapshot trick as slide-in (see above).
        let frames: Vec<(gtk4::Widget, Trajectory)> = trajs
            .iter()
            .map(|(w, t)| (w.clone(), *t))
            .collect();
        drop(trajs);
        let anim_rc = Rc::clone(&self.animating);
        let canvas = self.canvas.clone();
        let on_finish_rc = Rc::new(on_finish);

        self.canvas.add_tick_callback(move |_, _| {
            let elapsed = start_time.elapsed().as_secs_f64();
            let progress = (elapsed / duration).min(1.0);
            let factor = progress.powi(2);

            for (widget, traj) in frames.iter() {
                if widget.parent().is_some() {
                    let cx = traj.tx + (traj.sx - traj.tx) * factor;
                    let cy = traj.ty + (traj.sy - traj.ty) * factor;
                    canvas.move_(widget, cx, cy);
                }
            }

            if progress >= 1.0 {
                *anim_rc.borrow_mut() = false;
                on_finish_rc();
                return glib::ControlFlow::Break;
            }

            glib::ControlFlow::Continue
        });
    }

    fn calc_edge_start(&self, tx: f64, ty: f64, w: f64, h: f64) -> (f64, f64) {
        let sw = self.screen_width as f64;
        let sh = self.screen_height as f64;

        let d_left = tx;
        let d_right = sw - (tx + w);
        let d_top = ty;
        let d_bottom = sh - (ty + h);

        let min_d = d_left.min(d_right).min(d_top).min(d_bottom);
        if min_d == d_left {
            (-w - 40.0, ty)
        } else if min_d == d_right {
            (sw + 40.0, ty)
        } else if min_d == d_top {
            (tx, -h - 40.0)
        } else {
            (tx, sh + 40.0)
        }
    }

    fn update_counts(&self) {
        let n_notes = self.note_cards.borrow().len();
        let n_terms = self.terminal_cards.borrow().len();
        set_counts_label(&self.hud_badge, n_notes, n_terms);
    }


    pub fn raise_all_notes(&self) {
        let notes: Vec<Rc<StickyNote>> = self.note_cards.borrow().clone();
        raise_notes_on_canvas(&self.canvas, &notes);
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

fn raise_notes_on_canvas(canvas: &gtk4::Fixed, notes: &[Rc<crate::sticky_note::StickyNote>]) {
    for note in notes {
        if let Some(last) = canvas.last_child() {
            if &last != &note.container {
                note.container.insert_after(canvas, Some(&last));
            }
        }
    }
}

fn terminal_slide_geom(term: &MiniTerminalCard, sw: i32, sh: i32) -> (f64, f64, f64, f64) {
    let (w, h) = term.size(sw, sh);
    if term.is_expanded() {
        let (x, y, _, _) = expanded_rect(sw, sh);
        (x, y, w, h)
    } else {
        (term.data.borrow().x as f64, term.data.borrow().y as f64, w, h)
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
                card.collapse();
                canvas.move_(
                    &card.container,
                    card.data.borrow().x as f64,
                    card.data.borrow().y as f64,
                );
            }
        } else if card.is_expanded() {
            card.collapse();
            canvas.move_(
                &card.container,
                card.data.borrow().x as f64,
                card.data.borrow().y as f64,
            );
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
