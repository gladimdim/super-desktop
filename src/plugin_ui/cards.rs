//! Plugin cards: a plugin's `card` surface on the canvas, next to notes and
//! terminals. The user drags it by its header, resizes it by its edges and
//! closes it with ✕; Arrange places it with the terminals and it slides with
//! the other cards. Where it was and whether it was open is kept per plugin
//! (`<state>/<id>.cards.json`), never in the workspace sent to phones or PCs.
use super::surface::{QmlSurface, SurfaceSpec};
use gtk4::prelude::*;
use gtk4::{Align, Orientation};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

pub const MIN_WIDTH: i32 = 200;
pub const MIN_HEIGHT: i32 = 120;

/// The canvas cards live on, provided by the window.
pub struct CanvasHost {
    pub canvas: gtk4::Fixed,
    /// Raises a card above the other cards (and keeps the top bar on top).
    pub raise: Rc<dyn Fn(&gtk4::Widget)>,
    /// Moves a card while it is dragged (coalesced to one move per frame).
    pub drag: Rc<dyn Fn(gtk4::Widget, f64, f64)>,
    /// Ends a drag at `(x, y)`; returns where the card stays (kept on screen).
    pub drag_end: Rc<dyn Fn(&gtk4::Widget, i32, i32) -> (i32, i32)>,
    /// The workspace's logical size.
    pub size: Rc<dyn Fn() -> (i32, i32)>,
    /// The top bar's height.
    pub top: Rc<dyn Fn() -> i32>,
}

/// A card's place on the canvas, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

fn cards_file(plugin: &str) -> PathBuf {
    super_desktop::plugins::paths::state_dir().join(format!("{plugin}.cards.json"))
}

fn read_saved(plugin: &str) -> Value {
    std::fs::read_to_string(cards_file(plugin))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Saves a card's geometry and whether it is open.
pub fn save(plugin: &str, surface: &str, geometry: Geometry, open: bool) {
    let mut saved = read_saved(plugin);
    saved[surface] = json!({
        "x": geometry.x, "y": geometry.y, "width": geometry.width, "height": geometry.height, "open": open,
    });
    let path = cards_file(plugin);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, saved.to_string());
}

/// The key a card instance is saved under: its surface id, then `#n` for
/// the second and later copies.
pub fn card_key(surface: &str, instance: &str) -> String {
    if instance.is_empty() { surface.to_owned() } else { format!("{surface}#{instance}") }
}

/// Every saved copy of a card surface: `(instance, geometry, open)`.
pub fn saved_instances(plugin: &str, surface: &str) -> Vec<(String, Geometry, bool)> {
    let saved = read_saved(plugin);
    let Some(object) = saved.as_object() else { return Vec::new() };
    let mut found: Vec<(String, Geometry, bool)> = object
        .keys()
        .filter_map(|key| {
            let instance = if key == surface { String::new() } else { key.strip_prefix(&format!("{surface}#"))?.to_owned() };
            let (geometry, open) = self::saved(plugin, key)?;
            Some((instance, geometry, open))
        })
        .collect();
    found.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then(a.0.cmp(&b.0)));
    found
}

/// What was saved for a card: its geometry and whether it was open.
pub fn saved(plugin: &str, surface: &str) -> Option<(Geometry, bool)> {
    let saved = read_saved(plugin);
    let entry = saved.get(surface)?;
    let int = |key: &str| entry.get(key).and_then(Value::as_i64).map(|v| v as i32);
    Some((
        Geometry { x: int("x")?, y: int("y")?, width: int("width")?, height: int("height")? },
        entry.get("open").and_then(Value::as_bool).unwrap_or(false),
    ))
}

pub struct PluginCard {
    pub plugin: String,
    pub surface_id: String,
    pub instance: String,
    /// `card_key(surface_id, instance)`.
    pub key: String,
    pub container: gtk4::Overlay,
    body: gtk4::Box,
    title: gtk4::Label,
    surface: RefCell<Option<QmlSurface>>,
    geometry: Cell<Geometry>,
}

