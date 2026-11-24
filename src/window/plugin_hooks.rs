use super::*;

pub(in crate::window) fn install_plugin_hooks(
    owner: &Rc<SuperDesktopWindow>,
    hide: Rc<dyn Fn()>,
    detected: Vec<crate::tmux::HarnessInfo>,
) {
    let harnesses = {
        let owner = Rc::downgrade(owner);
        Rc::new(move || {
            let Some(owner) = owner.upgrade() else { return serde_json::json!([]) };
            let state = owner.state.borrow();
            let light = crate::theme::current_theme().mode == "light";
            let list: Vec<serde_json::Value> = crate::harness_settings::visible_keys(&state, &detected)
                .into_iter()
                .map(|key| {
                    let custom = state.custom_harnesses.iter().find(|c| c.id == key);
                    let name = custom.map(|c| c.name.clone())
                        .unwrap_or_else(|| crate::harness_bar::harness_label(&key).0.to_owned());
                    let icon = crate::brand::logo_path(&key, light)
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    serde_json::json!({"id": key, "name": name, "icon": icon})
                })
                .collect();
            serde_json::Value::Array(list)
        }) as Rc<dyn Fn() -> serde_json::Value>
    };
    let launch = {
        let owner = Rc::downgrade(owner);
        let harnesses = Rc::clone(&harnesses);
        Rc::new(move |key: &str, folder: Option<&str>| {
            let offered = harnesses();
            if !offered.as_array().into_iter().flatten().any(|h| h["id"] == key) {
                return Err(format!("{key:?} is not an agent offered in the top bar"));
            }
            let owner = owner.upgrade().ok_or("the overlay is closing")?;
            match folder {
                Some(folder) => { owner.create_new_terminal_in(key, None, None, None, Some(folder)); }
                None => owner.launch_new_terminal(key),
            }
            Ok(())
        }) as Rc<dyn Fn(&str, Option<&str>) -> Result<(), String>>
    };
    let cards = {
        let owner = Rc::downgrade(owner);
        Rc::new(move || {
            let Some(owner) = owner.upgrade() else { return serde_json::json!([]) };
            let mut list: Vec<serde_json::Value> = owner.terminal_cards.borrow().iter().map(|card| {
                let data = card.data.borrow();
                serde_json::json!({
                    "id": data.id, "kind": "terminal",
                    "title": card.desktop_presentation().title,
                    "harness": data.agent_type,
                    "folder": data.workspace_dir,
                    "status": card.status_word(),
                    "minimized": data.iconified,
                })
            }).collect();
            list.extend(owner.note_cards.borrow().iter().map(|note| {
                let data = note.data.borrow();
                serde_json::json!({
                    "id": data.id, "kind": "note",
                    "title": data.text.lines().next().unwrap_or("").chars().take(80).collect::<String>(),
                    "harness": serde_json::Value::Null, "folder": serde_json::Value::Null,
                    "status": serde_json::Value::Null, "minimized": false,
                })
            }));
            serde_json::Value::Array(list)
        }) as Rc<dyn Fn() -> serde_json::Value>
    };
    let open_card = {
        let owner = Rc::downgrade(owner);
        let harnesses = Rc::clone(&harnesses);
        Rc::new(move |key: &str, folder: &str| {
            if !harnesses().as_array().into_iter().flatten().any(|h| h["id"] == key) {
                return Err(format!("{key:?} is not an agent offered in the top bar"));
            }
            let owner = owner.upgrade().ok_or("the overlay is closing")?;
            let session = owner.create_new_terminal_in(key, None, None, None, Some(folder));
            if session.is_empty() { Err("the card could not be created".into()) } else { Ok(session) }
        }) as Rc<dyn Fn(&str, &str) -> Result<String, String>>
    };
    let focus_card = {
        let owner = Rc::downgrade(owner);
        Rc::new(move |id: &str| {
            let owner = owner.upgrade().ok_or("the overlay is closing")?;
            let card = owner.terminal_cards.borrow().iter().find(|c| c.data.borrow().id == id).cloned();
            if let Some(card) = card {
                card.select_with_keyboard();
                return Ok(());
            }
            let note = owner.note_cards.borrow().iter().find(|n| n.data.borrow().id == id).cloned();
            match note {
                Some(note) => {
                    raise_canvas_child(&owner.canvas, &note.container);
                    raise_canvas_child(&owner.canvas, &owner.hud);
                    note.text_view.grab_focus();
                    Ok(())
                }
                None => Err(format!("no card {id:?}")),
            }
        }) as Rc<dyn Fn(&str) -> Result<(), String>>
    };
    let card_session = {
        let owner = Rc::downgrade(owner);
        Rc::new(move |id: &str| {
            let owner = owner.upgrade()?;
            let cards = owner.terminal_cards.borrow();
            let card = cards.iter().find(|c| c.data.borrow().id == id)?;
            let data = card.data.borrow();
            Some((data.session_name.clone(), data.agent_type.clone()))
        }) as Rc<dyn Fn(&str) -> Option<(String, String)>>
    };
    crate::plugin_ui::api::set_hooks(crate::plugin_ui::api::Hooks { hide, harnesses, launch, cards, open_card, focus_card, card_session });
    crate::plugin_ui::api::set_open_custom_card({
        let owner = Rc::downgrade(owner);
        Rc::new(move |key: &str, folder: &str, cmd: Option<&str>| {
            let owner = owner.upgrade().ok_or("the overlay is closing")?;
            let session = owner.create_new_terminal_in(key, cmd, None, None, Some(folder));
            if session.is_empty() { Err("the card could not be created".into()) } else { Ok(session) }
        })
    });
    crate::plugin_ui::api::set_workspace_folder({
        let owner = Rc::downgrade(owner);
        Rc::new(move || {
            let Some(owner) = owner.upgrade() else { return serde_json::json!({"folder": crate::state::home_dir_string(), "recent": []}) };
            let state = owner.state.borrow();
            serde_json::json!({"folder": crate::state::effective_workspace_dir(&state), "recent": crate::state::shared_recent_dirs(&state)})
        })
    });
    let owner = Rc::downgrade(owner);
    crate::plugin_ui::api::set_card_commands(Rc::new(move |id, command, asker, done| {
        match owner.upgrade() {
            Some(owner) => owner.plugin_card_command(id, command, asker, done),
            None => done(Err(plugin_error("unavailable", "the overlay is closing", "Try again in a moment."))),
        }
    }));
}

