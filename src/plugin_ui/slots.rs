//! Where plugin surfaces live in the overlay: docks at the left and right
//! edges, items in the top bar's launcher row, floating panels, popups next
//! to the surface that opened them, and pages in Settings → Plugins. Every plugin folder is watched; saving any file in it
//! rebuilds that plugin's surfaces (open panels included), which is how an
//! agent sees its edits live.
use super::surface::{QmlSurface, SurfaceSpec};
use super_desktop::plugins::manifest::{self, Manifest, Problem, Slot, Surface};
use super_desktop::plugins::paths;
use gtk4::prelude::*;
use gtk4::{gio, glib, Align, Orientation};
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Space between a dock and the screen edge, the top bar and the next dock.
pub const DOCK_GAP: i32 = 8;
/// A toolbar item is this much shorter than the top bar.
const TOOLBAR_ITEM_INSET: i32 = 10;
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Plugin folders are watched this deep (the folder itself is depth 0).
const WATCH_DEPTH: usize = 3;
/// Space between a popup and what it opens next to.
const POPUP_GAP: i32 = 6;
/// A popup stays this far inside the overlay.
const POPUP_MARGIN: i32 = 8;
/// A press on the surface that opened a popup closes it; that surface's
/// popup.toggle within this time then leaves it closed.
const POPUP_TOGGLE_GRACE: Duration = Duration::from_millis(1500);

struct Loaded {
    manifest: Result<Manifest, Vec<Problem>>,
    /// Docks and toolbar items: they run as long as the plugin is loaded.
    surfaces: Vec<(Surface, QmlSurface)>,
    /// Installed from elsewhere and not approved yet: nothing of it runs.
    waiting: bool,
    _monitors: Vec<gio::FileMonitor>,
}

/// An open plugin panel: its frame stays while the plugin reloads.
struct OpenPanel {
    body: gtk4::Box,
    surface: Option<QmlSurface>,
}

/// Which side of what it opens next to a popup goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupSide {
    Below,
    Right,
    Left,
}

/// What a popup opens next to: the surface that asked for it (found again
/// at every layout, so it follows a reload) and, optionally, a rectangle in
/// that surface's own coordinates.
#[derive(Clone, Debug)]
pub struct PopupAnchor {
    pub caller: String,
    pub instance: String,
    pub rect: Option<(f64, f64, f64, f64)>,
}

/// The open popup: at most one in the whole overlay.
struct Popup {
    plugin: String,
    surface_id: String,
    container: gtk4::Box,
    surface: QmlSurface,
    anchor: PopupAnchor,
    size: (i32, i32),
}

/// Where a popup of `size` goes in an overlay of `area`: on `side` of
/// `anchor` (overlay coordinates: x, y, width, height), on the other side
/// when there is no room, and inside the overlay. No anchor: centered below
/// the top bar (`top` tall). Returns (x, y, width, height).
pub fn popup_rect(area: (i32, i32), top: i32, anchor: Option<((f64, f64, f64, f64), PopupSide)>, size: (i32, i32)) -> (i32, i32, i32, i32) {
    let (width, height) = area;
    let w = size.0.min(width - 2 * POPUP_MARGIN).max(1);
    let h = size.1.min(height - 2 * POPUP_MARGIN).max(1);
    let (x, y) = match anchor {
        None => ((width - w) / 2, top + POPUP_GAP),
        Some(((ax, ay, aw, ah), side)) => {
            let (ax, ay, aw, ah) = (ax.round() as i32, ay.round() as i32, aw.round() as i32, ah.round() as i32);
            match side {
                PopupSide::Below => {
                    let below = ay + ah + POPUP_GAP;
                    let above = ay - POPUP_GAP - h;
                    let y = if below + h > height - POPUP_MARGIN && above >= POPUP_MARGIN { above } else { below };
                    (ax + aw / 2 - w / 2, y)
                }
                PopupSide::Right | PopupSide::Left => {
                    let right = ax + aw + POPUP_GAP;
                    let left = ax - POPUP_GAP - w;
                    let fits_right = right + w <= width - POPUP_MARGIN;
                    let fits_left = left >= POPUP_MARGIN;
                    let x = match side {
                        PopupSide::Right if fits_right || !fits_left => right,
                        PopupSide::Right => left,
                        _ if fits_left || !fits_right => left,
                        _ => right,
                    };
                    (x, ay + ah / 2 - h / 2)
                }
            }
        }
    };
    let x = x.clamp(POPUP_MARGIN, (width - POPUP_MARGIN - w).max(POPUP_MARGIN));
    let y = y.clamp(POPUP_MARGIN, (height - POPUP_MARGIN - h).max(POPUP_MARGIN));
    (x, y, w, h)
}

/// The open full-screen surface.
struct Fullscreen {
    plugin: String,
    surface_id: String,
    container: gtk4::Box,
    surface: QmlSurface,
}

pub struct Slots {
    overlay: gtk4::Overlay,
    left: gtk4::Box,
    right: gtk4::Box,
    toolbar: gtk4::Box,
    toolbar_items: gtk4::Box,
    root: PathBuf,
    plugins: RefCell<BTreeMap<String, Loaded>>,
    panels: crate::floating_panel::CardPanels,
    open_panels: RefCell<BTreeMap<String, OpenPanel>>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    canvas: RefCell<Option<Rc<super::cards::CanvasHost>>>,
    open_cards: RefCell<BTreeMap<String, Rc<super::cards::PluginCard>>>,
    fullscreen: RefCell<Option<Fullscreen>>,
    popup: RefCell<Option<Popup>>,
    /// The popup a press on its own caller just closed, and when.
    popup_pressed: RefCell<Option<(String, String, Instant)>>,
    top: Cell<i32>,
    item_height: Cell<i32>,
    root_monitor: RefCell<Option<gio::FileMonitor>>,
    pending: RefCell<BTreeSet<String>>,
    rescan: Cell<bool>,
    debounce: RefCell<Option<glib::SourceId>>,
    /// The toolbar items' keys (`plugin:<id>/<surface>`) in the order the
    /// user dragged them to, and who saves a new one.
    toolbar_order: RefCell<Vec<String>>,
    on_toolbar_reordered: RefCell<Option<Rc<dyn Fn(Vec<String>)>>>,
}

thread_local! {
    static SLOTS: RefCell<Option<Rc<Slots>>> = const { RefCell::new(None) };
}

pub fn get() -> Option<Rc<Slots>> {
    SLOTS.with(|slots| slots.borrow().clone())
}

/// Width the docks take at each edge, gaps included: (left, right). Arrange
/// keeps cards out of it.
pub fn insets() -> (i32, i32) {
    get().map_or((0, 0), |slots| slots.insets())
}

/// Keeps docks below the top bar and toolbar items inside it after the top
/// bar's size changes.
pub fn set_top_inset(top: i32) {
    if let Some(slots) = get() {
        slots.set_top_inset(top);
    }
}

fn edge_box(align: Align) -> gtk4::Box {
    let edge = gtk4::Box::new(Orientation::Horizontal, DOCK_GAP);
    edge.set_halign(align);
    edge.set_valign(Align::Fill);
    edge.set_vexpand(true);
    edge.set_margin_bottom(DOCK_GAP);
    if align == Align::Start {
        edge.set_margin_start(DOCK_GAP);
    } else {
        edge.set_margin_end(DOCK_GAP);
    }
    edge.add_css_class("plugin-dock-edge");
    edge.set_visible(false);
    edge
}

/// A key's name as manifests write it: "t", "5", ",", "f5", "space",
/// "return", "page_up".
pub fn key_name(keyval: gtk4::gdk::Key) -> String {
    use gtk4::gdk::Key;
    if keyval == Key::KP_Enter {
        return "return".into();
    }
    match keyval.to_lower().to_unicode() {
        Some(c) if !c.is_control() && c != ' ' => c.to_lowercase().to_string(),
        _ => keyval.name().map(|n| n.to_lowercase()).unwrap_or_default(),
    }
}

fn panel_key(plugin: &str, surface: &str) -> String {
    format!("plugin:{plugin}/{surface}")
}

fn spec(manifest: &Manifest, surface: &Surface) -> SurfaceSpec {
    SurfaceSpec {
        plugin_id: manifest.id.clone(),
        plugin_name: manifest.name.clone(),
        surface_id: surface.id.clone(), instance: String::new(),
        qml: manifest.qml_path(surface),
        dir: manifest.dir.clone(),
    }
}

impl Slots {
    /// Adds the dock edges to `overlay`, builds the toolbar row (place it with
    /// `toolbar()`) and loads every plugin.
    pub fn install(overlay: &gtk4::Overlay, top_inset: i32) -> Rc<Slots> {
        let root = paths::plugins_dir();
        let _ = std::fs::create_dir_all(&root);
        let toolbar = gtk4::Box::new(Orientation::Horizontal, 6);
        toolbar.set_valign(Align::Center);
        toolbar.add_css_class("plugin-toolbar");
        let toolbar_items = gtk4::Box::new(Orientation::Horizontal, 6);
        toolbar_items.set_valign(Align::Center);
        toolbar.append(&toolbar_items);
        let create = gtk4::Button::with_label("✨");
        create.add_css_class("hud-button");
        create.add_css_class("plugin-create-button");
        create.set_tooltip_text(Some("Create a plugin with AI: a widget, toolbar, panel or game"));
        create.update_property(&[gtk4::accessible::Property::Label("Create a plugin with AI")]);
        create.connect_clicked(|_| super::create::open());
        toolbar.append(&create);

        let slots = Rc::new(Slots {
            overlay: overlay.clone(),
            left: edge_box(Align::Start),
            right: edge_box(Align::End),
            toolbar,
            toolbar_items,
            root,
            plugins: RefCell::new(BTreeMap::new()),
            panels: crate::floating_panel::CardPanels::new((480, 360), (200, 120)),
            open_panels: RefCell::new(BTreeMap::new()),
            listeners: RefCell::new(Vec::new()),
            canvas: RefCell::new(None),
            open_cards: RefCell::new(BTreeMap::new()),
            fullscreen: RefCell::new(None),
            popup: RefCell::new(None),
            popup_pressed: RefCell::new(None),
            top: Cell::new(top_inset),
            item_height: Cell::new(26),
            root_monitor: RefCell::new(None),
            pending: RefCell::new(BTreeSet::new()),
            rescan: Cell::new(false),
            debounce: RefCell::new(None),
            toolbar_order: RefCell::new(Vec::new()),
            on_toolbar_reordered: RefCell::new(None),
        });
        // The user drags toolbar items into another order, like the launchers.
        crate::toolbar_reorder::attach(&slots.toolbar_items, Rc::new({
            let weak = Rc::downgrade(&slots);
            move || {
                if let Some(slots) = weak.upgrade() {
                    slots.toolbar_reordered();
                }
            }
        }));
        for edge in [&slots.left, &slots.right] {
            overlay.add_overlay(edge);
            overlay.set_measure_overlay(edge, false);
            overlay.set_clip_overlay(edge, true);
        }
        slots.set_top_inset(top_inset);
        slots.bind_popups();
        SLOTS.with(|cell| *cell.borrow_mut() = Some(Rc::clone(&slots)));
        slots.watch_root();
        slots.scan();
        slots
    }

