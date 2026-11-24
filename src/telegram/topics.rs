//! Forum Topics synchronization (1 card = 1 Telegram Forum Topic) and shared keyboards.
use super::api::{ApiError, InlineButton, InlineKeyboardMarkup, TelegramApi};
use super::config::{Config, Permissions, RuntimeState, TopicBinding, MAX_TOPICS};
use super::desktop::CardSummary;
use super::redact::{escape_html, status_emoji, tag_to_topic_color, topic_title};

pub const TOPIC_RENAME_MIN_INTERVAL_SECS: u64 = 60;
pub const TOPIC_STATUS_DEBOUNCE_SECS: u64 = 4;
pub const TOPIC_STATUS_RENAME_COOLDOWN_SECS: u64 = 10;

pub fn card_action_keyboard(card: &CardSummary, perms: Permissions) -> Option<InlineKeyboardMarkup> {
    let mut row1 = Vec::new();
    if perms.read_output {
        row1.push(InlineButton::callback("📷 Screen", format!("scr:{}", card.id)));
        row1.push(InlineButton::callback("📜 History", format!("his:{}", card.id)));
        row1.push(InlineButton::callback("📎 Files", format!("fil:{}", card.id)));
    }
    let can_control = perms.controls && (!card.is_shell || perms.shell_input);
    let mut row2 = Vec::new();
    if can_control {
        row2.push(InlineButton::callback("⏎ Enter", format!("key:e:{}", card.id)));
        row2.push(InlineButton::callback("⛔ Ctrl-C", format!("key:c:{}", card.id)));
    }
    if perms.close {
        row2.push(InlineButton::callback("🗑 Close", format!("cls:{}", card.id)));
    }
    let mut rows = Vec::new();
    if !row1.is_empty() {
        rows.push(row1);
    }
    if !row2.is_empty() {
        rows.push(row2);
    }
    (!rows.is_empty()).then_some(InlineKeyboardMarkup {
        inline_keyboard: rows,
    })
}

pub fn waiting_keyboard(card: &CardSummary, perms: Permissions) -> Option<InlineKeyboardMarkup> {
    let mut rows = Vec::new();
    let can_control = perms.controls && (!card.is_shell || perms.shell_input);
    if can_control {
        rows.push(vec![
            InlineButton::callback("Yes", format!("key:y:{}", card.id)),
            InlineButton::callback("No", format!("key:n:{}", card.id)),
            InlineButton::callback("1", format!("key:1:{}", card.id)),
            InlineButton::callback("2", format!("key:2:{}", card.id)),
        ]);
        rows.push(vec![
            InlineButton::callback("⏎ Enter", format!("key:e:{}", card.id)),
            InlineButton::callback("Esc", format!("key:x:{}", card.id)),
            InlineButton::callback("⛔ Ctrl-C", format!("key:c:{}", card.id)),
        ]);
    }
    if perms.read_output {
        rows.push(vec![
            InlineButton::callback("📷 Screen", format!("scr:{}", card.id)),
            InlineButton::callback("📎 Files", format!("fil:{}", card.id)),
        ]);
    }
    (!rows.is_empty()).then_some(InlineKeyboardMarkup {
        inline_keyboard: rows,
    })
}

pub fn format_card_header_html(card: &CardSummary) -> String {
    let dot = status_emoji(&card.status);
    let mut out = format!(
        "{dot} <b>{} {}</b> · <code>{}</code>\nStatus: <b>{}</b>",
        escape_html(&card.icon),
        escape_html(&card.agent_name),
        escape_html(&card.directory_display),
        escape_html(&card.label)
    );
    if let Some(model) = card.model.as_deref().filter(|m| !m.is_empty()) {
        out.push_str(&format!(" · {}", escape_html(model)));
        if let Some(effort) = card.effort.as_deref().filter(|e| !e.is_empty()) {
            out.push_str(&format!(" ({})", escape_html(effort)));
        }
    }
    if let Some(title) = card.title.as_deref().filter(|t| !t.trim().is_empty()) {
        out.push_str(&format!("\nPrompt: <i>{}</i>", escape_html(title)));
    }
    out
}

