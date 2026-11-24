use super::*;

impl MiniTerminalCard {
    /// A local card on a 1024×768 screen whose overlay callbacks do nothing
    /// but report saves to `on_save`.
    #[cfg(test)]
    pub fn for_test(data: TerminalData, on_save: impl Fn(gtk4::Widget, &TerminalData) + 'static) -> Self {
        Self::new(
            data, |_, _, _| {}, on_save, |_| {}, |_| {}, |_, _, _, _, _| {}, || {}, |_| {}, |_| {}, || {},
            1024, 768, None, Some(Rc::new(Vec::new())), HoverRaiseLock::new(), CardSource::Local,
        )
    }

    pub fn desktop_presentation(&self) -> crate::workspace_model::CardPresentation {
        crate::workspace_model::CardPresentation {
            title: self.title_label.label().to_string(), expanded: self.is_expanded(),
        }
    }

    /// Expanded state and the title are presentation, not saved state, so a
    /// local card reports their changes to desktop event subscribers itself. A
    /// remote card mirrors another PC and never describes this one.
    pub(in crate::mini_terminal) fn notify_host_change(&self) {
        if self.remote.is_none() {
            crate::workspace_model::notify_changed();
        }
    }

    pub fn is_expanded(&self) -> bool {
        *self.expanded.borrow()
    }

    pub fn is_compact(&self) -> bool {
        !self.is_expanded() && self.data.borrow().iconified
    }

    pub fn size(&self, screen_w: i32, screen_h: i32) -> (f64, f64) {
        if self.is_expanded() {
            let (_, _, w, h) = expanded_rect(screen_w, screen_h);
            (w, h)
        } else {
            (
                self.data.borrow().width as f64,
                self.data.borrow().height as f64,
            )
        }
    }

    /// Where this card is drawn on the overlay canvas, in whichever form is
    /// currently up: the 80% expanded card, the 128×128 icon, or the plain
    /// card. Shared by the slide animation and the overlap ghosts.
    pub fn canvas_rect(&self, screen_w: i32, screen_h: i32) -> crate::card_resize::Rect {
        let (width, height) = self.size(screen_w, screen_h);
        let (x, y) = if self.is_expanded() {
            let (x, y, _, _) = expanded_rect(screen_w, screen_h);
            (x, y)
        } else {
            displayed_pos(&self.data.borrow())
        };
        crate::card_resize::Rect {
            x,
            y,
            width: width.round() as i32,
            height: height.round() as i32,
        }
    }

    /// True while the user is working in this card, which keeps the dotted
    /// ghost outlines of the cards it covers (and its own) off the screen.
    ///
    /// Expanded counts, and so do the pointer being on the card and any
    /// keystroke meant for it — but that last one only for [`ACTIVE_FOR`] after
    /// the last sign of use. Signals that expire on their own are the point:
    /// GTK focus would keep a card "in use" forever, because hover-raise grabs
    /// it and nothing releases it when the user moves on (see [`CardActivity`]).
    pub fn user_is_active(&self) -> bool {
        self.is_expanded() || self.activity.is_active(Instant::now())
    }

    /// Drop the "the pointer is over this card" state.
    ///
    /// Called when the overlay is unmapped: an unmap does not always deliver a
    /// pointer leave, and a card that still believed the pointer was on it
    /// would suppress the outlines of everything it covers after the next show.
    pub fn forget_pointer(&self) {
        self.activity.set_pointer_inside(false);
    }

    pub fn expand(&self, screen_w: i32, screen_h: i32) {
        self.expand_with_policy(screen_w, screen_h, true);
    }

    pub(in crate::mini_terminal) fn expand_with_policy(&self, screen_w: i32, screen_h: i32, interactive: bool) {
        if *self.expanded.borrow() {
            return;
        }
        *self.expanded.borrow_mut() = true;
        self.notify_host_change();

        let (x, y, w, h) = expanded_rect(screen_w, screen_h);
        *self.visual_pos.borrow_mut() = (x, y);
        self.container.set_size_request(w as i32, h as i32);
        self.chrome.paint_expanded(true);

        if self.vte.borrow().is_none() {
            self.attach_vte_with_policy(None, interactive);
        } else if let Some(term) = self.vte.borrow().as_ref() {
            set_card_font(term, true);
        }

        self.apply_chrome();
        if interactive { self.focus_terminal(); }
    }

