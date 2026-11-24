use super::*;

impl SuperDesktopWindow {
    pub(in crate::window) fn load_items(&self) {
        let mut terminals: Vec<TerminalData> = self.state.borrow().terminals.clone();
        let order = self.state.borrow().terminal_order.clone();
        terminals.sort_by_key(|t| order.iter().position(|id| id == &t.id).unwrap_or(usize::MAX));
        let inventory = std::sync::Arc::new(crate::tmux::SessionInventory::default());
        // One state.json read for the whole batch, not one per card.
        let custom = (!terminals.is_empty())
            .then(|| Rc::new(crate::state::load_state().custom_harnesses));
        for term_data in terminals {
            self.spawn_terminal_widget(
                term_data,
                false,
                Some(std::sync::Arc::clone(&inventory)),
                custom.clone(),
            );
        }

        let notes: Vec<NoteData> = self.state.borrow().notes.clone();
        for note_data in notes {
            self.spawn_note_widget(note_data, false);
        }

        self.raise_all_notes();
        raise_canvas_child(&self.canvas, &self.hud);
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
            color: "omarchy".to_string(),
            updated_at: now as f64 / 1000.0,
            tag: 0,
        };

        self.spawn_note_widget(data, true);
    }

    /// A card's drag-motion callback, shared by notes and terminals: the
    /// card's origin is clamped to the workspace and moved on the next frame,
    /// so the motion events of one frame cost a single move. `after_move` runs
    /// on every frame that moved a card.
    pub(in crate::window) fn card_drag_updater(&self, after_move: impl Fn() + 'static) -> impl Fn(gtk4::Widget, f64, f64) + 'static {
        let pending = Rc::clone(&self.drag_pending);
        let tick_active = Rc::clone(&self.drag_tick_active);
        let canvas = self.canvas.clone();
        let window = self.window.downgrade();
        let fallback = (self.screen_width(), self.screen_height());
        let after_move = Rc::new(after_move);
        move |widget: gtk4::Widget, x: f64, y: f64| {
            let size = live_workspace_size(&window, fallback);
            // Perf: .dragging disables hover transitions/shadows (see CSS)
            // so the card paints cheaply while it moves at 120Hz.
            if !widget.has_css_class("dragging") {
                widget.add_css_class("dragging");
            }
            pending.borrow_mut().insert(widget, clamp_card_origin(x, y, size));

            if !*tick_active.borrow() {
                *tick_active.borrow_mut() = true;
                let dp = Rc::clone(&pending);
                let dta = Rc::clone(&tick_active);
                let c = canvas.clone();
                let after_move = Rc::clone(&after_move);

                canvas.add_tick_callback(move |_, _| {
                    if dp.borrow().is_empty() {
                        *dta.borrow_mut() = false;
                        return glib::ControlFlow::Break;
                    }
                    let items: Vec<(gtk4::Widget, (f64, f64))> = dp.borrow_mut().drain().collect();
                    for (w, (px, py)) in items {
                        c.move_(&w, px, py);
                    }
                    after_move();
                    glib::ControlFlow::Continue
                });
            }
        }
    }

    pub(in crate::window) fn spawn_note_widget(&self, note_data: NoteData, save: bool) {
        let canvas = self.canvas.clone();
        let state = Rc::clone(&self.state);
        let note_cards = Rc::clone(&self.note_cards);
        let sw = self.screen_width();
        let sh = self.screen_height();

        let on_drag_update = self.card_drag_updater(|| {});

        let canvas_note_end = canvas.clone();
        let drag_pending_note_end = Rc::clone(&self.drag_pending);
        let state_end = Rc::clone(&state);
        let drag_window = self.window.downgrade();
        let on_drag_end = move |widget: gtk4::Widget, data: &NoteData| {
            let size = live_workspace_size(&drag_window, (sw, sh));
            widget.remove_css_class("dragging");
            drag_pending_note_end.borrow_mut().remove(&widget);
            let mut final_data = data.clone();
            (final_data.x, final_data.y) = clamp_card_origin(final_data.x, final_data.y, size);
            canvas_note_end.move_(&widget, final_data.x as f64, final_data.y as f64);
            // Perf: serialize + write the JSON off the main thread so the
            // drag-end frame completes within the 8.3ms 120Hz budget.
            persist(&state_end, |s| {
                if let Some(n) = s.notes.iter_mut().find(|n| n.id == final_data.id) {
                    *n = final_data;
                } else {
                    s.notes.push(final_data);
                }
            });
        };

        let canvas_del = canvas.clone();
        let state_del = Rc::clone(&state);
        let note_cards_del = Rc::clone(&note_cards);

        let on_delete = move |id: String| {
            // Take the note out of the shared list and drop the borrow BEFORE
            // touching GTK: `canvas.remove` unmaps the note subtree, which
            // emits pointer/focus signals that re-enter the raise handler.
            let note = {
                let mut cards = note_cards_del.borrow_mut();
                cards
                    .iter()
                    .position(|c| c.data.borrow().id == id)
                    .map(|pos| cards.remove(pos))
            };
            let Some(note) = note else { return };

            canvas_del.remove(&note.container);
            persist(&state_del, |s| s.notes.retain(|n| n.id != id));
        };

        let state_change = Rc::clone(&state);
        let on_change = move |data: &NoteData| {
            // Perf: typing already debounces 300ms; the remaining JSON
            // serialize + file write goes to a worker thread.
            persist(&state_change, |s| {
                if let Some(n) = s.notes.iter_mut().find(|n| n.id == data.id) {
                    *n = data.clone();
                }
            });
        };

        let canvas_raise = canvas.clone();
        let hud_raise = self.hud.clone();
        let note_cards_raise = Rc::clone(&note_cards);
        let note_id = note_data.id.clone();
        let on_raise = move |widget: gtk4::Widget| {
            raise_canvas_child(&canvas_raise, &widget);
            raise_canvas_child(&canvas_raise, &hud_raise);
            // GTK can call this while another handler is still holding the
            // list borrow (e.g. during a widget removal). The z-order above
            // already happened, so just skip the bookkeeping instead of
            // panicking on an active borrow.
            let Ok(mut cards) = note_cards_raise.try_borrow_mut() else {
                return;
            };
            if let Some(pos) = cards.iter().position(|c| c.data.borrow().id == note_id) {
                let note = cards.remove(pos);
                cards.push(note);
            }
        };

        let x = note_data.x;
        let y = note_data.y;

        if save {
            persist(&self.state, |s| s.notes.push(note_data.clone()));
        }

        let ghost = self.ghost_box.clone();
        let ghost_lbl = self.ghost_label.clone();
        let canvas_ghost = canvas.clone();
        let ghost_last: Rc<RefCell<Option<(f64, f64, i32, i32)>>> =
            Rc::new(RefCell::new(None));
        let ghost_last_show = Rc::clone(&ghost_last);
        let on_resize_ghost = move |x: f64, y: f64, w: i32, h: i32| {
            let qx = x.round();
            let qy = y.round();
            if *ghost_last_show.borrow() == Some((qx, qy, w, h)) {
                return;
            }
            let previous = *ghost_last_show.borrow();
            *ghost_last_show.borrow_mut() = Some((qx, qy, w, h));

            if !ghost.is_visible() {
                if ghost.parent().is_some() {
                    canvas_ghost.remove(&ghost);
                }
                canvas_ghost.put(&ghost, qx, qy);
            } else if previous.map(|(x, y, _, _)| (x, y)) != Some((qx, qy)) {
                canvas_ghost.move_(&ghost, qx, qy);
            }
            ghost.set_visible(true);
            if previous.map(|(_, _, w, h)| (w, h)) != Some((w, h)) {
                ghost.set_size_request(w, h);
            }
            ghost.remove_css_class("ghost-icon");
            ghost.add_css_class("ghost-note");
            let label = format!("📝 {w} × {h} (Note)");
            if ghost_lbl.label().as_str() != label {
                ghost_lbl.set_label(&label);
            }
        };

        let ghost_end = self.ghost_box.clone();
        let ghost_last_hide = Rc::clone(&ghost_last);
        let on_resize_end = move || {
            *ghost_last_hide.borrow_mut() = None;
            ghost_end.set_visible(false);
        };

        let note = StickyNote::new(
            note_data,
            on_drag_update,
            on_drag_end,
            on_delete,
            on_change,
            on_raise,
            on_resize_ghost,
            on_resize_end,
            sw,
            sh,
        );
        canvas.put(&note.container, x as f64, y as f64);
        note_cards.borrow_mut().push(Rc::new(note));
        raise_canvas_child(&canvas, &self.hud);
    }

    pub fn create_new_terminal(&self, agent_type: &str, cmd: Option<&str>, x: Option<i32>, y: Option<i32>) -> String {
        self.create_new_terminal_in(agent_type, cmd, x, y, None)
    }

    pub fn workspace_choices(&self) -> serde_json::Value {
        let state = self.state.borrow();
        serde_json::json!({"workspace": crate::state::effective_workspace_dir(&state),
            "recentDirectories": crate::state::shared_recent_dirs(&state),
            "usedDirectories": state.used_dirs})
    }

    /// Create a card and its tmux session; the session exists on return, so
    /// callers can report it (IPC, a viewer's request).
    pub fn create_new_terminal_in(&self, agent_type: &str, cmd: Option<&str>, x: Option<i32>, y: Option<i32>, directory: Option<&str>) -> String {
        self.new_terminal_card(agent_type, cmd, x, y, directory, true)
    }

    /// The top bar's launch: the card is on screen at the next frame and its
    /// session is started by the card's first attach, on a worker. Creating it
    /// here held GTK for one tmux process (several, before they were chained).
    pub fn launch_new_terminal(&self, agent_type: &str) {
        self.new_terminal_card(agent_type, None, None, None, None, false);
    }

    pub(in crate::window) fn new_terminal_card(&self, agent_type: &str, cmd: Option<&str>, x: Option<i32>, y: Option<i32>, directory: Option<&str>, wait_for_session: bool) -> String {
        crate::frame_profile::note_request("new terminal");
        // The folder from the top bar field: this card's harness starts there,
        // and keeps it for its whole life (see TerminalData::workspace_dir).
        let workspace_dir = directory.map(str::to_owned)
            .unwrap_or_else(|| crate::state::effective_workspace_dir(&self.state.borrow()));
        crate::state::remember_workspace_dir(&mut self.state.borrow_mut(), &workspace_dir);
        // Same folder, same label colour (see `folder_colors`).
        let tag = {
            let mut state = self.state.borrow_mut();
            let tag = crate::folder_colors::tag_for_new_card(&state, &workspace_dir);
            crate::folder_colors::remember(&mut state, &workspace_dir, tag);
            tag
        };
        let custom_command = self.state.borrow().custom_harnesses.iter()
            .find(|item| item.id == agent_type && item.validate().is_ok())
            .map(|item| item.command());
        let command = custom_command.as_deref().or(cmd);
        // Default size for a new harness: 640x480, clamped to the screen.
        let (def_w, def_h) =
            clamp_card_size(NEW_TERM_WIDTH, NEW_TERM_HEIGHT, self.screen_width(), self.screen_height());
        // A card too narrow to attach has no worker to start its session.
        let (sess, cmd_run, inventory) = if wait_for_session || def_w < crate::mini_terminal::MIN_CARD_WIDTH {
            let (sess, cmd_run) = create_session(agent_type, command, Some(&workspace_dir));
            (sess, cmd_run, None)
        } else {
            let cmd_run = crate::tmux::resolve_command(agent_type, command);
            let sess = super_desktop::session_id::candidate();
            let fresh = crate::tmux::SessionInventory::fresh(&sess, agent_type, &cmd_run, &workspace_dir);
            (sess, cmd_run, Some(std::sync::Arc::new(fresh)))
        };
        let (cx, cy) = self.new_card_origin(def_w, def_h);
        let nx = x.unwrap_or(cx);
        let ny = y.unwrap_or(cy);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let data = TerminalData {
            id: sess.clone(),
            session_name: sess.clone(),
            agent_type: agent_type.to_string(),
            command: cmd_run,
            x: nx,
            y: ny,
            width: def_w,
            height: def_h,
            restored_width: def_w,
            restored_height: def_h,
            iconified: false,
            icon_x: None,
            icon_y: None,
            created_at: now,
            tag,
            agent_session_id: None,
            workspace_dir: Some(workspace_dir),
        };

        self.spawn_terminal_widget(data, true, inventory, None);
        sess
    }

    /// Where a new card of this size opens: centered on screen, cascaded
    /// slightly so stacked harnesses don't overlap exactly.
    pub(in crate::window) fn new_card_origin(&self, width: i32, height: i32) -> (i32, i32) {
        let (sw, sh) = (self.screen_width(), self.screen_height());
        let cascade = (self.terminal_cards.borrow().len() as i32 % 5) * 32;
        (
            ((sw - width) / 2 + cascade).clamp(10, (sw - width - 10).max(10)),
            ((sh - height) / 2 + cascade).clamp(70, (sh - height - 10).max(70)),
        )
    }

    /// Adopt an already launched CLI session without mapping or focusing the overlay.
    pub(crate) fn adopt_cli_terminal(&self, mut data: TerminalData) -> Result<(), ()> {
        {
            let state = self.state.borrow();
            if state.terminals.len() >= crate::workspace_model::MAX_DESKTOP_CARDS
                || state
                    .terminals
                    .iter()
                    .any(|t| t.id == data.id || t.session_name == data.session_name)
            {
                return Err(());
            }
        }
        let (width, height) = clamp_card_size(
            NEW_TERM_WIDTH,
            NEW_TERM_HEIGHT,
            self.screen_width(),
            self.screen_height(),
        );
        (data.x, data.y) = self.new_card_origin(width, height);
        data.width = width;
        data.height = height;
        data.restored_width = width;
        data.restored_height = height;
        persist(&self.state, |state| {
            if let Some(directory) = &data.workspace_dir {
                crate::state::remember_workspace_dir(state, directory);
                data.tag = crate::folder_colors::tag_for_new_card(state, directory);
                crate::folder_colors::remember(state, directory, data.tag);
            }
            state.terminals.push(data.clone());
            crate::state::normalize_terminal_order(state);
        });
        let inventory = std::sync::Arc::new(crate::tmux::SessionInventory::cli_created(
            &data.session_name,
        ));
        self.spawn_terminal_widget(data, false, Some(inventory), None);
        Ok(())
    }

    pub(in crate::window) fn spawn_terminal_widget(
        &self,
        term_data: TerminalData,
        save: bool,
        startup_inventory: Option<std::sync::Arc<crate::tmux::SessionInventory>>,
        custom_harnesses: Option<Rc<Vec<crate::custom_harness::CustomHarness>>>,
    ) {
        let canvas = self.canvas.clone();
        let state = Rc::clone(&self.state);
        let term_cards = Rc::clone(&self.terminal_cards);
        // This card's callbacks have no `Rc<Self>`, so they hold the ghost
        // layer directly and redraw the buried-card outlines themselves.
        let ghosts = Rc::clone(&self.ghosts);
        let sw = self.screen_width();
        let sh = self.screen_height();

        // A card dragged over another one hides it: keep that card's ghost
        // outline following the drag.
        let ghosts_drag = Rc::clone(&ghosts);
        let on_drag_update = self.card_drag_updater(move || ghosts_drag.refresh());

        let canvas_term_end = canvas.clone();
        let drag_pending_term_end = Rc::clone(&self.drag_pending);
        let state_end = Rc::clone(&state);
        let ghosts_end = Rc::clone(&ghosts);
        let drag_window = self.window.downgrade();
        let on_drag_end = move |widget: gtk4::Widget, data: &TerminalData| {
            let size = live_workspace_size(&drag_window, (sw, sh));
            widget.remove_css_class("dragging");
            drag_pending_term_end.borrow_mut().remove(&widget);
            let mut final_data = data.clone();
            // The icon and the expanded card have separate remembered spots;
            // clamp and snap to whichever one this card is currently in.
            if final_data.iconified {
                let (x, y) = clamp_card_origin(
                    final_data.icon_x.unwrap_or(final_data.x),
                    final_data.icon_y.unwrap_or(final_data.y),
                    size,
                );
                (final_data.icon_x, final_data.icon_y) = (Some(x), Some(y));
            } else {
                (final_data.x, final_data.y) = clamp_card_origin(final_data.x, final_data.y, size);
            }
            let (px, py) = displayed_pos(&final_data);
            canvas_term_end.move_(&widget, px, py);
            // Perf: keep the drag-end frame inside the 8.3ms 120Hz budget.
            persist(&state_end, |s| {
                // A colour picked on the card's dot becomes its folder's colour.
                let previous = s.terminals.iter().find(|t| t.session_name == final_data.session_name).cloned();
                crate::folder_colors::note_card_saved(s, previous.as_ref(), &final_data);
                if let Some(t) = s.terminals.iter_mut().find(|t| t.session_name == final_data.session_name) {
                    *t = final_data;
                } else {
                    s.terminals.push(final_data);
                }
            });
            // A card that just landed on another one buries it (and an iconify
            // or restore commit arrives through this same callback).
            ghosts_end.refresh();
        };

        let terminal_cards_toggle = Rc::clone(&term_cards);
        let canvas_toggle = canvas.clone();
        let window_toggle = self.window.clone();
        let ghosts_toggle = Rc::clone(&ghosts);
        let on_double_click = move |data: &TerminalData| {
            // The display this card was made on may be gone: expand to 80% of
            // the one the overlay is on now.
            let (sw, sh) = allocated_workspace_size(&window_toggle, (sw, sh));
            apply_terminal_expand(
                &terminal_cards_toggle,
                &canvas_toggle,
                &window_toggle,
                &ghosts_toggle,
                sw,
                sh,
                &data.session_name,
            );
        };

        let canvas_del = canvas.clone();
        let state_del = Rc::clone(&state);
        let term_cards_del = Rc::clone(&term_cards);
        let ghosts_del = Rc::clone(&ghosts);
        let on_close = move |sess: String| {
            close_session_card(&term_cards_del, &canvas_del, &state_del, &ghosts_del, &sess);
        };

        // Restored cards reappear wherever their current mode lives: an
        // iconified card belongs at its remembered icon spot, not at the
        // expanded card position.
        let (x, y) = displayed_pos(&term_data);

        if save {
            persist(&self.state, |s| {
                s.terminals.push(term_data.clone());
                crate::state::normalize_terminal_order(s);
            });
        }

        let ghost = self.ghost_box.clone();
        let ghost_lbl = self.ghost_label.clone();
        let canvas_ghost = canvas.clone();
        // Perf: resize motion events arrive far faster than the 120Hz frame
        // clock. Coalesce them here: skip no-op updates and only touch
        // size-request / label / CSS classes when that aspect changed.
        // Each of those calls queues a relayout/restyle otherwise.
        let ghost_last: Rc<RefCell<Option<(f64, f64, i32, i32, bool)>>> =
            Rc::new(RefCell::new(None));
        let ghost_last_show = Rc::clone(&ghost_last);
        let on_resize_ghost = move |x: f64, y: f64, w: i32, h: i32, is_icon: bool| {
            let qx = x.round();
            let qy = y.round();
            if let Some((lx, ly, lw, lh, li)) = *ghost_last_show.borrow() {
                if lx == qx && ly == qy && lw == w && lh == h && li == is_icon {
                    return;
                }
            }
            let prev = ghost_last_show.borrow().clone();
            let pos_changed = prev.map(|(lx, ly, _, _, _)| lx != qx || ly != qy).unwrap_or(true);
            let size_changed = prev.map(|(_, _, lw, lh, _)| lw != w || lh != h).unwrap_or(true);
            let mode_changed = prev.map(|(_, _, _, _, li)| li != is_icon).unwrap_or(true);
            *ghost_last_show.borrow_mut() = Some((qx, qy, w, h, is_icon));

            if !ghost.is_visible() {
                if ghost.parent().is_some() {
                    canvas_ghost.remove(&ghost);
                }
                canvas_ghost.put(&ghost, qx, qy);
            } else if pos_changed {
                canvas_ghost.move_(&ghost, qx, qy);
            }
            ghost.set_visible(true);
            if size_changed {
                ghost.set_size_request(w, h);
            }
            if mode_changed {
                if is_icon {
                    ghost.add_css_class("ghost-icon");
                } else {
                    ghost.remove_css_class("ghost-icon");
                }
            }
            ghost.remove_css_class("ghost-note");
            let text = if is_icon {
                "🗕 128 × 128 (Icon)".to_string()
            } else {
                format!("💻 {w} × {h} (Terminal)")
            };
            if ghost_lbl.label().as_str() != text.as_str() {
                ghost_lbl.set_label(&text);
            }
        };

        let ghost_end = self.ghost_box.clone();
        let ghost_last_hide = Rc::clone(&ghost_last);
        let on_resize_end = move || {
            *ghost_last_hide.borrow_mut() = None;
            if ghost_end.is_visible() {
                ghost_end.set_visible(false);
            }
        };

        let canvas_raise = canvas.clone();
        let hud_raise = self.hud.clone();
        let term_cards_raise = Rc::clone(&term_cards);
        let sess_name = term_data.session_name.clone();
        let card_id_raise = term_data.id.clone();
        let state_raise = Rc::clone(&state);
        let ghosts_raise = Rc::clone(&ghosts);
        let on_raise = move |widget: gtk4::Widget| {
            raise_canvas_child(&canvas_raise, &widget);
            raise_canvas_child(&canvas_raise, &hud_raise);
            // GTK can call this while another handler is still holding the
            // list borrow (e.g. during a widget removal). The z-order above
            // already happened, so just skip the bookkeeping instead of
            // panicking on an active borrow. The ghosts are recomputed below,
            // where the lists are free again.
            let Ok(mut cards) = term_cards_raise.try_borrow_mut() else {
                return;
            };
            if let Some(pos) = cards.iter().position(|c| c.data.borrow().session_name == sess_name) {
                let card = cards.remove(pos);
                cards.push(card);
            }
            drop(cards);
            persist_if(&state_raise, |state| {
                let persisted = state.terminal_order.last() != Some(&card_id_raise)
                    && state.terminals.iter().any(|t| t.id == card_id_raise);
                if persisted {
                    state.terminal_order.retain(|id| id != &card_id_raise);
                    state.terminal_order.push(card_id_raise.clone());
                }
                persisted
            });
            // A raise changes which card covers which: the buried ones may now
            // need an outline (or lost the one they had).
            ghosts_raise.refresh();
        };

        let state_sess = Rc::clone(&state);
        let on_session_persist = move |updated: &TerminalData| {
            persist_if(&state_sess, |s| {
                let Some(slot) = s.terminals.iter_mut().find(|t| t.session_name == updated.session_name) else {
                    return false;
                };
                // Only the agent mapping changed here; keep geometry from the
                // live card to avoid clobbering an in-progress drag/resize.
                slot.agent_session_id = updated.agent_session_id.clone();
                true
            });
        };

        let hover_lock = self.hover_raise_lock.clone();
        if save {
            // Hold hover-raise on every other card so moving the pointer
            // toward this newly opened harness cannot bury it.
            hover_lock.lock(&term_data.session_name);
        }
        let ghosts_interaction = Rc::clone(&ghosts);
        let on_interaction = move || ghosts_interaction.refresh();
        let card = MiniTerminalCard::new(
            term_data,
            on_drag_update,
            on_drag_end,
            on_double_click,
            on_close,
            on_resize_ghost,
            on_resize_end,
            on_raise,
            on_session_persist,
            on_interaction,
            sw,
            sh,
            startup_inventory,
            custom_harnesses,
            hover_lock,
            // The local workspace's own session: this machine's tmux, attached
            // by the card's emulator.
            crate::card_source::CardSource::Local,
        );
        let card = Rc::new(card);
        card.set_first_icon_spot(self.first_icon_spot(sw, sh));
        card.keyboard_digit.set(crate::terminal_picker::next_digit(
            term_cards.borrow().iter().filter_map(|card| card.keyboard_digit.get())
        ));
        canvas.put(&card.container, x, y);
        if save {
            card.focus_terminal();
        }
        term_cards.borrow_mut().push(Rc::clone(&card));
        raise_canvas_child(&canvas, &self.hud);
        // A new card can land on top of an existing one (they cascade 32px):
        // its outline shows up right away while the user works in neither.
        ghosts.refresh();
    }

    /// Where a local card's icon lands the first time it is minimized: the
    /// first free spot in rows of icons that fill from the bottom-left corner
    /// to the right, then the row above (see `first_free_icon_slot`).
    pub(in crate::window) fn first_icon_spot(&self, sw: i32, sh: i32) -> Rc<dyn Fn(&str, i32) -> Option<(i32, i32)>> {
        let cards = Rc::clone(&self.terminal_cards);
        let window = self.window.downgrade();
        let hud = self.hud.clone();
        let state = Rc::clone(&self.state);
        Rc::new(move |session: &str, side: i32| {
            let (width, height) = live_workspace_size(&window, (sw, sh));
            let size = state.try_borrow().ok()?.top_bar_size;
            let top = hud.height().max(top_bar_height(size)) + ICON_ROW_GAP;
            let (dock_left, dock_right) = crate::plugin_ui::slots::insets();
            // Only other icons take a spot: an icon may land under an open
            // card, as an icon dragged there does.
            let icons: Vec<(i32, i32, i32, i32)> = cards
                .try_borrow()
                .ok()?
                .iter()
                .filter_map(|card| {
                    let data = card.data.try_borrow().ok()?;
                    if !data.iconified || data.session_name == session {
                        return None;
                    }
                    let (x, y) = displayed_pos(&data);
                    Some((x as i32, y as i32, data.width, data.height))
                })
                .collect();
            let area = IconArea {
                left: ICON_ROW_GAP + dock_left,
                top,
                right: width - ICON_ROW_GAP - dock_right,
                bottom: height - ICON_ROW_GAP,
            };
            Some(first_free_icon_slot(&icons, area, side, ICON_ROW_GAP))
        })
    }

    pub fn auto_arrange(&self) {
        // Snapshot the cards (cheap `Rc` clones) and drop both list borrows
        // before moving anything: `collapse`/`move_` emit signals that
        // re-enter the raise handlers.
        let terms: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        let notes: Vec<Rc<StickyNote>> = self.note_cards.borrow().clone();

        for term in terms.iter() {
            if term.is_expanded() {
                collapse_in_place(&self.canvas, term);
            }
        }
        set_overlay_keyboard_mode(&self.window, KeyboardMode::OnDemand);

        let margin = 12;
        let gap = 12;
        let top = self.hud.height().max(top_bar_height(self.state.borrow().top_bar_size)) + gap;
        // Plugin docks own the screen edges they sit on.
        let (dock_left, dock_right) = crate::plugin_ui::slots::insets();
        let width = (self.screen_width() - 2 * margin - dock_left - dock_right).max(1);
        let height = (self.screen_height() - top - margin).max(1);
        // Keep notes in their own lane; terminal geometry never depends on
        // saved card extents or a previously connected output.
        let note_width = if notes.is_empty() { 0 } else {
            notes.iter().map(|n| n.data.borrow().width).max().unwrap_or(0)
                .min(width / 3)
        };
        let note_area = crate::card_resize::Rect {
            x: f64::from(margin + dock_left), y: f64::from(top), width: note_width.max(1), height,
        };
        for (note, rect) in notes.iter().zip(crate::arrange::terminal_grid(note_area, notes.len())) {
            let mut data = note.data.borrow_mut();
            data.x = rect.x as i32;
            data.y = rect.y as i32;
            data.width = rect.width;
            data.height = rect.height;
            drop(data);
            note.container.set_size_request(rect.width, rect.height);
            self.canvas.move_(&note.container, rect.x, rect.y);
        }
        let left = margin + dock_left + if notes.is_empty() { 0 } else { note_width + gap };
        let area = crate::card_resize::Rect {
            x: f64::from(left), y: f64::from(top),
            width: (self.screen_width() - margin - dock_right - left).max(1), height,
        };
        let icons: Vec<_> = terms.iter().filter(|term| term.is_compact()).collect();
        let terminals: Vec<_> = terms.iter().filter(|term| !term.is_compact()).collect();
        let icon_side = icons.iter().map(|term| {
            let data = term.data.borrow();
            data.width.max(data.height)
        }).max().unwrap_or(0);
        let icon_columns = ((area.width + gap) / (icon_side + gap)).max(1) as usize;
        let icon_rows = icons.len().div_ceil(icon_columns);
        let icon_height = if icons.is_empty() { 0 } else {
            (icon_rows as i32 * (icon_side + gap)).min(area.height)
        };
        let terminal_area = crate::card_resize::Rect {
            height: (area.height - icon_height).max(1), ..area
        };
        // Plugin and SD Editor cards take the places after the terminals.
        let plugin_slots = crate::plugin_ui::slots::get();
        let plugin_cards = canvas_cards();
        let mut grid = crate::arrange::terminal_grid(terminal_area, terminals.len() + plugin_cards.len()).into_iter();
        for (term, rect) in terminals.iter().zip(grid.by_ref()) {
            term.apply_geometry(rect);
            self.canvas.move_(&term.container, rect.x, rect.y);
        }
        if let Some(host) = plugin_slots.as_ref().and_then(|s| s.canvas_host()) {
            for (card, rect) in plugin_cards.iter().zip(grid) {
                card.set_geometry(&host, crate::plugin_ui::cards::Geometry {
                    x: rect.x.round() as i32, y: rect.y.round() as i32, width: rect.width, height: rect.height,
                });
            }
        }
        for (index, term) in icons.iter().enumerate() {
            let x = area.x + ((index % icon_columns) as i32 * (icon_side + gap)) as f64;
            let y = area.y + (area.height - icon_height + (index / icon_columns) as i32 * (icon_side + gap)) as f64;
            // Icons retain their own size and independent restore position.
            set_displayed_pos(&mut term.data.borrow_mut(), x as i32, y as i32);
            self.canvas.move_(&term.container, x, y);
        }

        persist(&self.state, |s| {
            for note in notes.iter() {
                if let Some(n) = s.notes.iter_mut().find(|n| n.id == note.data.borrow().id) {
                    *n = note.data.borrow().clone();
                }
            }
            for term in terms.iter() {
                if let Some(t) = s.terminals.iter_mut().find(|t| t.session_name == term.data.borrow().session_name) {
                    *t = term.data.borrow().clone();
                }
            }
        });
        // Refresh outlines after every card has its final geometry.
        self.ghosts.refresh();
    }

    /// The overlay was given another size: another display, a new scale, or a
    /// restart on a display the cards were not laid out for. Every card, note
    /// and plugin card keeps its place in proportion on the new size
    /// (`arrange::follow_workspace`), so none is left past an edge, and an
    /// expanded card fills 80% of the new size.
    pub(in crate::window) fn follow_workspace_size(&self, size: (i32, i32)) {
        self.ghosts.set_screen_size(size.0, size.1);
        // Snapshot the lists: moving and resizing cards emits signals that
        // re-enter the raise handlers.
        let terms: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        let laid_out_for = self.state.borrow().layout_size;
        let from = match laid_out_for {
            Some(from) if from == size => {
                for term in &terms {
                    term.set_workspace_size(size.0, size.1);
                }
                return;
            }
            Some(from) => from,
            // A layout from before sizes were recorded: cards on screen stay,
            // and only those hanging past an edge come back in.
            None => size,
        };
        let follow = |x: i32, y: i32, width: i32, height: i32, min: (i32, i32)| {
            let rect = crate::card_resize::Rect { x: f64::from(x), y: f64::from(y), width, height };
            crate::arrange::follow_workspace(rect, from, size, min)
        };

        for term in &terms {
            term.follow_workspace(from, size);
        }
        let notes: Vec<Rc<StickyNote>> = self.note_cards.borrow().clone();
        for note in &notes {
            let rect = {
                let d = note.data.borrow();
                follow(d.x, d.y, d.width, d.height, (crate::sticky_note::MIN_NOTE_WIDTH, crate::sticky_note::MIN_NOTE_HEIGHT))
            };
            {
                let mut d = note.data.borrow_mut();
                (d.x, d.y, d.width, d.height) = (rect.x as i32, rect.y as i32, rect.width, rect.height);
            }
            note.container.set_size_request(rect.width, rect.height);
        }
        let plugin_cards = canvas_cards();
        for card in &plugin_cards {
            let g = card.geometry();
            let rect = follow(g.x, g.y, g.width, g.height, (crate::plugin_ui::cards::MIN_WIDTH, crate::plugin_ui::cards::MIN_HEIGHT));
            card.remember_geometry(crate::plugin_ui::cards::Geometry {
                x: rect.x as i32, y: rect.y as i32, width: rect.width, height: rect.height,
            });
        }

        persist(&self.state, |s| {
            s.layout_size = Some(size);
            for note in &notes {
                if let Some(n) = s.notes.iter_mut().find(|n| n.id == note.data.borrow().id) {
                    *n = note.data.borrow().clone();
                }
            }
            for term in &terms {
                if let Some(t) = s.terminals.iter_mut().find(|t| t.session_name == term.data.borrow().session_name) {
                    *t = term.data.borrow().clone();
                }
            }
        });

        if self.slide.running.get() {
            // The slide in flight takes the cards to their new places: its
            // tick rebuilds the paths once they no longer match the size.
            self.slide.built_for.set((0, 0));
        } else {
            for term in &terms {
                let rect = term.canvas_rect(size.0, size.1);
                self.canvas.move_(&term.container, rect.x, rect.y);
            }
            for note in &notes {
                let (x, y) = (note.data.borrow().x, note.data.borrow().y);
                self.canvas.move_(&note.container, f64::from(x), f64::from(y));
            }
            for card in &plugin_cards {
                let g = card.geometry();
                self.canvas.move_(&card.container, f64::from(g.x), f64::from(g.y));
            }
        }
        self.ghosts.refresh();
    }

}

