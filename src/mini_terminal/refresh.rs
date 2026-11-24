use super::*;

impl MiniTerminalCard {
    /// Refresh this card alone (one worker, one tmux inventory).
    pub fn refresh_status(&self) {
        run_status_refresh(self.prepare_status_refresh().into_iter().collect());
    }

    /// The worker request for this card's status/title/preview, plus how to
    /// apply the answer on the main thread. `None` for a remote card or while
    /// a previous refresh of this card is still running.
    pub fn prepare_status_refresh(&self) -> Option<PendingRefresh> {
        // A remote card's title, badge and preview come from the host's
        // snapshot: probing this machine's tmux or agent databases for another
        // machine's session would describe the wrong session.
        if self.source.is_remote() {
            return None;
        }
        if self.refresh_in_flight.get() {
            return None;
        }

        let sess_name = self.data.borrow().session_name.clone();
        let agent_type = self.data.borrow().agent_type.clone();
        let want_preview = self.vte.borrow().is_none() && !self.is_compact();
        let preview_lines = if want_preview {
            let h = self.data.borrow().height;
            Some(((h - 70) / 13).clamp(6, 28) as usize)
        } else {
            None
        };

        let status_badge = self.status_badge.downgrade();
        let compact_status = self.chrome.compact.status.downgrade();
        let preview_label = self.chrome.preview_label.downgrade();
        let meta_label = self.meta_label.downgrade();
        let title_label = self.title_label.downgrade();
        let title_prefix = self.title_prefix.clone();
        let opencode_cache = Rc::clone(&self.opencode_session);
        let cached_oc_id: Option<String> = opencode_cache.borrow().clone();
        // Fall back to the persisted id (loaded from state.json at startup)
        // when the in-memory cache is still empty. Other harnesses store their
        // own conversation id there (see `CardUpdate::own_session`).
        let cached_oc_id = cached_oc_id.or_else(|| {
            (agent_type == "opencode").then(|| self.data.borrow().agent_session_id.clone()).flatten()
        });
        // Re-resolve the mapping at most every 30s (immediately on the first
        // refresh). A guess made before a neighbouring console closed
        // otherwise sticks forever, showing that console's prompt here.
        // Skipped entirely for non-opencode cards: no tmux/DB probing.
        let need_resolve = agent_type == "opencode" && {
            let mut slot = self.oc_recheck_at.borrow_mut();
            let now = std::time::Instant::now();
            if cached_oc_id.is_none() || now >= *slot {
                *slot = now + std::time::Duration::from_secs(30);
                true
            } else {
                false
            }
        };
        let notice_parts = (self.notice.downgrade(), self.notice_label.downgrade(),
            Rc::clone(&self.notice_generation), self.chrome.header.downgrade(), self.chrome.compact.top_bar.downgrade());
        let notice_session = sess_name.clone();
        let data_weak = Rc::downgrade(&self.data);
        let data_snapshot: Option<TerminalData> =
            data_weak.upgrade().map(|d| d.borrow().clone());
        let on_persist = Rc::clone(&self.on_session_persist);
        let in_flight = Rc::downgrade(&self.refresh_in_flight);
        let jump = self.jump.clone();
        self.refresh_in_flight.set(true);

        let request = crate::card_status::CardRequest {
            session: sess_name,
            agent: agent_type,
            preview_lines,
            oc_id: cached_oc_id,
            need_resolve,
        };
        let apply = Box::new(move |update: Option<crate::card_status::CardUpdate>| {
            let Some(crate::card_status::CardUpdate { status: status_info, preview, prompt, oc_id, own_session, notice, scrolled_back }) = update else {
                if let Some(flag) = in_flight.upgrade() {
                    flag.set(false);
                }
                return;
            };

            if let (Some(badge), Some(compact)) = (status_badge.upgrade(), compact_status.upgrade()) {
                apply_status_view(&badge, &compact, status_info.status);
            }
            if let Some(scrolled_back) = scrolled_back {
                jump.listed(scrolled_back);
            }

            if let (Some(label), Some(meta)) = (preview_label.upgrade(), meta_label.upgrade()) {
                if let Some(preview) = preview.as_deref() {
                    if label.label().as_str() != preview {
                        label.set_label(preview);
                    }
                }
                let meta_text = format!("PID: {} • {}", status_info.pid, status_info.cmd);
                if meta.label() != meta_text {
                    meta.set_label(&meta_text);
                }
            }

            if let Some(title) = title_label.upgrade() {
                let user_text = prompt;
                let new_title = format_card_title(&title_prefix, user_text.as_deref());
                if title.label().as_str() != new_title {
                    title.set_label(&new_title);
                    title.set_tooltip_text(Some(&new_title));
                    // This refresh runs for local cards only; their title is
                    // part of the published workspace.
                    crate::workspace_model::notify_changed();
                }
            }
            // A setup hint shows once per session, and only on a card on screen.
            let (revealer, label, generation, header, compact) = &notice_parts;
            if let (Some(notice), Some(revealer), Some(label), Some(header), Some(compact)) =
                (notice, revealer.upgrade(), label.upgrade(), header.upgrade(), compact.upgrade())
            {
                if revealer.parent().is_some_and(|card| card.is_mapped())
                    && crate::card_status::claim_setup_notice(&notice_session)
                {
                    reveal_notice((&revealer, &label, generation), (&header, &compact), notice);
                }
            }
            if *opencode_cache.borrow() != oc_id {
                *opencode_cache.borrow_mut() = oc_id.clone();
            }
            // Persist the tmux-pane -> harness-conversation mapping to
            // state.json on first resolution (and when it changes: a stale
            // OpenCode guess heals, Claude's `/clear`, a Codex `/new`) so a
            // later reboot resumes THIS card's conversation (`opencode
            // --session`, `claude --resume`, `codex resume`) instead of every
            // card of a harness sharing the latest one.
            if let Some(new_id) = oc_id.or(own_session) {
                let needs_save = data_snapshot
                    .as_ref()
                    .map(|d| d.agent_session_id.as_deref() != Some(new_id.as_str()))
                    .unwrap_or(true);
                if needs_save {
                    if let Some(d) = data_weak.upgrade() {
                        d.borrow_mut().agent_session_id = Some(new_id.clone());
                        let snapshot = d.borrow().clone();
                        on_persist(&snapshot);
                    }
                }
            }
            if let Some(flag) = in_flight.upgrade() {
                flag.set(false);
            }
        });
        Some(PendingRefresh { request, apply })
    }
}

