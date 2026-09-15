use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Button, EventControllerKey, Fixed, GestureClick, Label,
    Orientation, Overlay, Separator,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::mini_terminal::MiniTerminalCard;
use crate::state::{load_state, save_state, AppState, NoteData, TerminalData};
use crate::sticky_note::StickyNote;
use crate::tmux::{create_session, kill_session, launch_fullscreen};

struct Trajectory {
    sx: f64,
    sy: f64,
    tx: f64,
    ty: f64,
}

pub struct SuperDesktopWindow {
    pub window: ApplicationWindow,
    canvas: Fixed,
    state: Rc<RefCell<AppState>>,
    note_cards: Rc<RefCell<Vec<StickyNote>>>,
    terminal_cards: Rc<RefCell<Vec<MiniTerminalCard>>>,
    hud_badge: Label,
    screen_width: i32,
    screen_height: i32,
    drag_pending: Rc<RefCell<HashMap<gtk4::Widget, (f64, f64)>>>,
    drag_tick_active: Rc<RefCell<bool>>,
    anim_trajectories: Rc<RefCell<HashMap<gtk4::Widget, Trajectory>>>,
    animating: Rc<RefCell<bool>>,
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

        let state = Rc::new(RefCell::new(load_state()));
        let note_cards = Rc::new(RefCell::new(Vec::new()));
        let terminal_cards = Rc::new(RefCell::new(Vec::new()));

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

