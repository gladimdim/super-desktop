//! Plugin views: themed GTK widgets built from a node tree that
//! `plugin_host::ui_model` already checked, updated by `ui.patch`, and
//! reporting `view.event`s back. Plugins never touch GTK.
use gtk4::glib;
use gtk4::prelude::*;
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

/// Called with (node id, event, value) when the user interacts.
pub type OnEvent = Rc<dyn Fn(&str, &str, Value)>;

/// One built node: its type and the widgets a `set` may change.
#[derive(Clone)]
struct Node {
    kind: String,
    /// The outermost widget (what a parent holds).
    widget: gtk4::Widget,
    /// For containers: where children go.
    container: Option<gtk4::Box>,
    label: Option<gtk4::Label>,
    button: Option<gtk4::Button>,
    check: Option<gtk4::CheckButton>,
    switch: Option<gtk4::Switch>,
    entry: Option<gtk4::Entry>,
    text: Option<gtk4::TextView>,
    dropdown: Option<(gtk4::DropDown, Rc<RefCell<Vec<String>>>)>,
    progress: Option<(gtk4::ProgressBar, Rc<Cell<Option<glib::SourceId>>>)>,
    image: Option<gtk4::Image>,
    scroll: Option<gtk4::ScrolledWindow>,
    markdown_holder: Option<gtk4::Box>,
    /// Set while the host itself changes a value, so no event echoes back.
    quiet: Rc<Cell<bool>>,
}

/// A view's widget tree, indexed by node id.
pub struct Tree {
    pub root: gtk4::Box,
    nodes: RefCell<HashMap<String, Node>>,
    on_event: OnEvent,
    plugin_dir: std::path::PathBuf,
}

impl Tree {
    pub fn new(plugin_dir: &std::path::Path, on_event: OnEvent) -> Rc<Self> {
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        root.add_css_class("plugin-view");
        Rc::new(Tree { root, nodes: RefCell::default(), on_event, plugin_dir: plugin_dir.to_path_buf() })
    }

    /// Replace everything with `model` (checked by the host API).
    pub fn set_model(self: &Rc<Self>, model: &Value) {
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        self.nodes.borrow_mut().clear();
        let widget = self.build(model);
        self.root.append(&widget);
    }

    /// Apply `ui.patch` ops in order. Returns an error naming the first op
    /// that refers to a node the view does not have.
    pub fn patch(self: &Rc<Self>, ops: &[Value]) -> Result<(), String> {
        for (index, op) in ops.iter().enumerate() {
            let id = op["id"].as_str().unwrap_or_default();
            let Some(node) = self.nodes.borrow().get(id).cloned() else {
                return Err(format!("ops[{index}]: no node `{id}` in this view"));
            };
            match op["op"].as_str().unwrap_or_default() {
                "set" => {
                    let props = op["props"].as_object().cloned().unwrap_or_default();
                    crate::plugin_host::ui_model::check_props(&node.kind, &props, id, true).map_err(|e| format!("ops[{index}]: {}", e.reason()))?;
                    self.apply(&node, &props);
                }
                "remove" => {
                    self.forget(&node.widget);
                    if let Some(parent) = node.widget.parent().and_downcast::<gtk4::Box>() {
                        parent.remove(&node.widget);
                    }
                }
                "append" => {
                    let Some(container) = node.container.clone() else {
                        return Err(format!("ops[{index}]: `{id}` ({}) cannot have children", node.kind));
                    };
                    self.check_new_ids(&op["node"], None).map_err(|e| format!("ops[{index}]: {e}"))?;
                    let child = self.build(&op["node"]);
                    container.append(&child);
                }
                "replace" => {
                    self.check_new_ids(&op["node"], Some(&node.widget)).map_err(|e| format!("ops[{index}]: {e}"))?;
                    let parent = node.widget.parent().and_downcast::<gtk4::Box>();
                    let previous = node.widget.prev_sibling();
                    self.forget(&node.widget);
                    let replacement = self.build(&op["node"]);
                    match parent {
                        Some(parent) => {
                            parent.insert_child_after(&replacement, previous.as_ref());
                            parent.remove(&node.widget);
                        }
                        None => {}
                    }
                }
                other => return Err(format!("ops[{index}]: unknown op `{other}`")),
            }
        }
        Ok(())
    }

