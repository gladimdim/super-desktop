//! Push notifications for FINISHED, WAITING, and ERROR card transitions.
use super::api::{ApiError, InlineKeyboardMarkup, TelegramApi};
use super::config::{Config, RuntimeState};
use super::desktop::CardSummary;
use super::redact::escape_html;
use super::topics::{card_action_keyboard, waiting_keyboard};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct NotifierState {
    seeded: bool,
    last_completion_id: HashMap<String, Option<String>>,
    waiting_streak: HashMap<String, u8>,
    notified_waiting: HashSet<String>,
    notified_error: HashSet<String>,
}

impl NotifierState {
    pub fn tick(
        &mut self,
        api: &dyn TelegramApi,
        cfg: &Config,
        runtime: &mut RuntimeState,
        cards: &[CardSummary],
    ) -> bool {
        let mut runtime_changed = false;
        let live_ids: HashSet<&str> = cards.iter().map(|c| c.id.as_str()).collect();
        self.last_completion_id
            .retain(|id, _| live_ids.contains(id.as_str()));
        self.waiting_streak
            .retain(|id, _| live_ids.contains(id.as_str()));
        self.notified_waiting
            .retain(|id| live_ids.contains(id.as_str()));
        self.notified_error
            .retain(|id| live_ids.contains(id.as_str()));

        if !self.seeded {
            for card in cards {
                self.last_completion_id
                    .insert(card.id.clone(), card.completion_id.clone());
                if card.status == "WAITING" {
                    self.notified_waiting.insert(card.id.clone());
                }
                if card.status == "ERROR" || card.completion_state == "error" {
                    self.notified_error.insert(card.id.clone());
                }
            }
            self.seeded = true;
            return false;
        }

        let Some(owner) = cfg.allowed_user.as_ref() else {
            return false;
        };

        for card in cards {
            let prev_cid = self
                .last_completion_id
                .get(&card.id)
                .cloned()
                .flatten();

            let completed_now = card.completion_supported
                && card.completion_state == "completed"
                && card.completion_id.is_some()
                && card.completion_id != prev_cid;

            if let Some(cid) = card.completion_id.as_ref() {
                self.last_completion_id
                    .insert(card.id.clone(), Some(cid.clone()));
            }

            if completed_now && cfg.permissions.notifications {
                let mut html = format!(
                    "✅ <b>Finished</b> · {} {} · <code>{}</code>",
                    escape_html(&card.icon),
                    escape_html(&card.agent_name),
                    escape_html(&card.directory_display)
                );
                if let Some(prompt) = card.last_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
                    html.push_str(&format!("\nPrompt: <i>{}</i>", escape_html(prompt)));
                }
                let kb = card_action_keyboard(card, cfg.permissions);
                if send_card_alert(api, cfg, runtime, owner.chat_id, &card.id, &html, kb.as_ref()) {
                    runtime_changed = true;
                }
            }

            if card.status == "WAITING" {
                let streak = self
                    .waiting_streak
                    .entry(card.id.clone())
                    .and_modify(|n| *n = n.saturating_add(1))
                    .or_insert(1);
                if *streak >= 2 && !self.notified_waiting.contains(&card.id) {
                    self.notified_waiting.insert(card.id.clone());
                    if cfg.permissions.notifications {
                        let mut html = format!(
                            "🟡 <b>Waiting for input</b> · {} {} · <code>{}</code>",
                            escape_html(&card.icon),
                            escape_html(&card.agent_name),
                            escape_html(&card.directory_display)
                        );
                        if let Some(prompt) =
                            card.last_prompt.as_deref().filter(|p| !p.trim().is_empty())
                        {
                            html.push_str(&format!("\nPrompt: <i>{}</i>", escape_html(prompt)));
                        }
                        let kb = waiting_keyboard(card, cfg.permissions);
                        if send_card_alert(
                            api,
                            cfg,
                            runtime,
                            owner.chat_id,
                            &card.id,
                            &html,
                            kb.as_ref(),
                        ) {
                            runtime_changed = true;
                        }
                    }
                }
            } else {
                self.waiting_streak.remove(&card.id);
                self.notified_waiting.remove(&card.id);
            }

            let is_error = card.status == "ERROR" || card.completion_state == "error";
            if is_error {
                if !self.notified_error.contains(&card.id) {
                    self.notified_error.insert(card.id.clone());
                    if cfg.permissions.notifications {
                        let html = format!(
                            "🔴 <b>Error</b> · {} {} · <code>{}</code>",
                            escape_html(&card.icon),
                            escape_html(&card.agent_name),
                            escape_html(&card.directory_display)
                        );
                        let kb = card_action_keyboard(card, cfg.permissions);
                        if send_card_alert(
                            api,
                            cfg,
                            runtime,
                            owner.chat_id,
                            &card.id,
                            &html,
                            kb.as_ref(),
                        ) {
                            runtime_changed = true;
                        }
                    }
                }
            } else {
                self.notified_error.remove(&card.id);
            }
        }

        runtime_changed
    }
}

fn send_card_alert(
    api: &dyn TelegramApi,
    cfg: &Config,
    runtime: &mut RuntimeState,
    private_chat_id: i64,
    card_id: &str,
    html: &str,
    keyboard: Option<&InlineKeyboardMarkup>,
) -> bool {
    let mut changed = false;
    let mut sent_to_topic = false;

    if let Some(forum) = cfg.forum.as_ref() {
        if let Some(idx) = runtime
            .topics
            .iter()
            .position(|t| t.card_id == card_id && !t.closed && !t.deleted_in_telegram)
        {
            let thread_id = runtime.topics[idx].thread_id;
            match api.send_message(forum.chat_id, Some(thread_id), html, None, keyboard) {
                Ok(msg_id) => {
                    runtime.record_message_card(forum.chat_id, msg_id, card_id);
                    sent_to_topic = true;
                    changed = true;
                }
                Err(ApiError::TopicNotFound) => {
                    runtime.topics[idx].deleted_in_telegram = true;
                    changed = true;
                }
                Err(_) => {}
            }
        }
    }

    if !sent_to_topic || cfg.mirror_alerts_to_private {
        if let Ok(msg_id) = api.send_message(private_chat_id, None, html, None, keyboard) {
            runtime.record_message_card(private_chat_id, msg_id, card_id);
            changed = true;
        }
    }
    changed
}