pub(in crate::window) fn raise_notes_on_canvas(canvas: &gtk4::Fixed, notes: &[Rc<crate::sticky_note::StickyNote>]) {
    for note in notes {
        raise_canvas_child(canvas, &note.container);
    }
}


pub(crate) fn raise_canvas_child(canvas: &Fixed, widget: &impl gtk4::glib::object::IsA<gtk4::Widget>) {
    let widget = widget.as_ref();
    if let Some(last) = canvas.last_child() {
        if &last != widget {
            widget.insert_after(canvas, Some(&last));
        }
    }
}

/// The workspace size now, for a card callback that holds only a weak window.
pub(in crate::window) fn live_workspace_size(window: &glib::WeakRef<ApplicationWindow>, fallback: (i32, i32)) -> (i32, i32) {
    window.upgrade().map(|w| allocated_workspace_size(&w, fallback)).unwrap_or(fallback)
}

/// The margin around the icon rows and the gap between icons.
pub(in crate::window) const ICON_ROW_GAP: i32 = 12;

/// The part of the workspace minimized icons fill.
#[derive(Clone, Copy, Debug)]
pub(in crate::window) struct IconArea {
    pub(in crate::window) left: i32,
    pub(in crate::window) top: i32,
    pub(in crate::window) right: i32,
    pub(in crate::window) bottom: i32,
}

