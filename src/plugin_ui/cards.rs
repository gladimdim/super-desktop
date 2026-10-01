//! Plugins on terminal cards: header buttons, a replacement control set,
//! title text and chips, and the `workspace.cards` / `card.*` / `terminal.*`
//! / `harness.launch` / `title.*` host methods.
//!
//! Card actions go through the window's `apply_workspace_command`, the path
//! remote PCs use, so a plugin moves, iconifies or closes a card with the same
//! code (and the same saving) as the card's own buttons. Plugin titles are
//! drawn on this PC only (`MiniTerminalCard::set_plugin_title`); the card
//! keeps publishing its own title.
use super::{manager, Manager, State};
use crate::desktop_protocol::{CardLayout, WorkspaceCommand};
use crate::mini_terminal::MiniTerminalCard;
use crate::plugin_host::manifest::{CardButton, Manifest};
use crate::plugin_host::rpc::{self, RpcError};
use gtk4::glib;
use gtk4::prelude::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

/// What the plugin manager needs from the local workspace. Implemented by
/// the window.
pub trait Workspace {
    /// This PC's cards (never another PC's).
    fn cards(&self) -> Vec<Rc<MiniTerminalCard>>;
    /// Screen width, height, and the bottom edge of the top bar.
    fn screen(&self) -> (i32, i32, i32);
    /// A workspace command, applied like a remote PC's.
    fn command(&self, command: &WorkspaceCommand) -> Result<Option<String>, &'static str>;
    /// The canvas the cards are drawn on.
    fn canvas(&self) -> Option<gtk4::Fixed>;
    /// Whether the show/hide slide is moving the cards.
    fn sliding(&self) -> bool;
    /// Recompute the outlines of cards other cards cover.
    fn refresh_ghosts(&self);
}

/// One plugin's contribution to a card's title.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TitleState {
    pub text: Option<String>,
    pub before: Vec<Value>,
    pub after: Vec<Value>,
}

impl Manager {
    pub fn set_workspace(&self, workspace: Rc<dyn Workspace>) {
        self.workspace.replace(Some(workspace));
    }

    fn workspace(&self) -> Result<Rc<dyn Workspace>, RpcError> {
        self.workspace
            .borrow()
            .clone()
            .ok_or_else(|| RpcError::new(rpc::UNAVAILABLE, "SUPER DESKTOP is not shown yet", "Try again once it has been shown.", "references/host-api.md#errors"))
    }

    fn card(&self, id: &str) -> Result<Rc<MiniTerminalCard>, RpcError> {
        self.workspace()?.cards().into_iter().find(|c| c.data.borrow().id == id).ok_or_else(|| {
            RpcError::new(rpc::NOT_FOUND, format!("no card `{id}` on this PC"), "Cards come and go: read workspace.cards again.", "references/host-api.md#workspacecards")
        })
    }

    /// Active plugins in a stable order, with their manifests.
    fn active(&self) -> Vec<(String, Arc<Manifest>)> {
        self.plugins.borrow().iter().filter(|(_, p)| !matches!(p.state, State::Failed(_))).map(|(id, p)| (id.clone(), Arc::clone(&p.manifest))).collect()
    }

    /// Redraw every card's plugin chrome (after a plugin was turned on or off).
    pub fn decorate_all(self: &Rc<Self>) {
        let Some(workspace) = self.workspace.borrow().clone() else { return };
        let cards = workspace.cards();
        let ids: Vec<String> = cards.iter().map(|c| c.data.borrow().id.clone()).collect();
        // Titles of cards that are gone are forgotten.
        self.titles.borrow_mut().retain(|card, _| ids.contains(card));
        for card in cards {
            self.decorate(&card);
        }
    }