    /// Ids in `node` must be new, except those under `replacing`.
    fn check_new_ids(&self, node: &Value, replacing: Option<&gtk4::Widget>) -> Result<(), String> {
        let incoming = crate::plugin_host::ui_model::check_tree(node).map_err(|e| e.reason())?;
        let nodes = self.nodes.borrow();
        for id in incoming {
            if let Some(existing) = nodes.get(&id) {
                let inside = replacing.is_some_and(|r| existing.widget == *r || existing.widget.is_ancestor(r));
                if !inside {
                    return Err(format!("id `{id}` already exists in the view"));
                }
            }
        }
        Ok(())
    }

    /// Every node's current state, for `super-desktop plugin views`.
    pub fn snapshot(&self) -> Value {
        let nodes = self.nodes.borrow();
        let mut out = Map::new();
        for (id, node) in nodes.iter() {
            let mut state = json!({"type": node.kind, "visible": node.widget.is_visible(), "enabled": node.widget.is_sensitive()});
            if let Some(label) = &node.label {
                state["text"] = json!(label.text().to_string());
            }
            if let Some(button) = &node.button {
                state["label"] = json!(button.label().map(|l| l.to_string()));
            }
            if let Some(check) = &node.check {
                state["value"] = json!(check.is_active());
                state["label"] = json!(check.label().map(|l| l.to_string()));
            }
            if let Some(switch) = &node.switch {
                state["value"] = json!(switch.is_active());
            }
            if let Some(entry) = &node.entry {
                state["value"] = json!(entry.text().to_string());
            }
            if let Some(text) = &node.text {
                let buffer = text.buffer();
                state["value"] = json!(buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string());
            }
            if let Some((dropdown, values)) = &node.dropdown {
                state["value"] = json!(values.borrow().get(dropdown.selected() as usize));
            }
            if let Some((bar, _)) = &node.progress {
                state["value"] = json!(bar.fraction());
            }
            out.insert(id.clone(), state);
        }
        Value::Object(out)
    }

    /// Operate a node's real widget as a user would (`super-desktop plugin
    /// interact`): the plugin gets exactly the events a person would cause.
    pub fn interact(&self, id: &str, event: &str, value: &Value) -> Result<(), String> {
        let node = self.nodes.borrow().get(id).cloned().ok_or_else(|| format!("no node `{id}` in this view"))?;
        // What a person cannot reach, an agent cannot either.
        let mut widget = Some(node.widget.clone());
        while let Some(current) = widget {
            if !current.is_visible() {
                return Err(format!("`{id}` is hidden"));
            }
            if current == self.root.clone().upcast::<gtk4::Widget>() {
                break;
            }
            widget = current.parent();
        }
        if !node.widget.is_sensitive() {
            return Err(format!("`{id}` is disabled"));
        }
        match (node.kind.as_str(), event) {
            ("button", "click") => node.button.as_ref().expect("button").emit_clicked(),
            ("checkbox", "change") => node.check.as_ref().expect("checkbox").set_active(value.as_bool().ok_or("value must be true or false")?),
            ("toggle", "change") => node.switch.as_ref().expect("toggle").set_active(value.as_bool().ok_or("value must be true or false")?),
            ("entry", "change") => node.entry.as_ref().expect("entry").set_text(value.as_str().ok_or("value must be text")?),
            ("entry", "submit") => {
                let entry = node.entry.as_ref().expect("entry");
                if let Some(text) = value.as_str() {
                    entry.set_text(text);
                }
                entry.emit_activate();
            }
            ("textArea", "change") => node.text.as_ref().expect("textArea").buffer().set_text(value.as_str().ok_or("value must be text")?),
            ("select", "change") => {
                let (dropdown, values) = node.dropdown.as_ref().expect("select");
                let wanted = value.as_str().ok_or("value must be one of the option values")?;
                let index = values.borrow().iter().position(|v| v == wanted).ok_or_else(|| format!("`{wanted}` is not an option"))?;
                dropdown.set_selected(index as u32);
            }
            (kind, event) => return Err(format!("a {kind} has no `{event}` event")),
        }
        Ok(())
    }