/// The first spot, in rows from the bottom of `area` upwards and in each row
/// from left to right, where a `side` px icon overlaps none of `icons`
/// (`x, y, width, height`). When every spot is taken it shares the
/// bottom-left one.
pub(in crate::window) fn first_free_icon_slot(icons: &[(i32, i32, i32, i32)], area: IconArea, side: i32, gap: i32) -> (i32, i32) {
    let side = side.max(1);
    let step = side + gap.max(0);
    let first = (area.left, (area.bottom - side).max(area.top));
    let overlaps = |x: i32, y: i32| {
        icons.iter().any(|&(ix, iy, iw, ih)| x < ix + iw && ix < x + side && y < iy + ih && iy < y + side)
    };
    let mut y = area.bottom - side;
    while y >= area.top {
        let mut x = area.left;
        while x + side <= area.right {
            if !overlaps(x, y) {
                return (x, y);
            }
            x += step;
        }
        y -= step;
    }
    first
}

/// Keep a dragged or dropped card's origin where its header can be grabbed
/// again: past the left margin, below the top bar, and with its grip on screen.
pub(in crate::window) fn clamp_card_origin<T: PartialOrd + From<i32>>(x: T, y: T, (sw, sh): (i32, i32)) -> (T, T) {
    let clamp = |value: T, low: i32, high: i32| {
        let (low, high) = (T::from(low), T::from(high));
        if value < low {
            low
        } else if value > high {
            high
        } else {
            value
        }
    };
    (clamp(x, 10, (sw - 80).max(10)), clamp(y, 70, (sh - 60).max(70)))
}