    /// The top-bar row: plugin toolbar items, then ✨ Create Plugin. It goes
    /// in the launcher group, which scrolls when the bar is full.
    /// Show the toolbar items in `order` (see `toolbar_order`); `on_reordered`
    /// saves the order the user drags them to.
    pub fn set_toolbar_order(&self, order: Vec<String>, on_reordered: Rc<dyn Fn(Vec<String>)>) {
        *self.toolbar_order.borrow_mut() = order;
        *self.on_toolbar_reordered.borrow_mut() = Some(on_reordered);
        self.sort_toolbar_items();
    }

    /// The order of the toolbar items now, by key.
    pub fn toolbar_item_keys(&self) -> Vec<String> {
        crate::toolbar_reorder::children(&self.toolbar_items)
            .iter()
            .filter_map(|item| self.toolbar_key(item))
            .collect()
    }

    fn toolbar_key(&self, item: &gtk4::Widget) -> Option<String> {
        self.plugins.borrow().iter().find_map(|(id, loaded)| {
            loaded.surfaces.iter()
                .find(|(surface, widget)| surface.slot == Slot::ToolbarItem && widget.widget().upcast_ref::<gtk4::Widget>() == item)
                .map(|(surface, _)| panel_key(id, &surface.id))
        })
    }

    fn sort_toolbar_items(&self) {
        let order = self.toolbar_order.borrow().clone();
        crate::toolbar_reorder::sort(&self.toolbar_items, &order, |item| self.toolbar_key(item));
    }

    /// A drag ended: keep the new order, with the places of items that are
    /// off now, and save it.
    fn toolbar_reordered(&self) {
        let order = crate::toolbar_reorder::merged(self.toolbar_item_keys(), &self.toolbar_order.borrow());
        *self.toolbar_order.borrow_mut() = order.clone();
        let callback = self.on_toolbar_reordered.borrow().clone();
        if let Some(on_reordered) = callback {
            on_reordered(order);
        }
    }

    pub fn toolbar(&self) -> &gtk4::Box {
        &self.toolbar
    }

    pub fn set_top_inset(&self, top: i32) {
        self.top.set(top);
        for edge in [&self.left, &self.right] {
            edge.set_margin_top(top + DOCK_GAP);
        }
        let height = (top - TOOLBAR_ITEM_INSET).max(20);
        self.item_height.set(height);
        for loaded in self.plugins.borrow().values() {
            for (surface, widget) in &loaded.surfaces {
                if surface.slot == Slot::ToolbarItem {
                    widget.widget().set_size_request(surface.size, height);
                }
            }
        }
    }

    pub fn insets(&self) -> (i32, i32) {
        let width = |slot: Slot| {
            let sizes: Vec<i32> = self
                .plugins
                .borrow()
                .values()
                .flat_map(|loaded| loaded.surfaces.iter())
                .filter(|(s, _)| s.slot == slot)
                .map(|(s, _)| s.size)
                .collect();
            if sizes.is_empty() { 0 } else { sizes.iter().sum::<i32>() + DOCK_GAP * sizes.len() as i32 }
        };
        (width(Slot::DockLeft), width(Slot::DockRight))
    }

