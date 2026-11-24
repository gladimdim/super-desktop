//! Push notifications for FINISHED, WAITING, and ERROR card transitions.
use super::api::{ApiError, InlineKeyboardMarkup, TelegramApi};
use super::config::{mask_token_in_text, Config, RuntimeState};
use super::desktop::{CardSummary, DesktopBackend};
use super::redact::{escape_html, redact_known_secrets, truncate_chars};
use super::topics::{
    card_action_keyboard_with_app, extract_done_summary, format_assistant_done_summary,
    screen_shows_active_work, tail_output_snippet, waiting_keyboard,
};
use std::collections::{HashMap, HashSet};

pub const IDLE_FINISH_SETTLE_TICKS: u8 = 4;

#[derive(Default)]
pub struct NotifierState {
    seeded: bool,
    last_completion_id: HashMap<String, Option<String>>,
    last_status: HashMap<String, String>,
    saw_working: HashSet<String>,
    idle_after_work_streak: HashMap<String, u8>,
    last_done_summary_fp: HashMap<String, String>,
    waiting_streak: HashMap<String, u8>,
    notified_waiting: HashSet<String>,
    notified_error: HashSet<String>,
}

impl NotifierState {
    pub fn tick(
        &mut self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        cards: &[CardSummary],
    ) -> bool {
        let mut runtime_changed = false;
        let live_ids: HashSet<&str> = cards.iter().map(|c| c.id.as_str()).collect();
        self.last_completion_id
            .retain(|id, _| live_ids.contains(id.as_str()));
        self.last_status
            .retain(|id, _| live_ids.contains(id.as_str()));
        self.saw_working
            .retain(|id| live_ids.contains(id.as_str()));
        self.idle_after_work_streak
            .retain(|id, _| live_ids.contains(id.as_str()));
        self.last_done_summary_fp
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
                self.last_status
                    .insert(card.id.clone(), card.status.clone());
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

        let app_url_for = |card_id: &str| {
            cfg.mini_app
                .card_deep_link(cfg.bot_username.as_deref(), card_id)
        };

        for card in cards {
            let prev_cid = self
                .last_completion_id
                .get(&card.id)
                .cloned()
                .flatten();
            let prev_status = self.last_status.get(&card.id).cloned();

            let mut completed_now = false;
            let mut status_finished_now = false;
            let mut idle_settled_now = false;

            if card.completion_supported {
                completed_now = card.completion_state == "completed"
                    && card.completion_id.is_some()
                    && card.completion_id != prev_cid;
                self.saw_working.remove(&card.id);
                self.idle_after_work_streak.remove(&card.id);
            } else {
                if card.status == "WORKING" {
                    self.saw_working.insert(card.id.clone());
                    self.idle_after_work_streak.remove(&card.id);
                } else if card.status == "ERROR" || card.completion_state == "error" {
                    self.saw_working.remove(&card.id);
                    self.idle_after_work_streak.remove(&card.id);
                } else if card.status == "WAITING" {
                    self.idle_after_work_streak.remove(&card.id);
                }

                status_finished_now =
                    card.status == "FINISHED" && prev_status.as_deref() != Some("FINISHED");
                if !card.is_shell
                    && card.status == "IDLE"
                    && self.saw_working.contains(&card.id)
                    && !status_finished_now
                {
                    let streak = self
                        .idle_after_work_streak
                        .entry(card.id.clone())
                        .and_modify(|n| *n = n.saturating_add(1))
                        .or_insert(1);
                    if *streak >= IDLE_FINISH_SETTLE_TICKS {
                        idle_settled_now = true;
                    }
                }
            }

            if let Some(cid) = card.completion_id.as_ref() {
                self.last_completion_id
                    .insert(card.id.clone(), Some(cid.clone()));
            }

            if completed_now || status_finished_now || idle_settled_now {
                let mut hold_for_active_screen = false;
                let summary: Option<(String, bool)> = if cfg.permissions.read_output {
                    if let Some(native_msg) = backend.final_assistant_message(&card.id) {
                        format_assistant_done_summary(
                            &native_msg,
                            &cfg.bot_token,
                            cfg.permissions.redact_secrets,
                            18,
                            1400,
                        )
                        .map(|s| (s, true))
                    } else if let Ok(raw) = backend.capture_screen(&card.id, 60) {
                        if idle_settled_now && screen_shows_active_work(&raw) {
                            hold_for_active_screen = true;
                            None
                        } else {
                            extract_done_summary(
                                &raw,
                                &cfg.bot_token,
                                cfg.permissions.redact_secrets,
                                16,
                                1200,
                            )
                            .map(|s| (s, false))
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                if hold_for_active_screen {
                    self.idle_after_work_streak
                        .insert(card.id.clone(), IDLE_FINISH_SETTLE_TICKS.saturating_sub(1));
                } else {
                    let skip_duplicate_idle = if idle_settled_now {
                        if cfg.permissions.read_output {
                            match summary.as_ref() {
                                None => true,
                                Some((text, _)) => {
                                    self.last_done_summary_fp.get(&card.id) == Some(text)
                                }
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    self.saw_working.remove(&card.id);
                    self.idle_after_work_streak.remove(&card.id);
                    if let Some((ref text, _)) = summary {
                        self.last_done_summary_fp
                            .insert(card.id.clone(), text.clone());
                    }

                    if !skip_duplicate_idle && cfg.permissions.notifications {
                        let mut html = format!(
                            "✅ <b>Done</b> · {} {} · <code>{}</code>",
                            escape_html(&card.icon),
                            escape_html(&card.agent_name),
                            escape_html(&card.directory_display)
                        );
                        if let Some(prompt) =
                            card.last_prompt.as_deref().filter(|p| !p.trim().is_empty())
                        {
                            let masked = mask_token_in_text(prompt.trim(), &cfg.bot_token);
                            let redacted = if cfg.permissions.redact_secrets {
                                redact_known_secrets(&masked)
                            } else {
                                masked
                            };
                            let short_prompt = truncate_chars(&redacted, 140);
                            html.push_str(&format!("\n<i>{}</i>", escape_html(&short_prompt)));
                        }
                        if let Some((text, is_prose)) = summary {
                            if is_prose {
                                html.push_str(&format!(
                                    "\n\n<blockquote>{}</blockquote>",
                                    escape_html(&text)
                                ));
                            } else {
                                html.push_str(&format!(
                                    "\n\n<pre><code>{}</code></pre>",
                                    escape_html(&text)
                                ));
                            }
                        }
                        let app_url = app_url_for(&card.id);
                        let kb = card_action_keyboard_with_app(
                            card,
                            cfg.permissions,
                            app_url.as_deref(),
                        );
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
                            let masked = mask_token_in_text(prompt.trim(), &cfg.bot_token);
                            let redacted = if cfg.permissions.redact_secrets {
                                redact_known_secrets(&masked)
                            } else {
                                masked
                            };
                            let short_prompt = truncate_chars(&redacted, 180);
                            html.push_str(&format!("\nPrompt: <i>{}</i>", escape_html(&short_prompt)));
                        }
                        if cfg.permissions.read_output {
                            if let Ok(raw) = backend.capture_screen(&card.id, 0) {
                                if let Some(snip) = tail_output_snippet(
                                    &raw,
                                    &cfg.bot_token,
                                    cfg.permissions.redact_secrets,
                                    22,
                                    1500,
                                ) {
                                    html.push_str(&format!(
                                        "\n<pre><code>{}</code></pre>",
                                        escape_html(&snip)
                                    ));
                                }
                            }
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
                        let app_url = app_url_for(&card.id);
                        let kb = card_action_keyboard_with_app(
                            card,
                            cfg.permissions,
                            app_url.as_deref(),
                        );
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

            self.last_status
                .insert(card.id.clone(), card.status.clone());
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