impl PluginCard {
    /// Builds the card at `geometry` and puts it on the canvas. `on_close`
    /// runs when the user presses ✕.
    pub fn new(host: &Rc<CanvasHost>, spec: SurfaceSpec, title: &str, geometry: Geometry, on_close: Rc<dyn Fn()>) -> Rc<Self> {
        let container = gtk4::Overlay::new();
        container.add_css_class("mini-terminal");
        container.add_css_class("plugin-card");
        let body = gtk4::Box::new(Orientation::Vertical, 0);
        container.set_child(Some(&body));

        let header = gtk4::Box::new(Orientation::Horizontal, 6);
        header.add_css_class("term-header");
        let badge = gtk4::Label::new(Some("🧩"));
        badge.add_css_class("plugin-card-badge");
        header.append(&badge);
        let title_label = gtk4::Label::new(Some(title));
        title_label.add_css_class("term-title");
        title_label.set_hexpand(true);
        title_label.set_halign(Align::Start);
        title_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        header.append(&title_label);
        let close = gtk4::Button::with_label("✕");
        close.add_css_class("term-btn");
        close.set_tooltip_text(Some("Close"));
        close.connect_clicked(move |_| on_close());
        header.append(&close);
        body.append(&header);

        let card = Rc::new(Self {
            plugin: spec.plugin_id.clone(),
            surface_id: spec.surface_id.clone(),
            instance: spec.instance.clone(),
            key: card_key(&spec.surface_id, &spec.instance),
            container,
            body,
            title: title_label,
            surface: RefCell::new(None),
            geometry: Cell::new(geometry),
        });
        card.set_surface(spec);
        card.container.set_size_request(geometry.width, geometry.height);
        host.canvas.put(&card.container, f64::from(geometry.x), f64::from(geometry.y));
        (host.raise)(card.container.upcast_ref());
        Self::bind(&card, host, &header);
        card
    }

    /// Replaces the QML (after the plugin reloads).
    pub fn set_surface(&self, spec: SurfaceSpec) {
        if let Some(old) = self.surface.borrow_mut().take() {
            old.dispose();
            self.body.remove(old.widget());
        }
        let surface = QmlSurface::new(spec);
        surface.set_keeps_escape(true);
        surface.widget().set_vexpand(true);
        surface.widget().set_hexpand(true);
        self.body.append(surface.widget());
        *self.surface.borrow_mut() = Some(surface);
    }

    pub fn set_title(&self, title: &str) {
        self.title.set_text(title);
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry.get()
    }

    /// Moves and resizes the card (Arrange) and remembers it.
    pub fn set_geometry(&self, host: &CanvasHost, geometry: Geometry) {
        self.geometry.set(geometry);
        self.container.set_size_request(geometry.width, geometry.height);
        host.canvas.move_(&self.container, f64::from(geometry.x), f64::from(geometry.y));
        save(&self.plugin, &self.key, geometry, true);
    }

    /// Ends the QML and takes the card off the canvas.
    pub fn remove(&self, host: &CanvasHost) {
        if let Some(surface) = self.surface.borrow_mut().take() {
            surface.dispose();
        }
        if self.container.parent().is_some() {
            host.canvas.remove(&self.container);
        }
    }