    /// Drop the index entries of `widget` and everything under it.
    fn forget(&self, widget: &gtk4::Widget) {
        self.nodes.borrow_mut().retain(|_, node| !(node.widget == *widget || node.widget.is_ancestor(widget)));
    }

    fn emit(&self, id: &str, event: &str, value: Value) {
        (self.on_event)(id, event, value);
    }

    fn build(self: &Rc<Self>, model: &Value) -> gtk4::Widget {
        let kind = model["type"].as_str().unwrap_or("label").to_string();
        let id = model["id"].as_str().unwrap_or_default().to_string();
        let props = model.as_object().cloned().unwrap_or_default();
        let quiet = Rc::new(Cell::new(false));
        let mut node = Node {
            kind: kind.clone(),
            widget: gtk4::Box::new(gtk4::Orientation::Horizontal, 0).upcast(),
            container: None,
            label: None,
            button: None,
            check: None,
            switch: None,
            entry: None,
            text: None,
            dropdown: None,
            progress: None,
            image: None,
            scroll: None,
            markdown_holder: None,
            quiet: Rc::clone(&quiet),
        };
        let weak = Rc::downgrade(self);
        let emitter = move |id: String, event: &'static str| {
            let weak = weak.clone();
            move |value: Value| {
                if let Some(tree) = weak.upgrade() {
                    tree.emit(&id, event, value);
                }
            }
        };
        match kind.as_str() {
            "column" | "row" | "list" => {
                let orientation = if kind == "row" { gtk4::Orientation::Horizontal } else { gtk4::Orientation::Vertical };
                let gap = props.get("gap").and_then(Value::as_i64).unwrap_or(if kind == "row" { 8 } else { 6 }) as i32;
                let container = gtk4::Box::new(orientation, gap);
                container.add_css_class(&format!("plugin-{kind}"));
                if kind == "row" {
                    container.set_valign(gtk4::Align::Center);
                }
                node.widget = container.clone().upcast();
                node.container = Some(container);
            }
            "scroll" => {
                let container = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
                let scroll = gtk4::ScrolledWindow::builder().hscrollbar_policy(gtk4::PolicyType::Never).propagate_natural_height(true).child(&container).build();
                node.widget = scroll.clone().upcast();
                node.container = Some(container);
                node.scroll = Some(scroll);
            }
            "label" | "code" | "badge" => {
                let label = gtk4::Label::new(None);
                label.set_xalign(0.0);
                label.set_selectable(kind == "code");
                match kind.as_str() {
                    "code" => {
                        label.add_css_class("plugin-code");
                        label.set_wrap(true);
                        label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
                    }
                    "badge" => {
                        label.add_css_class("plugin-badge");
                        label.set_valign(gtk4::Align::Center);
                    }
                    _ => label.set_ellipsize(gtk4::pango::EllipsizeMode::End),
                }
                node.widget = label.clone().upcast();
                node.label = Some(label);
            }
            "markdown" => {
                let holder = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
                node.widget = holder.clone().upcast();
                node.markdown_holder = Some(holder);
            }
            "icon" => {
                let holder = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
                node.widget = holder.clone().upcast();
                node.container = None;
                node.markdown_holder = Some(holder);
            }
            "button" => {
                let button = gtk4::Button::new();
                button.add_css_class("plugin-button");
                let send = emitter(id.clone(), "click");
                button.connect_clicked(move |_| send(Value::Null));
                node.widget = button.clone().upcast();
                node.button = Some(button);
            }
            "checkbox" => {
                let check = gtk4::CheckButton::new();
                let send = emitter(id.clone(), "change");
                let quiet = Rc::clone(&quiet);
                check.connect_toggled(move |check| {
                    if !quiet.get() {
                        send(json!(check.is_active()));
                    }
                });
                node.widget = check.clone().upcast();
                node.check = Some(check);
            }
            "toggle" => {
                let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                let switch = gtk4::Switch::new();
                switch.set_valign(gtk4::Align::Center);
                let label = gtk4::Label::new(None);
                row.append(&switch);
                row.append(&label);
                let send = emitter(id.clone(), "change");
                let quiet = Rc::clone(&quiet);
                switch.connect_active_notify(move |switch| {
                    if !quiet.get() {
                        send(json!(switch.is_active()));
                    }
                });
                node.widget = row.upcast();
                node.switch = Some(switch);
                node.label = Some(label);
            }
            "entry" => {
                let entry = gtk4::Entry::new();
                entry.set_hexpand(true);
                let change = debounced(emitter(id.clone(), "change"));
                let quiet_change = Rc::clone(&quiet);
                entry.connect_changed(move |entry| {
                    if !quiet_change.get() {
                        change(json!(entry.text().to_string()));
                    }
                });
                let submit = emitter(id.clone(), "submit");
                entry.connect_activate(move |entry| submit(json!(entry.text().to_string())));
                node.widget = entry.clone().upcast();
                node.entry = Some(entry);
            }
            "textArea" => {
                let text = gtk4::TextView::new();
                text.set_wrap_mode(gtk4::WrapMode::WordChar);
                text.add_css_class("plugin-textarea");
                let frame = gtk4::Frame::new(None);
                frame.set_child(Some(&text));
                let change = debounced(emitter(id.clone(), "change"));
                let quiet_change = Rc::clone(&quiet);
                text.buffer().connect_changed(move |buffer| {
                    if !quiet_change.get() {
                        change(json!(buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string()));
                    }
                });
                node.widget = frame.upcast();
                node.text = Some(text);
            }
            "select" => {
                let values: Rc<RefCell<Vec<String>>> = Rc::default();
                let dropdown = gtk4::DropDown::from_strings(&[]);
                let send = emitter(id.clone(), "change");
                let quiet_change = Rc::clone(&quiet);
                let list = Rc::clone(&values);
                dropdown.connect_selected_notify(move |dropdown| {
                    if quiet_change.get() {
                        return;
                    }
                    if let Some(value) = list.borrow().get(dropdown.selected() as usize) {
                        send(json!(value));
                    }
                });
                node.widget = dropdown.clone().upcast();
                node.dropdown = Some((dropdown, values));
            }
            "progress" => {
                let bar = gtk4::ProgressBar::new();
                bar.set_valign(gtk4::Align::Center);
                node.widget = bar.clone().upcast();
                node.progress = Some((bar, Rc::new(Cell::new(None))));
            }
            "spinner" => {
                let spinner = gtk4::Spinner::new();
                spinner.start();
                node.widget = spinner.upcast();
            }
            _ => {
                let separator = gtk4::Separator::new(gtk4::Orientation::Horizontal);
                node.widget = separator.upcast();
            }
        }
        self.nodes.borrow_mut().insert(id, node.clone());
        self.apply(&node, &props);
        if let (Some(container), Some(children)) = (&node.container, props.get("children").and_then(Value::as_array)) {
            for child in children {
                let widget = self.build(child);
                container.append(&widget);
            }
        }
        node.widget
    }

    /// Apply `props` (already checked for this node type).
    fn apply(&self, node: &Node, props: &Map<String, Value>) {
        node.quiet.set(true);
        let text = |key: &str| props.get(key).and_then(Value::as_str);
        if let Some(visible) = props.get("visible").and_then(Value::as_bool) {
            node.widget.set_visible(visible);
        }
        if let Some(enabled) = props.get("enabled").and_then(Value::as_bool) {
            node.widget.set_sensitive(enabled);
        }
        match node.kind.as_str() {
            "column" | "row" | "list" => {
                if let (Some(gap), Some(container)) = (props.get("gap").and_then(Value::as_i64), &node.container) {
                    container.set_spacing(gap as i32);
                }
            }
            "scroll" => {
                if let (Some(height), Some(scroll)) = (props.get("maxHeight").and_then(Value::as_i64), &node.scroll) {
                    scroll.set_max_content_height(height as i32);
                }
            }
            "label" | "code" | "badge" => {
                let label = node.label.as_ref().expect("label node");
                if let Some(t) = text("text") {
                    label.set_text(t);
                }
                if let Some(wrap) = props.get("wrap").and_then(Value::as_bool) {
                    label.set_wrap(wrap);
                    label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
                    label.set_ellipsize(if wrap { gtk4::pango::EllipsizeMode::None } else { gtk4::pango::EllipsizeMode::End });
                }
                for (key, prefix) in [("style", "plugin-text-"), ("tone", "plugin-tone-")] {
                    if let Some(value) = text(key) {
                        for class in label.css_classes() {
                            if class.starts_with(prefix) {
                                label.remove_css_class(&class);
                            }
                        }
                        label.add_css_class(&format!("{prefix}{value}"));
                    }
                }
            }
            "markdown" => {
                if let (Some(t), Some(holder)) = (text("text"), &node.markdown_holder) {
                    while let Some(child) = holder.first_child() {
                        holder.remove(&child);
                    }
                    holder.append(&crate::markdown_view::view(t));
                }
            }
            "icon" => {
                if let (Some(name), Some(holder)) = (text("name"), &node.markdown_holder) {
                    while let Some(child) = holder.first_child() {
                        holder.remove(&child);
                    }
                    let size = props.get("size").and_then(Value::as_i64).unwrap_or(16) as i32;
                    holder.append(&icon_widget(&self.plugin_dir, name, size));
                }
            }
            "button" => {
                let button = node.button.as_ref().expect("button node");
                if let Some(label) = text("label") {
                    let shown = match text("icon") {
                        Some(icon) if !is_image(icon) => format!("{icon} {label}"),
                        _ => label.to_string(),
                    };
                    button.set_label(&shown);
                }
                if let Some(tone) = text("tone") {
                    for class in ["plugin-button-primary", "plugin-button-danger", "suggested-action", "destructive-action"] {
                        button.remove_css_class(class);
                    }
                    match tone {
                        "primary" => button.add_css_class("plugin-button-primary"),
                        "danger" => button.add_css_class("plugin-button-danger"),
                        _ => {}
                    }
                }
            }
            "checkbox" => {
                let check = node.check.as_ref().expect("checkbox node");
                if let Some(label) = text("label") {
                    check.set_label(Some(label));
                }
                if let Some(value) = props.get("value").and_then(Value::as_bool) {
                    check.set_active(value);
                }
            }
            "toggle" => {
                if let (Some(label), Some(widget)) = (text("label"), &node.label) {
                    widget.set_text(label);
                }
                if let (Some(value), Some(switch)) = (props.get("value").and_then(Value::as_bool), &node.switch) {
                    switch.set_active(value);
                }
            }
            "entry" => {
                let entry = node.entry.as_ref().expect("entry node");
                if let Some(placeholder) = text("placeholder") {
                    entry.set_placeholder_text(Some(placeholder));
                }
                // Never overwrite what the user is typing.
                if let Some(value) = text("value") {
                    if !entry.has_focus() && entry.text() != value {
                        entry.set_text(value);
                    }
                }
            }
            "textArea" => {
                let view = node.text.as_ref().expect("textArea node");
                if let Some(rows) = props.get("rows").and_then(Value::as_i64) {
                    view.set_size_request(-1, rows as i32 * 18 + 12);
                }
                if let Some(value) = text("value") {
                    let buffer = view.buffer();
                    let current = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
                    if !view.has_focus() && current != value {
                        buffer.set_text(value);
                    }
                }
            }
            "select" => {
                let (dropdown, values) = node.dropdown.as_ref().expect("select node");
                if let Some(options) = props.get("options").and_then(Value::as_array) {
                    let labels: Vec<&str> = options.iter().filter_map(|o| o["label"].as_str()).collect();
                    dropdown.set_model(Some(&gtk4::StringList::new(&labels)));
                    *values.borrow_mut() = options.iter().filter_map(|o| o["value"].as_str().map(str::to_string)).collect();
                }
                if let Some(value) = text("value") {
                    if let Some(index) = values.borrow().iter().position(|v| v == value) {
                        dropdown.set_selected(index as u32);
                    }
                }
            }
            "progress" => {
                let (bar, pulse) = node.progress.as_ref().expect("progress node");
                if props.contains_key("value") {
                    if let Some(source) = pulse.take() {
                        source.remove();
                    }
                    match props.get("value").and_then(Value::as_f64) {
                        Some(fraction) => bar.set_fraction(fraction),
                        None => {
                            // Indeterminate: pulse while the bar is on screen.
                            let weak = bar.downgrade();
                            let source = glib::timeout_add_local(Duration::from_millis(120), move || match weak.upgrade() {
                                Some(bar) => {
                                    if bar.is_mapped() {
                                        bar.pulse();
                                    }
                                    glib::ControlFlow::Continue
                                }
                                None => glib::ControlFlow::Break,
                            });
                            pulse.set(Some(source));
                        }
                    }
                }
            }
            _ => {}
        }
        node.quiet.set(false);
    }
}