    /// A card's plugin buttons, controls and title from the active plugins.
    pub fn decorate(self: &Rc<Self>, card: &Rc<MiniTerminalCard>) {
        if card.is_remote() {
            return;
        }
        let chrome = card.plugin_chrome.clone();
        for container in [&chrome.header_buttons, &chrome.plugin_controls, &chrome.compact_buttons, &chrome.compact_plugin_controls] {
            while let Some(child) = container.first_child() {
                container.remove(&child);
            }
        }
        let active = self.active();
        let (agent, status) = {
            let data = card.data.borrow();
            (data.agent_type.clone(), card.title_inputs().2)
        };
        for (plugin, manifest) in &active {
            for button in &manifest.contributes.card_buttons {
                if !shows_on(button, &agent, &status) {
                    continue;
                }
                if button.when.as_ref().and_then(|w| w.iconified) != Some(true) {
                    chrome.header_buttons.append(&self.card_button(card, plugin, manifest, &button.id, &button.icon, &button.tooltip, &button.command, "cardButton"));
                }
                if button.show_in_icon && button.when.as_ref().and_then(|w| w.iconified) != Some(false) {
                    let widget = self.card_button(card, plugin, manifest, &button.id, &button.icon, &button.tooltip, &button.command, "cardButton");
                    widget.add_css_class("term-compact-btn");
                    chrome.compact_buttons.append(&widget);
                }
            }
        }
        // At most one control set (activation refuses a second provider).
        let controls = active.iter().find_map(|(plugin, m)| m.contributes.card_controls.clone().map(|c| (plugin.clone(), Arc::clone(m), c)));
        match &controls {
            Some((plugin, manifest, set)) => {
                for control in &set.controls {
                    chrome.plugin_controls.append(&self.control(card, plugin, manifest, control, false));
                }
                if let Some(icon_controls) = &set.icon_controls {
                    for control in icon_controls {
                        let widget = self.control(card, plugin, manifest, control, true);
                        widget.add_css_class("term-compact-btn");
                        chrome.compact_plugin_controls.append(&widget);
                    }
                }
            }
            None => {}
        }
        let has_icon_controls = controls.as_ref().is_some_and(|(_, _, set)| set.icon_controls.is_some());
        chrome.builtin_controls.set_visible(controls.is_none());
        chrome.plugin_controls.set_visible(controls.is_some());
        chrome.compact_builtin.set_visible(!has_icon_controls);
        chrome.compact_plugin_controls.set_visible(has_icon_controls);
        chrome.header_buttons.set_visible(chrome.header_buttons.first_child().is_some());
        chrome.compact_buttons.set_visible(chrome.compact_buttons.first_child().is_some());
        card.refit_compact();
        self.paint_title(card);
        // A new or changed card: a window renderer draws it on the next frame.
        self.renderer_wake();
    }

    #[allow(clippy::too_many_arguments)]
    fn card_button(&self, card: &Rc<MiniTerminalCard>, plugin: &str, manifest: &Manifest, id: &str, icon: &str, tooltip: &str, command: &str, source: &'static str) -> gtk4::Button {
        let button = gtk4::Button::new();
        // `plugin press` finds it by its contribution id.
        button.set_widget_name(id);
        button.set_child(Some(&super::view::icon_widget(&manifest_dir(self, plugin), icon, 14)));
        // Says whose button it is.
        button.set_tooltip_text(Some(&format!("{tooltip} ({})", manifest.name)));
        button.add_css_class("term-btn");
        button.add_css_class("plugin-card-btn");
        let (plugin, command, card) = (plugin.to_string(), command.to_string(), Rc::downgrade(card));
        button.connect_clicked(move |_| {
            let Some(card) = card.upgrade() else { return };
            let manager = manager();
            let context = json!({"source": source, "card": manager.card_info(&card)});
            let _ = manager.run_command(&plugin, &command, context);
        });
        button
    }