    /// Every plugin folder and what its manifest says, in id order.
    pub fn plugins(&self) -> Vec<(String, Result<Manifest, Vec<Problem>>)> {
        self.plugins.borrow().iter().map(|(id, loaded)| (id.clone(), loaded.manifest.clone())).collect()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Called after plugins load, reload or go away (Settings → Plugins).
    pub fn on_change(&self, listener: Rc<dyn Fn()>) {
        self.listeners.borrow_mut().push(listener);
    }

    fn notify(&self) {
        let listeners: Vec<Rc<dyn Fn()>> = self.listeners.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    // ── panels ────────────────────────────────────────────────────────────

    fn panel_surface(&self, plugin: &str, surface: &str) -> Result<(Manifest, Surface), String> {
        self.slot_surface(plugin, surface, Slot::Panel)
    }

    /// `surface` of a loaded, enabled `plugin`, if it is in `slot`.
    fn slot_surface(&self, plugin: &str, surface: &str, slot: Slot) -> Result<(Manifest, Surface), String> {
        let plugins = self.plugins.borrow();
        let loaded = plugins.get(plugin).ok_or_else(|| format!("plugin {plugin:?} is not loaded"))?;
        if loaded.waiting {
            return Err(format!("plugin {plugin:?} was installed and waits for the user's approval"));
        }
        let manifest = loaded.manifest.as_ref().map_err(|_| format!("plugin {plugin:?} has an invalid plugin.json"))?;
        if !manifest.enabled {
            return Err(format!("plugin {plugin:?} is turned off"));
        }
        let found = manifest
            .surface(surface)
            .filter(|s| s.slot == slot)
            .ok_or_else(|| format!("{plugin:?} has no {} surface {surface:?}", slot.name()))?;
        Ok((manifest.clone(), found.clone()))
    }

    /// Whether `surface` is a panel `plugin` can open.
    pub fn panel_check(&self, plugin: &str, surface: &str) -> Result<(), String> {
        self.panel_surface(plugin, surface).map(|_| ())
    }

    pub fn is_panel_open(&self, plugin: &str, surface: &str) -> bool {
        self.open_panels.borrow().contains_key(&panel_key(plugin, surface))
    }

    /// Opens (or brings to the front) a plugin's panel surface.
    pub fn open_panel(self: &Rc<Self>, plugin: &str, surface: &str) -> Result<(), String> {
        let (manifest, found) = self.panel_surface(plugin, surface)?;
        let key = panel_key(plugin, surface);
        if !self.panels.wants_new(&key) {
            return Ok(());
        }
        let badge = gtk4::Label::new(Some("🧩"));
        let (outer, header) = crate::floating_panel::card_panel(&badge, manifest.title_of(&found), &manifest.name, (200, 120));
        outer.set_size_request(found.size, found.height);
        let close = header.close_button("Close");
        let body = gtk4::Box::new(Orientation::Vertical, 0);
        body.set_vexpand(true);
        body.set_hexpand(true);
        outer.append(&body);
        let qml = QmlSurface::new(spec(&manifest, &found));
        body.append(qml.widget());
        qml.widget().set_vexpand(true);
        qml.widget().set_hexpand(true);
        self.open_panels.borrow_mut().insert(key.clone(), OpenPanel { body, surface: Some(qml) });
        let weak = Rc::downgrade(self);
        let (plugin_id, surface_id) = (plugin.to_owned(), surface.to_owned());
        close.connect_clicked(move |_| {
            if let Some(slots) = weak.upgrade() {
                slots.close_panel(&plugin_id, &surface_id);
            }
        });
        self.panels.show(&key, &outer, Rc::new(Cell::new(0)), None);
        Ok(())
    }

    pub fn close_panel(&self, plugin: &str, surface: &str) {
        let key = panel_key(plugin, surface);
        if let Some(open) = self.open_panels.borrow_mut().remove(&key) {
            if let Some(surface) = open.surface {
                surface.dispose();
            }
        }
        self.panels.close(&key);
    }

    /// Rebuilds or closes the open panels of `plugin` after it reloads.
    fn refresh_panels(&self, plugin: &str) {
        let prefix = panel_key(plugin, "");
        let keys: Vec<String> = self.open_panels.borrow().keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        for key in keys {
            let surface_id = key[prefix.len()..].to_owned();
            match self.panel_surface(plugin, &surface_id) {
                Ok((manifest, found)) => {
                    let mut open = self.open_panels.borrow_mut();
                    let Some(entry) = open.get_mut(&key) else { continue };
                    if let Some(old) = entry.surface.take() {
                        old.dispose();
                        entry.body.remove(old.widget());
                    }
                    let qml = QmlSurface::new(spec(&manifest, &found));
                    qml.widget().set_vexpand(true);
                    qml.widget().set_hexpand(true);
                    entry.body.append(qml.widget());
                    entry.surface = Some(qml);
                }
                Err(_) => self.close_panel(plugin, &surface_id),
            }
        }
    }

    // ── cards ─────────────────────────────────────────────────────────────

    /// Where cards go; called by the window once its canvas exists. Cards
    /// that were open when the overlay last ran open again.
    pub fn set_canvas_host(self: &Rc<Self>, host: super::cards::CanvasHost) {
        *self.canvas.borrow_mut() = Some(Rc::new(host));
        let ids: Vec<String> = self.plugins.borrow().keys().cloned().collect();
        for id in ids {
            self.restore_cards(&id);
        }
    }

    /// Whether `surface` is a card `plugin` can open.
    pub fn card_check(&self, plugin: &str, surface: &str) -> Result<(), String> {
        if self.canvas.borrow().is_none() {
            return Err("cards are not available in this window".into());
        }
        self.slot_surface(plugin, surface, Slot::Card).map(|_| ())
    }

    /// Whether a `multiple` card surface.
    pub fn card_is_multiple(&self, plugin: &str, surface: &str) -> bool {
        self.slot_surface(plugin, surface, Slot::Card).is_ok_and(|(_, s)| s.multiple)
    }

    fn card_slot_key(plugin: &str, surface: &str, instance: &str) -> String {
        panel_key(plugin, &super::cards::card_key(surface, instance))
    }

    pub fn is_card_open(&self, plugin: &str, surface: &str, instance: &str) -> bool {
        self.open_cards.borrow().contains_key(&Self::card_slot_key(plugin, surface, instance))
    }

    /// The open copies of a card surface.
    pub fn card_instances(&self, plugin: &str, surface: &str) -> Vec<String> {
        self.open_cards
            .borrow()
            .values()
            .filter(|c| c.plugin == plugin && c.surface_id == surface)
            .map(|c| c.instance.clone())
            .collect()
    }

    /// Opens another copy of a `multiple` card; returns its instance id.
    pub fn new_card(self: &Rc<Self>, plugin: &str, surface: &str) -> Result<String, String> {
        let (_, found) = self.slot_surface(plugin, surface, Slot::Card)?;
        if !found.multiple {
            return Err(format!("{surface:?} is not \"multiple\": open it with card.open"));
        }
        let saved: Vec<String> = super::cards::saved_instances(plugin, surface).into_iter().map(|(i, _, _)| i).collect();
        let open = self.card_instances(plugin, surface);
        // The first copy that is neither open nor remembered somewhere else.
        let instance = std::iter::once(String::new())
            .chain((2..).map(|n: u32| n.to_string()))
            .find(|i| !open.contains(i) && !saved.iter().any(|s| s == i && super::cards::saved(plugin, &super::cards::card_key(surface, i)).is_some_and(|(_, o)| o)))
            .unwrap_or_default();
        self.open_card(plugin, surface, &instance)?;
        Ok(instance)
    }

    /// Opens (or raises) a copy of a plugin's card on the canvas.
    pub fn open_card(self: &Rc<Self>, plugin: &str, surface: &str, instance: &str) -> Result<(), String> {
        let (manifest, found) = self.slot_surface(plugin, surface, Slot::Card)?;
        if !instance.is_empty() && !found.multiple {
            return Err(format!("{surface:?} is not \"multiple\": it has one copy"));
        }
        let host = self.canvas.borrow().clone().ok_or("cards are not available in this window")?;
        let key = Self::card_slot_key(plugin, surface, instance);
        if let Some(card) = self.open_cards.borrow().get(&key) {
            (host.raise)(card.container.upcast_ref());
            return Ok(());
        }
        let saved_key = super::cards::card_key(surface, instance);
        let saved = super::cards::saved(plugin, &saved_key).map(|(g, _)| g);
        let mut geometry = super::cards::initial_geometry(&host, saved, (found.size, found.height));
        if saved.is_none() && !instance.is_empty() {
            // A new copy is offset from the first so both stay visible.
            let step = 32 * instance.parse::<i32>().unwrap_or(2).saturating_sub(1).min(8);
            geometry = super::cards::initial_geometry(&host, Some(super::cards::Geometry { x: geometry.x + step, y: geometry.y + step, ..geometry }), (found.size, found.height));
        }
        let on_close: Rc<dyn Fn()> = Rc::new({
            let weak = Rc::downgrade(self);
            let (plugin, surface, instance) = (plugin.to_owned(), surface.to_owned(), instance.to_owned());
            move || {
                if let Some(slots) = weak.upgrade() {
                    // After this click handler returns: closing removes the button.
                    let (plugin, surface, instance) = (plugin.clone(), surface.clone(), instance.clone());
                    glib::idle_add_local_once(move || slots.close_card(&plugin, &surface, &instance, true));
                }
            }
        });
        let mut card_spec = spec(&manifest, &found);
        card_spec.instance = instance.to_owned();
        let title = if instance.is_empty() { manifest.title_of(&found).to_owned() } else { format!("{} {instance}", manifest.title_of(&found)) };
        let card = super::cards::PluginCard::new(&host, card_spec, &title, geometry, on_close);
        super::cards::save(plugin, &saved_key, geometry, true);
        self.open_cards.borrow_mut().insert(key, card);
        Ok(())
    }

    /// Closes a card; `forget` records it as closed so it does not open again
    /// next time (a ✕ or the plugin's own request, not a reload). A forgotten
    /// extra copy is dropped from the saved places altogether.
    pub fn close_card(&self, plugin: &str, surface: &str, instance: &str, forget: bool) {
        let card = self.open_cards.borrow_mut().remove(&Self::card_slot_key(plugin, surface, instance));
        if let (Some(card), Some(host)) = (card, self.canvas.borrow().clone()) {
            if forget {
                super::cards::save(plugin, &card.key, card.geometry(), false);
            }
            card.remove(&host);
        }
    }

    /// Opens the cards of `plugin` saved as open; refreshes the open ones.
    fn restore_cards(self: &Rc<Self>, plugin: &str) {
        if self.canvas.borrow().is_none() {
            return;
        }
        let surfaces: Vec<Surface> = self
            .plugins
            .borrow()
            .get(plugin)
            .and_then(|loaded| loaded.manifest.as_ref().ok())
            .filter(|m| m.enabled)
            .map(|m| m.surfaces.iter().filter(|s| s.slot == Slot::Card).cloned().collect())
            .unwrap_or_default();
        for surface in surfaces {
            for (instance, _, open) in super::cards::saved_instances(plugin, &surface.id) {
                if open && (instance.is_empty() || surface.multiple) && !self.is_card_open(plugin, &surface.id, &instance) {
                    let _ = self.open_card(plugin, &surface.id, &instance);
                }
            }
        }
    }

    /// Rebuilds the open cards of `plugin` after it reloads, or closes them.
    fn refresh_cards(&self, plugin: &str) {
        let open: Vec<Rc<super::cards::PluginCard>> =
            self.open_cards.borrow().values().filter(|c| c.plugin == plugin).cloned().collect();
        for card in open {
            match self.slot_surface(plugin, &card.surface_id, Slot::Card) {
                Ok((manifest, found)) if card.instance.is_empty() || found.multiple => {
                    let title = if card.instance.is_empty() { manifest.title_of(&found).to_owned() } else { format!("{} {}", manifest.title_of(&found), card.instance) };
                    card.set_title(&title);
                    let mut card_spec = spec(&manifest, &found);
                    card_spec.instance = card.instance.clone();
                    card.set_surface(card_spec);
                }
                _ => self.close_card(plugin, &card.surface_id, &card.instance, false),
            }
        }
    }

    /// Open cards, for Arrange.
    pub fn cards(&self) -> Vec<Rc<super::cards::PluginCard>> {
        self.open_cards.borrow().values().cloned().collect()
    }

    pub fn canvas_host(&self) -> Option<Rc<super::cards::CanvasHost>> {
        self.canvas.borrow().clone()
    }

    // ── popups ────────────────────────────────────────────────────────────

    /// Places the open popup and closes it on a press anywhere else.
    fn bind_popups(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.overlay.connect_get_child_position(move |overlay, child| {
            let slots = weak.upgrade()?;
            let popup = slots.popup.borrow();
            let popup = popup.as_ref().filter(|p| child == p.container.upcast_ref::<gtk4::Widget>())?;
            let (x, y, width, height) = slots.place_popup(overlay, popup);
            Some(gtk4::gdk::Rectangle::new(x, y, width, height))
        });
        // Observe presses without taking them: what was clicked still works.
        let press = gtk4::GestureClick::new();
        press.set_button(0);
        press.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        press.connect_pressed(move |gesture, _, x, y| {
            // A press in a GTK popover is on its own surface, not here.
            if gesture.current_event().and_then(|e| e.surface()).is_some_and(|s| s.is::<gtk4::gdk::Popup>()) {
                return;
            }
            if let Some(slots) = weak.upgrade() {
                slots.popup_press(x, y);
            }
        });
        self.overlay.add_controller(press);
    }

    /// The widget a running surface of `plugin` is drawn in, and its slot.
    fn caller_widget(&self, plugin: &str, caller: &str, instance: &str) -> Option<(gtk4::Widget, Slot)> {
        let running = self.plugins.borrow().get(plugin).and_then(|loaded| {
            loaded.surfaces.iter().find(|(s, _)| s.id == caller).map(|(s, q)| (q.widget().clone().upcast::<gtk4::Widget>(), s.slot))
        });
        if running.is_some() {
            return running;
        }
        if let Some(widget) = self.open_panels.borrow().get(&panel_key(plugin, caller)).and_then(|open| open.surface.as_ref()) {
            return Some((widget.widget().clone().upcast(), Slot::Panel));
        }
        if let Some(widget) = self.open_cards.borrow().get(&Self::card_slot_key(plugin, caller, instance)).and_then(|c| c.surface_widget()) {
            return Some((widget, Slot::Card));
        }
        self.fullscreen.borrow().as_ref()
            .filter(|f| f.plugin == plugin && f.surface_id == caller)
            .map(|f| (f.surface.widget().clone().upcast(), Slot::Fullscreen))
    }

    /// The popup's place in `overlay`: beside a dock, below a toolbar item,
    /// below the rectangle the caller gave; else centered below the top bar.
    fn place_popup(&self, overlay: &gtk4::Overlay, popup: &Popup) -> (i32, i32, i32, i32) {
        let anchor = self.caller_widget(&popup.plugin, &popup.anchor.caller, &popup.anchor.instance).and_then(|(widget, slot)| {
            if !widget.is_mapped() {
                return None;
            }
            let side = match slot {
                Slot::DockLeft => PopupSide::Right,
                Slot::DockRight => PopupSide::Left,
                _ => PopupSide::Below,
            };
            let rect = match (popup.anchor.rect, slot) {
                (Some(rect), _) => rect,
                (None, Slot::ToolbarItem | Slot::DockLeft | Slot::DockRight) => (0.0, 0.0, f64::from(widget.width()), f64::from(widget.height())),
                (None, _) => return None,
            };
            let bounds = widget.compute_bounds(overlay)?;
            Some(((f64::from(bounds.x()) + rect.0, f64::from(bounds.y()) + rect.1, rect.2, rect.3), side))
        });
        popup_rect((overlay.width(), overlay.height()), self.top.get(), anchor, popup.size)
    }

    pub fn popup_check(&self, plugin: &str, surface: &str) -> Result<(), String> {
        self.slot_surface(plugin, surface, Slot::Popup).map(|_| ())
    }

    pub fn is_popup_open(&self, plugin: &str, surface: &str) -> bool {
        self.popup.borrow().as_ref().is_some_and(|p| p.plugin == plugin && p.surface_id == surface)
    }

    /// Opens a plugin's popup next to `anchor`, closing any other popup.
    /// It has the keyboard at once; Esc or a press outside it closes it.
    pub fn open_popup(self: &Rc<Self>, plugin: &str, surface: &str, anchor: PopupAnchor) -> Result<(), String> {
        let (manifest, found) = self.slot_surface(plugin, surface, Slot::Popup)?;
        self.close_popup();
        let container = gtk4::Box::new(Orientation::Vertical, 0);
        container.add_css_class("plugin-popup");
        container.set_overflow(gtk4::Overflow::Hidden);
        let qml = QmlSurface::new(spec(&manifest, &found));
        qml.widget().set_hexpand(true);
        qml.widget().set_vexpand(true);
        container.append(qml.widget());
        // Esc closes the popup instead of hiding the overlay.
        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let weak = Rc::downgrade(self);
            move |_, key, _, _| {
                if key != gtk4::gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                if let Some(slots) = weak.upgrade() {
                    glib::idle_add_local_once(move || { slots.close_popup(); });
                }
                glib::Propagation::Stop
            }
        });
        container.add_controller(keys);
        // Hiding the overlay closes it: a popup is for the moment.
        container.connect_unmap({
            let weak = Rc::downgrade(self);
            move |container| {
                let (Some(slots), container) = (weak.upgrade(), container.clone()) else { return };
                glib::idle_add_local_once(move || {
                    let current = slots.popup.borrow().as_ref().is_some_and(|p| p.container == container);
                    if current && !container.is_mapped() {
                        slots.close_popup();
                    }
                });
            }
        });
        self.overlay.add_overlay(&container);
        self.overlay.set_measure_overlay(&container, false);
        let view = qml.widget().child();
        *self.popup.borrow_mut() = Some(Popup {
            plugin: plugin.to_owned(),
            surface_id: surface.to_owned(),
            container,
            surface: qml,
            anchor,
            size: (found.size, found.height),
        });
        if let Some(view) = view {
            glib::idle_add_local_once(move || {
                view.grab_focus();
            });
        }
        Ok(())
    }