/// Events at most every 300 ms after the last change (entry, textArea).
fn debounced(send: impl Fn(Value) + 'static) -> impl Fn(Value) {
    let send = Rc::new(send);
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    move |value: Value| {
        if let Some(source) = pending.borrow_mut().take() {
            source.remove();
        }
        let send = Rc::clone(&send);
        let slot = Rc::clone(&pending);
        let source = glib::timeout_add_local_once(Duration::from_millis(300), move || {
            slot.borrow_mut().take();
            send(value);
        });
        *pending.borrow_mut() = Some(source);
    }
}

pub fn is_image(icon: &str) -> bool {
    icon.ends_with(".svg") || icon.ends_with(".png")
}

/// An emoji label, or a plugin-relative image (already validated in the manifest).
pub fn icon_widget(dir: &std::path::Path, icon: &str, size: i32) -> gtk4::Widget {
    if is_image(icon) && crate::plugin_host::manifest::is_relative_path(icon) {
        let image = gtk4::Image::from_file(dir.join(icon));
        image.set_pixel_size(size);
        image.upcast()
    } else {
        gtk4::Label::new(Some(icon)).upcast()
    }
}

/// The panel chrome around a view: the same card header Files & links uses,
/// dragged by its header, closed by ✕.
pub struct Chrome {
    pub widget: gtk4::Box,
    pub close: gtk4::Button,
}