    /// One entry of a control set: a built-in action, or the plugin's command.
    fn control(&self, card: &Rc<MiniTerminalCard>, plugin: &str, manifest: &Manifest, control: &Value, icon_form: bool) -> gtk4::Button {
        if let Some(builtin) = control.as_str() {
            let (label, tooltip) = match builtin {
                "builtin:iconify" => ("🗕", "Iconify to 128×128"),
                "builtin:restore" => ("🗖", "Restore"),
                "builtin:expand" => ("⛶", "Expand or collapse"),
                _ => ("✕", "Kill Session"),
            };
            let button = gtk4::Button::with_label(label);
            button.set_widget_name(builtin);
            button.set_tooltip_text(Some(tooltip));
            button.add_css_class("term-btn");
            let (card, builtin) = (Rc::downgrade(card), builtin.to_string());
            button.connect_clicked(move |_| {
                if let Some(card) = card.upgrade() {
                    let _ = manager().builtin(&card, &builtin);
                }
            });
            let _ = icon_form;
            return button;
        }
        let icon = control["icon"].as_str().unwrap_or("•");
        let tooltip = control["tooltip"].as_str().unwrap_or("");
        let command = control["command"].as_str().unwrap_or("");
        let id = control["id"].as_str().unwrap_or("");
        self.card_button(card, plugin, manifest, id, icon, tooltip, command, "cardControl")
    }