pub(in crate::window) fn plugin_error(code: &str, message: &str, hint: &str) -> serde_json::Value {
    serde_json::json!({"code": code, "message": message, "hint": hint})
}


impl SuperDesktopWindow {
    /// Where a plugin's terminal card is and how it may move: `cards.geometry`
    /// and the answer of every other card command. `area` is the part of the
    /// screen cards may cover (below the top bar, inside the docks).
    pub(in crate::window) fn plugin_card_geometry(&self, card: &MiniTerminalCard) -> serde_json::Value {
        let (sw, sh) = (self.screen_width(), self.screen_height());
        let data = card.data.borrow();
        let (mode, x, y, width, height) = if card.is_expanded() {
            let (x, y, w, h) = crate::mini_terminal::expanded_rect(sw, sh);
            ("expanded", x as i32, y as i32, w as i32, h as i32)
        } else if data.iconified {
            ("minimized", data.icon_x.unwrap_or(data.x), data.icon_y.unwrap_or(data.y), data.width, data.height)
        } else {
            ("normal", data.x, data.y, data.width, data.height)
        };
        let (left, top, right, bottom) = self.plugin_card_area();
        let (min_width, min_height) = crate::mini_terminal::min_card_size(1.0);
        serde_json::json!({
            "id": data.id, "mode": mode, "x": x, "y": y, "width": width, "height": height,
            "area": {"x": left, "y": top, "width": right - left, "height": bottom - top},
            "minWidth": min_width, "minHeight": min_height,
        })
    }

    /// The box (left, top, right, bottom) cards may cover.
    pub(in crate::window) fn plugin_card_area(&self) -> (i32, i32, i32, i32) {
        let (sw, sh) = (self.screen_width(), self.screen_height());
        let (dock_left, dock_right) = crate::plugin_ui::slots::insets();
        let top = 70.max(top_bar_height(self.state.borrow().top_bar_size) + 10);
        let left = 10 + dock_left;
        let right = (sw - 10 - dock_right).max(left + 1);
        (left, top, right, (sh - 10).max(top + 1))
    }