pub fn chrome(plugin_name: &str, title: &str, tree: &Tree) -> Chrome {
    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    outer.add_css_class("mini-terminal");
    outer.add_css_class("harness-panel");
    outer.add_css_class("plugin-panel");
    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    header.add_css_class("term-header");
    let badge = gtk4::Label::new(Some("🧩"));
    badge.add_css_class("launcher-head-badge");
    badge.set_valign(gtk4::Align::Center);
    header.append(&badge);
    let titles = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    titles.set_hexpand(true);
    titles.set_valign(gtk4::Align::Center);
    let heading = gtk4::Label::new(Some(title));
    heading.add_css_class("term-title");
    heading.set_halign(gtk4::Align::Start);
    // Says whose panel this is, so a plugin cannot pass for SUPER DESKTOP.
    let subtitle = gtk4::Label::new(Some(&format!("Plugin · {plugin_name}")));
    subtitle.add_css_class("launcher-subtitle");
    subtitle.set_halign(gtk4::Align::Start);
    titles.append(&heading);
    titles.append(&subtitle);
    header.append(&titles);
    let close = gtk4::Button::with_label("✕");
    close.set_tooltip_text(Some("Close"));
    close.add_css_class("term-btn");
    close.set_valign(gtk4::Align::Center);
    header.append(&close);
    outer.append(&header);
    let scroll = gtk4::ScrolledWindow::builder().vexpand(true).hexpand(true).hscrollbar_policy(gtk4::PolicyType::Never).build();
    tree.root.set_margin_top(12);
    tree.root.set_margin_bottom(12);
    tree.root.set_margin_start(12);
    tree.root.set_margin_end(12);
    scroll.set_child(Some(&tree.root));
    outer.append(&scroll);
    Chrome { widget: outer, close }
}