        let win_rc = Rc::new(Self {
            window,
            canvas,
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

        // Agents
        let agents = [
            ("antigravity", "🌌 Antigravity"),
            ("claude", "⚡ Claude"),
            ("codex", "🤖 Codex"),
            ("opencode", "🔮 OpenCode"),
            ("grok", "🚀 Grok"),
            ("shell", "💻 Shell"),
        ];

        for (agent_key, label) in agents {
            let btn = Button::with_label(label);
            btn.add_css_class("hud-button");
            let win_w = Rc::downgrade(&win_rc);
            let a_key = agent_key.to_string();
            btn.connect_clicked(move |_| {
                if let Some(w) = win_w.upgrade() {
                    w.create_new_terminal(&a_key, None, None, None);
                }
            });
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
                on_close_key();
                return glib::Propagation::Stop;
            } else if state.contains(gdk::ModifierType::CONTROL_MASK) && (key == gdk::Key::n || key == gdk::Key::N) {
                if let Some(w) = win_w.upgrade() {
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
        glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
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
        let notes: Vec<NoteData> = self.state.borrow().notes.clone();
        for note_data in notes {
            self.spawn_note_widget(note_data, false);
        }

        let terminals: Vec<TerminalData> = self.state.borrow().terminals.clone();
        for term_data in terminals {
            self.spawn_terminal_widget(term_data, false);
        }

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
            color: "yellow".to_string(),
            updated_at: now as f64 / 1000.0,
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

        let drag_pending = Rc::clone(&self.drag_pending);
        let drag_tick_active = Rc::clone(&self.drag_tick_active);
        let sw = self.screen_width;
        let sh = self.screen_height;

        let canvas_for_tick = canvas.clone();
        let on_drag_update = move |widget: gtk4::Widget, x: f64, y: f64| {
            let cx = x.clamp(10.0, (sw - 80) as f64);
            let cy = y.clamp(70.0, (sh - 60) as f64);
            drag_pending.borrow_mut().insert(widget, (cx, cy));

            if !*drag_tick_active.borrow() {
                *drag_tick_active.borrow_mut() = true;
                let dp = Rc::clone(&drag_pending);
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

        let state_end = Rc::clone(&state);
        let on_drag_end = move |_widget: gtk4::Widget, data: &NoteData| {
            let mut s = state_end.borrow_mut();
            if let Some(n) = s.notes.iter_mut().find(|n| n.id == data.id) {
                *n = data.clone();
            } else {
                s.notes.push(data.clone());
            }
            save_state(&s);
        };

        let canvas_del = canvas.clone();
        let state_del = Rc::clone(&state);
        let note_cards_del = Rc::clone(&note_cards);
        let hud_del = hud_badge.clone();

        let on_delete = move |id: String| {
            let mut cards = note_cards_del.borrow_mut();
            if let Some(pos) = cards.iter().position(|c| c.data.borrow().id == id) {
                let note = cards.remove(pos);
                canvas_del.remove(&note.container);
                let mut s = state_del.borrow_mut();
                s.notes.retain(|n| n.id != id);
                save_state(&s);
                hud_del.set_label(&format!("{} Notes • {} Terminals", cards.len(), term_len));
            }
        };

        let state_change = Rc::clone(&state);
        let on_change = move |data: &NoteData| {
            let mut s = state_change.borrow_mut();
            if let Some(n) = s.notes.iter_mut().find(|n| n.id == data.id) {
                *n = data.clone();
            }
            save_state(&s);
        };

        let x = note_data.x;
        let y = note_data.y;

        if save {
            self.state.borrow_mut().notes.push(note_data.clone());
            save_state(&self.state.borrow());
        }

        let note = StickyNote::new(note_data, on_drag_update, on_drag_end, on_delete, on_change);
        canvas.put(&note.container, x as f64, y as f64);
        note_cards.borrow_mut().push(note);
    }

    pub fn create_new_terminal(&self, agent_type: &str, cmd: Option<&str>, x: Option<i32>, y: Option<i32>) {
        let (sess, cmd_run) = create_session(agent_type, cmd);
        let idx = self.terminal_cards.borrow().len();

        let nx = x.unwrap_or((self.screen_width - 340 - (idx as i32 % 3) * 310).max(400));
        let ny = y.unwrap_or(140 + (idx as i32 % 3) * 200);

        let data = TerminalData {
            id: sess.clone(),
            session_name: sess,
            agent_type: agent_type.to_string(),
            command: cmd_run,
            x: nx,
            y: ny,
            created_at: 0.0,
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

        let drag_pending = Rc::clone(&self.drag_pending);
        let drag_tick_active = Rc::clone(&self.drag_tick_active);
        let sw = self.screen_width;
        let sh = self.screen_height;

        let canvas_for_tick = canvas.clone();
        let on_drag_update = move |widget: gtk4::Widget, x: f64, y: f64| {
            let cx = x.clamp(10.0, (sw - 80) as f64);
            let cy = y.clamp(70.0, (sh - 60) as f64);
            drag_pending.borrow_mut().insert(widget, (cx, cy));

            if !*drag_tick_active.borrow() {
                *drag_tick_active.borrow_mut() = true;
                let dp = Rc::clone(&drag_pending);
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

        let state_end = Rc::clone(&state);
        let on_drag_end = move |_widget: gtk4::Widget, data: &TerminalData| {
            let mut s = state_end.borrow_mut();
            if let Some(t) = s.terminals.iter_mut().find(|t| t.session_name == data.session_name) {
                *t = data.clone();
            } else {
                s.terminals.push(data.clone());
            }
            save_state(&s);
        };

        let win = self.window.clone();
        let on_double_click = move |data: &TerminalData| {
            launch_fullscreen(&data.session_name, &data.agent_type);
            win.close();
        };

        let canvas_del = canvas.clone();
        let state_del = Rc::clone(&state);
        let term_cards_del = Rc::clone(&term_cards);
        let hud_del = hud_badge.clone();

        let on_close = move |sess: String| {
            kill_session(&sess);
            let mut cards = term_cards_del.borrow_mut();
            if let Some(pos) = cards.iter().position(|c| c.data.borrow().session_name == sess) {
                let card = cards.remove(pos);
                canvas_del.remove(&card.container);
                let mut s = state_del.borrow_mut();
                s.terminals.retain(|t| t.session_name != sess);
                save_state(&s);
                hud_del.set_label(&format!("{} Notes • {} Terminals", notes_len, cards.len()));
            }
        };

        let x = term_data.x;
        let y = term_data.y;

        if save {
            self.state.borrow_mut().terminals.push(term_data.clone());
            save_state(&self.state.borrow());
        }

        let card = MiniTerminalCard::new(term_data, on_drag_update, on_drag_end, on_double_click, on_close);
        canvas.put(&card.container, x as f64, y as f64);
        term_cards.borrow_mut().push(card);
    }

    pub fn auto_arrange(&self) {
        let start_y = 110.0;
        let gap = 20.0;

        // Notes left
        let mut col_x = 60.0;
        let mut curr_y = start_y;
        for note in self.note_cards.borrow().iter() {
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
        let mut col_x = (self.screen_width - 320) as f64;
        let mut curr_y = start_y;
        for term in self.terminal_cards.borrow().iter() {
            let w = 290.0;
            let h = 185.0;
            if curr_y + h > (self.screen_height - 60) as f64 {
                col_x -= w + gap;
                curr_y = start_y;
            }
            term.data.borrow_mut().x = col_x as i32;
            term.data.borrow_mut().y = curr_y as i32;
            self.canvas.move_(&term.container, col_x, curr_y);
            curr_y += h + gap;
        }

        let mut s = self.state.borrow_mut();
        for note in self.note_cards.borrow().iter() {
            if let Some(n) = s.notes.iter_mut().find(|n| n.id == note.data.borrow().id) {
                *n = note.data.borrow().clone();
            }
        }
        for term in self.terminal_cards.borrow().iter() {
            if let Some(t) = s.terminals.iter_mut().find(|t| t.session_name == term.data.borrow().session_name) {
                *t = term.data.borrow().clone();
            }
        }
        save_state(&s);
    }

    pub fn start_slide_in(&self) {
        let mut trajs = self.anim_trajectories.borrow_mut();
        trajs.clear();

        for note in self.note_cards.borrow().iter() {
            let tx = note.data.borrow().x as f64;
            let ty = note.data.borrow().y as f64;
            let w = note.data.borrow().width as f64;
            let h = note.data.borrow().height as f64;
            let (sx, sy) = self.calc_edge_start(tx, ty, w, h);
            self.canvas.move_(&note.container, sx, sy);
            trajs.insert(note.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
        }

        for term in self.terminal_cards.borrow().iter() {
            let tx = term.data.borrow().x as f64;
            let ty = term.data.borrow().y as f64;
            let w = 290.0;
            let h = 185.0;
            let (sx, sy) = self.calc_edge_start(tx, ty, w, h);
            self.canvas.move_(&term.container, sx, sy);
            trajs.insert(term.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
        }

        *self.animating.borrow_mut() = true;
        let start_time = Instant::now();
        let duration = 0.26;
        let trajs_rc = Rc::clone(&self.anim_trajectories);
        let anim_rc = Rc::clone(&self.animating);
        let canvas = self.canvas.clone();

        self.canvas.add_tick_callback(move |_, _| {
            let elapsed = start_time.elapsed().as_secs_f64();
            let progress = (elapsed / duration).min(1.0);
            let factor = 1.0 - (1.0 - progress).powi(3);

            for (widget, traj) in trajs_rc.borrow().iter() {
                let cx = traj.sx + (traj.tx - traj.sx) * factor;
                let cy = traj.sy + (traj.ty - traj.sy) * factor;
                canvas.move_(widget, cx, cy);
            }

            if progress >= 1.0 {
                *anim_rc.borrow_mut() = false;
                for (widget, traj) in trajs_rc.borrow().iter() {
                    canvas.move_(widget, traj.tx, traj.ty);
                }
                return glib::ControlFlow::Break;
            }

            glib::ControlFlow::Continue
        });
    }

    pub fn start_slide_out<F: Fn() + 'static>(&self, on_finish: F) {
        let mut trajs = self.anim_trajectories.borrow_mut();
        if trajs.is_empty() {
            for note in self.note_cards.borrow().iter() {
                let tx = note.data.borrow().x as f64;
                let ty = note.data.borrow().y as f64;
                let w = note.data.borrow().width as f64;
                let h = note.data.borrow().height as f64;
                let (sx, sy) = self.calc_edge_start(tx, ty, w, h);
                trajs.insert(note.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
            }
            for term in self.terminal_cards.borrow().iter() {
                let tx = term.data.borrow().x as f64;
                let ty = term.data.borrow().y as f64;
                let w = 290.0;
                let h = 185.0;
                let (sx, sy) = self.calc_edge_start(tx, ty, w, h);
                trajs.insert(term.container.clone().upcast(), Trajectory { sx, sy, tx, ty });
            }
        }

        *self.animating.borrow_mut() = true;
        let start_time = Instant::now();
        let duration = 0.14;
        let trajs_rc = Rc::clone(&self.anim_trajectories);
        let anim_rc = Rc::clone(&self.animating);
        let canvas = self.canvas.clone();
        let on_finish_rc = Rc::new(on_finish);

        self.canvas.add_tick_callback(move |_, _| {
            let elapsed = start_time.elapsed().as_secs_f64();
            let progress = (elapsed / duration).min(1.0);
            let factor = progress.powi(2);

            for (widget, traj) in trajs_rc.borrow().iter() {
                let cx = traj.tx + (traj.sx - traj.tx) * factor;
                let cy = traj.ty + (traj.sy - traj.ty) * factor;
                canvas.move_(widget, cx, cy);
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
        self.hud_badge.set_label(&format!("{} Notes • {} Terminals", n_notes, n_terms));
    }

    fn periodic_refresh(&self) {
        for card in self.terminal_cards.borrow().iter() {
            card.refresh_status();
        }
        self.update_counts();
    }
}