    fn bind(card: &Rc<Self>, host: &Rc<CanvasHost>, header: &gtk4::Box) {
        // A press anywhere raises the card.
        let press = gtk4::GestureClick::new();
        press.set_propagation_phase(gtk4::PropagationPhase::Capture);
        press.connect_pressed({
            let host = Rc::clone(host);
            let container = card.container.downgrade();
            move |_, _, _, _| {
                if let Some(container) = container.upgrade() {
                    (host.raise)(container.upcast_ref());
                }
            }
        });
        card.container.add_controller(press);

        // Drag by the header.
        let drag = gtk4::GestureDrag::new();
        let start = Rc::new(Cell::new((0.0, 0.0)));
        let grab: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));
        drag.connect_drag_begin({
            let card = Rc::downgrade(card);
            let (start, grab) = (Rc::clone(&start), Rc::clone(&grab));
            move |gesture, _, _| {
                let Some(card) = card.upgrade() else { return };
                let g = card.geometry.get();
                let origin = (f64::from(g.x), f64::from(g.y));
                start.set(origin);
                grab.set(gesture.current_event().and_then(|e| e.position()).map(|(mx, my)| (mx - origin.0, my - origin.1)));
            }
        });
        drag.connect_drag_update({
            let card = Rc::downgrade(card);
            let host = Rc::clone(host);
            let (start, grab) = (Rc::clone(&start), Rc::clone(&grab));
            move |gesture, dx, dy| {
                if dx.abs() > 2.0 || dy.abs() > 2.0 {
                    gesture.set_state(gtk4::EventSequenceState::Claimed);
                }
                let Some(card) = card.upgrade() else { return };
                let (x, y) = crate::mini_terminal::dragged_origin(gesture, grab.get(), start.get(), (dx, dy));
                (host.drag)(card.container.clone().upcast(), x, y);
            }
        });
        drag.connect_drag_end({
            let card = Rc::downgrade(card);
            let host = Rc::clone(host);
            move |gesture, dx, dy| {
                let Some(card) = card.upgrade() else { return };
                let (x, y) = crate::mini_terminal::dragged_origin(gesture, grab.get(), start.get(), (dx, dy));
                let (x, y) = (host.drag_end)(card.container.upcast_ref(), x.round() as i32, y.round() as i32);
                let geometry = Geometry { x, y, ..card.geometry.get() };
                card.geometry.set(geometry);
                save(&card.plugin, &card.key, geometry, true);
            }
        });
        header.add_controller(drag);

        // Resize by the edges, live.
        let limits = {
            let host = Rc::clone(host);
            Rc::new(move || {
                let (width, height) = (host.size)();
                crate::card_resize::Limits {
                    min_width: MIN_WIDTH,
                    min_height: MIN_HEIGHT,
                    max_width: width.max(MIN_WIDTH),
                    max_height: height.max(MIN_HEIGHT),
                    left: 0.0,
                    top: f64::from((host.top)()),
                    right: f64::from(width),
                    bottom: f64::from(height),
                }
            })
        };
        let get_start: Rc<dyn Fn() -> Option<crate::card_resize::Rect>> = Rc::new({
            let card = Rc::downgrade(card);
            move || {
                let g = card.upgrade()?.geometry.get();
                Some(crate::card_resize::Rect { x: f64::from(g.x), y: f64::from(g.y), width: g.width, height: g.height })
            }
        });
        let on_begin: Rc<dyn Fn()> = Rc::new({
            let card = Rc::downgrade(card);
            let host = Rc::clone(host);
            move || {
                if let Some(card) = card.upgrade() {
                    card.container.add_css_class("resizing");
                    (host.raise)(card.container.upcast_ref());
                }
            }
        });
        let place = {
            let card = Rc::downgrade(card);
            let host = Rc::clone(host);
            move |rect: crate::card_resize::Rect, commit: bool| {
                let Some(card) = card.upgrade() else { return };
                let geometry = Geometry { x: rect.x.round() as i32, y: rect.y.round() as i32, width: rect.width, height: rect.height };
                card.geometry.set(geometry);
                card.container.set_size_request(geometry.width, geometry.height);
                host.canvas.move_(&card.container, rect.x, rect.y);
                if commit {
                    card.container.remove_css_class("resizing");
                    save(&card.plugin, &card.key, geometry, true);
                }
            }
        };
        let place = Rc::new(place);
        let on_preview: Rc<dyn Fn(crate::card_resize::Rect)> = Rc::new({
            let place = Rc::clone(&place);
            move |rect| place(rect, false)
        });
        let on_commit: Rc<dyn Fn(crate::card_resize::Rect)> = Rc::new(move |rect| place(rect, true));
        crate::card_resize::attach_resize_borders_with(&card.container, limits, get_start, on_begin, on_preview, on_commit);
    }
}

/// Where a new card goes: its saved place, else centered below the top bar,
/// kept inside the workspace.
pub fn initial_geometry(host: &CanvasHost, saved: Option<Geometry>, size: (i32, i32)) -> Geometry {
    let (sw, sh) = (host.size)();
    let top = (host.top)() + 12;
    let mut g = saved.unwrap_or(Geometry {
        x: (sw - size.0) / 2,
        y: top + ((sh - top - size.1) / 2).max(0),
        width: size.0,
        height: size.1,
    });
    g.width = g.width.clamp(MIN_WIDTH, sw.max(MIN_WIDTH));
    g.height = g.height.clamp(MIN_HEIGHT, (sh - top).max(MIN_HEIGHT));
    g.x = g.x.clamp(0, (sw - 48).max(0));
    g.y = g.y.clamp(top - 12, (sh - 48).max(top - 12));
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_cards_keep_geometry_and_open_state_per_surface() {
        // The state folder is the private test root (test_isolation).
        let (game, other) = (format!("game-{}", std::process::id()), format!("other-{}", std::process::id()));
        let g = Geometry { x: 10, y: 60, width: 300, height: 200 };
        save(&game, "board", g, true);
        save(&game, "scores", Geometry { x: 0, y: 0, width: 200, height: 120 }, false);
        assert_eq!(saved(&game, "board"), Some((g, true)));
        assert_eq!(saved(&game, "scores").map(|(_, open)| open), Some(false));
        assert_eq!(saved(&game, "missing"), None);
        assert_eq!(saved(&other, "board"), None);
        save(&game, &card_key("board", "2"), g, true);
        let instances: Vec<String> = saved_instances(&game, "board").into_iter().map(|(i, _, _)| i).collect();
        assert_eq!(instances, ["", "2"]);
        let _ = std::fs::remove_file(cards_file(&game));
    }
}
