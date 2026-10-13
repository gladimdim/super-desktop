//! Where plugin surfaces live in the overlay: docks at the left and right
//! edges, items in the top bar's launcher row, floating panels, and pages in
//! Settings → Plugins. Every plugin folder is watched; saving any file in it
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
use std::time::Duration;

/// Space between a dock and the screen edge, the top bar and the next dock.
pub const DOCK_GAP: i32 = 8;
/// A toolbar item is this much shorter than the top bar.
const TOOLBAR_ITEM_INSET: i32 = 10;
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Plugin folders are watched this deep (the folder itself is depth 0).
const WATCH_DEPTH: usize = 3;

struct Loaded {
    manifest: Result<Manifest, Vec<Problem>>,
    /// Docks and toolbar items: they run as long as the plugin is loaded.
    surfaces: Vec<(Surface, QmlSurface)>,
    _monitors: Vec<gio::FileMonitor>,
}

/// An open plugin panel: its frame stays while the plugin reloads.
struct OpenPanel {
    body: gtk4::Box,
    surface: Option<QmlSurface>,
}

pub struct Slots {
    left: gtk4::Box,
    right: gtk4::Box,
    toolbar: gtk4::Box,
    toolbar_items: gtk4::Box,
    root: PathBuf,
    plugins: RefCell<BTreeMap<String, Loaded>>,
    panels: crate::floating_panel::CardPanels,
    open_panels: RefCell<BTreeMap<String, OpenPanel>>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    item_height: Cell<i32>,
    root_monitor: RefCell<Option<gio::FileMonitor>>,
    pending: RefCell<BTreeSet<String>>,
    rescan: Cell<bool>,
    debounce: RefCell<Option<glib::SourceId>>,
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

fn panel_key(plugin: &str, surface: &str) -> String {
    format!("plugin:{plugin}/{surface}")
}

fn spec(manifest: &Manifest, surface: &Surface) -> SurfaceSpec {
    SurfaceSpec {
        plugin_id: manifest.id.clone(),
        plugin_name: manifest.name.clone(),
        surface_id: surface.id.clone(),
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
            left: edge_box(Align::Start),
            right: edge_box(Align::End),
            toolbar,
            toolbar_items,
            root,
            plugins: RefCell::new(BTreeMap::new()),
            panels: crate::floating_panel::CardPanels::new((480, 360), (200, 120)),
            open_panels: RefCell::new(BTreeMap::new()),
            listeners: RefCell::new(Vec::new()),
            item_height: Cell::new(26),
            root_monitor: RefCell::new(None),
            pending: RefCell::new(BTreeSet::new()),
            rescan: Cell::new(false),
            debounce: RefCell::new(None),
        });
        for edge in [&slots.left, &slots.right] {
            overlay.add_overlay(edge);
            overlay.set_measure_overlay(edge, false);
            overlay.set_clip_overlay(edge, true);
        }
        slots.set_top_inset(top_inset);
        SLOTS.with(|cell| *cell.borrow_mut() = Some(Rc::clone(&slots)));
        slots.watch_root();
        slots.scan();
        slots
    }

    /// The top-bar row: plugin toolbar items, then ✨ Create Plugin. It goes
    /// in the launcher group, which scrolls when the bar is full.
    pub fn toolbar(&self) -> &gtk4::Box {
        &self.toolbar
    }

    pub fn set_top_inset(&self, top: i32) {
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
        let plugins = self.plugins.borrow();
        let manifest = plugins
            .get(plugin)
            .and_then(|loaded| loaded.manifest.as_ref().ok())
            .ok_or_else(|| format!("plugin {plugin:?} is not loaded"))?;
        if !manifest.enabled {
            return Err(format!("plugin {plugin:?} is turned off"));
        }
        let found = manifest
            .surface(surface)
            .filter(|s| s.slot == Slot::Panel)
            .ok_or_else(|| format!("{plugin:?} has no panel surface {surface:?}"))?;
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
        let surfaces = match &manifest {
            Err(problems) => {
                for p in problems {
                    paths::append_log(id, &format!("plugin.json: {}: {} (hint: {})", p.field, p.message, p.hint));
                }
                paths::write_status(id, "error", &serde_json::to_value(problems).unwrap_or_default());
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
        self.plugins.borrow_mut().insert(id.to_owned(), Loaded { manifest, surfaces, _monitors: monitors });
        self.refresh_panels(id);
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
                    Slot::Panel | Slot::SettingsPage => continue,
                };
                target.append(widget.widget());
            }
        }
        self.left.set_visible(self.left.first_child().is_some());
        self.right.set_visible(self.right.first_child().is_some());
        self.toolbar_items.set_visible(self.toolbar_items.first_child().is_some());
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
            plugin_id: "tool".into(), plugin_name: "tool".into(), surface_id: "button".into(),
            qml: plugins.join("tool/Button.qml"), dir: plugins.join("tool"),
        };
        crate::plugin_ui::api::set_hooks(crate::plugin_ui::api::Hooks {
            hide: Rc::new(|| {}),
            harnesses: Rc::new(|| serde_json::json!([])),
            launch: Rc::new(|_, _| Ok(())),
            cards: Rc::new(|| serde_json::json!([])),
            open_card: Rc::new(|_, _| Err("no cards in tests".into())),
        });
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
}
