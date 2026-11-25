use super::*;

impl SuperDesktopWindow {
    pub fn item_counts(&self) -> (usize, usize) {
        (self.note_cards.borrow().len(), self.terminal_cards.borrow().len())
    }

    pub fn close_terminal(&self, sess: &str) -> bool {
        close_session_card(&self.terminal_cards, &self.canvas, &self.state, &self.ghosts, sess)
    }

    pub fn relaunch_terminal(&self, sess: &str, resume: bool) -> Result<(), &'static str> {
        let card = self
            .terminal_cards
            .borrow()
            .iter()
            .find(|c| c.data.borrow().session_name == sess)
            .cloned()
            .ok_or("no_such_session")?;
        let snapshot = card.data.borrow().clone();
        crate::control_relaunch::prepare_for_bridge(&snapshot, resume, None).map_err(|(code, _)| code)?;
        card.restart(resume);
        Ok(())
    }

    /// Move one local card to a host-pixel origin and raise it.
    ///
    /// Used when a remote viewer drops the card. The icon and the open card
    /// keep separate spots; which one moves is the card's own state, not the
    /// viewer's. Expanded cards stay put: their on-screen rectangle is
    /// transient and must not overwrite the saved origin.
    pub fn move_terminal_card(&self, card_id: &str, x: i32, y: i32) -> Result<(), &'static str> {
        let card = self.terminal_card(card_id)?;
        let (x, y) = crate::remote_workspace::clamp_card_origin(
            self.screen_width().max(0) as u32,
            self.screen_height().max(0) as u32,
            x,
            y,
        );
        crate::mini_terminal::set_displayed_pos(&mut card.data.borrow_mut(), x, y);
        let (px, py) = crate::mini_terminal::displayed_pos(&card.data.borrow());
        self.canvas.move_(&card.container, px, py);
        raise_canvas_child(&self.canvas, &card.container);
        raise_canvas_child(&self.canvas, &self.hud);
        {
            let mut list = self.terminal_cards.borrow_mut();
            if let Some(pos) = list.iter().position(|c| c.data.borrow().id == card_id) {
                let raised = list.remove(pos);
                list.push(raised);
            }
        }
        let data = card.data.borrow().clone();
        let saved = persist_if(&self.state, |state| {
            let Some(saved) = state.terminals.iter_mut().find(|t| t.id == card_id) else {
                return false;
            };
            saved.x = data.x;
            saved.y = data.y;
            saved.icon_x = data.icon_x;
            saved.icon_y = data.icon_y;
            state.terminal_order.retain(|id| id != card_id);
            state.terminal_order.push(card_id.to_string());
            true
        });
        if !saved {
            return Err("unknown_card");
        }
        self.ghosts.refresh();
        Ok(())
    }

    /// Apply one validated workspace command to this window's own cards.
    ///
    /// Identity, epoch, revision and bounds are already checked by the bridge
    /// and the owner IPC; what is left here are the card's own state rules, and
    /// the same actions its local buttons and gestures run. An expanded card
    /// refuses layout commands because its rectangle is transient, and an
    /// iconified card keeps its square: only its icon spot moves.
    ///
    /// Returns the card a create made, so the caller can report the published
    /// id and revision for it.
    pub fn apply_workspace_command(
        &self,
        command: &crate::desktop_protocol::WorkspaceCommand,
    ) -> Result<Option<String>, &'static str> {
        use crate::desktop_protocol::WorkspaceCommand as Command;
        match command {
            Command::SetLayout { card_id, layout, .. } => {
                self.set_terminal_card_layout(card_id, layout).map(|()| None)
            }
            Command::CloseTerminal { card_id, .. } => {
                self.close_terminal_card(card_id).map(|()| None)
            }
            Command::CreateTerminal {
                agent_type,
                workspace,
            } => self.create_terminal_card(agent_type, workspace).map(Some),
            Command::SetExpanded {
                card_id, expanded, ..
            } => self
                .set_terminal_card_expanded(card_id, *expanded)
                .map(|()| None),
            Command::SetWorkspace { workspace, .. } => {
                self.set_workspace_folder(workspace).map(|()| None)
            }
        }
    }

    /// Create one harness card at a viewer's request, on this machine.
    ///
    /// The viewer can only name a harness this host offers and the folder this
    /// host published (both are checked before this runs), so a create can
    /// never choose a command, a flag or an arbitrary path. The card is built
    /// exactly like a local launch, and without forcing this overlay to show.
    pub(in crate::window) fn create_terminal_card(
        &self,
        agent_type: &str,
        workspace: &str,
    ) -> Result<String, &'static str> {
        if !crate::tmux::HARNESS_KEYS.contains(&agent_type)
            && !self.state.borrow().custom_harnesses.iter().any(|item| item.id == agent_type && item.validate().is_ok()) {
            return Err("unsupported_harness");
        }
        let Some(directory) = crate::state::clean_dir(workspace) else {
            return Err("invalid_workspace");
        };
        let session = self.create_new_terminal_in(agent_type, None, None, None, Some(&directory));
        if session.is_empty() || !crate::tmux::session_exists(&session) {
            return Err("terminal_unavailable");
        }
        Ok(session)
    }

    /// Put this machine's workspace folder where a viewer asked for it.
    ///
    /// The folder has to be one this host itself offers: the same list the
    /// snapshot publishes is the menu and the permission, so a viewer can never
    /// point this machine at a path it did not offer. The change is this
    /// machine's own, visible and persisted like typing it here.
    pub(in crate::window) fn set_workspace_folder(&self, workspace: &str) -> Result<(), &'static str> {
        let known = {
            let state = self.state.borrow();
            crate::workspace_model::offered_folders(&state, &crate::state::effective_workspace_dir(&state))
        };
        if !known.iter().any(|folder| folder == workspace) {
            return Err("invalid_workspace");
        }
        let Some(directory) = crate::state::clean_dir(workspace) else {
            return Err("invalid_workspace");
        };
        persist(&self.state, |state| {
            state.workspace_dir = Some(directory.clone());
            crate::state::remember_workspace_dir(state, &directory);
        });
        self.ws_bar.show_folder(&directory);
        Ok(())
    }

    /// Expand or collapse one card at a viewer's request.
    ///
    /// The card's rectangle, its VTE and the other cards' outlines end up
    /// exactly as they would after a local double-click on its header. The
    /// host's own layer-shell keyboard mode is left alone: a viewer may look
    /// into this card, but it does not take this machine's keyboard.
    pub(in crate::window) fn set_terminal_card_expanded(
        &self,
        card_id: &str,
        expanded: bool,
    ) -> Result<(), &'static str> {
        // An expanded card accepts this one: collapsing is how it stops being
        // expanded, and `SetLayout` is the command that stays refused.
        let card = self.any_terminal_card(card_id)?;
        if card.is_expanded() == expanded {
            return Ok(());
        }
        // At most one card is expanded, exactly like a local expand.
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        set_card_expanded(
            &cards,
            &self.canvas,
            Some(&card),
            expanded,
            (self.screen_width(), self.screen_height()),
            false,
        );
        // Cards the expanded one covers lose their outlines, and a collapse
        // sets them free again.
        self.ghosts.refresh();
        Ok(())
    }

    /// Move, resize and iconify one card to a viewer's request, clamped to this
    /// machine's own screen and persisted like the matching local gesture.
    pub(in crate::window) fn set_terminal_card_layout(
        &self,
        card_id: &str,
        layout: &crate::desktop_protocol::CardLayout,
    ) -> Result<(), &'static str> {
        let card = self.terminal_card(card_id)?;
        if card.data.borrow().iconified != layout.iconified
            && !card.set_iconified(layout.iconified)
        {
            return Err("terminal_unavailable");
        }
        // A resize only applies to a card the host shows at its own size: an
        // iconified card is a fixed square, and the local UI cannot resize one
        // either. Clamping both sides is what keeps a move from silently
        // resizing a card the host's screen no longer fits.
        let (width, height) = clamp_card_size(
            layout.width as i32,
            layout.height as i32,
            self.screen_width(),
            self.screen_height(),
        );
        let current = {
            let data = card.data.borrow();
            (
                clamp_card_size(
                    data.width,
                    data.height,
                    self.screen_width(),
                    self.screen_height(),
                ),
                data.iconified,
            )
        };
        if !current.1 && (width, height) != current.0 {
            card.apply_geometry(crate::card_resize::Rect {
                x: layout.x as f64,
                y: layout.y as f64,
                width,
                height,
            });
        }
        // The card keeps two spots: which one this command addresses is the
        // mode it is in, exactly as the viewer drew it.
        let (x, y) = if layout.iconified {
            (
                layout.icon_x.unwrap_or(layout.x),
                layout.icon_y.unwrap_or(layout.y),
            )
        } else {
            (layout.x, layout.y)
        };
        self.move_terminal_card(card_id, x, y)
    }

    /// Close one card at a viewer's request. The host owns the lifecycle: the
    /// card, its widget and its session go together, exactly like the local
    /// close button. Closing writes no geometry, so it is allowed on an
    /// expanded card — a close must win over a layout gesture.
    pub(in crate::window) fn close_terminal_card(&self, card_id: &str) -> Result<(), &'static str> {
        let card = self.any_terminal_card(card_id)?;
        let session = card.data.borrow().session_name.clone();
        if self.close_terminal(&session) {
            Ok(())
        } else {
            Err("unknown_card")
        }
    }

    /// Find one card by the desktop snapshot's own id, refusing the ones whose
    /// rectangle is transient. Every layout command starts here, so identity
    /// and the expanded-card rule cannot drift between them.
    pub(in crate::window) fn terminal_card(&self, card_id: &str) -> Result<Rc<MiniTerminalCard>, &'static str> {
        let card = self.any_terminal_card(card_id)?;
        if card.is_expanded() {
            return Err("terminal_expanded");
        }
        Ok(card)
    }

    /// Identity check only; the caller decides what an expanded card may take.
    pub(in crate::window) fn any_terminal_card(&self, card_id: &str) -> Result<Rc<MiniTerminalCard>, &'static str> {
        if !crate::desktop_protocol::valid_card_id(card_id) {
            return Err("unknown_card");
        }
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        cards
            .into_iter()
            .find(|card| card.data.borrow().id == card_id)
            .ok_or("unknown_card")
    }

    pub fn raise_all_notes(&self) {
        let notes: Vec<Rc<StickyNote>> = self.note_cards.borrow().clone();
        raise_notes_on_canvas(&self.canvas, &notes);
        raise_canvas_child(&self.canvas, &self.hud);
    }

    pub fn reload_theme(&self) {
        let theme = crate::styles::reload_styles();
        crate::plugin_ui::apply_theme(&theme);
        crate::editor::theme_changed();
        // Snapshot before restyling: `apply_theme` touches VTE/fonts, which
        // can emit signals back into our handlers.
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        for card in cards.iter() {
            card.apply_theme(&theme);
        }
        // Remote consoles too: their colors are set only when an emulator is
        // built and here, never again while its font is fitted.
        self.machine_view.apply_theme(&theme);
        // Swap monochrome toolbar logos for the new mode (light/dark).
        let light_theme = theme.mode == "light";
        for (img, agent) in self.brand_images.borrow().iter() {
            if let Some(logo) = crate::brand::logo_path(agent, light_theme) {
                img.set_from_file(Some(logo));
            }
        }
        // The settings card keeps its own logo images: re-detect + re-render
        // them for the new mode (it re-reads the stored selection too).
        (self.settings_refresh)();
    }

    /// A window opened somewhere on the desktop (Hyprland's `openwindow`).
    /// If it took the keyboard from the overlay, or is about to, the overlay
    /// takes it back (see `focus_guard`).
    pub fn window_opened_elsewhere(&self) {
        if !overlay_owns_input(&self.window) {
            return;
        }
        let has_keyboard = self.window.is_active();
        let reclaim = FOCUS_GUARD.with(|guard| {
            guard.borrow_mut().window_opened(std::time::Instant::now(), has_keyboard)
        });
        if reclaim {
            reclaim_keyboard(&self.window);
        }
    }

    /// Re-map a window that was hidden with `hide_now`.
    ///
    /// Every card, VTE and `tmux attach` client is still alive, so the overlay
    /// is back on screen within a frame — instead of the full rebuild (state
    /// load, panels, one `tmux` exec per card, ~30 forks) that used to sit
    /// between the shortcut and the overlay appearing.
    pub fn show_again(&self) {
        crate::frame_profile::note_request("show");
        // First: paused tmux clients redraw within a few ms of this, before
        // the slide-in brings their cards on screen.
        self.hidden_pause.on_shown();
        self.hidden_notify_gen.set(self.hidden_notify_gen.get().wrapping_add(1));
        self.show_token.set(self.show_token.get().wrapping_add(1));
        self.reclaim_input();
        // Before drawing is re-enabled, so a terminal whose image is stale
        // comes back live rather than for one frame as an image.
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        for card in &cards {
            card.keep_slide_frame_for_show();
        }
        self.set_terminal_gpu_mapped(true);
        crate::desktop_shell::before_present(&self.window);
        self.window.present();
        self.window.set_visible(true);
        // Re-assert keyboard interactivity: typing in a card flips it to
        // Exclusive (see `apply_terminal_expand`), and a re-mapped layer surface
        // must not come back without it.
        set_overlay_keyboard_mode(&self.window, KeyboardMode::OnDemand);
        self.start_slide_in();
        // Card statuses went stale while off screen (the periodic refresh is
        // paused then); this refreshes them on worker threads.
        self.periodic_refresh();
        // A fresh install's first show introduces the app, once.
        let first_show = !self.state.borrow().welcome_tour_seen;
        if first_show {
            self.open_welcome_tour();
        }
    }

    /// Open the welcome tour over the local workspace and remember it was
    /// shown, so it never opens by itself again.
    pub fn open_welcome_tour(&self) {
        // Settings (where a replay comes from) closes; the tour opens below.
        for panel in &self.overlay_panels {
            panel.set_visible(false);
        }
        self.pairing_wizard.close();
        let combo = crate::shortcut::current_combo(self.state.borrow().toggle_shortcut.as_deref());
        persist_if(&self.state, |state| !std::mem::replace(&mut state.welcome_tour_seen, true));
        self.welcome_tour.open(&combo);
    }

    /// Put waiting pairing requests in front of the user: the approval panel.
    pub fn review_pairing_requests(&self) -> bool {
        self.pairing_requests.open();
        true
    }

    /// Token to hand back to `hide_if_unchanged` when a slide-out starts.
    pub fn current_show_token(&self) -> u64 {
        self.show_token.get()
    }

    /// Hide once the slide-out finished — unless the overlay was shown again
    /// while it was animating, in which case the unmap would undo that show.
    pub fn hide_if_unchanged(&self, token: u64) {
        if self.show_token.get() == token {
            self.hide_now();
        }
    }

    /// Unmap the window but keep the whole widget tree alive for the next show.
    pub(in crate::window) fn hide_now(&self) {
        // Outlines are hints on a visible desk: they do not survive the unmap.
        self.ghosts.suspend();
        // An unmap does not always deliver a pointer leave, and a card that
        // still believed the pointer was on it would keep suppressing the
        // outlines of the cards it covers after the next show.
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        for card in cards.iter() {
            card.forget_pointer();
        }
        // Stop a vsync tick that may never fire (GPU stall) from later
        // painting or calling `on_slide_hidden` after we already unmapped.
        if self.slide.running.get() {
            self.slide.motion.take().report("slide out, cut short by the unmap");
        }
        self.slide.running.set(false);
        self.slide.gen.set(self.slide.gen.get().wrapping_add(1));
        // Drop live terminal surfaces with the unmap: nothing composites a GPU
        // terminal buffer while the overlay is hidden, and `show_again`
        // re-enables drawing for the next show. The slide-out's still images
        // stay: the next slide-in reuses those that still show their terminal
        // (`MiniTerminalCard::keep_slide_frame_for_show`).
        self.set_terminal_gpu_mapped(false);
        self.slide.freeze_pending.set(false);
        // Settings keeps its current page when its parent window is unmapped.
        self.welcome_tour.close();
        self.command_palette.root.set_visible(false);
        self.pairing_wizard.close();
        self.pairing_requests.close();
        // …and neither may the workspace list, which is not part of the
        // overlay's widget tree (it is its own popup surface).
        self.ws_popover.popdown();
        self.window.set_visible(false);
        // Remote consoles are only released once the overlay is gone: stopping
        // them earlier blanked the view before it could animate out. Their
        // cards keep the last frame, so showing again reconnects immediately.
        self.machine_view.suspend_streams();
        self.arm_hidden_pause();
        self.arm_hidden_notifications();
    }

    pub fn arm_hidden_notifications(&self) {
        let gen = self.hidden_notify_gen.get().wrapping_add(1);
        self.hidden_notify_gen.set(gen);
        let working: Vec<(String, String, String)> = self
            .terminal_cards
            .borrow()
            .iter()
            .filter(|c| c.status_name() == "WORKING")
            .map(|c| {
                let d = c.data.borrow();
                (d.session_name.clone(), d.agent_type.clone(), c.title_text())
            })
            .collect();
        if working.is_empty() {
            return;
        }
        let notify_gen = Rc::clone(&self.hidden_notify_gen);
        let cards_w = Rc::downgrade(&self.terminal_cards);
        let watched = Rc::new(RefCell::new(working));
        glib::timeout_add_local(Duration::from_secs(3), move || {
            if notify_gen.get() != gen {
                return glib::ControlFlow::Break;
            }
            let current = watched.borrow().clone();
            if current.is_empty() {
                return glib::ControlFlow::Break;
            }
            let requests: Vec<crate::card_status::CardRequest> = current
                .iter()
                .map(|(session, agent, _)| crate::card_status::CardRequest {
                    session: session.clone(),
                    agent: agent.clone(),
                    preview_lines: None,
                    oc_id: None,
                    need_resolve: false,
                })
                .collect();
            let watched_c = Rc::clone(&watched);
            let notify_gen_c = Rc::clone(&notify_gen);
            let cards_c = std::rc::Weak::clone(&cards_w);
            glib::MainContext::default().spawn_local(async move {
                let Ok(updates) =
                    gtk4::gio::spawn_blocking(move || crate::card_status::refresh(requests)).await
                else {
                    return;
                };
                if notify_gen_c.get() != gen {
                    return;
                }
                let titles: HashMap<String, String> = cards_c
                    .upgrade()
                    .map(|rc| {
                        rc.borrow()
                            .iter()
                            .map(|c| (c.data.borrow().session_name.clone(), c.title_text()))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut still_working = Vec::new();
                for ((session, agent, old_title), update) in current.into_iter().zip(updates) {
                    let title = titles.get(&session).cloned().unwrap_or(old_title);
                    let st = update.status.status;
                    crate::mini_terminal::observe_hidden_status(&session, &title, st);
                    if st == "WORKING" {
                        still_working.push((session, agent, title));
                    }
                }
                *watched_c.borrow_mut() = still_working;
            });
            glib::ControlFlow::Continue
        });
    }

    /// Render one small image with the window's renderer while the overlay is
    /// still unmapped, so the first show does not pay for the renderer's first
    /// off-screen render. That one is slow where the GPU has gone to sleep
    /// since the daemon started (a laptop's discrete GPU: one to two seconds
    /// on the main thread, in the middle of the first slide-in). Right after
    /// start the GPU is still awake from GTK's own initialization.
    pub fn prewarm_renderer(&self) {
        let started = std::time::Instant::now();
        // Creates the surface and its renderer; the window stays unmapped.
        WidgetExt::realize(&self.window);
        let Some(renderer) = self.window.renderer() else {
            return;
        };
        let layout = self.window.create_pango_layout(Some("SUPER DESKTOP 0123456789 ─│┌┐█"));
        let snapshot = gtk4::Snapshot::new();
        snapshot.append_color(
            &gdk::RGBA::BLACK,
            &gtk4::graphene::Rect::new(0.0, 0.0, 256.0, 64.0),
        );
        snapshot.append_layout(&layout, &gdk::RGBA::WHITE);
        if let Some(node) = snapshot.to_node() {
            let _ = renderer.render_texture(&node, None);
        }
        crate::frame_profile::note_duration("renderer warm-up", started);
    }

    /// The overlay is off screen (unmapped, or built but never shown): pause
    /// the local cards' tmux clients if it stays that way (`hidden_pause`).
    pub fn arm_hidden_pause(&self) {
        let cards = Rc::clone(&self.terminal_cards);
        self.hidden_pause.on_hidden(move || {
            let cards: Vec<Rc<MiniTerminalCard>> = cards.borrow().clone();
            cards.iter().filter_map(|card| card.attach_target()).collect()
        });
    }

    /// The daemon is about to exit: wake any paused tmux client first.
    pub fn resume_hidden_clients(&self) {
        self.hidden_pause.resume_before_exit();
    }

    /// Give the keyboard and pointer back to the desktop as the hide starts.
    ///
    /// The unmap waits for the slide-out (up to `HIDE_FALLBACK`), and until
    /// then a full-screen surface that still takes input swallows the keys
    /// and clicks meant for the window underneath (with a card expanded, the
    /// keyboard is even `Exclusive`). The window-system adapter lets clicks
    /// through; `reclaim_input` undoes both on the next show.
    pub(in crate::window) fn release_input(&self) {
        INPUT_RELEASED.with(|released| released.set(true));
        FOCUS_GUARD.with(|guard| guard.borrow_mut().reset());
        set_overlay_keyboard_mode(&self.window, KeyboardMode::None);
        crate::desktop_shell::set_pointer_input(&self.window, false);
    }

    pub(in crate::window) fn reclaim_input(&self) {
        INPUT_RELEASED.with(|released| released.set(false));
        FOCUS_GUARD.with(|guard| guard.borrow_mut().reset());
        crate::desktop_shell::set_pointer_input(&self.window, true);
    }

    /// Hide VTE widgets so hide/unmap does not composite live GPU terminals.
    ///
    /// Mapping again only brings back the terminals no card hides: the overlap
    /// layer pauses the buried ones (see `MiniTerminalCard::set_vte_covered`).
    /// A show starts a slide, which suspends that layer and so lets them all
    /// draw until the cards are at rest.
    pub(in crate::window) fn set_terminal_gpu_mapped(&self, mapped: bool) {
        // Snapshot: hiding a widget can emit signals that re-enter the card
        // list (see `terminal_cards`).
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        for card in cards.iter() {
            card.set_vte_drawing(mapped);
        }
    }

    pub(in crate::window) fn periodic_refresh(&self) {
        if crate::theme::check_theme_changed() {
            self.reload_theme();
        }
        // Safety net for the overlap ghosts: the pointer, focus, drag and resize
        // callbacks cover the interactive cases, and this catches the rest. It
        // also picks the outlines back up when the settle of the slide-in was
        // never reported (a stalled compositor skips that frame tick), so an
        // idle desk always ends up with the ghosts it should have.
        if !self.slide.running.get() {
            self.ghosts.resume();
        }
        // One worker and one `tmux list-panes -a` for every card, instead of
        // several tmux processes per card per second.
        let cards: Vec<Rc<MiniTerminalCard>> = self.terminal_cards.borrow().clone();
        crate::mini_terminal::run_status_refresh(
            cards.iter().filter_map(|card| card.prepare_status_refresh()).collect(),
        );
    }

}
