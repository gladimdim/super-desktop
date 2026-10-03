//! The window renderer on the desktop: each frame, this PC's cards go to the
//! plugin's renderer (`plugin_host::renderer`) and come back as where and
//! how they are drawn (`MiniTerminalCard::present`). Saved layout is never
//! touched, except by a drop target, which is applied like a user's drop.
//!
//! The frame clock runs only while something moves: a drag, a renderer that
//! says it is animating, or a change that asked for a frame (`wake`). Expanded
//! cards keep the built-in layout, and so does everything during the show/hide
//! slide. A failed frame draws the built-in layout; after three failures the
//! renderer is off and its plugin marked failed.
use super::{manager, Manager, State};
use crate::card_resize::Rect;
use crate::desktop_protocol::WorkspaceCommand;
use crate::mini_terminal::{MiniTerminalCard, Relayout};
use crate::plugin_host::api;
use crate::plugin_host::renderer::{self, CardIn, Frame, Mode, Renderer};
use gtk4::glib;
use gtk4::prelude::*;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long cards take to go back to the built-in layout.
const RETURN: Duration = Duration::from_millis(200);
/// Without a frame for this long while something moves, a timer draws the
/// next one: a frame clock can stop ticking (a compositor pausing a hidden
/// surface, a display with no client attached) and the animation must finish.
const FRAME_WATCHDOG: Duration = Duration::from_millis(40);

pub struct Driver {
    pub plugin: String,
    renderer: Renderer,
    ids: HashMap<String, u32>,
    next_id: u32,
    ticking: bool,
    started: Instant,
    last: Option<Instant>,
    dropped: HashSet<String>,
    /// The plugin's `renderer.params` settings, read when it starts and when
    /// its settings change.
    params: Vec<f64>,
}



/// Cards on their way back to the built-in layout after a renderer stopped.
pub struct Return {
    from: Vec<(String, Rect, bool)>,
    started: Instant,
}

fn lerp(a: Rect, b: Rect, t: f64) -> Rect {
    let mix = |x: f64, y: f64| x + (y - x) * t;
    let size = |x: i32, y: i32| mix(f64::from(x), f64::from(y)).round().max(1.0) as i32;
    Rect { x: mix(a.x, b.x), y: mix(a.y, b.y), width: size(a.width, b.width), height: size(a.height, b.height) }
}

fn status_code(status: &str) -> u32 {
    match status {
        s if s.contains("work") || s.contains("busy") || s.contains("active") => 1,
        s if s.contains("idle") => 2,
        s if s.contains("wait") => 3,
        s if s.contains("complet") => 4,
        s if s.contains("error") || s.contains("exit") => 5,
        _ => 0,
    }
}

/// Where the built-in layout draws a card.
fn built_in_rect(card: &MiniTerminalCard) -> (Rect, bool) {
    let data = card.data.borrow();
    let (x, y) = crate::mini_terminal::displayed_pos(&data);
    (Rect { x, y, width: data.width, height: data.height }, data.iconified)
}

impl Manager {
    /// Load a plugin's renderer. Only one at a time (activation refuses a
    /// second provider).
    pub(super) fn start_renderer(self: &Rc<Self>, plugin: &str, dir: &std::path::Path, manifest: &crate::plugin_host::manifest::Manifest) -> Result<(), String> {
        let spec = manifest.contributes.renderer.as_ref().ok_or("the plugin has no renderer")?;
        let wasm = &spec.wasm;
        let bytes = std::fs::read(dir.join(wasm)).map_err(|e| format!("cannot read {wasm}: {e} (build it first)"))?;
        let renderer = Renderer::load_with_params(&bytes, &crate::plugin_host::log_file(plugin), spec.params.len())?;
        api::append_log(&crate::plugin_host::log_file(plugin), "info", "renderer loaded");
        self.renderer.replace(Some(Driver {
            plugin: plugin.to_string(),
            renderer,
            ids: HashMap::new(),
            next_id: 1,
            ticking: false,
            started: Instant::now(),
            last: None,
            dropped: HashSet::new(),
            params: renderer::params_of(manifest),
        }));
        self.renderer_wake();
        Ok(())
    }