    /// Changes the open popup's size (either side, within the slot's limits);
    /// the size it has now, or an error when that popup is not open.
    pub fn resize_popup(&self, plugin: &str, surface: &str, width: Option<i32>, height: Option<i32>) -> Result<(i32, i32), String> {
        let mut popup = self.popup.borrow_mut();
        let popup = popup.as_mut().filter(|p| p.plugin == plugin && p.surface_id == surface)
            .ok_or_else(|| format!("popup {surface:?} is not open"))?;
        let sizes = Slot::Popup.sizes();
        let ((wmin, _, wmax), (hmin, _, hmax)) = (sizes.size.unwrap_or((1, 1, i32::MAX)), sizes.height.unwrap_or((1, 1, i32::MAX)));
        popup.size = (width.map_or(popup.size.0, |w| w.clamp(wmin, wmax)), height.map_or(popup.size.1, |h| h.clamp(hmin, hmax)));
        popup.container.queue_resize();
        self.overlay.queue_allocate();
        Ok(popup.size)
    }

    /// Closes the popup; whether one was open.
    pub fn close_popup(&self) -> bool {
        let Some(popup) = self.popup.borrow_mut().take() else { return false };
        popup.surface.dispose();
        if popup.container.parent().is_some() {
            self.overlay.remove_overlay(&popup.container);
        }
        true
    }

    /// A press at (x, y) in the overlay: outside the popup it closes it. A
    /// press on the surface that opened it is remembered, so that surface's
    /// popup.toggle right after leaves it closed instead of opening it again.
    pub fn popup_press(&self, x: f64, y: f64) {
        let (plugin, surface, on_caller) = {
            let popup = self.popup.borrow();
            let Some(popup) = popup.as_ref() else { return };
            let picked = self.overlay.pick(x, y, gtk4::PickFlags::DEFAULT);
            let within = |widget: &gtk4::Widget| picked.as_ref().is_some_and(|p| p == widget || p.is_ancestor(widget));
            if within(popup.container.upcast_ref()) {
                return;
            }
            let caller = self.caller_widget(&popup.plugin, &popup.anchor.caller, &popup.anchor.instance);
            (popup.plugin.clone(), popup.surface_id.clone(), caller.is_some_and(|(w, _)| within(&w)))
        };
        *self.popup_pressed.borrow_mut() = on_caller.then(|| (plugin, surface, Instant::now()));
        self.close_popup();
    }

    /// Whether a press on its caller just closed this popup (asked once, by
    /// popup.toggle).
    pub fn popup_just_closed(&self, plugin: &str, surface: &str) -> bool {
        let pressed = self.popup_pressed.borrow_mut().take();
        pressed.is_some_and(|(p, s, at)| p == plugin && s == surface && at.elapsed() < POPUP_TOGGLE_GRACE)
    }

    /// Rebuilds the open popup of `plugin` after it reloads, or closes it.
    fn refresh_popup(&self, plugin: &str) {
        let open = self.popup.borrow().as_ref().filter(|p| p.plugin == plugin).map(|p| p.surface_id.clone());
        let Some(surface) = open else { return };
        match self.slot_surface(plugin, &surface, Slot::Popup) {
            Ok((manifest, found)) => {
                let mut popup = self.popup.borrow_mut();
                let Some(popup) = popup.as_mut() else { return };
                let qml = QmlSurface::new(spec(&manifest, &found));
                qml.widget().set_hexpand(true);
                qml.widget().set_vexpand(true);
                popup.surface.dispose();
                popup.container.remove(popup.surface.widget());
                popup.container.append(qml.widget());
                popup.surface = qml;
                popup.size = (found.size, found.height);
                popup.container.queue_resize();
            }
            Err(_) => {
                self.close_popup();
            }
        }
    }

    // ── full screen ───────────────────────────────────────────────────────

    pub fn fullscreen_check(&self, plugin: &str, surface: &str) -> Result<(), String> {
        self.slot_surface(plugin, surface, Slot::Fullscreen).map(|_| ())
    }

    pub fn is_fullscreen_open(&self, plugin: &str, surface: &str) -> bool {
        self.fullscreen.borrow().as_ref().is_some_and(|f| f.plugin == plugin && f.surface_id == surface)
    }

    /// Shows a plugin's full-screen surface above everything; Esc closes it.
    pub fn open_fullscreen(self: &Rc<Self>, plugin: &str, surface: &str) -> Result<(), String> {
        let (manifest, found) = self.slot_surface(plugin, surface, Slot::Fullscreen)?;
        self.close_fullscreen();
        let container = gtk4::Box::new(Orientation::Vertical, 0);
        container.add_css_class("plugin-fullscreen");
        container.set_halign(Align::Fill);
        container.set_valign(Align::Fill);
        let qml = QmlSurface::new(spec(&manifest, &found));
        qml.widget().set_hexpand(true);
        qml.widget().set_vexpand(true);
        container.append(qml.widget());
        // Esc leaves full screen instead of hiding the overlay.
        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let weak = Rc::downgrade(self);
            move |_, key, _, _| {
                if key == gtk4::gdk::Key::Escape {
                    if let Some(slots) = weak.upgrade() {
                        glib::idle_add_local_once(move || { slots.close_fullscreen(); });
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        container.add_controller(keys);
        self.overlay.add_overlay(&container);
        self.overlay.set_measure_overlay(&container, false);
        self.overlay.set_clip_overlay(&container, true);
        let view = qml.widget().child();
        *self.fullscreen.borrow_mut() = Some(Fullscreen {
            plugin: plugin.to_owned(),
            surface_id: surface.to_owned(),
            container,
            surface: qml,
        });
        // Keys go to the game at once.
        if let Some(view) = view {
            glib::idle_add_local_once(move || {
                view.grab_focus();
            });
        }
        Ok(())
    }

    /// Closes the full-screen surface; whether one was open.
    pub fn close_fullscreen(&self) -> bool {
        let Some(open) = self.fullscreen.borrow_mut().take() else { return false };
        open.surface.dispose();
        self.overlay.remove_overlay(&open.container);
        true
    }

    // ── shortcuts ─────────────────────────────────────────────────────────

    /// Runs "panel.toggle", "card.open", "card.new", "fullscreen.close"…
    /// for `plugin`. `instance` picks a copy of a card ("" for the first).
    pub fn act(self: &Rc<Self>, plugin: &str, action: &str, surface: &str, instance: &str) -> Result<(), String> {
        let (kind, verb) = action.split_once('.').ok_or("bad action")?;
        if action == "card.new" {
            return self.new_card(plugin, surface).map(|_| ());
        }
        let open = match kind {
            "panel" => self.is_panel_open(plugin, surface),
            "card" => self.is_card_open(plugin, surface, instance),
            "fullscreen" => self.is_fullscreen_open(plugin, surface),
            _ => return Err(format!("unknown action {action:?}")),
        };
        let want = match verb {
            "open" => true,
            "close" => false,
            _ => !open,
        };
        match (kind, want) {
            ("panel", true) => self.open_panel(plugin, surface),
            ("panel", false) => {
                self.close_panel(plugin, surface);
                Ok(())
            }
            ("card", true) => self.open_card(plugin, surface, instance),
            ("card", false) => {
                self.close_card(plugin, surface, instance, true);
                Ok(())
            }
            (_, true) => self.open_fullscreen(plugin, surface),
            (_, false) => {
                if open {
                    self.close_fullscreen();
                }
                Ok(())
            }
        }
    }

    /// A key press while the overlay is shown: runs the first plugin
    /// shortcut (in plugin-id order) that matches. Whether one did.
    pub fn handle_key(self: &Rc<Self>, keyval: gtk4::gdk::Key, state: gtk4::gdk::ModifierType) -> bool {
        use gtk4::gdk::ModifierType as M;
        let name = key_name(keyval);
        let ctrl = state.contains(M::CONTROL_MASK);
        let alt = state.contains(M::ALT_MASK);
        let shift = state.contains(M::SHIFT_MASK);
        let super_key = state.intersects(M::SUPER_MASK | M::META_MASK);
        let found = self.plugins.borrow().iter().find_map(|(id, loaded)| {
            let manifest = loaded.manifest.as_ref().ok().filter(|m| m.enabled && !loaded.waiting)?;
            manifest.shortcuts.iter().find(|s| {
                s.key == name && s.ctrl == ctrl && s.alt == alt && s.shift == shift && s.super_key == super_key
            }).map(|s| (id.clone(), s.action.clone(), s.surface.clone()))
        });
        let Some((plugin, action, surface)) = found else { return false };
        paths::append_log(&plugin, &format!("shortcut {action} {surface}"));
        // After the key event: opening moves the keyboard focus.
        let slots = Rc::clone(self);
        glib::idle_add_local_once(move || {
            if let Err(e) = slots.act(&plugin, &action, &surface, "") {
                paths::append_log(&plugin, &format!("shortcut failed: {e}"));
            }
        });
        true
    }

    // ── watching ──────────────────────────────────────────────────────────

    fn watch_root(self: &Rc<Self>) {
        let file = gio::File::for_path(&self.root);
        match file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
            Ok(monitor) => {
                let weak = Rc::downgrade(self);
                monitor.connect_changed(move |_, _, _, _| {
                    if let Some(slots) = weak.upgrade() {
                        slots.rescan.set(true);
                        slots.schedule();
                    }
                });
                *self.root_monitor.borrow_mut() = Some(monitor);
            }
            Err(e) => eprintln!("SUPER DESKTOP: cannot watch {}: {e}", self.root.display()),
        }
    }

    /// Monitors for a plugin folder and its subfolders (hidden ones and the
    /// copied skill are skipped).
    fn watch_plugin(self: &Rc<Self>, id: &str, dir: &Path) -> Vec<gio::FileMonitor> {
        let mut dirs = vec![(dir.to_path_buf(), 0usize)];
        let mut monitors = Vec::new();
        while let Some((path, depth)) = dirs.pop() {
            if depth < WATCH_DEPTH {
                for entry in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                    let name = entry.file_name();
                    let hidden = name.to_str().is_none_or(|n| n.starts_with('.'));
                    if !hidden && entry.path().is_dir() {
                        dirs.push((entry.path(), depth + 1));
                    }
                }
            }
            let Ok(monitor) = gio::File::for_path(&path)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            else {
                continue;
            };
            let weak = Rc::downgrade(self);
            let id = id.to_owned();
            monitor.connect_changed(move |_, file, _, _| {
                // Notes, editor swap files and the skill copy do not change
                // what the plugin draws.
                let name = file.basename().and_then(|n| n.to_str().map(str::to_owned)).unwrap_or_default();
                if name.starts_with('.') || name.ends_with('~') || name.ends_with(".swp") || name.ends_with(".md") {
                    return;
                }
                if let Some(slots) = weak.upgrade() {
                    slots.pending.borrow_mut().insert(id.clone());
                    slots.schedule();
                }
            });
            monitors.push(monitor);
        }
        monitors
    }

    fn schedule(self: &Rc<Self>) {
        if let Some(id) = self.debounce.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(DEBOUNCE, move || {
            if let Some(slots) = weak.upgrade() {
                slots.debounce.take();
                slots.flush();
            }
        });
        *self.debounce.borrow_mut() = Some(id);
    }

    fn flush(self: &Rc<Self>) {
        let pending: Vec<String> = std::mem::take(&mut *self.pending.borrow_mut()).into_iter().collect();
        if self.rescan.replace(false) {
            self.scan_with(&pending);
        } else {
            for id in &pending {
                self.reload(id);
            }
            self.relayout();
            self.notify();
        }
    }

    fn scan(self: &Rc<Self>) {
        self.scan_with(&[]);
    }

    /// Loads new folders, unloads removed ones and reloads `changed`.
    fn scan_with(self: &Rc<Self>, changed: &[String]) {
        let found: BTreeSet<String> = manifest::discover(&self.root).into_iter().map(|f| f.folder).collect();
        let loaded: Vec<String> = self.plugins.borrow().keys().cloned().collect();
        for id in loaded.iter().filter(|id| !found.contains(*id)) {
            self.unload(id);
        }
        for id in &found {
            let known = self.plugins.borrow().contains_key(id);
            if !known || changed.contains(id) {
                self.reload(id);
            }
        }
        self.relayout();
        self.notify();
    }

    /// Reloads one plugin now (Settings uses it after changing plugin.json).
    pub fn reload_now(self: &Rc<Self>, id: &str) {
        self.reload(id);
        self.relayout();
        self.notify();
    }

    fn unload(&self, id: &str) {
        if let Some(loaded) = self.plugins.borrow_mut().remove(id) {
            for (_, surface) in &loaded.surfaces {
                surface.dispose();
                if let Some(parent) = surface.widget().parent().and_downcast::<gtk4::Box>() {
                    parent.remove(surface.widget());
                }
            }
            paths::write_status(id, "stopped", &json!([]));
        }
        let prefix = panel_key(id, "");
        let keys: Vec<String> = self.open_panels.borrow().keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        for key in keys {
            self.close_panel(id, &key[prefix.len()..]);
        }
        let cards: Vec<(String, String)> = self.open_cards.borrow().values()
            .filter(|c| c.plugin == id).map(|c| (c.surface_id.clone(), c.instance.clone())).collect();
        for (surface, instance) in cards {
            self.close_card(id, &surface, &instance, false);
        }
        if self.fullscreen.borrow().as_ref().is_some_and(|f| f.plugin == id) {
            self.close_fullscreen();
        }
        if self.popup.borrow().as_ref().is_some_and(|p| p.plugin == id) {
            self.close_popup();
        }
    }

    fn reload(self: &Rc<Self>, id: &str) {
        // Panels keep their frames; their surfaces are rebuilt below.
        if let Some(loaded) = self.plugins.borrow_mut().remove(id) {
            for (_, surface) in &loaded.surfaces {
                surface.dispose();
                if let Some(parent) = surface.widget().parent().and_downcast::<gtk4::Box>() {
                    parent.remove(surface.widget());
                }
            }
        }
        let dir = self.root.join(id);
        if !dir.is_dir() {
            self.unload(id);
            return;
        }
        let monitors = self.watch_plugin(id, &dir);
        let manifest = manifest::load(&dir);
        let waiting = !super_desktop::plugins::install::approved(&super_desktop::plugins::install::Places::current(), id);
        let surfaces = match &manifest {
            Err(problems) => {
                for p in problems {
                    paths::append_log(id, &format!("plugin.json: {}: {} (hint: {})", p.field, p.message, p.hint));
                }
                paths::write_status(id, "error", &serde_json::to_value(problems).unwrap_or_default());
                Vec::new()
            }
            Ok(_) if waiting => {
                paths::write_status(id, "waiting-approval", &json!([]));
                Vec::new()
            }
            Ok(manifest) if !manifest.enabled => {
                paths::write_status(id, "disabled", &json!([]));
                Vec::new()
            }
            Ok(manifest) => {
                let surfaces: Vec<(Surface, QmlSurface)> = manifest
                    .surfaces
                    .iter()
                    .filter(|s| matches!(s.slot, Slot::DockLeft | Slot::DockRight | Slot::ToolbarItem))
                    .map(|s| {
                        let surface = QmlSurface::new(spec(manifest, s));
                        let widget = surface.widget();
                        if !s.tooltip.is_empty() {
                            widget.set_tooltip_text(Some(&s.tooltip));
                        }
                        if s.slot == Slot::ToolbarItem {
                            widget.set_size_request(s.size, self.item_height.get());
                            widget.set_valign(Align::Center);
                            widget.add_css_class("plugin-toolbar-item");
                        } else {
                            widget.set_size_request(s.size, -1);
                            widget.set_vexpand(true);
                        }
                        widget.set_hexpand(false);
                        (s.clone(), surface)
                    })
                    .collect();
                if surfaces.is_empty() {
                    // Panels and settings pages run only while shown.
                    paths::write_status(id, "ready", &json!([]));
                }
                paths::append_log(id, &format!("loaded {} surface(s)", manifest.surfaces.len()));
                surfaces
            }
        };
        self.plugins.borrow_mut().insert(id.to_owned(), Loaded { manifest, surfaces, waiting, _monitors: monitors });
        self.refresh_panels(id);
        self.refresh_cards(id);
        self.refresh_fullscreen(id);
        self.refresh_popup(id);
        self.restore_cards(id);
    }

    /// Rebuilds the open full-screen surface of `plugin`, or closes it.
    fn refresh_fullscreen(self: &Rc<Self>, plugin: &str) {
        let open = self.fullscreen.borrow().as_ref().filter(|f| f.plugin == plugin).map(|f| f.surface_id.clone());
        if let Some(surface) = open {
            if self.open_fullscreen(plugin, &surface).is_err() {
                self.close_fullscreen();
            }
        }
    }

    /// Puts every running surface in its place, in plugin-id order.
    fn relayout(&self) {
        for target in [&self.left, &self.right, &self.toolbar_items] {
            while let Some(child) = target.first_child() {
                target.remove(&child);
            }
        }
        for loaded in self.plugins.borrow().values() {
            for (surface, widget) in &loaded.surfaces {
                let target = match surface.slot {
                    Slot::DockLeft => &self.left,
                    Slot::DockRight => &self.right,
                    Slot::ToolbarItem => &self.toolbar_items,
                    Slot::Panel | Slot::SettingsPage | Slot::Card | Slot::Fullscreen | Slot::Popup => continue,
                };
                target.append(widget.widget());
            }
        }
        self.left.set_visible(self.left.first_child().is_some());
        self.right.set_visible(self.right.first_child().is_some());
        self.toolbar_items.set_visible(self.toolbar_items.first_child().is_some());
        self.sort_toolbar_items();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_ui::surface::SurfaceView;
    use gtk4::subclass::prelude::ObjectSubclassIsExt;

    fn write_plugin(root: &std::path::Path, id: &str, slot: &str, size: i32, qml: &str) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Dock.qml"), qml).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            serde_json::json!({"id": id, "api": 1, "surfaces": [{"id": "main", "slot": slot, "qml": "Dock.qml", "size": size}]}).to_string(),
        ).unwrap();
    }

