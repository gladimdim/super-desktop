//! Plugin docks: vertical toolbars at the left and right edges of the
//! overlay, below the top bar. Each plugin folder is watched; saving any file
//! in it rebuilds that plugin's surfaces, which is how an agent sees its
//! edits live.
use super::surface::{QmlSurface, SurfaceSpec};
use super_desktop::plugins::manifest::{self, Manifest, Slot};
use super_desktop::plugins::paths;
use gtk4::prelude::*;
use gtk4::{gio, glib, Align, Orientation};
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Space between a dock and the screen edge, the top bar and the next dock.
pub const DOCK_GAP: i32 = 8;
const DEBOUNCE: Duration = Duration::from_millis(250);

struct Loaded {
    manifest: Manifest,
    surfaces: Vec<(Slot, i32, QmlSurface)>,
    _monitor: Option<gio::FileMonitor>,
}

pub struct Docks {
    left: gtk4::Box,
    right: gtk4::Box,
    root: PathBuf,
    plugins: RefCell<BTreeMap<String, Loaded>>,
    root_monitor: RefCell<Option<gio::FileMonitor>>,
    pending: RefCell<BTreeSet<String>>,
    rescan: Cell<bool>,
    debounce: RefCell<Option<glib::SourceId>>,
}

thread_local! {
    static DOCKS: RefCell<Option<Rc<Docks>>> = const { RefCell::new(None) };
}

/// Width the docks take at each edge, gaps included: (left, right). Arrange
/// keeps cards out of it.
pub fn insets() -> (i32, i32) {
    DOCKS.with(|docks| docks.borrow().as_ref().map_or((0, 0), |d| d.insets()))
}