    /// Whether this plugin's renderer is the one drawing (or ready to draw).
    pub fn renderer_running(&self, plugin: &str) -> bool {
        self.renderer.borrow().as_ref().is_some_and(|d| d.plugin == plugin && d.renderer.off.is_none())
    }

    /// The plugin's settings changed: pass the new params on the next frame.
    pub(super) fn refresh_renderer_params(self: &Rc<Self>, plugin: &str, manifest: &crate::plugin_host::manifest::Manifest) {
        if let Some(driver) = self.renderer.borrow_mut().as_mut().filter(|d| d.plugin == plugin) {
            driver.params = renderer::params_of(manifest);
        }
        self.renderer_wake();
    }

    /// Stop drawing: cards glide back to the built-in layout.
    pub(super) fn stop_renderer(self: &Rc<Self>, plugin: &str) {
        let is_it = self.renderer.borrow().as_ref().is_some_and(|d| d.plugin == plugin);
        if !is_it {
            return;
        }
        self.renderer.replace(None);
        let Some(workspace) = self.workspace.borrow().clone() else { return };
        let from: Vec<(String, Rect, bool)> = workspace
            .cards()
            .iter()
            .filter_map(|c| c.presented().map(|p| (c.data.borrow().id.clone(), p.rect, p.icon)))
            .collect();
        if from.is_empty() {
            return;
        }
        self.returning.replace(Some(Return { from, started: Instant::now() }));
        self.renderer_wake();
    }

    pub fn renderer_drawing(&self) -> bool {
        self.overlay_shown.get() && self.renderer.borrow().as_ref().is_some_and(|d| d.renderer.off.is_none())
    }