    /// The built-in actions, exactly as the card's own buttons run them.
    fn builtin(&self, card: &Rc<MiniTerminalCard>, action: &str) -> Result<(), &'static str> {
        let workspace = self.workspace.borrow().clone().ok_or("no workspace")?;
        let id = card.data.borrow().id.clone();
        match action {
            "builtin:iconify" => {
                card.set_iconified(true);
                Ok(())
            }
            "builtin:restore" => {
                card.set_iconified(false);
                Ok(())
            }
            "builtin:expand" => workspace.command(&WorkspaceCommand::SetExpanded { card_id: id, expected_revision: 0, expanded: !card.is_expanded() }).map(|_| ()),
            _ => workspace.command(&WorkspaceCommand::CloseTerminal { card_id: id, expected_revision: 0 }).map(|_| ()),
        }
    }

    /// Draw a card's title from the plugins' title states: chips combine in
    /// plugin order; the first plugin that set text wins.
    fn paint_title(&self, card: &Rc<MiniTerminalCard>) {
        let id = card.data.borrow().id.clone();
        let states = self.titles.borrow().get(&id).cloned().unwrap_or_default();
        let active: Vec<String> = self.active().into_iter().map(|(p, _)| p).collect();
        let ordered: Vec<&TitleState> = active.iter().filter_map(|p| states.get(p)).collect();
        let text = ordered.iter().find_map(|s| s.text.clone());
        card.set_plugin_title(text.as_deref());
        let chrome = &card.plugin_chrome;
        for (container, chips) in [
            (&chrome.chips_before, ordered.iter().flat_map(|s| s.before.iter()).collect::<Vec<_>>()),
            (&chrome.chips_after, ordered.iter().flat_map(|s| s.after.iter()).collect::<Vec<_>>()),
        ] {
            while let Some(child) = container.first_child() {
                container.remove(&child);
            }
            for chip in &chips {
                let label = gtk4::Label::new(chip["text"].as_str());
                label.add_css_class("plugin-chip");
                label.add_css_class(&format!("plugin-tone-{}", chip["tone"].as_str().unwrap_or("neutral")));
                if let Some(tooltip) = chip["tooltip"].as_str() {
                    label.set_tooltip_text(Some(tooltip));
                }
                container.append(&label);
            }
            container.set_visible(!chips.is_empty());
        }
    }

    pub fn card_info(&self, card: &MiniTerminalCard) -> Value {
        let data = card.data.borrow();
        let (_, prompt, status) = card.title_inputs();
        let focused = card.container.root().and_then(|r| gtk4::prelude::RootExt::focus(&r)).is_some_and(|f| f.is_ancestor(&card.container));
        json!({
            "id": data.id,
            "session": data.session_name,
            "agent": data.agent_type,
            "folder": data.workspace_dir.clone().unwrap_or_default(),
            "status": status_name(&status),
            "prompt": prompt,
            "rect": {"x": data.x, "y": data.y, "w": data.width, "h": data.height},
            "icon": {"x": data.icon_x.unwrap_or(data.x), "y": data.icon_y.unwrap_or(data.y)},
            "iconified": data.iconified,
            "expanded": card.is_expanded(),
            "focused": focused,
            "local": true,
        })
    }

    /// The `title.inputs` of one card.
    fn title_input(&self, card: &MiniTerminalCard) -> Value {
        let data = card.data.borrow();
        let (prefix, prompt, status) = card.title_inputs();
        json!({
            "id": data.id,
            "agent": data.agent_type,
            "folder": data.workspace_dir.clone().unwrap_or_default(),
            "status": status_name(&status),
            "prefix": prefix,
            "prompt": prompt,
            "local": true,
        })
    }

    /// Send `title.inputs` for `cards` to every running plugin that sets titles.
    pub(super) fn send_title_inputs(&self, plugin: Option<&str>, cards: &[Rc<MiniTerminalCard>]) {
        let inputs: Vec<Value> = cards.iter().filter(|c| !c.is_remote()).map(|c| self.title_input(c)).collect();
        if inputs.is_empty() {
            return;
        }
        for (id, p) in self.plugins.borrow().iter() {
            if p.manifest.contributes.titles.is_none() || plugin.is_some_and(|only| only != id) {
                continue;
            }
            if let Some(session) = &p.session {
                session.notify("title.inputs", json!({"cards": inputs}));
            }
        }
    }

    /// A plugin was turned off: its buttons, controls and titles go.
    pub(super) fn forget_cards_of(self: &Rc<Self>, plugin: &str) {
        for states in self.titles.borrow_mut().values_mut() {
            states.remove(plugin);
        }
        self.decorate_all();
    }

    /// This PC's cards and what plugins did to them (`plugin cards`).
    pub fn cards_snapshot(&self) -> Value {
        let Some(workspace) = self.workspace.borrow().clone() else { return json!([]) };
        let names = |container: &gtk4::Box| {
            let mut out = Vec::new();
            let mut child = container.first_child();
            while let Some(widget) = child {
                if widget.is_visible() {
                    out.push(Value::String(widget.widget_name().to_string()));
                }
                child = widget.next_sibling();
            }
            out
        };
        let texts = |container: &gtk4::Box| {
            let mut out = Vec::new();
            let mut child = container.first_child();
            while let Some(widget) = child {
                if let Ok(label) = widget.clone().downcast::<gtk4::Label>() {
                    out.push(Value::String(label.text().to_string()));
                }
                child = widget.next_sibling();
            }
            out
        };
        let cards: Vec<Value> = workspace
            .cards()
            .iter()
            .filter(|c| !c.is_remote())
            .map(|card| {
                let mut info = self.card_info(card);
                let chrome = &card.plugin_chrome;
                let controls = if chrome.plugin_controls.is_visible() { names(&chrome.plugin_controls) } else { vec![json!("builtin:iconify"), json!("builtin:expand"), json!("builtin:close")] };
                info["title"] = json!({"drawn": card.drawn_title(), "published": card.desktop_presentation().title, "chipsBefore": texts(&chrome.chips_before), "chipsAfter": texts(&chrome.chips_after)});
                info["buttons"] = Value::Array(names(&chrome.header_buttons));
                info["controls"] = Value::Array(controls);
                info["iconButtons"] = Value::Array(names(&chrome.compact_buttons));
                info["iconControls"] = Value::Array(if chrome.compact_plugin_controls.is_visible() { names(&chrome.compact_plugin_controls) } else { vec![json!("builtin:restore"), json!("builtin:close")] });
                // Where a window renderer draws it (null: the built-in layout).
                info["drawn"] = card.presented().map_or(Value::Null, |p| json!({"x": p.rect.x, "y": p.rect.y, "w": p.rect.width, "h": p.rect.height, "icon": p.icon}));
                info
            })
            .collect();
        Value::Array(cards)
    }

    /// Press a card's plugin button or control (or a built-in control) as a
    /// person would. Hidden ones refuse, like for a person.
    pub fn press(&self, card_id: &str, control: &str) -> Result<(), String> {
        let workspace = self.workspace.borrow().clone().ok_or("SUPER DESKTOP is not shown yet")?;
        let card = workspace.cards().into_iter().find(|c| c.data.borrow().id == card_id).ok_or_else(|| format!("no card `{card_id}` (see plugin cards)"))?;
        let chrome = card.plugin_chrome.clone();
        // Built-in controls: the card's own, unless a plugin's set replaced them.
        if control.starts_with("builtin:") && !chrome.plugin_controls.is_visible() {
            return self.builtin(&card, control).map_err(str::to_string);
        }
        let iconified = card.data.borrow().iconified;
        let places = if iconified { [&chrome.compact_buttons, &chrome.compact_plugin_controls] } else { [&chrome.header_buttons, &chrome.plugin_controls] };
        for container in places {
            if !container.is_visible() {
                continue;
            }
            let mut child = container.first_child();
            while let Some(widget) = child {
                if widget.widget_name() == control && widget.is_visible() {
                    let button = widget.downcast::<gtk4::Button>().map_err(|_| "not a button")?;
                    if !button.is_sensitive() {
                        return Err(format!("`{control}` is disabled"));
                    }
                    button.emit_clicked();
                    return Ok(());
                }
                child = widget.next_sibling();
            }
        }
        Err(format!("`{control}` is not shown on that card{}", if iconified { " (it is an icon)" } else { "" }))
    }

    /// What of `plugin` is still on cards (for the footprint check).
    pub(super) fn cards_footprint(&self, plugin: &str) -> Vec<String> {
        let mut left = Vec::new();
        if self.titles.borrow().values().any(|s| s.contains_key(plugin)) {
            left.push("card titles".into());
        }
        if self.renderer.borrow().as_ref().is_some_and(|d| d.plugin == plugin) {
            left.push("window renderer".into());
        }
        if self.returning.borrow().is_none() {
            if let Some(workspace) = self.workspace.borrow().clone() {
                if workspace.cards().iter().any(|c| c.presented().is_some()) && self.renderer.borrow().is_none() {
                    left.push("cards drawn by a renderer".into());
                }
            }
        }
        if let Some(workspace) = self.workspace.borrow().clone() {
            let tag = format!("({})", plugin_name_of(self, plugin));
            for card in workspace.cards() {
                let chrome = &card.plugin_chrome;
                for container in [&chrome.header_buttons, &chrome.plugin_controls, &chrome.compact_buttons, &chrome.compact_plugin_controls] {
                    let mut child = container.first_child();
                    while let Some(widget) = child {
                        if widget.tooltip_text().is_some_and(|t| t.ends_with(&tag)) {
                            left.push("card buttons".into());
                        }
                        child = widget.next_sibling();
                    }
                }
            }
        }
        left.dedup();
        left
    }

    /// The card and workspace host methods (already checked by `api`).
    pub(super) fn card_call(self: &Rc<Self>, plugin: &str, method: &str, params: &Value) -> Result<Value, RpcError> {
        let card_param = || params["card"].as_str().unwrap_or_default().to_string();
        match method {
            "workspace.cards" => {
                let workspace = self.workspace()?;
                let (w, h, top) = workspace.screen();
                let cards: Vec<Value> = workspace.cards().iter().filter(|c| !c.is_remote()).map(|c| self.card_info(c)).collect();
                Ok(json!({"screen": {"w": w, "h": h, "top": top}, "cards": cards}))
            }
            "card.session" => {
                let card = self.card(&card_param())?;
                let session = card.data.borrow().session_name.clone();
                Ok(json!({"session": session}))
            }
            "card.focus" => {
                let card = self.card(&card_param())?;
                card.select_with_keyboard();
                Ok(json!({}))
            }
            "card.expand" | "card.collapse" => {
                let card = self.card(&card_param())?;
                let expanded = method == "card.expand";
                self.workspace()?
                    .command(&WorkspaceCommand::SetExpanded { card_id: card.data.borrow().id.clone(), expected_revision: 0, expanded })
                    .map_err(refused)?;
                Ok(json!({}))
            }
            "card.iconify" | "card.restore" | "card.setRect" => {
                let card = self.card(&card_param())?;
                if card.is_expanded() {
                    return Err(RpcError::new(rpc::UNAVAILABLE, "the card is expanded", "Collapse it first (card.collapse).", "references/host-api.md#card"));
                }
                let mut layout = layout_of(&card);
                match method {
                    "card.iconify" => {
                        layout.iconified = true;
                        if let Some(at) = params.get("at") {
                            layout.icon_x = at["x"].as_f64().map(|v| v.round() as i32);
                            layout.icon_y = at["y"].as_f64().map(|v| v.round() as i32);
                        }
                    }
                    "card.restore" => layout.iconified = false,
                    _ => {
                        let rect = &params["rect"];
                        let number = |key: &str| rect[key].as_f64().unwrap_or(0.0).round();
                        layout.iconified = false;
                        layout.x = number("x") as i32;
                        layout.y = number("y") as i32;
                        layout.width = number("w").max(1.0) as u32;
                        layout.height = number("h").max(1.0) as u32;
                        layout.restored_width = layout.width;
                        layout.restored_height = layout.height;
                    }
                }
                let id = card.data.borrow().id.clone();
                self.workspace()?.command(&WorkspaceCommand::SetLayout { card_id: id, expected_revision: 0, layout }).map_err(refused)?;
                self.renderer_wake();
                let data = card.data.borrow();
                Ok(json!({"rect": {"x": data.x, "y": data.y, "w": data.width, "h": data.height}}))
            }
            "harness.launch" => {
                let agent = params["agent"].as_str().unwrap_or_default().to_string();
                let folder = params["folder"].as_str().unwrap_or_default().to_string();
                let session = self
                    .workspace()?
                    .command(&WorkspaceCommand::CreateTerminal { agent_type: agent, workspace: folder })
                    .map_err(refused)?
                    .unwrap_or_default();
                let card = self.workspace()?.cards().into_iter().find(|c| c.data.borrow().session_name == session);
                Ok(json!({"card": card.map(|c| c.data.borrow().id.clone())}))
            }
            "title.set" | "title.clear" => {
                let card = self.card(&card_param())?;
                let id = card.data.borrow().id.clone();
                {
                    let mut titles = self.titles.borrow_mut();
                    let states = titles.entry(id).or_default();
                    if method == "title.clear" {
                        states.remove(plugin);
                    } else {
                        let state = states.entry(plugin.to_string()).or_default();
                        match params.get("text") {
                            Some(Value::Null) => state.text = None,
                            Some(Value::String(text)) => state.text = Some(clean_title(text)),
                            _ => {}
                        }
                        if let Some(chips) = params.get("chipsBefore").and_then(Value::as_array) {
                            state.before = chips.clone();
                        }
                        if let Some(chips) = params.get("chipsAfter").and_then(Value::as_array) {
                            state.after = chips.clone();
                        }
                    }
                }
                self.paint_title(&card);
                Ok(json!({}))
            }
            _ => Err(RpcError::new(rpc::METHOD_NOT_FOUND, format!("{method} is not a card call"), "Report this as a SUPER DESKTOP bug.", "references/host-api.md#errors")),
        }
    }

    /// `card.close`: the user decides, in a small popover on the card. The
    /// plugin's request waits for the answer.
    pub(super) fn confirm_close(self: &Rc<Self>, plugin: &str, params: &Value, reply: std::sync::mpsc::Sender<Result<Value, RpcError>>) {
        let card = match self.card(params["card"].as_str().unwrap_or_default()) {
            Ok(card) => card,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let name = plugin_name_of(self, plugin);
        let popover = gtk4::Popover::new();
        popover.add_css_class("plugin-confirm");
        let body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        let question = gtk4::Label::new(Some(&format!("{name} wants to close this terminal. Its session ends.")));
        question.set_wrap(true);
        question.set_max_width_chars(36);
        body.append(&question);
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let keep = gtk4::Button::with_label("Keep it");
        let close = gtk4::Button::with_label("Close");
        close.add_css_class("plugin-button-danger");
        row.append(&keep);
        row.append(&close);
        body.append(&row);
        popover.set_child(Some(&body));
        let anchor: gtk4::Widget = if card.data.borrow().iconified { card.container.clone().upcast() } else { card.plugin_chrome.builtin_controls.parent().unwrap_or_else(|| card.container.clone().upcast()) };
        popover.set_parent(&anchor);
        let answer: Rc<std::cell::RefCell<Option<std::sync::mpsc::Sender<Result<Value, RpcError>>>>> = Rc::new(std::cell::RefCell::new(Some(reply)));
        {
            let (answer, popover, card) = (Rc::clone(&answer), popover.downgrade(), Rc::downgrade(&card));
            close.connect_clicked(move |_| {
                let closed = card.upgrade().is_some_and(|c| manager().builtin(&c, "builtin:close").is_ok());
                if let Some(reply) = answer.borrow_mut().take() {
                    let _ = reply.send(Ok(json!({"closed": closed})));
                }
                if let Some(p) = popover.upgrade() {
                    p.popdown();
                }
            });
        }
        {
            let popover = popover.downgrade();
            keep.connect_clicked(move |_| {
                if let Some(p) = popover.upgrade() {
                    p.popdown();
                }
            });
        }
        popover.connect_closed(move |p| {
            if let Some(reply) = answer.borrow_mut().take() {
                let _ = reply.send(Ok(json!({"closed": false})));
            }
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    }
}

fn refused(why: &'static str) -> RpcError {
    RpcError::new(rpc::UNAVAILABLE, format!("the desktop refused: {why}"), "Read workspace.cards and try again; the card may have changed.", "references/host-api.md#card")
}

/// A card's current layout, as the remote-command path takes it.
pub(super) fn layout_of(card: &MiniTerminalCard) -> CardLayout {
    let data = card.data.borrow();
    CardLayout {
        x: data.x,
        y: data.y,
        width: data.width.max(1) as u32,
        height: data.height.max(1) as u32,
        restored_width: data.restored_width.max(1) as u32,
        restored_height: data.restored_height.max(1) as u32,
        iconified: data.iconified,
        icon_x: data.icon_x,
        icon_y: data.icon_y,
        tag: data.tag,
    }
}

/// One line, at most 200 characters, no control characters.
fn clean_title(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(200).collect::<String>().trim().to_string()
}

/// The card's status in the API's words.
fn status_name(status: &str) -> &'static str {
    match status {
        s if s.contains("work") || s.contains("busy") || s.contains("active") => "working",
        s if s.contains("wait") => "waiting",
        s if s.contains("complet") || s.contains("done") => "completed",
        s if s.contains("error") || s.contains("exit") => "error",
        s if s.contains("idle") => "idle",
        _ => "unknown",
    }
}

fn shows_on(button: &CardButton, agent: &str, status: &str) -> bool {
    let Some(when) = &button.when else { return true };
    when.agents.as_ref().is_none_or(|agents| agents.iter().any(|a| a == agent))
        && when.status.as_ref().is_none_or(|list| list.iter().any(|s| s == status_name(status)))
}

fn manifest_dir(manager: &Manager, plugin: &str) -> std::path::PathBuf {
    manager.plugins.borrow().get(plugin).map(|p| p.dir.clone()).unwrap_or_default()
}

fn plugin_name_of(manager: &Manager, plugin: &str) -> String {
    manager.plugins.borrow().get(plugin).map(|p| p.manifest.name.clone()).unwrap_or_else(|| plugin.to_string())
}

/// A card's prompt or status changed: tell the title plugins, and redraw its
/// buttons when one depends on the status.
pub fn card_inputs_changed(card_id: &str) {
    let manager = manager();
    let Some(workspace) = manager.workspace.borrow().clone() else { return };
    let Some(card) = workspace.cards().into_iter().find(|c| c.data.borrow().id == card_id) else { return };
    manager.send_title_inputs(None, std::slice::from_ref(&card));
    let status_dependent = manager.plugins.borrow().values().any(|p| p.manifest.contributes.card_buttons.iter().any(|b| b.when.as_ref().is_some_and(|w| w.status.is_some())));
    if status_dependent {
        manager.decorate(&card);
    }
}

#[allow(dead_code)]
type Titles = BTreeMap<String, BTreeMap<String, TitleState>>;