pub fn format_topic_header_html(card: &CardSummary) -> String {
    format!(
        "{}\n\n<i>Send messages here to talk directly to this card.</i>",
        format_card_header_html(card)
    )
}

pub fn topic_header_keyboard(card: &CardSummary, perms: Permissions) -> Option<InlineKeyboardMarkup> {
    if card.status == "WAITING" {
        waiting_keyboard(card, perms)
    } else {
        card_action_keyboard(card, perms)
    }
}

fn status_card_fingerprint(html: &str, kb: Option<&InlineKeyboardMarkup>) -> String {
    format!("{html}\0{}", serde_json::to_string(&kb).unwrap_or_default())
}

fn format_closed_topic_header_html(topic_name: &str) -> String {
    format!(
        "🛑 <b>{}</b>\nStatus: <b>Closed on PC</b>",
        escape_html(topic_name)
    )
}

pub fn sync_topics(
    api: &dyn TelegramApi,
    cfg: &Config,
    runtime: &mut RuntimeState,
    cards: &[CardSummary],
    now_unix: u64,
    force_recreate_deleted: bool,
) -> bool {
    let Some(forum) = cfg.forum.as_ref() else {
        return false;
    };
    let mut changed = false;

    for card in cards {
        let desired_name = topic_title(
            &card.status,
            &card.icon,
            &card.agent_name,
            &card.directory_display,
            card.title.as_deref(),
        );
        let color = tag_to_topic_color(card.tag);

        let existing_idx = runtime.topics.iter().position(|t| t.card_id == card.id);
        match existing_idx {
            Some(idx) => {
                let deleted = runtime.topics[idx].deleted_in_telegram;
                if deleted && !force_recreate_deleted {
                    continue;
                }
                if deleted && force_recreate_deleted {
                    if let Some(binding) = create_binding(
                        api,
                        forum.chat_id,
                        card,
                        &desired_name,
                        color,
                        now_unix,
                        cfg.permissions,
                    ) {
                        if let Some(msg_id) = binding.status_message_id {
                            runtime.record_message_card(forum.chat_id, msg_id, &card.id);
                        }
                        runtime.topics[idx] = binding;
                        changed = true;
                    }
                    continue;
                }

                let thread_id = runtime.topics[idx].thread_id;
                if runtime.topics[idx].closed {
                    match api.reopen_forum_topic(forum.chat_id, thread_id) {
                        Ok(()) => {
                            runtime.topics[idx].closed = false;
                            changed = true;
                        }
                        Err(ApiError::TopicNotFound) => {
                            runtime.topics[idx].deleted_in_telegram = true;
                            changed = true;
                            continue;
                        }
                        Err(_) => continue,
                    }
                }

                // A. Best-effort one-time silent pin (upgrades pre-existing topics).
                if let Some(msg_id) = runtime.topics[idx].status_message_id {
                    if !runtime.topics[idx].status_pinned {
                        match api.pin_chat_message(forum.chat_id, msg_id, true) {
                            Ok(()) => {
                                runtime.topics[idx].status_pinned = true;
                                changed = true;
                            }
                            Err(ApiError::TopicNotFound) => {
                                runtime.topics[idx].deleted_in_telegram = true;
                                changed = true;
                                continue;
                            }
                            Err(ApiError::MessageNotFound) => {
                                runtime.topics[idx].status_message_id = None;
                                runtime.topics[idx].last_status_card = None;
                                runtime.topics[idx].status_pinned = true;
                                changed = true;
                            }
                            Err(ApiError::BadRequest(_)) => {
                                // Missing pin permission etc. — don't retry every 2s.
                                runtime.topics[idx].status_pinned = true;
                                changed = true;
                            }
                            Err(_) => {}
                        }
                    }
                }

                // B. Live silent status header card edit (no topic rename service line).
                let desired_header = format_topic_header_html(card);
                let desired_kb = topic_header_keyboard(card, cfg.permissions);
                let desired_fp = status_card_fingerprint(&desired_header, desired_kb.as_ref());
                if let Some(msg_id) = runtime.topics[idx].status_message_id {
                    if runtime.topics[idx].last_status_card.as_deref() != Some(desired_fp.as_str()) {
                        match api.edit_message_text(
                            forum.chat_id,
                            msg_id,
                            &desired_header,
                            desired_kb.as_ref(),
                        ) {
                            Ok(()) => {
                                runtime.topics[idx].last_status_card = Some(desired_fp);
                                changed = true;
                            }
                            Err(ApiError::TopicNotFound) => {
                                runtime.topics[idx].deleted_in_telegram = true;
                                changed = true;
                                continue;
                            }
                            Err(ApiError::MessageNotFound) => {
                                runtime.topics[idx].status_message_id = None;
                                runtime.topics[idx].last_status_card = None;
                                changed = true;
                            }
                            Err(_) => {}
                        }
                    }
                }

                // C. Topic title update (debounced on status changes, 60s-throttled on prompt-only changes).
                let status_changed = runtime.topics[idx].last_status != card.status;
                if status_changed {
                    if runtime.topics[idx].pending_status.as_deref() != Some(card.status.as_str()) {
                        runtime.topics[idx].pending_status = Some(card.status.clone());
                        runtime.topics[idx].pending_status_since = now_unix;
                        changed = true;
                    }
                    let stable = force_recreate_deleted
                        || now_unix.saturating_sub(runtime.topics[idx].pending_status_since)
                            >= TOPIC_STATUS_DEBOUNCE_SECS;
                    let allowed = force_recreate_deleted
                        || now_unix >= runtime.topics[idx].next_rename_allowed_at;
                    if stable && allowed {
                        if runtime.topics[idx].topic_name == desired_name {
                            runtime.topics[idx].last_status = card.status.clone();
                            runtime.topics[idx].pending_status = None;
                            changed = true;
                        } else {
                            match api.edit_forum_topic(forum.chat_id, thread_id, &desired_name) {
                                Ok(()) => {
                                    runtime.topics[idx].topic_name = desired_name;
                                    runtime.topics[idx].last_status = card.status.clone();
                                    runtime.topics[idx].pending_status = None;
                                    runtime.topics[idx].last_renamed_at = now_unix;
                                    runtime.topics[idx].next_rename_allowed_at =
                                        now_unix.saturating_add(TOPIC_STATUS_RENAME_COOLDOWN_SECS);
                                    changed = true;
                                }
                                Err(ApiError::TopicNotFound) => {
                                    runtime.topics[idx].deleted_in_telegram = true;
                                    changed = true;
                                }
                                Err(ApiError::RateLimited { retry_after }) => {
                                    runtime.topics[idx].next_rename_allowed_at =
                                        now_unix.saturating_add(retry_after.max(5));
                                    changed = true;
                                }
                                Err(_) => {}
                            }
                        }
                    }
                } else {
                    if runtime.topics[idx].pending_status.is_some() {
                        runtime.topics[idx].pending_status = None;
                        changed = true;
                    }
                    let prompt_rename_ready = force_recreate_deleted
                        || (now_unix.saturating_sub(runtime.topics[idx].last_renamed_at)
                            >= TOPIC_RENAME_MIN_INTERVAL_SECS
                            && now_unix >= runtime.topics[idx].next_rename_allowed_at);
                    if runtime.topics[idx].topic_name != desired_name && prompt_rename_ready {
                        match api.edit_forum_topic(forum.chat_id, thread_id, &desired_name) {
                            Ok(()) => {
                                runtime.topics[idx].topic_name = desired_name;
                                runtime.topics[idx].last_renamed_at = now_unix;
                                runtime.topics[idx].next_rename_allowed_at =
                                    now_unix.saturating_add(TOPIC_STATUS_RENAME_COOLDOWN_SECS);
                                changed = true;
                            }
                            Err(ApiError::TopicNotFound) => {
                                runtime.topics[idx].deleted_in_telegram = true;
                                changed = true;
                            }
                            Err(ApiError::RateLimited { retry_after }) => {
                                runtime.topics[idx].next_rename_allowed_at =
                                    now_unix.saturating_add(retry_after.max(5));
                                changed = true;
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
            None => {
                if !cfg.auto_sync_topics && !force_recreate_deleted {
                    continue;
                }
                if let Some(binding) = create_binding(
                    api,
                    forum.chat_id,
                    card,
                    &desired_name,
                    color,
                    now_unix,
                    cfg.permissions,
                ) {
                    if let Some(msg_id) = binding.status_message_id {
                        runtime.record_message_card(forum.chat_id, msg_id, &card.id);
                    }
                    runtime.topics.push(binding);
                    changed = true;
                }
            }
        }
    }

    for idx in 0..runtime.topics.len() {
        let card_alive = cards.iter().any(|c| c.id == runtime.topics[idx].card_id);
        if card_alive || runtime.topics[idx].closed || runtime.topics[idx].deleted_in_telegram {
            continue;
        }
        let thread_id = runtime.topics[idx].thread_id;
        if let Some(msg_id) = runtime.topics[idx].status_message_id {
            let closed_html = format_closed_topic_header_html(&runtime.topics[idx].topic_name);
            let closed_fp = status_card_fingerprint(&closed_html, None);
            if runtime.topics[idx].last_status_card.as_deref() != Some(closed_fp.as_str()) {
                match api.edit_message_text(forum.chat_id, msg_id, &closed_html, None) {
                    Ok(()) => {
                        runtime.topics[idx].last_status = "CLOSED".into();
                        runtime.topics[idx].last_status_card = Some(closed_fp);
                        runtime.topics[idx].pending_status = None;
                        changed = true;
                    }
                    Err(ApiError::TopicNotFound) => {
                        runtime.topics[idx].deleted_in_telegram = true;
                        changed = true;
                        continue;
                    }
                    Err(ApiError::MessageNotFound) => {
                        runtime.topics[idx].status_message_id = None;
                        runtime.topics[idx].last_status_card = None;
                        changed = true;
                    }
                    Err(_) => {}
                }
            }
        }
        if cfg.permissions.notifications {
            match api.send_message(
                forum.chat_id,
                Some(thread_id),
                "🛑 Card closed on PC.",
                None,
                None,
            ) {
                Ok(msg_id) => {
                    let card_id = runtime.topics[idx].card_id.clone();
                    runtime.record_message_card(forum.chat_id, msg_id, &card_id);
                }
                Err(ApiError::TopicNotFound) => {
                    runtime.topics[idx].deleted_in_telegram = true;
                    changed = true;
                    continue;
                }
                Err(_) => {}
            }
        }
        if cfg.archive_closed_topics && !runtime.topics[idx].deleted_in_telegram {
            match api.close_forum_topic(forum.chat_id, thread_id) {
                Ok(()) => {
                    runtime.topics[idx].closed = true;
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

    if runtime.topics.len() > MAX_TOPICS {
        let mut idx = 0;
        while runtime.topics.len() > MAX_TOPICS && idx < runtime.topics.len() {
            let live = cards.iter().any(|c| c.id == runtime.topics[idx].card_id);
            if !live && (runtime.topics[idx].closed || runtime.topics[idx].deleted_in_telegram) {
                runtime.topics.remove(idx);
                changed = true;
            } else {
                idx += 1;
            }
        }
    }

    changed
}

fn create_binding(
    api: &dyn TelegramApi,
    chat_id: i64,
    card: &CardSummary,
    desired_name: &str,
    color: u32,
    now_unix: u64,
    perms: Permissions,
) -> Option<TopicBinding> {
    let topic = api.create_forum_topic(chat_id, desired_name, color).ok()?;
    let header = format_topic_header_html(card);
    let kb = topic_header_keyboard(card, perms);
    let status_message_id = api
        .send_message(
            chat_id,
            Some(topic.message_thread_id),
            &header,
            None,
            kb.as_ref(),
        )
        .ok();
    if let Some(msg_id) = status_message_id {
        let _ = api.pin_chat_message(chat_id, msg_id, true);
    }
    let last_status_card =
        status_message_id.map(|_| status_card_fingerprint(&header, kb.as_ref()));
    Some(TopicBinding {
        card_id: card.id.clone(),
        thread_id: topic.message_thread_id,
        topic_name: desired_name.to_string(),
        icon_color: color,
        closed: false,
        deleted_in_telegram: false,
        last_renamed_at: now_unix,
        status_message_id,
        last_status: card.status.clone(),
        last_status_card,
        status_pinned: status_message_id.is_some(),
        next_rename_allowed_at: 0,
        pending_status: None,
        pending_status_since: 0,
    })
}