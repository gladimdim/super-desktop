//! Plugin toolbar items: one group placed after the harness launchers, inside
//! the top bar's scrolling part — never next to Arrange, Settings and Hide
//! (AGENTS.md, "Responsive toolbar invariant"). Labels hide on narrow
//! displays together with the shortcut hint.
use super::view::icon_widget;
use crate::plugin_host::manifest::ToolbarItem;
use gtk4::prelude::*;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

/// (plugin id, item id) of a click.
pub type OnClick = Rc<dyn Fn(&str, &str)>;

struct Item {
    plugin: String,
    id: String,
    button: gtk4::Button,
    icon: gtk4::Box,
    label: gtk4::Label,
    badge: gtk4::Label,
    tooltip: RefCell<String>,
    /// The plugin's own `enabled`, kept apart from "the plugin stopped".
    enabled: Cell<bool>,
    dir: std::path::PathBuf,
}

pub struct PluginBar {
    pub group: gtk4::Box,
    items: RefCell<Vec<Rc<Item>>>,
    compact: Cell<bool>,
    on_click: OnClick,
}

impl PluginBar {
    pub fn new(on_click: OnClick) -> Rc<Self> {
        let group = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        group.add_css_class("hud-launchers");
        group.add_css_class("plugin-items");
        group.set_valign(gtk4::Align::Center);
        group.set_visible(false);
        Rc::new(PluginBar { group, items: RefCell::default(), compact: Cell::new(false), on_click })
    }

    pub fn add(self: &Rc<Self>, plugin: &str, dir: &Path, item: &ToolbarItem) {
        let button = gtk4::Button::new();
        button.add_css_class("hud-button");
        button.add_css_class("plugin-item");
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        let icon = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        icon.append(&icon_widget(dir, &item.icon, crate::brand::BRAND_ICON_SIZE));
        let label = gtk4::Label::new(item.label.as_deref());
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        label.set_max_width_chars(14);
        label.set_visible(item.label.is_some() && !self.compact.get());
        let badge = gtk4::Label::new(None);
        badge.add_css_class("plugin-badge");
        badge.set_visible(false);
        row.append(&icon);
        row.append(&label);
        row.append(&badge);
        button.set_child(Some(&row));
        button.set_tooltip_text(Some(&item.tooltip));
        button.update_property(&[gtk4::accessible::Property::Label(&item.tooltip)]);
        let entry = Rc::new(Item {
            plugin: plugin.to_string(),
            id: item.id.clone(),
            button: button.clone(),
            icon,
            label,
            badge,
            tooltip: RefCell::new(item.tooltip.clone()),
            enabled: Cell::new(true),
            dir: dir.to_path_buf(),
        });
        let weak = Rc::downgrade(self);
        let (plugin, id) = (entry.plugin.clone(), entry.id.clone());
        button.connect_clicked(move |_| {
            if let Some(bar) = weak.upgrade() {
                (bar.on_click)(&plugin, &id);
            }
        });
        self.group.append(&button);
        self.items.borrow_mut().push(entry);
        self.group.set_visible(true);
    }

    /// Remove every item of `plugin`.
    pub fn remove_plugin(&self, plugin: &str) {
        let removed: Vec<Rc<Item>> = {
            let mut items = self.items.borrow_mut();
            let (gone, kept) = items.drain(..).partition(|i| i.plugin == plugin);
            *items = kept;
            gone
        };
        for item in removed {
            self.group.remove(&item.button);
        }
        self.group.set_visible(!self.items.borrow().is_empty());
    }