    pub fn collapse(&self) {
        if !*self.expanded.borrow() {
            return;
        }
        *self.expanded.borrow_mut() = false;
        self.notify_host_change();
        *self.visual_pos.borrow_mut() = displayed_pos(&self.data.borrow());

        let iconified = self.data.borrow().iconified;
        if iconified {
            self.detach_vte();
        } else if let Some(term) = self.vte.borrow().as_ref() {
            set_card_font(term, false);
        }

        let w = self.data.borrow().width;
        let h = self.data.borrow().height;
        self.container.set_size_request(w, h);
        self.chrome.paint_expanded(false);
        self.apply_chrome();
        self.refresh_status();
    }

    /// Iconify or restore this card through its own button action.
    ///
    /// Returns whether the state changed, so a caller can tell "already there"
    /// from "cannot". A remote command must not invent a state the local UI
    /// could not reach, which is why the action, not the geometry, is reused.
    /// Decide where this card's icon goes the first time it is minimized.
    pub fn set_first_icon_spot(&self, spot: Rc<dyn Fn(&str, i32) -> Option<(i32, i32)>>) {
        *self.first_icon_spot.borrow_mut() = Some(spot);
    }

    pub fn set_iconified(&self, iconified: bool) -> bool {
        if self.data.borrow().iconified == iconified || self.is_expanded() {
            return false;
        }
        let action = if iconified {
            self.iconify_action.borrow().clone()
        } else {
            self.restore_action.borrow().clone()
        };
        match action {
            Some(action) => {
                action();
                true
            }
            None => false,
        }
    }

    /// Local CLI presentation changes never prepare a saved launcher or
    /// explicitly focus the terminal. Interactive and bridge actions keep
    /// their existing attachment policy.
    pub(crate) fn cli_set_mode(&self, action: crate::control::ModeAction, width: i32, height: i32) -> bool {
        use crate::control::ModeAction;
        match action {
            ModeAction::Minimize | ModeAction::Restore => {
                self.cli_mode_change.set(true);
                let changed = self.set_iconified(action == ModeAction::Minimize);
                self.cli_mode_change.set(false);
                changed
            }
            ModeAction::Expand => {
                self.expand_with_policy(width, height, false);
                true
            }
            ModeAction::Collapse => {
                self.collapse();
                true
            }
        }
    }