    fn surface_view(edge: &gtk4::Box) -> Option<SurfaceView> {
        edge.first_child()?.downcast::<gtk4::Overlay>().ok()?.child()?.downcast::<SurfaceView>().ok()
    }

    fn has_frame(edge: &gtk4::Box) -> bool {
        surface_view(edge).is_some_and(|view| view.imp().texture.borrow().is_some())
    }

    fn error_text(edge: &gtk4::Box) -> String {
        let Some(overlay) = edge.first_child().and_then(|w| w.downcast::<gtk4::Overlay>().ok()) else { return String::new() };
        crate::gtk_test::descendants(overlay.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk4::Label>().ok())
            .find(|label| label.is_visible())
            .map(|label| label.text().to_string())
            .unwrap_or_default()
    }

    #[test]
    fn plugins_slots_docks_draw_at_the_edges_and_reload_on_save() {
        if super_desktop::plugins::paths::qml_host_binary().is_none() {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        }
        crate::gtk_test::run_in_child_process("plugin_ui::slots::tests::docks_inner");
    }

    #[test]
    fn docks_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-docks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
        std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
        let plugins = root.join("plugins");
        write_plugin(&plugins, "left-bar", "dock.left", 56, "import QtQuick\nRectangle { color: \"red\" }\n");
        write_plugin(&plugins, "right-bar", "dock.right", 72, "import QtQuick\nRectangle { color: sd.theme.accent }\n");

        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&gtk4::Box::new(Orientation::Vertical, 0)));
        let docks = Slots::install(&overlay, 46);
        let window = gtk4::Window::new();
        window.set_default_size(1000, 700);
        window.set_child(Some(&overlay));
        window.present();

        crate::gtk_test::pump_until("frames in both docks", || has_frame(&docks.left) && has_frame(&docks.right));
        assert_eq!(insets(), (56 + DOCK_GAP, 72 + DOCK_GAP));
        let left = docks.left.compute_bounds(&overlay).unwrap();
        let right = docks.right.compute_bounds(&overlay).unwrap();
        assert_eq!(left.x() as i32, DOCK_GAP);
        assert_eq!(left.width() as i32, 56);
        assert_eq!(left.y() as i32, 46 + DOCK_GAP);
        assert_eq!((right.x() + right.width()) as i32, overlay.width() - DOCK_GAP);
        assert_eq!(right.width() as i32, 72);
        assert_eq!((left.y() + left.height()) as i32, overlay.height() - DOCK_GAP);
        let status = super_desktop::plugins::paths::read_status("right-bar").unwrap();
        assert_eq!(status["state"], "running");

        // Saving the manifest with a new size rebuilds the dock.
        write_plugin(&plugins, "right-bar", "dock.right", 120, "import QtQuick\nRectangle { color: \"blue\" }\n");
        crate::gtk_test::pump_until("the resized dock", || {
            docks.right.compute_bounds(&overlay).is_some_and(|b| b.width() as i32 == 120) && has_frame(&docks.right)
        });

        // A QML error shows in place, with the file and line.
        std::fs::write(plugins.join("left-bar/Dock.qml"), "import QtQuick\nRectangle { colr: \"red\" }\n").unwrap();
        crate::gtk_test::pump_until("the error placeholder", || error_text(&docks.left).contains("Dock.qml:2"));

        // Fixing it brings the dock back.
        std::fs::write(plugins.join("left-bar/Dock.qml"), "import QtQuick\nRectangle { color: \"green\" }\n").unwrap();
        crate::gtk_test::pump_until("the fixed dock", || error_text(&docks.left).is_empty() && has_frame(&docks.left));

        // Turning a plugin off removes its dock and gives the edge back.
        std::fs::write(
            plugins.join("left-bar/plugin.json"),
            serde_json::json!({"id": "left-bar", "api": 1, "enabled": false, "surfaces": [{"id": "main", "slot": "dock.left", "qml": "Dock.qml"}]}).to_string(),
        ).unwrap();
        crate::gtk_test::pump_until("the left edge to empty", || !docks.left.is_visible());
        assert_eq!(insets().0, 0);

        // Removing the folder unloads the plugin.
        std::fs::remove_dir_all(plugins.join("right-bar")).unwrap();
        crate::gtk_test::pump_until("the right edge to empty", || !docks.right.is_visible());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn write_manifest(root: &std::path::Path, id: &str, surfaces: serde_json::Value) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.json"), serde_json::json!({"id": id, "api": 1, "surfaces": surfaces}).to_string()).unwrap();
    }

    fn view_frame(widget: &gtk4::Widget) -> Option<gtk4::gdk::Texture> {
        crate::gtk_test::descendants(widget)
            .into_iter()
            .find_map(|w| w.downcast::<SurfaceView>().ok())
            .and_then(|view| view.imp().texture.borrow().clone())
    }

    #[test]
    fn plugins_slots_key_names_match_manifests() {
        use gtk4::gdk::Key;
        assert_eq!(key_name(Key::t), "t");
        assert_eq!(key_name(Key::T), "t");
        assert_eq!(key_name(Key::_5), "5");
        assert_eq!(key_name(Key::comma), ",");
        assert_eq!(key_name(Key::F5), "f5");
        assert_eq!(key_name(Key::space), "space");
        assert_eq!(key_name(Key::Return), "return");
        assert_eq!(key_name(Key::Page_Up), "page_up");
        // Every name the manifest accepts is one GDK produces.
        for name in ["space", "return", "tab", "backspace", "delete", "insert", "home", "end", "page_up", "page_down", "up", "down", "left", "right", "f1", "f12"] {
            assert!(super_desktop::plugins::manifest::parse_keys(&format!("Ctrl+Alt+{name}")).is_ok(), "{name}");
        }
    }

    #[test]
    fn toolbar_plugin_items_rearrange_by_dragging() {
        crate::gtk_test::run_in_child_process("plugin_ui::slots::tests::toolbar_items_rearrange_inner");
    }

    #[test]
    fn toolbar_items_rearrange_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-reorder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
        std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
        let plugins = root.join("plugins");
        for id in ["alpha", "beta", "gamma"] {
            write_manifest(&plugins, id, serde_json::json!([
                {"id": "button", "slot": "toolbar.item", "qml": "Button.qml", "size": 80}
            ]));
            std::fs::write(plugins.join(id).join("Button.qml"), "import QtQuick\nRectangle { color: \"orange\" }\n").unwrap();
        }
        gtk4::init().unwrap();
        let overlay = gtk4::Overlay::new();
        let top = gtk4::Box::new(Orientation::Vertical, 0);
        overlay.set_child(Some(&top));
        let slots = Slots::install(&overlay, 46);
        top.append(slots.toolbar());
        let window = gtk4::Window::new();
        window.set_default_size(800, 200);
        window.set_child(Some(&overlay));
        window.present();
        let saved = Rc::new(RefCell::new(None::<Vec<String>>));
        // A plugin that is off keeps its place in the saved order.
        slots.set_toolbar_order(vec!["plugin:off/button".into()], Rc::new({
            let saved = Rc::clone(&saved);
            move |order| *saved.borrow_mut() = Some(order)
        }));
        let row = slots.toolbar_items.clone();
        crate::gtk_test::pump_until("three toolbar items", || crate::toolbar_reorder::children(&row).len() == 3);
        crate::toolbar_reorder::wait_for_layout(&row);
        assert_eq!(slots.toolbar_item_keys(), ["plugin:alpha/button", "plugin:beta/button", "plugin:gamma/button"]);

        // Alpha dropped just right of beta goes after it.
        let item = |n: usize| crate::toolbar_reorder::children(&row)[n].clone();
        let beta = item(1).compute_bounds(&row).unwrap();
        let drag = crate::toolbar_reorder::drag_to(&row, &item(0), f64::from(beta.x() + beta.width()) + 2.0);
        assert!(drag.marked && drag.outlines && drag.line.is_some());
        let order = vec!["plugin:beta/button".to_string(), "plugin:alpha/button".into(), "plugin:gamma/button".into()];
        assert_eq!(slots.toolbar_item_keys(), order);
        let mut expected = order.clone();
        expected.push("plugin:off/button".into());
        assert_eq!(saved.borrow().clone(), Some(expected));

        // A reload lays the items out again, in the saved order.
        slots.relayout();
        assert_eq!(slots.toolbar_item_keys(), order);
        window.close();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn toolbar_plugin_item_press_taken_by_a_drag_is_no_click() {
        if super_desktop::plugins::paths::qml_host_binary().is_none() {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        }
        crate::gtk_test::run_in_child_process("plugin_ui::slots::tests::press_taken_inner");
    }

    #[test]
    fn press_taken_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-press-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
        std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
        let plugins = root.join("plugins");
        write_manifest(&plugins, "tap", serde_json::json!([
            {"id": "button", "slot": "toolbar.item", "qml": "Button.qml", "size": 80}
        ]));
        // Like the plugins people write: a click is `onClicked` of a MouseArea.
        std::fs::write(plugins.join("tap/Button.qml"), "import QtQuick\nRectangle {\n    color: \"orange\"\n    \
            MouseArea {\n        anchors.fill: parent\n        hoverEnabled: true\n        \
            onPressed: sd.log(\"pressed\")\n        onClicked: sd.log(\"clicked\")\n    }\n}\n").unwrap();
        gtk4::init().unwrap();
        let overlay = gtk4::Overlay::new();
        let top = gtk4::Box::new(Orientation::Vertical, 0);
        overlay.set_child(Some(&top));
        let slots = Slots::install(&overlay, 46);
        top.append(slots.toolbar());
        let window = gtk4::Window::new();
        window.set_default_size(600, 200);
        window.set_child(Some(&overlay));
        window.present();
        crate::gtk_test::pump_until("the toolbar item's frame", || view_frame(slots.toolbar_items.upcast_ref()).is_some());
        let log = || std::fs::read_to_string(paths::log_path("tap")).unwrap_or_default();
        let count = |word: &str| log().lines().filter(|line| line.ends_with(word)).count();
        let plugins_ref = slots.plugins.borrow();
        let surface = &plugins_ref["tap"].surfaces[0].1;

        // Pressed, then taken by the bar's drag: no click, though the
        // pointer was over the item all along.
        surface.send_pointer("move", 20.0, 10.0, 0);
        surface.send_pointer("press", 20.0, 10.0, 1);
        surface.take_press(1);
        // A plain click after it does click, so the first one would have shown.
        surface.send_pointer("move", 20.0, 10.0, 0);
        surface.send_pointer("press", 20.0, 10.0, 1);
        surface.send_pointer("release", 20.0, 10.0, 1);
        crate::gtk_test::pump_until("the click", || count("clicked") > 0);
        assert_eq!(count("pressed"), 2, "{}", log());
        assert_eq!(count("clicked"), 1, "a press taken by the bar must not click:\n{}", log());
        drop(plugins_ref);
        window.close();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugins_slots_toolbar_items_and_panels() {
        if super_desktop::plugins::paths::qml_host_binary().is_none() {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        }
        crate::gtk_test::run_in_child_process("plugin_ui::slots::tests::toolbar_and_panels_inner");
    }

    #[test]
    fn toolbar_and_panels_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-slots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
        std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
        let plugins = root.join("plugins");
        write_manifest(&plugins, "tool", serde_json::json!([
            {"id": "button", "slot": "toolbar.item", "qml": "Button.qml", "size": 80},
            {"id": "win", "slot": "panel", "qml": "Panel.qml", "size": 300, "height": 200, "title": "Window"}
        ]));
        std::fs::write(plugins.join("tool/Button.qml"), "import QtQuick\nRectangle { color: \"orange\" }\n").unwrap();
        std::fs::write(plugins.join("tool/Panel.qml"), "import QtQuick\nRectangle { color: \"purple\" }\n").unwrap();

        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let overlay = gtk4::Overlay::new();
        let top = gtk4::Box::new(Orientation::Vertical, 0);
        overlay.set_child(Some(&top));
        let slots = Slots::install(&overlay, 46);
        top.append(slots.toolbar());
        let ceiling = gtk4::Box::new(Orientation::Vertical, 0);
        ceiling.set_visible(false);
        overlay.add_overlay(&ceiling);
        crate::floating_panel::set_host(&overlay, Rc::new(|| 46), &ceiling);
        let window = gtk4::Window::new();
        window.set_default_size(1000, 700);
        window.set_child(Some(&overlay));
        window.present();

        // The toolbar item runs at once, as tall as the bar allows; the
        // panel runs only when opened.
        crate::gtk_test::pump_until("the toolbar item's frame", || view_frame(slots.toolbar_items.upcast_ref()).is_some());
        let item = slots.toolbar_items.first_child().unwrap();
        let bounds = item.compute_bounds(&overlay).unwrap();
        assert_eq!((bounds.width() as i32, bounds.height() as i32), (80, 46 - TOOLBAR_ITEM_INSET));
        assert!(!slots.is_panel_open("tool", "win"));
        assert_eq!(insets(), (0, 0));

        // The item opens the panel through sd.call, as its QML would.
        let spec = SurfaceSpec {
            plugin_id: "tool".into(), plugin_name: "tool".into(), surface_id: "button".into(), instance: String::new(),
            qml: plugins.join("tool/Button.qml"), dir: plugins.join("tool"),
        };
        crate::plugin_ui::api::set_hooks(crate::plugin_ui::api::Hooks {
            hide: Rc::new(|| {}),
            harnesses: Rc::new(|| serde_json::json!([])),
            launch: Rc::new(|_, _| Ok(())),
            cards: Rc::new(|| serde_json::json!([])),
            focus_card: Rc::new(|_| Ok(())),
            card_session: Rc::new(|_| None),
            open_card: Rc::new(|_, _| Err("no cards in tests".into())),
        });
        // cards.focus wants an id that cards.list has.
        assert_eq!(crate::plugin_ui::api::call(&spec, "cards.focus", &serde_json::json!({})).unwrap_err()["code"], "bad_params");
        assert_eq!(crate::plugin_ui::api::call(&spec, "cards.focus", &serde_json::json!({"id": "gone"})).unwrap_err()["code"], "bad_params");
        let toggle = serde_json::json!({"surface": "win"});
        assert_eq!(crate::plugin_ui::api::call(&spec, "panel.toggle", &toggle).unwrap()["open"], true);
        let missing = crate::plugin_ui::api::call(&spec, "panel.open", &serde_json::json!({"surface": "button"})).unwrap_err();
        assert_eq!(missing["code"], "bad_params");
        let panel_frame = || {
            slots.open_panels.borrow().values().next()
                .and_then(|open| view_frame(open.body.upcast_ref()))
        };
        crate::gtk_test::pump_until("the panel's frame", || panel_frame().is_some());
        assert!(slots.is_panel_open("tool", "win"));

        // Saving the panel's QML rebuilds it in place.
        let before = panel_frame().unwrap();
        std::fs::write(plugins.join("tool/Panel.qml"), "import QtQuick\nRectangle { color: \"teal\" }\n").unwrap();
        crate::gtk_test::pump_until("the reloaded panel", || panel_frame().is_some_and(|f| f != before));
        assert!(slots.is_panel_open("tool", "win"));

        assert_eq!(crate::plugin_ui::api::call(&spec, "panel.toggle", &toggle).unwrap()["open"], false);
        crate::gtk_test::pump_until("the panel to close", || !slots.is_panel_open("tool", "win"));

        // A panel left open closes when its plugin is turned off.
        crate::plugin_ui::api::call(&spec, "panel.open", &toggle).unwrap();
        crate::gtk_test::pump_until("the panel to open again", || slots.is_panel_open("tool", "win"));
        crate::plugin_ui::settings_page::set_enabled(&plugins.join("tool"), false).unwrap();
        crate::gtk_test::pump_until("the plugin to turn off", || !slots.is_panel_open("tool", "win") && !slots.toolbar_items.is_visible());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plugins_slots_cards_and_fullscreen() {
        if super_desktop::plugins::paths::qml_host_binary().is_none() {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        }
        crate::gtk_test::run_in_child_process("plugin_ui::slots::tests::cards_and_fullscreen_inner");
    }

    #[test]
    fn cards_and_fullscreen_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-cards-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
        std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
        let plugins = root.join("plugins");
        write_manifest(&plugins, "game", serde_json::json!([
            {"id": "board", "slot": "card", "qml": "Board.qml", "size": 300, "height": 200, "title": "Board"},
            {"id": "big", "slot": "fullscreen", "qml": "Big.qml"}
        ]));
        // The same plugin, with a shortcut for its card.
        let manifest_path = plugins.join("game/plugin.json");
        let mut manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["shortcuts"] = serde_json::json!([{"keys": "Ctrl+Alt+B", "action": "card.toggle", "surface": "board"}]);
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();
        std::fs::write(plugins.join("game/Board.qml"), "import QtQuick\nRectangle { color: \"navy\" }\n").unwrap();
        std::fs::write(plugins.join("game/Big.qml"), "import QtQuick\nRectangle { color: \"black\" }\n").unwrap();

        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let overlay = gtk4::Overlay::new();
        let canvas = gtk4::Fixed::new();
        overlay.set_child(Some(&canvas));
        let slots = Slots::install(&overlay, 46);
        let host_canvas = canvas.clone();
        slots.set_canvas_host(super::super::cards::CanvasHost {
            canvas: canvas.clone(),
            raise: Rc::new(|_| {}),
            drag: Rc::new(move |w, x, y| host_canvas.move_(&w, x, y)),
            drag_end: Rc::new(|_, x, y| (x, y)),
            size: Rc::new(|| (1000, 700)),
            top: Rc::new(|| 46),
        });
        let window = gtk4::Window::new();
        window.set_default_size(1000, 700);
        window.set_child(Some(&overlay));
        window.present();
        let spec = SurfaceSpec {
            plugin_id: "game".into(), plugin_name: "game".into(), surface_id: "board".into(), instance: String::new(),
            qml: plugins.join("game/Board.qml"), dir: plugins.join("game"),
        };
        crate::plugin_ui::api::set_hooks(crate::plugin_ui::api::Hooks {
            hide: Rc::new(|| {}),
            harnesses: Rc::new(|| serde_json::json!([])),
            launch: Rc::new(|_, _| Ok(())),
            cards: Rc::new(|| serde_json::json!([])),
            focus_card: Rc::new(|_| Ok(())),
            card_session: Rc::new(|_| None),
            open_card: Rc::new(|_, _| Err("no cards in tests".into())),
        });
        let call = |method: &str, surface: &str| crate::plugin_ui::api::call(&spec, method, &serde_json::json!({"surface": surface}));

        // A card opens centered below the bar, at its declared size.
        assert_eq!(call("card.toggle", "board").unwrap()["open"], true);
        assert_eq!(call("card.open", "big").unwrap_err()["code"], "bad_params");
        let card = || slots.cards().into_iter().next();
        crate::gtk_test::pump_until("the card's frame", || card().is_some_and(|c| view_frame(c.container.upcast_ref()).is_some()));
        let g = card().unwrap().geometry();
        assert_eq!((g.width, g.height), (300, 200));
        assert_eq!((g.x, g.y), ((1000 - 300) / 2, 58 + (700 - 58 - 200) / 2));
        assert!(canvas.first_child().is_some());
        assert_eq!(super::super::cards::saved("game", "board"), Some((g, true)));

        // Arrange-style placement is kept.
        let moved = super::super::cards::Geometry { x: 20, y: 80, width: 400, height: 250 };
        card().unwrap().set_geometry(&slots.canvas_host().unwrap(), moved);
        assert_eq!(super::super::cards::saved("game", "board"), Some((moved, true)));

        // Saving its QML rebuilds it in place.
        let before = view_frame(card().unwrap().container.upcast_ref()).unwrap();
        std::fs::write(plugins.join("game/Board.qml"), "import QtQuick\nRectangle { color: \"maroon\" }\n").unwrap();
        crate::gtk_test::pump_until("the reloaded card", || {
            card().and_then(|c| view_frame(c.container.upcast_ref())).is_some_and(|f| f != before)
        });
        assert_eq!(card().unwrap().geometry(), moved);

        // Turning the plugin off closes the card but remembers it was open;
        // turning it on again brings it back where it was.
        crate::plugin_ui::settings_page::set_enabled(&plugins.join("game"), false).unwrap();
        crate::gtk_test::pump_until("the card to close", || card().is_none());
        assert!(canvas.first_child().is_none());
        crate::plugin_ui::settings_page::set_enabled(&plugins.join("game"), true).unwrap();
        crate::gtk_test::pump_until("the card to come back", || card().is_some());
        assert_eq!(card().unwrap().geometry(), moved);

        // Its shortcut toggles it; a weaker combination does nothing.
        use gtk4::gdk::{Key, ModifierType as M};
        assert!(!slots.handle_key(Key::b, M::CONTROL_MASK));
        assert!(slots.handle_key(Key::b, M::CONTROL_MASK | M::ALT_MASK));
        crate::gtk_test::pump_until("the shortcut to close the card", || card().is_none());
        assert!(slots.handle_key(Key::B, M::CONTROL_MASK | M::ALT_MASK));
        crate::gtk_test::pump_until("the shortcut to open it again", || card().is_some());

        // ✕ closes it for good.
        let close = crate::gtk_test::descendants(card().unwrap().container.upcast_ref()).into_iter()
            .filter_map(|w| w.downcast::<gtk4::Button>().ok())
            .find(|b| b.label().as_deref() == Some("✕"))
            .unwrap();
        close.emit_clicked();
        crate::gtk_test::pump_until("the card to go", || card().is_none());
        assert_eq!(super::super::cards::saved("game", "board"), Some((moved, false)));

        // A "multiple" card opens one copy per card.new; each keeps its place.
        let mut manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["surfaces"][0]["multiple"] = serde_json::json!(true);
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();
        crate::gtk_test::pump_until("the multiple manifest", || slots.card_is_multiple("game", "board"));
        assert_eq!(call("card.new", "board").unwrap(), serde_json::json!({}));
        crate::gtk_test::pump_until("the first copy", || slots.cards().len() == 1);
        call("card.new", "board").unwrap();
        crate::gtk_test::pump_until("the second copy", || slots.cards().len() == 2);
        let mut instances = slots.card_instances("game", "board");
        instances.sort();
        assert_eq!(instances, ["", "2"]);
        // The second copy is offset from the first.
        let first = slots.cards().into_iter().find(|c| c.instance.is_empty()).unwrap().geometry();
        let second = slots.cards().into_iter().find(|c| c.instance == "2").unwrap().geometry();
        assert_ne!((first.x, first.y), (second.x, second.y));
        // Off and on: both come back.
        crate::plugin_ui::settings_page::set_enabled(&plugins.join("game"), false).unwrap();
        crate::gtk_test::pump_until("the copies to close", || slots.cards().is_empty());
        crate::plugin_ui::settings_page::set_enabled(&plugins.join("game"), true).unwrap();
        crate::gtk_test::pump_until("both copies back", || slots.cards().len() == 2);
        // A copy closes by its instance.
        crate::plugin_ui::api::call(&spec, "card.close", &serde_json::json!({"surface": "board", "instance": "2"})).unwrap();
        crate::gtk_test::pump_until("the second copy to close", || slots.cards().len() == 1);
        assert_eq!(super::super::cards::saved("game", "board#2").map(|(_, open)| open), Some(false));
        crate::plugin_ui::api::call(&spec, "card.close", &serde_json::json!({"surface": "board"})).unwrap();
        crate::gtk_test::pump_until("no copies", || slots.cards().is_empty());

        // Full screen covers the whole overlay until closed.
        assert_eq!(call("fullscreen.open", "big").unwrap()["open"], true);
        crate::gtk_test::pump_until("the full-screen frame", || {
            slots.fullscreen.borrow().as_ref().is_some_and(|f| view_frame(f.container.upcast_ref()).is_some())
        });
        let bounds = slots.fullscreen.borrow().as_ref().unwrap().container.compute_bounds(&overlay).unwrap();
        assert_eq!((bounds.width() as i32, bounds.height() as i32), (overlay.width(), overlay.height()));
        assert!(slots.close_fullscreen());
        assert!(!slots.close_fullscreen());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plugins_slots_popup_placement() {
        let area = (1000, 700);
        // Below a toolbar item, centered on it.
        assert_eq!(popup_rect(area, 46, Some(((400.0, 10.0, 52.0, 36.0), PopupSide::Below)), (200, 150)), (326, 52, 200, 150));
        // At the right end of the bar it stays inside the screen.
        assert_eq!(popup_rect(area, 46, Some(((970.0, 10.0, 20.0, 36.0), PopupSide::Below)), (200, 150)).0, 1000 - POPUP_MARGIN - 200);
        // Near the bottom it flips above its anchor.
        assert_eq!(popup_rect(area, 46, Some(((100.0, 600.0, 40.0, 40.0), PopupSide::Below)), (200, 150)).1, 600 - POPUP_GAP - 150);
        // Beside a left dock, centered on the point; a right dock opens to the left.
        assert_eq!(popup_rect(area, 46, Some(((8.0, 300.0, 64.0, 40.0), PopupSide::Right)), (200, 100)), (78, 270, 200, 100));
        assert_eq!(popup_rect(area, 46, Some(((928.0, 300.0, 64.0, 40.0), PopupSide::Left)), (200, 100)), (722, 270, 200, 100));
        // A left dock's popup with no room on the right goes left of it.
        assert_eq!(popup_rect((300, 700), 46, Some(((200.0, 300.0, 64.0, 40.0), PopupSide::Right)), (100, 100)).0, 200 - POPUP_GAP - 100);
        // No anchor: centered below the bar. Too big: fitted to the screen.
        assert_eq!(popup_rect(area, 46, None, (200, 150)), (400, 52, 200, 150));
        assert_eq!(popup_rect((300, 200), 46, None, (800, 800)), (POPUP_MARGIN, POPUP_MARGIN, 284, 184));
    }

    #[test]
    fn plugins_slots_popups() {
        if super_desktop::plugins::paths::qml_host_binary().is_none() {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        }
        crate::gtk_test::run_in_child_process("plugin_ui::slots::tests::popups_inner");
    }

    #[test]
    fn popups_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-popups-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
        std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
        let plugins = root.join("plugins");
        write_manifest(&plugins, "menu", serde_json::json!([
            {"id": "button", "slot": "toolbar.item", "qml": "Button.qml", "size": 60, "tooltip": "Right-click for more"},
            {"id": "side", "slot": "dock.left", "qml": "Button.qml", "size": 64},
            {"id": "list", "slot": "popup", "qml": "List.qml", "size": 220, "height": 160},
            {"id": "form", "slot": "popup", "qml": "List.qml", "size": 300, "height": 200}
        ]));
        std::fs::write(plugins.join("menu/Button.qml"), "import QtQuick\nRectangle { color: \"orange\" }\n").unwrap();
        std::fs::write(plugins.join("menu/List.qml"), "import QtQuick\nRectangle { color: \"purple\" }\n").unwrap();

        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let overlay = gtk4::Overlay::new();
        let top = gtk4::Box::new(Orientation::Vertical, 0);
        overlay.set_child(Some(&top));
        let slots = Slots::install(&overlay, 46);
        let bar = gtk4::Box::new(Orientation::Horizontal, 0);
        bar.set_size_request(-1, 46);
        bar.set_margin_start(300);
        bar.append(slots.toolbar());
        top.append(&bar);
        let window = gtk4::Window::new();
        window.set_default_size(1000, 700);
        window.set_child(Some(&overlay));
        window.present();
        crate::plugin_ui::api::set_hooks(crate::plugin_ui::api::Hooks {
            hide: Rc::new(|| {}),
            harnesses: Rc::new(|| serde_json::json!([])),
            launch: Rc::new(|_, _| Ok(())),
            cards: Rc::new(|| serde_json::json!([])),
            focus_card: Rc::new(|_| Ok(())),
            card_session: Rc::new(|_| None),
            open_card: Rc::new(|_, _| Err("no cards in tests".into())),
        });
        let caller = |surface: &str| SurfaceSpec {
            plugin_id: "menu".into(), plugin_name: "menu".into(), surface_id: surface.into(), instance: String::new(),
            qml: plugins.join("menu/Button.qml"), dir: plugins.join("menu"),
        };
        let call = |from: &str, method: &str, params: serde_json::Value| crate::plugin_ui::api::call(&caller(from), method, &params);
        crate::gtk_test::pump_until("the toolbar item's frame", || view_frame(slots.toolbar_items.upcast_ref()).is_some());
        // Its own hover text; the dock without one shows the plugin name.
        assert_eq!(slots.toolbar_items.first_child().unwrap().tooltip_text().as_deref(), Some("Right-click for more"));
        assert_eq!(slots.left.first_child().unwrap().tooltip_text().as_deref(), Some("menu"));
        let item = slots.toolbar_items.first_child().unwrap().compute_bounds(&overlay).unwrap();
        let popup_bounds = || slots.popup.borrow().as_ref().and_then(|p| p.container.compute_bounds(&overlay));
        let popup_frame = || slots.popup.borrow().as_ref().and_then(|p| view_frame(p.container.upcast_ref()));

        // Only popup surfaces open as popups.
        assert_eq!(call("button", "popup.open", serde_json::json!({"surface": "button"})).unwrap_err()["code"], "bad_params");
        assert_eq!(call("button", "popup.open", serde_json::json!({})).unwrap_err()["code"], "bad_params");

        // From the toolbar item: below it, centered on it, at its size.
        assert_eq!(call("button", "popup.toggle", serde_json::json!({"surface": "list"})).unwrap()["open"], true);
        crate::gtk_test::pump_until("the popup's frame", || popup_frame().is_some());
        assert!(slots.is_popup_open("menu", "list"));
        let b = popup_bounds().unwrap();
        assert_eq!((b.width() as i32, b.height() as i32), (220, 160));
        assert_eq!(b.y() as i32, (item.y() + item.height()) as i32 + POPUP_GAP);
        assert_eq!((b.x() + b.width() / 2.0) as i32, (item.x() + item.width() / 2.0) as i32);

        // Saving its QML rebuilds it in place.
        let before = popup_frame().unwrap();
        std::fs::write(plugins.join("menu/List.qml"), "import QtQuick\nRectangle { color: \"teal\" }\n").unwrap();
        crate::gtk_test::pump_until("the reloaded popup", || popup_frame().is_some_and(|f| f != before));
        assert!(slots.is_popup_open("menu", "list"));

        // A press inside it keeps it; toggle closes it.
        slots.popup_press(b.x() as f64 + 20.0, b.y() as f64 + 20.0);
        assert!(slots.is_popup_open("menu", "list"));
        assert_eq!(call("button", "popup.toggle", serde_json::json!({"surface": "list"})).unwrap()["open"], false);
        crate::gtk_test::pump_until("the popup to close", || slots.popup.borrow().is_none());

        // A press elsewhere closes it.
        call("button", "popup.open", serde_json::json!({"surface": "list"})).unwrap();
        crate::gtk_test::pump_until("the popup again", || slots.is_popup_open("menu", "list"));
        crate::gtk_test::pump(100);
        slots.popup_press(900.0, 600.0);
        assert!(slots.popup.borrow().is_none());

        // A press on the item that opened it closes it, and the item's
        // toggle right after leaves it closed.
        call("button", "popup.open", serde_json::json!({"surface": "list"})).unwrap();
        crate::gtk_test::pump_until("the popup once more", || slots.is_popup_open("menu", "list"));
        crate::gtk_test::pump(100);
        slots.popup_press((item.x() + item.width() / 2.0) as f64, (item.y() + item.height() / 2.0) as f64);
        assert!(slots.popup.borrow().is_none());
        assert_eq!(call("button", "popup.toggle", serde_json::json!({"surface": "list"})).unwrap()["open"], false);
        crate::gtk_test::pump(100);
        assert!(slots.popup.borrow().is_none());
        // The next toggle opens it again.
        assert_eq!(call("button", "popup.toggle", serde_json::json!({"surface": "list"})).unwrap()["open"], true);
        crate::gtk_test::pump_until("the popup after the grace", || slots.is_popup_open("menu", "list"));

        // One popup at a time: opening another replaces it.
        call("button", "popup.open", serde_json::json!({"surface": "form"})).unwrap();
        crate::gtk_test::pump_until("the second popup", || slots.is_popup_open("menu", "form"));
        assert!(!slots.is_popup_open("menu", "list"));
        crate::gtk_test::pump_until("the second popup's frame", || popup_bounds().is_some_and(|b| b.width() as i32 == 300));

        // From a dock: beside it, centered on the box the dock gives.
        call("side", "popup.open", serde_json::json!({"surface": "list", "x": 0, "y": 200, "width": 64, "height": 40})).unwrap();
        crate::gtk_test::pump_until("the dock's popup", || slots.is_popup_open("menu", "list") && popup_frame().is_some());
        let dock = slots.left.compute_bounds(&overlay).unwrap();
        crate::gtk_test::pump_until("the popup beside the dock", || {
            popup_bounds().is_some_and(|b| b.x() as i32 == (dock.x() + dock.width()) as i32 + POPUP_GAP && b.y() as i32 == (dock.y() + 220.0) as i32 - 80)
        });

        // Esc closes it (and does not reach the overlay).
        let keys = {
            let popup = slots.popup.borrow();
            let controllers = popup.as_ref().unwrap().container.observe_controllers();
            (0..controllers.n_items()).find_map(|i| controllers.item(i).and_downcast::<gtk4::EventControllerKey>()).unwrap()
        };
        let stopped: bool = keys.emit_by_name("key-pressed", &[&gtk4::gdk::Key::Escape, &9u32, &gtk4::gdk::ModifierType::empty()]);
        assert!(stopped);
        crate::gtk_test::pump_until("Esc to close the popup", || slots.popup.borrow().is_none());
        call("side", "popup.open", serde_json::json!({"surface": "list"})).unwrap();
        crate::gtk_test::pump_until("the dock's popup again", || slots.is_popup_open("menu", "list"));

        // The popup can resize itself, within the slot's limits; only while open.
        assert_eq!(call("list", "popup.resize", serde_json::json!({"surface": "list", "height": 300})).unwrap(), serde_json::json!({"width": 220, "height": 300}));
        crate::gtk_test::pump_until("the taller popup", || popup_bounds().is_some_and(|b| b.height() as i32 == 300));
        assert_eq!(call("list", "popup.resize", serde_json::json!({"surface": "list", "width": 5000})).unwrap()["width"], 800);
        assert_eq!(call("list", "popup.resize", serde_json::json!({"surface": "list"})).unwrap_err()["code"], "bad_params");
        assert_eq!(call("list", "popup.resize", serde_json::json!({"surface": "form", "height": 100})).unwrap_err()["code"], "bad_params");

        // The popup itself can close it.
        assert_eq!(call("list", "popup.close", serde_json::json!({"surface": "list"})).unwrap()["open"], false);
        crate::gtk_test::pump_until("the self-closed popup", || slots.popup.borrow().is_none());

        // Turning the plugin off closes an open popup.
        call("button", "popup.open", serde_json::json!({"surface": "list"})).unwrap();
        crate::gtk_test::pump_until("an open popup", || slots.is_popup_open("menu", "list"));
        crate::plugin_ui::settings_page::set_enabled(&plugins.join("menu"), false).unwrap();
        crate::gtk_test::pump_until("the popup to go with its plugin", || slots.popup.borrow().is_none());
        assert_eq!(call("button", "popup.open", serde_json::json!({"surface": "list"})).unwrap_err()["code"], "bad_params");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plugins_slots_installed_plugins_wait_for_approval() {
        if super_desktop::plugins::paths::qml_host_binary().is_none() {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        }
        crate::gtk_test::run_in_child_process("plugin_ui::slots::tests::approval_inner");
    }

    #[test]
    fn approval_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-approval-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
        std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
        // Installed from a folder: recorded as not approved.
        let upstream = root.join("upstream/shared");
        write_plugin(&root.join("upstream"), "shared", "dock.left", 60, "import QtQuick\nRectangle { color: \"red\" }\n");
        let places = super_desktop::plugins::install::Places::current();
        super_desktop::plugins::install::install(&places, upstream.to_str().unwrap()).unwrap();

        gtk4::init().unwrap();
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&gtk4::Box::new(Orientation::Vertical, 0)));
        let slots = Slots::install(&overlay, 46);
        let window = gtk4::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&overlay));
        window.present();
        crate::gtk_test::pump(400);
        assert!(!slots.left.is_visible(), "nothing of an unapproved plugin runs");
        assert_eq!(super_desktop::plugins::paths::read_status("shared").unwrap()["state"], "waiting-approval");

        super_desktop::plugins::install::approve(&places, "shared").unwrap();
        crate::gtk_test::pump_until("the approved dock", || has_frame(&slots.left));
        let _ = std::fs::remove_dir_all(&root);
    }
}