/// Change the saved state, then write it off the main thread. The borrow ends
/// before the write is queued: GTK signals that follow may borrow it again.
pub(in crate::window) fn persist(state: &RefCell<AppState>, change: impl FnOnce(&mut AppState)) {
    persist_if(state, |state| {
        change(state);
        true
    });
}

/// [`persist`] for a change that may turn out to change nothing: the state is
/// written only when `change` returns true, and that is returned.
pub(in crate::window) fn persist_if(state: &RefCell<AppState>, change: impl FnOnce(&mut AppState) -> bool) -> bool {
    let snapshot = {
        let mut state = state.borrow_mut();
        change(&mut state).then(|| state.clone())
    };
    let changed = snapshot.is_some();
    if let Some(snapshot) = snapshot {
        crate::state::save_state_async(snapshot);
    }
    changed
}

/// Close one card's session and take the card off the canvas and out of the
/// saved state; false when no card has that session.
pub(in crate::window) fn close_session_card(
    cards: &RefCell<Vec<Rc<MiniTerminalCard>>>,
    canvas: &Fixed,
    state: &RefCell<AppState>,
    ghosts: &GhostLayer,
    sess: &str,
) -> bool {
    // Pull the card out of the shared list and drop the borrow BEFORE
    // touching GTK. `canvas.remove` unparents the whole card subtree
    // (VTE included), and that unmap emits pointer/focus-enter signals
    // which synchronously re-enter the raise handler of the remaining
    // cards. Holding the list borrow across it aborted the daemon with
    // "RefCell already borrowed" (see `SuperDesktopWindow::terminal_cards`).
    let card = {
        let mut cards = cards.borrow_mut();
        cards
            .iter()
            .position(|c| c.data.borrow().session_name == sess)
            .map(|pos| cards.remove(pos))
    };
    let Some(card) = card else { return false };

    card.close_session();
    canvas.remove(&card.container);
    crate::desktop_shell::terminal_removed(canvas);
    persist(state, |s| {
        s.terminals.retain(|t| t.session_name != sess);
        crate::state::normalize_terminal_order(s);
    });
    // The closed card's outline is dropped and the cards it used to hide
    // become visible again.
    ghosts.refresh();
    true
}