    /// Apply a checked `contrib.update`. `false` when the id is not here.
    pub fn update(&self, id: &str, change: &Value) -> bool {
        let Some(item) = self.items.borrow().iter().find(|i| i.id == id).cloned() else { return false };
        if let Some(label) = change.get("label").and_then(Value::as_str) {
            item.label.set_text(label);
            item.label.set_visible(!label.is_empty() && !self.compact.get());
        }
        match change.get("badge") {
            Some(Value::String(text)) if !text.is_empty() => {
                item.badge.set_text(text);
                item.badge.set_visible(true);
            }
            Some(_) => item.badge.set_visible(false),
            None => {}
        }
        if let Some(tooltip) = change.get("tooltip").and_then(Value::as_str) {
            item.tooltip.replace(tooltip.to_string());
            item.button.set_tooltip_text(Some(tooltip));
        }
        if let Some(icon) = change.get("icon").and_then(Value::as_str) {
            while let Some(child) = item.icon.first_child() {
                item.icon.remove(&child);
            }
            item.icon.append(&icon_widget(&item.dir, icon, crate::brand::BRAND_ICON_SIZE));
        }
        if let Some(enabled) = change.get("enabled").and_then(Value::as_bool) {
            item.enabled.set(enabled);
            item.button.set_sensitive(enabled);
        }
        if let Some(visible) = change.get("visible").and_then(Value::as_bool) {
            item.button.set_visible(visible);
        }
        true
    }

    /// Draw an item as pressed (on) or not: a toolbar toggle such as a
    /// `renderer.toggle` command.
    pub fn set_pressed(&self, plugin: &str, item: &str, pressed: bool) {
        for i in self.items.borrow().iter().filter(|i| i.plugin == plugin && i.id == item) {
            if pressed {
                i.button.add_css_class("plugin-item-on");
            } else {
                i.button.remove_css_class("plugin-item-on");
            }
        }
    }

    /// The plugin's items drawn pressed (`plugin status` reports them).
    pub fn pressed_of(&self, plugin: &str) -> Vec<String> {
        self.items.borrow().iter().filter(|i| i.plugin == plugin && i.button.has_css_class("plugin-item-on")).map(|i| i.id.clone()).collect()
    }

    /// A plugin that stopped working: its items stay, disabled, saying why.
    pub fn set_failed(&self, plugin: &str, reason: Option<&str>) {
        for item in self.items.borrow().iter().filter(|i| i.plugin == plugin) {
            match reason {
                Some(reason) => {
                    item.button.set_sensitive(false);
                    item.button.set_tooltip_text(Some(&format!("{} — {reason}", item.tooltip.borrow())));
                }
                None => {
                    item.button.set_sensitive(item.enabled.get());
                    item.button.set_tooltip_text(Some(&item.tooltip.borrow()));
                }
            }
        }
    }

    /// Narrow displays: icons and badges only.
    pub fn set_compact(&self, compact: bool) {
        self.compact.set(compact);
        for item in self.items.borrow().iter() {
            item.label.set_visible(!compact && !item.label.text().is_empty());
        }
    }

    pub fn button(&self, id: &str) -> Option<gtk4::Button> {
        self.items.borrow().iter().find(|i| i.id == id).map(|i| i.button.clone())
    }

    pub fn button_count_of(&self, plugin: &str) -> usize {
        self.items.borrow().iter().filter(|i| i.plugin == plugin).count()
    }