    /// Asks on this card, next to its ✕, whether `asker` (a plugin) may close
    /// it, and gives the answer: a plugin never ends a session without the
    /// user saying so. Clicking elsewhere is a no. Returns false, without
    /// asking, when the card cannot show the question now or already does.
    pub fn confirm_close(&self, asker: &str, answer: Box<dyn FnOnce(bool)>) -> bool {
        let anchor = if self.is_compact() { self.chrome.compact.close.clone() } else { self.chrome.close_btn.clone() };
        let asking = std::iter::successors(anchor.first_child(), |w| w.next_sibling()).any(|w| w.is::<gtk4::Popover>());
        if !anchor.is_mapped() || asking {
            return false;
        }
        let answer = Rc::new(RefCell::new(Some(answer)));
        let reply = {
            let answer = Rc::clone(&answer);
            move |yes: bool| {
                if let Some(answer) = answer.borrow_mut().take() {
                    answer(yes);
                }
            }
        };
        let popover = gtk4::Popover::new();
        popover.add_css_class("ws-pop");
        popover.add_css_class("card-close-confirm");
        let column = gtk4::Box::new(Orientation::Vertical, 8);
        column.add_css_class("card-close-confirm-box");
        let question = Label::new(Some(&format!("{asker} wants to close this card.\nIts session ends.")));
        question.set_xalign(0.0);
        column.append(&question);
        let buttons = gtk4::Box::new(Orientation::Horizontal, 6);
        buttons.set_halign(Align::End);
        let cancel = Button::with_label("Cancel");
        let close = Button::with_label("Close");
        close.add_css_class("destructive-action");
        buttons.append(&cancel);
        buttons.append(&close);
        column.append(&buttons);
        popover.set_child(Some(&column));
        for (button, yes) in [(&cancel, false), (&close, true)] {
            let (reply, weak) = (reply.clone(), popover.downgrade());
            button.connect_clicked(move |_| {
                reply(yes);
                if let Some(popover) = weak.upgrade() {
                    popover.popdown();
                }
            });
        }
        popover.connect_closed(move |popover| {
            reply(false);
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.set_parent(&anchor);
        popover.popup();
        cancel.grab_focus();
        true
    }

    /// Shows the card buttons of the plugins running now.
    pub fn set_plugin_buttons(&self, specs: &[crate::plugin_ui::card_buttons::Spec]) {
        if let Some(row) = &self.plugin_buttons {
            row.set(specs);
        }
    }

    /// The header's ✕, where a plugin's close request asks.
    #[cfg(test)]
    pub fn close_button(&self) -> &Button {
        &self.chrome.close_btn
    }

    #[cfg(test)]
    pub fn plugin_buttons(&self) -> Option<&crate::plugin_ui::card_buttons::ButtonRow> {
        self.plugin_buttons.as_ref()
    }

    pub(crate) fn cli_raise(&self) { (self.on_raise)(self.container.clone().upcast()); }
    pub(crate) fn cli_focus(&self) -> bool {
        self.cli_raise();
        self.vte.borrow().as_ref().is_some_and(|vte| vte.grab_focus())
    }
    pub(crate) fn cli_set_tag(&self,tag:u8) {
        self.data.borrow_mut().tag=tag;
        for button in self.cli_tag_buttons.borrow().iter().filter_map(|b|b.upgrade()) {crate::tag::apply_tag(&button,tag);}
    }

    /// Apply a geometry commit exactly like a local resize gesture: stored
    /// size, restored size, chrome and persisted state. Returns whether the
    /// card was ready to take one.
    pub fn apply_geometry(&self, rect: crate::card_resize::Rect) -> bool {
        let Some(commit) = self.geometry_commit.borrow().clone() else {
            return false;
        };
        commit(rect);
        true
    }

    /// What the card's harness is doing, as its badge shows it: "working",
    /// "waiting", "finished", "idle", "error", "exited", "unknown" or "remote".
    pub fn status_word(&self) -> String {
        self.status_badge.label().split_whitespace().last().unwrap_or("IDLE").to_lowercase()
    }

    /// An explicit keyboard selection restores compact cards and protects the
    /// selected terminal from the pointer left over a different card.
    pub fn select_with_keyboard(&self) {
        self.set_iconified(false);
        let session = self.data.borrow().session_name.clone();
        self.hover_lock.lock_for(&session, Duration::from_secs(1));
        self.hover_lock.note_hover(&session, &self.container);
        (self.on_raise)(self.container.clone().upcast());
        self.focus_terminal();
    }

    pub fn focus_terminal(&self) {
        // A launch or an expand is a deliberate "I am working here" (see
        // `CardActivity`): it keeps the outlines of the covered cards off the
        // screen for a moment, and expires on its own like everything else.
        self.activity.note_activity(Instant::now());
        if let Some(term) = self.vte.borrow().as_ref() {
            term.grab_focus();
        }
    }

    pub(in crate::mini_terminal) fn apply_chrome(&self) {
        let vte_attached = self.vte.borrow().is_some();
        self.chrome.apply(
            self.is_expanded(),
            self.data.borrow().width,
            self.data.borrow().height,
            self.data.borrow().iconified,
            vte_attached,
        );
    }

    /// Press the card's ⛶ button, as the user does.
    #[cfg(test)]
    pub fn press_expand_button(&self) {
        self.chrome.expand_btn.emit_clicked();
    }

    /// What the card's footer is telling the user, so a view can check that the
    /// host's state reached the chrome.
    #[cfg(test)]
    pub fn footer_text(&self) -> String {
        self.chrome.hint_label.label().to_string()
    }

    /// Show a brief, non-blocking line about a command's result, just under
    /// the chrome, and dismiss it after the notice's own duration.
    pub fn show_notice(&self, notice: crate::command_feedback::Notice) {
        reveal_notice(
            (&self.notice, &self.notice_label, &self.notice_generation),
            (&self.chrome.header, &self.chrome.compact.top_bar),
            notice,
        );
    }

    /// The notice on screen right now, if any.
    #[cfg(test)]
    pub fn notice_text(&self) -> Option<String> {
        self.notice
            .reveals_child()
            .then(|| self.notice_label.text().to_string())
    }

    /// The tone class the notice carries, for checks of its colour.
    #[cfg(test)]
    pub fn notice_has_class(&self, class: &str) -> bool {
        self.notice_label.has_css_class(class)
    }

    /// Whether the header is on screen. A compact or expanded card is not
    /// showing it, and a view sizing a card's terminal has to know.
    pub fn header_visible(&self) -> bool {
        self.chrome.header.is_visible()
    }

    pub fn remote_session(&self) -> Option<&Rc<RemoteSession>> {
        self.remote.as_ref()
    }

    /// The workspace changed size: the local overlay changed output/scale,
    /// or a remote view switched between Fit and 100%, zoomed, or resized.
    pub fn set_workspace_size(&self, width: i32, height: i32) {
        self.workspace.set((width.max(1), height.max(1)));
    }

    /// The local workspace went from `from` to `to` (another display or
    /// scale): move the card's place and its icon's place in proportion (see
    /// `arrange::follow_workspace`), shrink it if it no longer fits, and size
    /// the widget for its current form. Moving the widget is the caller's.
    pub fn follow_workspace(&self, from: (i32, i32), to: (i32, i32)) {
        self.set_workspace_size(to.0, to.1);
        let scale = self.fit.get().2;
        {
            let mut d = self.data.borrow_mut();
            // An icon's card keeps its place and size for the restore.
            let (width, height) = if d.iconified {
                (d.restored_width, d.restored_height)
            } else {
                (d.width, d.height)
            };
            let rect = crate::card_resize::Rect { x: f64::from(d.x), y: f64::from(d.y), width, height };
            let rect = crate::arrange::follow_workspace(rect, from, to, min_card_size(scale));
            (d.x, d.y) = (rect.x as i32, rect.y as i32);
            (d.restored_width, d.restored_height) = (rect.width, rect.height);
            if !d.iconified {
                (d.width, d.height) = (rect.width, rect.height);
            }
            if let (Some(x), Some(y)) = (d.icon_x, d.icon_y) {
                let side = icon_side(scale);
                let icon = crate::card_resize::Rect { x: f64::from(x), y: f64::from(y), width: side, height: side };
                let icon = crate::arrange::follow_workspace(icon, from, to, (side, side));
                (d.icon_x, d.icon_y) = (Some(icon.x as i32), Some(icon.y as i32));
            }
        }
        if self.is_expanded() {
            let (x, y, w, h) = expanded_rect(to.0, to.1);
            *self.visual_pos.borrow_mut() = (x, y);
            self.container.set_size_request(w as i32, h as i32);
        } else {
            *self.visual_pos.borrow_mut() = displayed_pos(&self.data.borrow());
            let (w, h) = (self.data.borrow().width, self.data.borrow().height);
            self.container.set_size_request(w, h);
        }
        self.apply_chrome();
    }

    /// The scale this card's emulator font was last fitted at.
    #[cfg(test)]
    pub fn fit_scale(&self) -> f64 {
        self.fit.get().2
    }

    /// The bounds a resize of this card starts with right now.
    #[cfg(test)]
    pub fn resize_limits(&self) -> crate::card_resize::Limits {
        workspace_limits(self.workspace.get(), self.fit.get().2)
    }

    /// Remember the size and fit scale the owning view drew this card at, and
    /// size the emulator's font for the host's grid.
    pub fn set_fit(&self, body_width: f64, body_height: f64, scale: f64) {
        self.fit.set((body_width, body_height, scale));
        let Some(terminal) = self.vte.borrow().as_ref().cloned() else {
            return;
        };
        let grid = self.remote.as_ref().and_then(|session| session.grid());
        crate::card_source::fit_font(
            &terminal,
            body_width,
            body_height,
            grid,
            scale,
            &self.font_fit,
        );
    }

    /// Fit the emulator's font again for the body the view last drew this
    /// card at. Nothing is touched unless the emulator's space or font changed
    /// since (its body was allocated, or something else set a font).
    pub fn refit(&self) {
        let (body_width, body_height, scale) = self.fit.get();
        self.set_fit(body_width, body_height, scale);
    }

    /// Show what the host reports about this card: its title, whether its
    /// session is alive, and anything to say about the stream.
    pub fn apply_host_state(&self, title: &str, alive: Option<bool>, message: Option<&str>) {
        let title = title.trim();
        if !title.is_empty() && self.title_label.label() != title {
            self.title_label.set_label(title);
        }
        apply_status_view(
            &self.status_badge,
            &self.chrome.compact.status,
            match alive {
                Some(false) => "EXITED",
                // The host's session is alive but its state is the host's
                // business: this card only claims that it is remote.
                _ => "REMOTE",
            },
        );
        let (workspace, session) = {
            let data = self.data.borrow();
            (data.workspace_dir.clone(), data.session_name.clone())
        };
        let meta = match workspace.as_deref().and_then(|dir| {
            std::path::Path::new(dir)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }) {
            Some(name) => format!("Remote · {name}"),
            None => format!("Remote · {session}"),
        };
        if self.meta_label.label() != meta {
            self.meta_label.set_label(&meta);
        }
        *self.chrome.source_message.borrow_mut() = message.map(str::to_string);
        self.paint_source_message();
    }

    /// Mirror the host's own card mode. The host decides whether a card is
    /// iconified or expanded, so this never asks for anything: it is what a
    /// snapshot does to a card.
    pub fn mirror_host_mode(&self, iconified: bool, expanded: bool) {
        let was_icon = self.data.borrow().iconified;
        {
            let mut data = self.data.borrow_mut();
            data.iconified = iconified;
        }
        *self.expanded.borrow_mut() = expanded;
        if iconified && !was_icon {
            // An icon is not a terminal: stop drawing and stop streaming.
            self.detach_vte();
            if let Some(session) = &self.remote {
                session.detach();
            }
        }
        self.apply_chrome();
        self.paint_source_message();
    }

    /// Adopt the size the owning view fitted this card to, without saving
    /// anything or asking for anything: the host's rectangle is the truth.
    pub fn adopt_host_geometry(&self, width: i32, height: i32) {
        self.container.set_size_request(width, height);
        *self.visual_pos.borrow_mut() = displayed_pos(&self.data.borrow());
        self.apply_chrome();
        self.paint_source_message();
    }

    /// The footer hint, or the body when there is nothing to show there,
    /// carries whatever the card's source is saying.
    pub(in crate::mini_terminal) fn paint_source_message(&self) {
        if !self.source.is_remote() {
            return;
        }
        let message = self.chrome.source_message.borrow().clone();
        if self.vte.borrow().is_none() && !self.is_compact() && !self.is_expanded() {
            self.chrome.preview_label
                .set_text(message.as_deref().unwrap_or("Connecting…"));
            self.chrome.preview_label.set_visible(true);
        }
        self.chrome.hint_label
            .set_label(&card_hint_or(&self.chrome.source_message, CARD_HINT));
    }

    pub fn attach_vte(&self) {
        self.attach_vte_with_inventory(None);
    }

    /// Unmap the VTE widget without detaching tmux. Hide uses this so a
    /// GPU-heavy local model cannot stall compositor frames on live terminals.
    /// A terminal that other cards hide stays hidden when the overlay comes
    /// back (see [`VteDrawing`]).
    pub fn set_vte_drawing(&self, drawing: bool) {
        // Out of the borrow: hiding a widget can emit signals that re-enter
        // this card.
        let term = self.vte.borrow().clone();
        self.vte_drawing.set_mapped(drawing, term.as_ref());
    }

    /// Slide this card with a still image of its terminal instead of the live
    /// emulator. A moving card is drawn again on every frame, and a terminal's
    /// screen (thousands of glyphs, its cell backgrounds, overlapping fills)
    /// was most of the GPU time of a slide: on a 3440×1440 120 Hz display
    /// driven by a handheld APU, four live terminals held the slide-in to
    /// ~75 FPS, and ~118 FPS without them. The texture is rendered once here,
    /// and moving it costs one textured quad per frame. The emulator keeps
    /// reading its PTY and comes back with its current screen at `thaw`.
    ///
    /// `false` when the terminal has not been laid out since it was shown
    /// again: try again on a later frame.
    pub fn freeze_for_slide(&self) -> bool {
        // Out of the borrow: hiding a widget can emit signals that re-enter
        // this card.
        let term = self.vte.borrow().clone();
        let was_frozen = self.vte_drawing.frozen();
        let done = freeze_terminal(term.as_ref(), &self.vte_drawing, &self.slide_frame);
        if let (false, true, Some(term)) = (was_frozen, self.vte_drawing.frozen(), term.as_ref()) {
            self.slide_watch.watch(term, self.slide_look());
        }
        done
    }

    /// The slide is over: the live terminal again.
    pub fn thaw_after_slide(&self) {
        let term = self.vte.borrow().clone();
        self.slide_watch.forget();
        thaw_terminal(term.as_ref(), &self.vte_drawing, &self.slide_frame);
    }

    /// The overlay is about to show again. The image taken at the hide stays
    /// for the slide-in when it still shows this terminal exactly: no output
    /// since, same emulator, same size and mode. The slide then starts without
    /// laying out and drawing the live terminal on its first frame and
    /// rendering a new image on its second, the two heaviest frames of a show.
    /// Otherwise the live terminal slides in and is frozen anew.
    pub fn keep_slide_frame_for_show(&self) {
        let term = self.vte.borrow().clone();
        if !self.slide_watch.still_shows(term.as_ref(), self.slide_look()) {
            self.thaw_after_slide();
        }
    }

    /// What the card's terminal looks like on screen, for `SlideFrameWatch`.
    pub(in crate::mini_terminal) fn slide_look(&self) -> SlideLook {
        let data = self.data.borrow();
        (data.width, data.height, self.is_expanded(), data.iconified)
    }

    /// The cards painted above hide this terminal completely (`true`), so it
    /// stops drawing, or no longer do. See `overlap_ghost::hidden_indexes`.
    pub fn set_vte_covered(&self, covered: bool) {
        let term = self.vte.borrow().clone();
        self.vte_drawing.set_covered(covered, term.as_ref());
    }

    /// True when this card's terminal can stop drawing without side effects.
    ///
    /// It needs a terminal that does not hold the keyboard focus (hiding would
    /// drop it) and that was laid out at `rect`'s size, so its PTY and the tmux
    /// client already have the card's grid: a terminal that never drew would
    /// attach at the emulator's default size and resize tmux when it finally
    /// appears, and one hidden before a pending resize would keep tmux at the
    /// old size. A card allocated larger than `rect` fails the same check,
    /// since the planner would miss the part that sticks out. A terminal that
    /// is already paused may stay so; one just shown again has no size until
    /// the next frame, and pauses at a later refresh (the overlay runs one
    /// every second).
    pub fn vte_can_pause(&self, canvas: &gtk4::Fixed, rect: crate::card_resize::Rect) -> bool {
        let Some(term) = self.vte.borrow().clone() else {
            return false;
        };
        if !term.is_visible() {
            return true;
        }
        if term.is_focus() || term.width() <= 0 || term.height() <= 0 {
            return false;
        }
        // The card's 1.5px agent border puts its border box half a pixel
        // outside the allocation on each side, so an up-to-date card measures
        // 1px more than `rect`; a stale one is off by a whole resize.
        self.container.compute_bounds(canvas).is_some_and(|bounds| {
            (bounds.width() as f64 - rect.width as f64).abs() <= 2.0
                && (bounds.height() as f64 - rect.height as f64).abs() <= 2.0
        })
    }

    /// True while this card is dragged (the window's drag handler marks it).
    pub fn is_being_dragged(&self) -> bool {
        self.container.has_css_class("dragging")
    }

    /// Open this iconified card at `width`×`height` with an emulator that runs
    /// nothing, for GTK tests that must not start tmux sessions.
    #[cfg(test)]
    pub fn open_with_bare_terminal(&self, width: i32, height: i32) {
        {
            let mut data = self.data.borrow_mut();
            data.iconified = false;
            data.width = width;
            data.height = height;
        }
        self.container.set_size_request(width, height);
        let term = VteTerminal::new();
        term.set_hexpand(true);
        term.set_vexpand(true);
        self.preview_box.append(&term);
        *self.vte.borrow_mut() = Some(term);
        self.apply_chrome();
    }

    /// Whether this card's emulator widget is shown, for tests of the wiring.
    #[cfg(test)]
    pub fn vte_visible(&self) -> bool {
        self.vte.borrow().as_ref().is_some_and(|term| term.is_visible())
    }

    /// This card's emulator widget, for tests that move the keyboard focus.
    #[cfg(test)]
    pub fn vte_widget(&self) -> Option<VteTerminal> {
        self.vte.borrow().clone()
    }

    /// This card's own `tmux attach` client, to pause while the overlay stays
    /// hidden (see `hidden_pause`). `None` for a remote card, an iconified one
    /// (no VTE) and a VTE that has not spawned its client yet.
    pub fn attach_target(&self) -> Option<crate::hidden_pause::Target> {
        if self.remote.is_some() || self.session_task.is_closed() {
            return None;
        }
        let pty = self.vte.borrow().as_ref()?.pty()?;
        Some(crate::hidden_pause::Target {
            session: self.data.borrow().session_name.clone(),
            tty: crate::platform::pty::name(pty.fd()).ok()?,
        })
    }

    pub(in crate::mini_terminal) fn attach_vte_with_inventory(&self, inventory: Option<Arc<crate::tmux::SessionInventory>>) {
        self.attach_vte_with_policy(inventory, true);
    }

    pub(in crate::mini_terminal) fn attach_vte_with_policy(&self, inventory: Option<Arc<crate::tmux::SessionInventory>>, prepare: bool) {
        if self.vte.borrow().is_some() {
            return;
        }
        spawn_vte(
            &self.vte,
            &self.preview_box,
            &self.data,
            self.is_expanded(),
            &self.on_toggle,
            &self.expanded,
            &self.session_task,
            inventory,
            self.hover_lock.clone(),
            &self.activity,
            self.remote.clone(),
            &self.fit,
            &self.font_fit,
            prepare,
        );
    }

    pub(crate) fn cli_session_task(&self) -> Arc<crate::session_task::SessionTask> {
        Arc::clone(&self.session_task)
    }

    /// Cancel attachment/preparation without scheduling a name-based kill.
    /// The CLI worker already holds the preparation lock and owns destruction.
    pub(crate) fn detach_for_cli_close(&self) {
        self.session_task.close();
        self.detach_vte();
    }

    pub fn close_session(&self) {
        if let Some(session) = &self.remote {
            // The host owns the session: this card only stops reading it.
            session.stop();
            self.detach_vte();
            return;
        }
        self.session_task.close();
        self.detach_vte();
        let task = Arc::clone(&self.session_task);
        let session = self.data.borrow().session_name.clone();
        gtk4::gio::spawn_blocking(move || {
            task.finish_close(|| crate::tmux::kill_session(&session));
        });
    }

    pub fn detach_vte(&self) {
        remove_vte(&self.vte, &self.preview_box);
    }

    pub fn apply_theme(&self, theme: &crate::theme::OmarchyTheme) {
        // A still image would keep the old colors and font.
        self.thaw_after_slide();
        if let Some(path) = crate::brand::logo_path(&self.data.borrow().agent_type, theme.mode == "light") {
            for image in &self.brand_images { image.set_from_file(Some(&path)); }
        }
        let Some(term) = self.vte.borrow().as_ref().cloned() else {
            return;
        };
        if self.source.is_remote() {
            // A remote emulator's font follows the host's grid, not a fixed
            // size: refit it, in the new theme's family if that changed.
            apply_vte_colors(&term, theme);
            self.refit();
            return;
        }
        let font_size = if self.is_expanded() { 11 } else { 10 };
        let font = gtk4::pango::FontDescription::from_string(&format!("{} {}", theme.font_family, font_size));
        term.set_font(Some(&font));
        apply_vte_colors(&term, theme);
    }

}