    /// Ask for frames until the layout settles.
    pub fn renderer_wake(self: &Rc<Self>) {
        let wants = self.renderer.borrow().is_some() || self.returning.borrow().is_some();
        if !wants || !self.overlay_shown.get() {
            return;
        }
        if let Some(driver) = self.renderer.borrow_mut().as_mut() {
            if driver.ticking {
                return;
            }
            driver.ticking = true;
            driver.last = None;
        } else if self.return_ticking.replace(true) {
            return;
        }
        let Some(canvas) = self.workspace.borrow().as_ref().and_then(|w| w.canvas()) else {
            self.stop_ticking();
            return;
        };
        self.frame_at.set(Some(Instant::now()));
        canvas.add_tick_callback(|_, _| {
            let manager = manager();
            if !manager.ticking() {
                return glib::ControlFlow::Break;
            }
            if manager.timed_frame() {
                glib::ControlFlow::Continue
            } else {
                manager.stop_ticking();
                glib::ControlFlow::Break
            }
        });
        glib::timeout_add_local(FRAME_WATCHDOG, || {
            let manager = manager();
            if !manager.ticking() {
                return glib::ControlFlow::Break;
            }
            let stalled = manager.frame_at.get().is_none_or(|at| at.elapsed() >= FRAME_WATCHDOG);
            if stalled && !manager.timed_frame() {
                manager.stop_ticking();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    /// Whether frames are wanted (the tick callback and the watchdog run).
    fn ticking(&self) -> bool {
        self.renderer.borrow().as_ref().is_some_and(|d| d.ticking) || self.return_ticking.get()
    }

    fn timed_frame(self: &Rc<Self>) -> bool {
        self.frame_at.set(Some(Instant::now()));
        self.frame()
    }

    fn stop_ticking(&self) {
        if let Some(driver) = self.renderer.borrow_mut().as_mut() {
            driver.ticking = false;
        }
        self.return_ticking.set(false);
    }

    pub fn renderer_dropped(self: &Rc<Self>, card_id: &str) {
        if let Some(driver) = self.renderer.borrow_mut().as_mut() {
            driver.dropped.insert(card_id.to_string());
        }
        self.renderer_wake();
    }

    /// One frame. Returns whether another one is needed.
    fn frame(self: &Rc<Self>) -> bool {
        if !self.overlay_shown.get() {
            return false;
        }
        let Some(workspace) = self.workspace.borrow().clone() else { return false };
        let Some(canvas) = workspace.canvas() else { return false };
        if self.returning.borrow().is_some() {
            return self.return_frame(&workspace, &canvas);
        }
        // The show/hide slide owns the cards while it runs.
        if workspace.sliding() {
            return true;
        }
        let cards: Vec<Rc<MiniTerminalCard>> = workspace.cards().into_iter().filter(|c| !c.is_remote()).collect();
        let (w, h, top) = workspace.screen();
        let mut driver_ref = self.renderer.borrow_mut();
        let Some(driver) = driver_ref.as_mut() else { return false };
        let now = Instant::now();
        let dt = driver.last.map(|l| now.duration_since(l).as_secs_f64() * 1000.0).unwrap_or(0.0);
        driver.last = Some(now);
        let mut frame = Frame {
            screen_w: f64::from(w),
            screen_h: f64::from(h),
            top: f64::from(top),
            time_ms: now.duration_since(driver.started).as_secs_f64() * 1000.0,
            dt_ms: dt.min(100.0),
            params: driver.params.clone(),
            ..Frame::default()
        };
        let mut drawn: Vec<(u32, Rc<MiniTerminalCard>)> = Vec::new();
        let mut dragging = false;
        let (min_w, min_h) = crate::mini_terminal::min_card_size(1.0);
        for (z, card) in cards.iter().enumerate() {
            if card.is_expanded() {
                card.unpresent(&canvas);
                continue;
            }
            let card_id = card.data.borrow().id.clone();
            let id = *driver.ids.entry(card_id.clone()).or_insert_with(|| {
                driver.next_id += 1;
                driver.next_id - 1
            });
            let data = card.data.borrow().clone();
            let (sw, sh) = if data.iconified { (data.restored_width, data.restored_height) } else { (data.width, data.height) };
            let being_dragged = card.is_being_dragged();
            dragging |= being_dragged;
            let focused = card.container.root().and_then(|r| gtk4::prelude::RootExt::focus(&r)).is_some_and(|f| f.is_ancestor(&card.container));
            let mut flags = 0;
            for (on, flag) in [
                (data.iconified, renderer::FLAG_ICONIFIED),
                (being_dragged, renderer::FLAG_DRAGGING),
                (focused, renderer::FLAG_FOCUSED),
                (driver.dropped.remove(&card_id), renderer::FLAG_DROPPED),
            ] {
                if on {
                    flags |= flag;
                }
            }
            if focused {
                frame.focused = id;
            }
            if being_dragged {
                frame.pointer = (f64::from(data.x + sw / 2), f64::from(data.y + sh / 2));
                frame.pointer_down = true;
            }
            frame.cards.push(CardIn {
                id,
                flags,
                saved: renderer::Rect { x: f64::from(data.x), y: f64::from(data.y), w: f64::from(sw), h: f64::from(sh) },
                icon_x: f64::from(data.icon_x.unwrap_or(data.x)),
                icon_y: f64::from(data.icon_y.unwrap_or(data.y)),
                icon_side: f64::from(crate::mini_terminal::icon_side(1.0)),
                status: status_code(&card.title_inputs().2),
                agent_hash: renderer::agent_hash(&data.agent_type),
                z: z as u32,
                min_w: f64::from(min_w),
                min_h: f64::from(min_h),
            });
            drawn.push((id, Rc::clone(card)));
            if frame.cards.len() == renderer::MAX_CARDS {
                break;
            }
        }
        frame.phase = u32::from(dragging);
        let result = driver.renderer.present(&frame);
        let off = driver.renderer.off.clone();
        let plugin = driver.plugin.clone();
        drop(driver_ref);
        match result {
            Ok(output) => {
                // Terminals are resized once nothing moves, and a dragged
                // card's a few times a second: never on every frame.
                let settled = !output.animating && !dragging;
                for out in &output.cards {
                    if let Some((_, card)) = drawn.iter().find(|(id, _)| *id == out.id) {
                        let rect = Rect { x: out.rect.x, y: out.rect.y, width: out.rect.w.round() as i32, height: out.rect.h.round() as i32 };
                        match out.mode {
                            Mode::Resized => {
                                let relayout = if settled {
                                    Relayout::Now
                                } else if card.is_being_dragged() {
                                    Relayout::Live
                                } else {
                                    Relayout::Never
                                };
                                card.present_resized(&canvas, rect, relayout, out.opacity)
                            }
                            mode => card.present(&canvas, rect, mode == Mode::Icon, out.opacity),
                        }
                    }
                }
                if let Some(drop) = output.drop {
                    if let Some((_, card)) = drawn.iter().find(|(id, _)| *id == drop.id) {
                        let card = Rc::clone(card);
                        let workspace = Rc::clone(&workspace);
                        // After this frame: a command saves state and moves widgets.
                        glib::idle_add_local_once(move || {
                            let mut layout = super::cards::layout_of(&card);
                            layout.iconified = drop.to_icon;
                            if drop.to_icon {
                                layout.icon_x = Some(drop.x.round() as i32);
                                layout.icon_y = Some(drop.y.round() as i32);
                            } else {
                                layout.x = drop.x.round() as i32;
                                layout.y = drop.y.round() as i32;
                            }
                            let id = card.data.borrow().id.clone();
                            let _ = workspace.command(&WorkspaceCommand::SetLayout { card_id: id, expected_revision: 0, layout });
                            manager().renderer_wake();
                        });
                    }
                }
                workspace.refresh_ghosts();
                output.animating || dragging
            }
            Err(why) => {
                for (_, card) in &drawn {
                    card.unpresent(&canvas);
                }
                workspace.refresh_ghosts();
                if let Some(off) = off {
                    api::append_log(&crate::plugin_host::log_file(&plugin), "error", &format!("renderer {off}"));
                    self.renderer.replace(None);
                    if let Some(p) = self.plugins.borrow_mut().get_mut(&plugin) {
                        p.state = State::Failed(format!("renderer {off}"));
                    }
                    self.sync_toggles(&plugin);
                    self.changed();
                    return false;
                }
                api::append_log(&crate::plugin_host::log_file(&plugin), "warn", &format!("renderer frame failed, built-in layout used: {why}"));
                true
            }
        }
    }

    /// Glide every card from where the renderer left it to the built-in layout.
    fn return_frame(&self, workspace: &Rc<dyn super::cards::Workspace>, canvas: &gtk4::Fixed) -> bool {
        let (from, started) = {
            let returning = self.returning.borrow();
            let r = returning.as_ref().expect("checked");
            (r.from.clone(), r.started)
        };
        let t = (started.elapsed().as_secs_f64() / RETURN.as_secs_f64()).min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        let cards = workspace.cards();
        for (id, rect, _) in &from {
            let Some(card) = cards.iter().find(|c| c.data.borrow().id == *id) else { continue };
            if t >= 1.0 || card.is_expanded() {
                card.unpresent(canvas);
                continue;
            }
            let (target, icon) = built_in_rect(card);
            card.present(canvas, lerp(*rect, target, eased), icon, 1.0);
        }
        workspace.refresh_ghosts();
        if t >= 1.0 {
            self.returning.replace(None);
            // A renderer started meanwhile (turned back on, reloaded) takes
            // over on the next frame instead of waiting for a change.
            return self.renderer.borrow().is_some();
        }
        true
    }
}

/// For the window: is a renderer drawing the cards now?
pub fn drawing() -> bool {
    manager().renderer_drawing()
}

/// For the window: something changed that a renderer may draw differently.
pub fn wake() {
    manager().renderer_wake();
}

/// For the window: the user let go of a dragged card.
pub fn dropped(card_id: &str) {
    manager().renderer_dropped(card_id);
}