/// `MiniTerminalCard::show_notice` for the card's notice widgets, so a status
/// refresh holding only weak references can show one too.
pub(in crate::mini_terminal) fn reveal_notice(
    (revealer, label, generation): (&gtk4::Revealer, &Label, &Rc<Cell<u64>>),
    (header, compact_top_bar): (&gtk4::Box, &gtk4::Box),
    notice: crate::command_feedback::Notice,
) {
    for class in crate::command_feedback::TONE_CLASSES {
        label.remove_css_class(class);
    }
    label.add_css_class(notice.tone.css_class());
    label.set_text(notice.text);
    // Below whichever bar the card shows, so the title and buttons stay
    // readable; a card that has not been allocated yet uses a small gap.
    let top = if header.is_visible() {
        header.height()
    } else if compact_top_bar.is_visible() {
        compact_top_bar.height()
    } else {
        0
    };
    revealer.set_margin_top(top + 4);
    revealer.set_visible(true);
    revealer.set_reveal_child(true);
    let next = generation.get().wrapping_add(1);
    generation.set(next);
    let current = Rc::clone(generation);
    let revealer = revealer.downgrade();
    glib::timeout_add_local_once(notice.duration, move || {
        if current.get() != next {
            return;
        }
        if let Some(revealer) = revealer.upgrade() {
            revealer.set_reveal_child(false);
            // Without a frame clock the crossfade never finishes.
            if !revealer.is_mapped() {
                revealer.set_visible(false);
            }
        }
    });
}

/// One card's part of a batched status refresh.
pub struct PendingRefresh {
    pub(in crate::mini_terminal) request: crate::card_status::CardRequest,
    pub(in crate::mini_terminal) apply: Box<dyn FnOnce(Option<crate::card_status::CardUpdate>)>,
}

/// Refresh several cards with one worker and one tmux inventory, then apply
/// each answer on the main thread. Cards dropped meanwhile are skipped (the
/// appliers only hold weak references).
pub fn run_status_refresh(pending: Vec<PendingRefresh>) {
    if pending.is_empty() {
        return;
    }
    let (requests, appliers): (Vec<_>, Vec<_>) =
        pending.into_iter().map(|p| (p.request, p.apply)).unzip();
    glib::MainContext::default().spawn_local(async move {
        let handle = gtk4::gio::spawn_blocking(move || crate::card_status::refresh(requests));
        match handle.await {
            Ok(updates) if updates.len() == appliers.len() => {
                for (apply, update) in appliers.into_iter().zip(updates) {
                    apply(Some(update));
                }
            }
            _ => {
                for apply in appliers {
                    apply(None);
                }
            }
        }
    });
}

pub(in crate::mini_terminal) fn status_view_texts(status: &str) -> (&'static str, &'static str, &'static str) {
    match status {
        "BUSY" | "WORKING" => ("● WORKING", "●", "status-busy"),
        "FINISHED" => ("✓ FINISHED", "✓", "status-idle"),
        "WAITING" => ("◌ WAITING", "◌", "status-idle"),
        "ERROR" => ("⚠ ERROR", "⚠", "status-exited"),
        "UNKNOWN" => ("? UNKNOWN", "?", "status-idle"),
        "EXITED" => ("○ EXITED", "○", "status-exited"),
        // A live card whose session runs on another machine says so, rather
        // than claiming to be idle here.
        "REMOTE" => ("● REMOTE", "●", "status-idle"),
        _ => ("● IDLE", "●", "status-idle"),
    }
}

/// Build the header title: `prefix` (`icon + agent name`) plus the last
/// prompt snippet when available. Keeps the default prefix when prompt is None/empty.
pub(in crate::mini_terminal) fn format_card_title(prefix: &str, last_prompt: Option<&str>) -> String {
    match last_prompt {
        Some(prompt) if !prompt.trim().is_empty() => format!("{prefix} • {prompt}"),
        _ => prefix.to_string(),
    }
}

pub(in crate::mini_terminal) fn apply_status_view(badge: &Label, compact: &Label, status: &str) {
    let (badge_text, compact_text, css_class) = status_view_texts(status);
    let changed = badge.label().as_str() != badge_text || compact.label().as_str() != compact_text;
    if changed {
        for cls in ["status-active", "status-idle", "status-busy", "status-exited"] {
            if badge.has_css_class(cls) {
                badge.remove_css_class(cls);
            }
            if compact.has_css_class(cls) {
                compact.remove_css_class(cls);
            }
        }
        badge.add_css_class(css_class);
        compact.add_css_class(css_class);
        badge.set_label(badge_text);
        compact.set_label(compact_text);
    }
}