/// What a view shows until the plugin fills it (`view.opened`).
pub fn placeholder() -> Value {
    json!({"type": "column", "id": "root", "children": [{"type": "spinner", "id": "root-spinner"}]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_view_builds_patches_and_reports() {
        crate::gtk_test::run_in_child_process("plugin_ui::view::tests::plugin_view_inner");
    }

    #[test]
    fn plugin_view_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let events: Rc<RefCell<Vec<(String, String, Value)>>> = Rc::default();
        let sink = Rc::clone(&events);
        let tree = Tree::new(std::path::Path::new("/tmp"), Rc::new(move |id, event, value| sink.borrow_mut().push((id.into(), event.into(), value))));
        tree.set_model(&json!({"type": "column", "id": "root", "children": [
            {"type": "label", "id": "status", "text": "idle", "style": "muted"},
            {"type": "checkbox", "id": "pick", "label": "Repo", "value": true},
            {"type": "button", "id": "go", "label": "Go", "tone": "primary"},
            {"type": "list", "id": "rows", "children": []},
            {"type": "select", "id": "mode", "options": [{"value": "a", "label": "A"}, {"value": "b", "label": "B"}], "value": "b"},
            {"type": "progress", "id": "p", "value": 0.5}
        ]}));
        let nodes = tree.nodes.borrow().clone();
        assert_eq!(nodes.len(), 7);
        // Clicks and changes reach the plugin; host-made changes do not echo.
        nodes["go"].button.as_ref().unwrap().emit_clicked();
        nodes["pick"].check.as_ref().unwrap().set_active(false);
        nodes["mode"].dropdown.as_ref().unwrap().0.set_selected(0);
        assert_eq!(
            *events.borrow(),
            vec![("go".into(), "click".into(), Value::Null), ("pick".into(), "change".into(), json!(false)), ("mode".into(), "change".into(), json!("a"))]
        );
        events.borrow_mut().clear();
        tree.patch(&[
            json!({"op": "set", "id": "status", "props": {"text": "done", "style": "success"}}),
            json!({"op": "set", "id": "pick", "props": {"value": true}}),
            json!({"op": "append", "id": "rows", "node": {"type": "label", "id": "row-1", "text": "one"}}),
            json!({"op": "replace", "id": "go", "node": {"type": "button", "id": "go", "label": "Again"}}),
        ])
        .unwrap();
        assert!(events.borrow().is_empty(), "host changes must not echo as events");
        let nodes = tree.nodes.borrow().clone();
        assert_eq!(nodes["status"].label.as_ref().unwrap().text(), "done");
        assert!(nodes["status"].label.as_ref().unwrap().has_css_class("plugin-text-success"));
        assert!(nodes.contains_key("row-1"));
        assert_eq!(nodes["go"].button.as_ref().unwrap().label().unwrap(), "Again");
        // The replaced button stays where it was, before the list.
        assert_eq!(nodes["go"].widget.next_sibling().unwrap(), nodes["rows"].widget);
        // Errors name the op; nothing half-applied after them.
        assert!(tree.patch(&[json!({"op": "set", "id": "missing", "props": {}})]).unwrap_err().contains("no node `missing`"));
        assert!(tree.patch(&[json!({"op": "append", "id": "rows", "node": {"type": "label", "id": "status", "text": "dup"}})]).unwrap_err().contains("already exists"));
        assert!(tree.patch(&[json!({"op": "append", "id": "status", "node": {"type": "spinner", "id": "s"}})]).unwrap_err().contains("cannot have children"));
        tree.patch(&[json!({"op": "set", "id": "go", "props": {"visible": false}})]).expect("a partial set needs no label");
        assert!(tree.interact("go", "click", &Value::Null).unwrap_err().contains("hidden"));
        tree.patch(&[json!({"op": "set", "id": "go", "props": {"visible": true, "enabled": false}})]).unwrap();
        assert!(tree.interact("go", "click", &Value::Null).unwrap_err().contains("disabled"));
        tree.patch(&[json!({"op": "remove", "id": "rows"})]).unwrap();
        assert!(!tree.nodes.borrow().contains_key("row-1"), "removing a container forgets its children");
    }
}