    pub fn len(&self) -> usize {
        self.items.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk4::Orientation;

    fn item(plugin: &str, n: usize) -> ToolbarItem {
        ToolbarItem {
            id: format!("{plugin}.item-{n}"),
            icon: "🧪".into(),
            label: Some(format!("A rather long plugin label {n}")),
            tooltip: "t".into(),
            command: Some(format!("{plugin}.run")),
            view: None,
        }
    }

    #[test]
    fn toolbar_plugin_items_fit() {
        crate::gtk_test::run_in_child_process("plugin_ui::toolbar::tests::toolbar_plugin_items_inner");
    }

    /// Plugin items sit in the launchers' scroller: any number of them, with
    /// long labels, never pushes Arrange, Settings or Hide off the bar, at
    /// every bar size and across shrinking and growing (unmapped layout; the
    /// mapped hit test is `window::tests::toolbar_controls_stay_on_screen`).
    #[test]
    fn toolbar_plugin_items_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let clicks: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
        let sink = Rc::clone(&clicks);
        let bar = PluginBar::new(Rc::new(move |plugin: &str, id: &str| sink.borrow_mut().push((plugin.into(), id.into()))));
        let left = gtk4::Box::new(Orientation::Horizontal, 0);
        let brand = gtk4::Label::new(Some("SUPER DESKTOP"));
        left.append(&brand);
        let launchers = gtk4::Box::new(Orientation::Horizontal, 6);
        let harnesses = gtk4::Box::new(Orientation::Horizontal, 0);
        harnesses.append(&gtk4::Button::with_label("Claude"));
        launchers.append(&harnesses);
        launchers.append(&bar.group);
        let right = gtk4::Box::new(Orientation::Horizontal, 10);
        let actions: Vec<gtk4::Button> = ["Arrange", "Settings", "Hide"].iter().map(|l| {
            let b = gtk4::Button::with_label(l);
            right.append(&b);
            b
        }).collect();
        let hint = gtk4::Label::new(Some("[SUPER + SHIFT + Q]"));
        right.append(&hint);
        hint.connect_visible_notify({
            let bar = Rc::clone(&bar);
            move |hint| bar.set_compact(!hint.is_visible())
        });
        let hud = gtk4::Box::new(Orientation::Horizontal, 0);
        hud.add_css_class("hud-bar");
        hud.append(&crate::window::top_bar_content(&left, &launchers, &right));
        assert!(!bar.group.is_visible(), "an empty plugin group takes no space");
        for plugins in [1usize, 8] {
            for p in 0..plugins {
                for n in 0..4 {
                    bar.add(&format!("plugin-{p}"), Path::new("/tmp"), &item(&format!("plugin-{p}"), n));
                }
            }
            for size in [crate::state::TopBarSize::Small, crate::state::TopBarSize::Medium, crate::state::TopBarSize::Large] {
                crate::window::paint_top_bar_size(&hud, size);
                for width in [1280, 320, 2560, 640, 1199, 1200, 480, 3440, 1024] {
                    crate::window::fit_top_bar(&hud, width, &brand, &hint);
                    let (_, natural, _, _) = hud.measure(Orientation::Horizontal, -1);
                    assert_eq!(natural, width, "plugin items must not inflate the bar");
                    let (_, height, _, _) = hud.measure(Orientation::Vertical, width);
                    hud.allocate(width, height, -1, None);
                    for action in &actions {
                        let bounds = action.compute_bounds(&hud).unwrap();
                        assert!(bounds.width() > 0.0 && bounds.x() >= 0.0 && bounds.x() + bounds.width() <= hud.width() as f32 + 0.5, "{width}: {bounds:?}");
                    }
                    let labels_shown = bar.items.borrow().iter().any(|i| i.label.is_visible());
                    assert_eq!(labels_shown, width >= 1200, "labels follow the hint at {width}");
                }
            }
            bar.items.borrow()[0].button.emit_clicked();
            assert_eq!(clicks.borrow().last().unwrap(), &("plugin-0".to_string(), "plugin-0.item-0".to_string()));
            assert!(bar.update("plugin-0.item-0", &serde_json::json!({"badge": "3", "enabled": false})));
            assert!(bar.items.borrow()[0].badge.is_visible() && !bar.items.borrow()[0].button.is_sensitive());
            bar.set_failed("plugin-0", Some("stopped"));
            bar.set_failed("plugin-0", None);
            assert!(!bar.items.borrow()[0].button.is_sensitive(), "a plugin's own `enabled: false` survives a restart");
            for p in 0..plugins {
                bar.remove_plugin(&format!("plugin-{p}"));
            }
            assert_eq!(bar.len(), 0);
            assert!(!bar.group.is_visible());
        }
    }
}