/// Put an expanded card on top of the canvas, at the expanded rectangle.
pub(in crate::window) fn place_expanded(canvas: &Fixed, card: &MiniTerminalCard, (sw, sh): (i32, i32)) {
    let (x, y, _, _) = expanded_rect(sw, sh);
    canvas.remove(&card.container);
    canvas.put(&card.container, x, y);
}

/// Move a card to the spot of its current form (the icon spot when it is
/// minimized).
pub(in crate::window) fn place_at_rest(canvas: &Fixed, card: &MiniTerminalCard) {
    let (px, py) = displayed_pos(&card.data.borrow());
    canvas.move_(&card.container, px, py);
}

pub(in crate::window) fn collapse_in_place(canvas: &Fixed, card: &MiniTerminalCard) {
    card.collapse();
    place_at_rest(canvas, card);
}

/// Expand `target` or collapse it, and collapse any other expanded card: at
/// most one card is expanded. `focus` gives the expanded card the keyboard
/// again after it moved to the top of the canvas.
pub(in crate::window) fn set_card_expanded(
    cards: &[Rc<MiniTerminalCard>],
    canvas: &Fixed,
    target: Option<&Rc<MiniTerminalCard>>,
    expand: bool,
    screen: (i32, i32),
    focus: bool,
) {
    let is_target = |card: &Rc<MiniTerminalCard>| target.is_some_and(|target| Rc::ptr_eq(target, card));
    for card in cards.iter().filter(|card| !is_target(card) && card.is_expanded()) {
        collapse_in_place(canvas, card);
    }
    let Some(card) = target else { return };
    if expand {
        card.expand(screen.0, screen.1);
        place_expanded(canvas, card, screen);
        if focus {
            card.focus_terminal();
        }
    } else {
        collapse_in_place(canvas, card);
    }
}


pub(in crate::window) fn apply_terminal_expand(
    terminal_cards: &Rc<RefCell<Vec<Rc<MiniTerminalCard>>>>,
    canvas: &gtk4::Fixed,
    window: &ApplicationWindow,
    ghosts: &Rc<GhostLayer>,
    sw: i32,
    sh: i32,
    session_name: &str,
) {
    // Snapshot the list (cheap `Rc` clones) and release the borrow before the
    // expand/collapse/focus/unparent calls below: those emit pointer- and
    // focus-enter signals that re-enter the raise handlers, which would panic
    // if a list borrow were still alive.
    let cards: Vec<Rc<MiniTerminalCard>> = terminal_cards.borrow().clone();
    let target = cards.iter().find(|c| c.data.borrow().session_name == session_name);
    let will_expand = !target.is_some_and(|card| card.is_expanded());
    set_card_expanded(&cards, canvas, target, will_expand, (sw, sh), true);

    set_overlay_keyboard_mode(window, if will_expand {
        KeyboardMode::Exclusive
    } else {
        KeyboardMode::OnDemand
    });
    // Expanding buries everything under the card that grew; collapsing sets
    // the stacked cards free again.
    ghosts.refresh();
}