    /// A plugin's `cards.*` command on a local terminal card: what the card's
    /// own buttons, header and edges do. `cards.close` asks the user first.
    pub(in crate::window) fn plugin_card_command(
        self: &Rc<Self>,
        id: &str,
        command: crate::plugin_ui::api::CardCommand,
        asker: &str,
        done: Box<dyn FnOnce(Result<serde_json::Value, serde_json::Value>)>,
    ) {
        use crate::plugin_ui::api::CardCommand;
        let card = self.terminal_cards.borrow().iter().find(|c| c.data.borrow().id == id).cloned();
        let Some(card) = card else {
            let note = self.note_cards.borrow().iter().any(|n| n.data.borrow().id == id);
            let message = if note { format!("{id:?} is a note") } else { format!("no terminal card {id:?}") };
            return done(Err(plugin_error("bad_params", &message, "Pass the id of a terminal card from cards.list; cards come and go, and notes have no such buttons.")));
        };
        let busy = card.is_being_dragged() || card.container.has_css_class("term-resizing") || self.slide.running.get();
        if busy && command != CardCommand::Geometry {
            return done(Err(plugin_error("busy", "the card is being moved, resized or slid right now", "Try again when the user lets go.")));
        }
        let expanded = card.is_expanded();
        let iconified = card.data.borrow().iconified;
        let screen = (self.screen_width(), self.screen_height());
        let toggle_expand = |card: &Rc<MiniTerminalCard>| {
            let session = card.data.borrow().session_name.clone();
            apply_terminal_expand(&self.terminal_cards, &self.canvas, &self.window, &self.ghosts, screen.0, screen.1, &session);
        };
        match command {
            CardCommand::Geometry => {}
            CardCommand::Move { .. } | CardCommand::Resize { .. } if expanded => {
                return done(Err(plugin_error("bad_params", "the card is expanded", "Collapse it first (cards.collapse).")));
            }
            CardCommand::Move { x, y } if iconified => {
                let (left, top, right, bottom) = self.plugin_card_area();
                let side = card.data.borrow().width;
                let x = x.clamp(left, (right - side).max(left));
                let y = y.clamp(top, (bottom - side).max(top));
                if self.move_terminal_card(id, x, y).is_err() {
                    return done(Err(plugin_error("unavailable", "the card could not move", "Try again.")));
                }
            }
            CardCommand::Resize { .. } if iconified => {
                return done(Err(plugin_error("bad_params", "the card is minimized", "Restore it first (cards.restore).")));
            }
            CardCommand::Move { .. } | CardCommand::Resize { .. } => {
                let (left, top, right, bottom) = self.plugin_card_area();
                let (min_width, min_height) = crate::mini_terminal::min_card_size(1.0);
                let data = card.data.borrow().clone();
                let (x, y, width, height) = match command {
                    CardCommand::Move { x, y } => (x, y, data.width, data.height),
                    CardCommand::Resize { width, height } => (data.x, data.y, width, height),
                    _ => unreachable!(),
                };
                let width = width.clamp(min_width, (right - left).max(min_width));
                let height = height.clamp(min_height, (bottom - top).max(min_height));
                let x = x.clamp(left, (right - width).max(left));
                let y = y.clamp(top, (bottom - height).max(top));
                // Placed, saved and outlined like the end of a resize gesture.
                if !card.apply_geometry(crate::card_resize::Rect { x: f64::from(x), y: f64::from(y), width, height }) {
                    return done(Err(plugin_error("unavailable", "the card is not ready to change", "Try again in a moment.")));
                }
            }
            CardCommand::Minimize => {
                if expanded {
                    toggle_expand(&card);
                }
                card.set_iconified(true);
            }
            CardCommand::Restore => {
                card.set_iconified(false);
            }
            CardCommand::Expand => {
                if !expanded {
                    toggle_expand(&card);
                }
            }
            CardCommand::Collapse => {
                if expanded {
                    toggle_expand(&card);
                }
            }
            CardCommand::Raise => card.cli_raise(),
            CardCommand::Tag(tag) => {
                let before = card.data.borrow().clone();
                card.cli_set_tag(tag);
                let after = card.data.borrow().clone();
                persist(&self.state, |state| {
                    crate::folder_colors::note_card_saved(state, Some(&before), &after);
                    if let Some(saved) = state.terminals.iter_mut().find(|c| c.id == after.id) {
                        saved.tag = tag;
                    }
                });
            }
            CardCommand::Close => {
                if !self.window.is_visible() {
                    return done(Err(plugin_error("unavailable", "SUPER DESKTOP is hidden, so it cannot ask", "Close a card only on the user's click, while SUPER DESKTOP is shown.")));
                }
                let owner = Rc::downgrade(self);
                let session = card.data.borrow().session_name.clone();
                let done = Rc::new(RefCell::new(Some(done)));
                let answer = Rc::clone(&done);
                let asked = card.confirm_close(asker, Box::new(move |yes| {
                    if yes {
                        if let Some(owner) = owner.upgrade() {
                            close_session_card(&owner.terminal_cards, &owner.canvas, &owner.state, &owner.ghosts, &session);
                        }
                    }
                    if let Some(done) = answer.borrow_mut().take() {
                        done(Ok(serde_json::json!({"closed": yes})));
                    }
                }));
                if !asked {
                    if let Some(done) = done.borrow_mut().take() {
                        done(Err(plugin_error("busy", "the card cannot ask now (it is out of view or already asking)", "Try again when the card is on screen.")));
                    }
                }
                return;
            }
        }
        done(Ok(self.plugin_card_geometry(&card)))
    }

}