/// Keeps the docks below the top bar after its size changes.
pub fn set_top_inset(top: i32) {
    DOCKS.with(|docks| {
        if let Some(docks) = docks.borrow().as_ref() {
            docks.set_top_inset(top);
        }
    });
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

impl Docks {
    /// Adds the two dock edges to the overlay and loads every plugin.
    pub fn install(overlay: &gtk4::Overlay, top_inset: i32) -> Rc<Docks> {
        let root = paths::plugins_dir();
        let _ = std::fs::create_dir_all(&root);
        let docks = Rc::new(Docks {
            left: edge_box(Align::Start),
            right: edge_box(Align::End),
            root,
            plugins: RefCell::new(BTreeMap::new()),
            root_monitor: RefCell::new(None),
            pending: RefCell::new(BTreeSet::new()),
            rescan: Cell::new(false),
            debounce: RefCell::new(None),
        });
        for edge in [&docks.left, &docks.right] {
            overlay.add_overlay(edge);
            overlay.set_measure_overlay(edge, false);
            overlay.set_clip_overlay(edge, true);
        }
        docks.set_top_inset(top_inset);
        docks.watch_root();
        docks.scan();
        DOCKS.with(|cell| *cell.borrow_mut() = Some(Rc::clone(&docks)));
        docks
    }

    pub fn set_top_inset(&self, top: i32) {
        for edge in [&self.left, &self.right] {
            edge.set_margin_top(top + DOCK_GAP);
        }
    }

    pub fn insets(&self) -> (i32, i32) {
        let width = |slot: Slot| {
            let sizes: Vec<i32> = self
                .plugins
                .borrow()
                .values()
                .flat_map(|loaded| loaded.surfaces.iter())
                .filter(|(s, _, _)| *s == slot)
                .map(|(_, size, _)| *size)
                .collect();
            if sizes.is_empty() {
                0
            } else {
                sizes.iter().sum::<i32>() + DOCK_GAP * sizes.len() as i32
            }
        };
        (width(Slot::DockLeft), width(Slot::DockRight))
    }

    fn watch_root(self: &Rc<Self>) {
        let file = gio::File::for_path(&self.root);
        match file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
            Ok(monitor) => {
                let weak = Rc::downgrade(self);
                monitor.connect_changed(move |_, _, _, _| {
                    if let Some(docks) = weak.upgrade() {
                        docks.rescan.set(true);
                        docks.schedule();
                    }
                });
                *self.root_monitor.borrow_mut() = Some(monitor);
            }
            Err(e) => eprintln!("SUPER DESKTOP: cannot watch {}: {e}", self.root.display()),
        }
    }

    fn watch_plugin(self: &Rc<Self>, id: &str, dir: &std::path::Path) -> Option<gio::FileMonitor> {
        let monitor = gio::File::for_path(dir)
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            .ok()?;
        let weak = Rc::downgrade(self);
        let id = id.to_owned();
        monitor.connect_changed(move |_, file, _, _| {
            // The skill copy and editor swap files do not change the plugin.
            let name = file.basename().and_then(|n| n.to_str().map(str::to_owned)).unwrap_or_default();
            if name.starts_with('.') || name.ends_with('~') || name.ends_with(".swp") || name.ends_with(".md") {
                return;
            }
            if let Some(docks) = weak.upgrade() {
                docks.pending.borrow_mut().insert(id.clone());
                docks.schedule();
            }
        });
        Some(monitor)
    }

    fn schedule(self: &Rc<Self>) {
        if let Some(id) = self.debounce.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(DEBOUNCE, move || {
            if let Some(docks) = weak.upgrade() {
                docks.debounce.take();
                docks.flush();
            }
        });
        *self.debounce.borrow_mut() = Some(id);
    }

    fn flush(self: &Rc<Self>) {
        let pending: Vec<String> = std::mem::take(&mut *self.pending.borrow_mut()).into_iter().collect();
        if self.rescan.replace(false) {
            self.scan_with(&pending);
        } else {
            for id in pending {
                self.reload(&id);
            }
            self.relayout();
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
    }

    fn unload(&self, id: &str) {
        if let Some(loaded) = self.plugins.borrow_mut().remove(id) {
            for (_, _, surface) in &loaded.surfaces {
                surface.dispose();
                if let Some(parent) = surface.widget().parent().and_downcast::<gtk4::Box>() {
                    parent.remove(surface.widget());
                }
            }
            paths::write_status(id, "stopped", &json!([]));
        }
    }

    fn reload(self: &Rc<Self>, id: &str) {
        self.unload(id);
        let dir = self.root.join(id);
        if !dir.is_dir() {
            return;
        }
        let monitor = self.watch_plugin(id, &dir);
        match manifest::load(&dir) {
            Err(problems) => {
                for p in &problems {
                    paths::append_log(id, &format!("plugin.json: {}: {} (hint: {})", p.field, p.message, p.hint));
                }
                paths::write_status(id, "error", &serde_json::to_value(&problems).unwrap_or_default());
                // Keep watching so a fixed manifest loads.
                self.plugins.borrow_mut().insert(id.to_owned(), Loaded {
                    manifest: Manifest { id: id.to_owned(), name: id.to_owned(), description: String::new(), enabled: false, surfaces: Vec::new(), dir },
                    surfaces: Vec::new(),
                    _monitor: monitor,
                });
            }
            Ok(manifest) => {
                let surfaces = if manifest.enabled {
                    manifest
                        .surfaces
                        .iter()
                        .map(|s| {
                            let surface = QmlSurface::new(SurfaceSpec {
                                plugin_id: manifest.id.clone(),
                                plugin_name: manifest.name.clone(),
                                surface_id: s.id.clone(),
                                qml: manifest.qml_path(s),
                                dir: manifest.dir.clone(),
                            });
                            surface.widget().set_size_request(s.size, -1);
                            surface.widget().set_hexpand(false);
                            surface.widget().set_vexpand(true);
                            (s.slot, s.size, surface)
                        })
                        .collect()
                } else {
                    paths::write_status(id, "disabled", &json!([]));
                    Vec::new()
                };
                paths::append_log(id, &format!("loaded {} surface(s)", surfaces.len()));
                self.plugins.borrow_mut().insert(id.to_owned(), Loaded { manifest, surfaces, _monitor: monitor });
            }
        }
    }

    /// Puts every surface in its edge, in plugin-id order.
    fn relayout(&self) {
        for edge in [&self.left, &self.right] {
            while let Some(child) = edge.first_child() {
                edge.remove(&child);
            }
        }
        for loaded in self.plugins.borrow().values() {
            for (slot, _, surface) in &loaded.surfaces {
                let edge = match slot {
                    Slot::DockLeft => &self.left,
                    Slot::DockRight => &self.right,
                };
                edge.append(surface.widget());
            }
            let _ = &loaded.manifest;
        }
        self.left.set_visible(self.left.first_child().is_some());
        self.right.set_visible(self.right.first_child().is_some());
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
    fn plugins_docks_draw_at_the_edges_and_reload_on_save() {
        if super_desktop::plugins::paths::qml_host_binary().is_none() {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        }
        crate::gtk_test::run_in_child_process("plugin_ui::docks::tests::docks_inner");
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
        let docks = Docks::install(&overlay, 46);
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
}
