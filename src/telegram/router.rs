//! Command, message, media, and callback-query router for the Telegram bot.
use super::api::{
    CallbackQuery, InlineButton, InlineKeyboardMarkup, Message, PhotoSize, TelegramApi, Update,
};
use super::config::{
    save_config_at, save_runtime_at, Config, LiveStream, RuntimeState, MAX_LIVE_STREAMS,
};
use super::desktop::{
    normalize_telegram_image, CardAsset, CardSummary, DesktopBackend, HarnessChoice, NamedKey,
};
use super::pairing;
use super::redact::{
    escape_html, sanitize_terminal_text, split_chunks, status_emoji, truncate_chars,
    DEFAULT_CHUNK_CHARS,
};
use super::topics::{
    self, card_action_keyboard, dashboard_keyboard, format_card_header_html,
    format_dashboard_html, format_live_message_html, format_usage_html, live_stream_keyboard,
    quick_keys_keyboard, quick_slash_commands, tail_output_snippet, usage_keyboard,
};
use crate::prompt_attachments::{safe_name, Attachment, Kind, MAX_TOTAL};
use crate::prompt_image::MAX_IMAGE;
use std::collections::VecDeque;
use std::path::Path;

const MAX_CACHED_FILE_PICKS: usize = 256;
const FILES_PAGE_SIZE: usize = 8;

pub const BOT_COMMANDS: &[(&str, &str)] = &[
    ("app", "Open the Android-like Telegram Mini App"),
    ("cards", "List active terminal cards"),
    ("dashboard", "Show or refresh live dashboard + usage"),
    ("usage", "Show weekly allowance & provider progress"),
    ("status", "Show selected card status"),
    ("screen", "Capture current screen"),
    ("history", "Capture recent scrollback"),
    ("live", "Stream live screen output for a card"),
    ("keys", "Show quick keys & agent slash shortcuts"),
    ("files", "List referenced images, files, and links"),
    ("new", "Launch a new harness card"),
    ("close", "Close selected card"),
    ("ctrlc", "Send Ctrl-C to selected card"),
    ("enter", "Send Enter to selected card"),
    ("esc", "Send Escape to selected card"),
    ("sync", "Sync Forum Topics with cards"),
    ("linkforum", "Link a 2-member Forum group"),
    ("unlinkforum", "Unlink Forum group"),
    ("unlink", "Unlink this Forum Topic from its card"),
    ("help", "Show help and active permissions"),
];

#[derive(Clone, Debug)]
pub struct CachedFilePick {
    pub card_id: String,
    pub asset_id: String,
    pub relative_path: String,
}

pub struct Router {
    pub bot_id: i64,
    pub bot_username: Option<String>,
    pub started_at: u64,
    pub new_wizard_folders: Vec<String>,
    pub new_wizard_harnesses: Vec<HarnessChoice>,
    next_file_pick_id: u64,
    file_picks: VecDeque<(u64, CachedFilePick)>,
}

impl Router {
    pub fn new(bot_id: i64, bot_username: Option<String>, started_at: u64) -> Self {
        Self {
            bot_id,
            bot_username,
            started_at,
            new_wizard_folders: Vec::new(),
            new_wizard_harnesses: Vec::new(),
            next_file_pick_id: 1,
            file_picks: VecDeque::new(),
        }
    }

    fn cache_file_pick(&mut self, pick: CachedFilePick) -> u64 {
        let id = self.next_file_pick_id;
        self.next_file_pick_id = self.next_file_pick_id.saturating_add(1);
        self.file_picks.push_back((id, pick));
        while self.file_picks.len() > MAX_CACHED_FILE_PICKS {
            self.file_picks.pop_front();
        }
        id
    }

    fn lookup_file_pick(&self, id: u64) -> Option<CachedFilePick> {
        self.file_picks
            .iter()
            .rev()
            .find(|(pick_id, _)| *pick_id == id)
            .map(|(_, pick)| pick.clone())
    }

    pub fn handle_update(
        &mut self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        state_dir: &Path,
        cfg: &mut Config,
        runtime: &mut RuntimeState,
        update: &Update,
        now_unix: u64,
    ) {
        if let Some(msg) = update.message.as_ref() {
            self.handle_message(api, backend, state_dir, cfg, runtime, msg, now_unix);
        } else if let Some(cq) = update.callback_query.as_ref() {
            self.handle_callback(api, backend, state_dir, cfg, runtime, cq, now_unix);
        }
    }

    fn reply_and_track(
        &self,
        api: &dyn TelegramApi,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        html: &str,
        reply_to: Option<i64>,
        keyboard: Option<&InlineKeyboardMarkup>,
        card_id: Option<&str>,
    ) {
        if let Ok(msg_id) = api.send_message(chat_id, thread_id, html, reply_to, keyboard) {
            if let Some(cid) = card_id {
                runtime.record_message_card(chat_id, msg_id, cid);
            }
        }
    }

    fn parse_command<'a>(&self, text: &'a str) -> Option<(&'a str, &'a str)> {
        let trimmed = text.trim();
        if !trimmed.starts_with('/') {
            return None;
        }
        let mut split = trimmed.splitn(2, char::is_whitespace);
        let raw_cmd = split.next()?;
        let arg = split.next().map(str::trim).unwrap_or("");
        let cmd = match raw_cmd.split_once('@') {
            Some((head, target_bot)) => {
                if let Some(own) = self.bot_username.as_deref() {
                    if !target_bot.eq_ignore_ascii_case(own) {
                        return None;
                    }
                }
                head
            }
            None => raw_cmd,
        };
        Some((cmd, arg))
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_message(
        &mut self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        state_dir: &Path,
        cfg: &mut Config,
        runtime: &mut RuntimeState,
        msg: &Message,
        now_unix: u64,
    ) {
        let Some(from) = msg.from.as_ref() else {
            return;
        };
        if from.is_bot {
            return;
        }
        let text = msg
            .text
            .as_deref()
            .or(msg.caption.as_deref())
            .unwrap_or("")
            .trim();
        let has_media = msg.photo.is_some()
            || msg.document.is_some()
            || msg.voice.is_some()
            || msg.audio.is_some();
        if text.is_empty() && !has_media {
            return;
        }

        // Pairing check BEFORE owner authorization so an invited user can request pairing.
        if msg.chat.chat_type == "private" {
            if let Some((cmd, arg)) = self.parse_command(text) {
                if cmd == "/start"
                    && !arg.is_empty()
                    && (cfg.allowed_user.is_none()
                        || pairing::has_active_invite(state_dir, now_unix))
                {
                    let reply = pairing::handle_pairing_start(
                        state_dir,
                        cfg,
                        from,
                        msg.chat.id,
                        &msg.chat.chat_type,
                        arg,
                        now_unix,
                    );
                    if let Some(html) = reply.reply_html {
                        let _ = api.send_message(msg.chat.id, None, &html, None, None);
                    }
                    return;
                }
            }
        }

        // Strict owner check: silently ignore every non-owner message.
        let Some(owner) = cfg.allowed_user.clone() else {
            return;
        };
        if from.id != owner.user_id {
            return;
        }

        let cmd_parsed = self.parse_command(text);
        let is_private = msg.chat.id == owner.chat_id && msg.chat.chat_type == "private";
        let is_linked_forum = cfg.forum.as_ref().is_some_and(|f| f.chat_id == msg.chat.id);
        let is_linkforum_cmd = matches!(cmd_parsed, Some(("/linkforum", _)));
        if !is_private && !is_linked_forum && !is_linkforum_cmd {
            return;
        }

        // Stale queued-message guard after offline/reboot downtime.
        if msg.date + 120 < now_unix
            || (msg.date + 5 < self.started_at && msg.date + 15 < now_unix)
        {
            return;
        }

        let thread_id = if msg.is_topic_message == Some(true) {
            msg.message_thread_id.filter(|&id| id > 1)
        } else {
            None
        };
        let reply_thread = if is_linked_forum { thread_id } else { None };

        let resolved_card_id = if is_linked_forum {
            thread_id.and_then(|tid| runtime.card_for_thread(tid).map(str::to_string))
        } else if let Some(reply_to) = msg.reply_to_message.as_ref() {
            runtime
                .card_for_message(msg.chat.id, reply_to.message_id)
                .map(str::to_string)
                .or_else(|| if is_private { runtime.active_card_id.clone() } else { None })
        } else if is_private {
            runtime.active_card_id.clone()
        } else {
            None
        };

        if let Some((cmd, arg)) = cmd_parsed {
            self.handle_command(
                api,
                backend,
                state_dir,
                cfg,
                runtime,
                msg.chat.id,
                reply_thread,
                is_linked_forum,
                resolved_card_id.as_deref(),
                cmd,
                arg,
                now_unix,
            );
            let _ = save_runtime_at(&state_dir.join("runtime.json"), runtime);
            return;
        }

        let cards = backend.list_cards();
        let Some(card) = resolved_card_id
            .as_deref()
            .and_then(|id| cards.iter().find(|c| c.id == id))
        else {
            let hint = if is_linked_forum {
                "This topic is not bound to an active card. Run /sync in General or pick a card with /cards."
            } else {
                "No card selected. Send /cards to choose a terminal card, or reply to a card notification."
            };
            self.reply_and_track(api, runtime, msg.chat.id, reply_thread, hint, None, None, None);
            let _ = save_runtime_at(&state_dir.join("runtime.json"), runtime);
            return;
        };

        if has_media {
            self.handle_media(
                api,
                backend,
                cfg,
                runtime,
                msg,
                reply_thread,
                is_linked_forum,
                card,
                text,
                owner.user_id,
            );
            let _ = save_runtime_at(&state_dir.join("runtime.json"), runtime);
            return;
        }

        // Plain text prompt to the resolved card.
        if !self.check_input_permission(api, cfg, runtime, msg.chat.id, reply_thread, card) {
            return;
        }
        match backend.send_prompt(&card.id, text) {
            Ok(()) => {
                runtime.active_card_id = Some(card.id.clone());
                let acked_with_reaction = is_linked_forum
                    && api
                        .set_message_reaction(msg.chat.id, msg.message_id, "👍")
                        .is_ok();
                if !acked_with_reaction {
                    let kb = card_action_keyboard(card, cfg.permissions);
                    let reply = format!(
                        "✓ Sent to {} <b>{}</b> · <code>{}</code>",
                        escape_html(&card.icon),
                        escape_html(&card.agent_name),
                        escape_html(&card.directory_display)
                    );
                    self.reply_and_track(
                        api,
                        runtime,
                        msg.chat.id,
                        reply_thread,
                        &reply,
                        Some(msg.message_id),
                        kb.as_ref(),
                        Some(&card.id),
                    );
                }
            }
            Err(err) => {
                let err_html = format!("Could not send prompt: <code>{}</code>", escape_html(&err));
                self.reply_and_track(
                    api,
                    runtime,
                    msg.chat.id,
                    reply_thread,
                    &err_html,
                    Some(msg.message_id),
                    None,
                    Some(&card.id),
                );
            }
        }
        let _ = save_runtime_at(&state_dir.join("runtime.json"), runtime);
    }

    fn check_input_permission(
        &self,
        api: &dyn TelegramApi,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        card: &CardSummary,
    ) -> bool {
        if card.is_shell && !cfg.permissions.shell_input {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                "Input to plain Shell cards is disabled. Turn on <b>Allow shell input</b> in <b>Settings → Telegram</b>.",
                None,
                None,
                Some(&card.id),
            );
            return false;
        }
        if !card.is_shell && !cfg.permissions.prompts {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                "Sending prompts from Telegram is disabled. Turn on <b>Submit prompts</b> in <b>Settings → Telegram</b>.",
                None,
                None,
                Some(&card.id),
            );
            return false;
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_media(
        &self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        msg: &Message,
        thread_id: Option<i64>,
        is_linked_forum: bool,
        card: &CardSummary,
        caption: &str,
        owner_user_id: i64,
    ) {
        if !cfg.permissions.attachments {
            self.reply_and_track(
                api,
                runtime,
                msg.chat.id,
                thread_id,
                "Sending attachments from Telegram is disabled. Turn on <b>Receive attachments</b> in <b>Settings → Telegram</b>.",
                None,
                None,
                Some(&card.id),
            );
            return;
        }
        if !self.check_input_permission(api, cfg, runtime, msg.chat.id, thread_id, card) {
            return;
        }

        let owner_key = format!("tg_{owner_user_id}");
        let prepared: Result<(String, Attachment), String> = if let Some(photos) = msg.photo.as_ref() {
            prepare_photo(api, photos).map(|att| (caption.to_string(), att))
        } else if let Some(doc) = msg.document.as_ref() {
            prepare_file(
                api,
                &doc.file_id,
                doc.file_name.as_deref().unwrap_or("attachment"),
                doc.file_size,
            )
            .map(|att| (caption.to_string(), att))
        } else if let Some(voice) = msg.voice.as_ref() {
            let text = if caption.is_empty() {
                "Voice message attached. Transcribe it if you have a local speech-to-text tool and respond."
                    .to_string()
            } else {
                caption.to_string()
            };
            prepare_file(api, &voice.file_id, "voice.ogg", voice.file_size).map(|att| (text, att))
        } else if let Some(audio) = msg.audio.as_ref() {
            let text = if caption.is_empty() {
                "Audio message attached. Transcribe it if you have a local speech-to-text tool and respond."
                    .to_string()
            } else {
                caption.to_string()
            };
            prepare_file(
                api,
                &audio.file_id,
                audio.file_name.as_deref().unwrap_or("audio.mp3"),
                audio.file_size,
            )
            .map(|att| (text, att))
        } else {
            Err("Unsupported attachment type.".into())
        };

        match prepared.and_then(|(text, att)| backend.send_attachment(&card.id, &owner_key, &text, att)) {
            Ok(()) => {
                runtime.active_card_id = Some(card.id.clone());
                let acked_with_reaction = is_linked_forum
                    && api
                        .set_message_reaction(msg.chat.id, msg.message_id, "👍")
                        .is_ok();
                if !acked_with_reaction {
                    let kb = card_action_keyboard(card, cfg.permissions);
                    let reply = format!(
                        "📎 Sent attachment to {} <b>{}</b> · <code>{}</code>",
                        escape_html(&card.icon),
                        escape_html(&card.agent_name),
                        escape_html(&card.directory_display)
                    );
                    self.reply_and_track(
                        api,
                        runtime,
                        msg.chat.id,
                        thread_id,
                        &reply,
                        Some(msg.message_id),
                        kb.as_ref(),
                        Some(&card.id),
                    );
                }
            }
            Err(err) => {
                let err_html = format!("Attachment failed: <code>{}</code>", escape_html(&err));
                self.reply_and_track(
                    api,
                    runtime,
                    msg.chat.id,
                    thread_id,
                    &err_html,
                    Some(msg.message_id),
                    None,
                    Some(&card.id),
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_command(
        &mut self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        state_dir: &Path,
        cfg: &mut Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        is_linked_forum: bool,
        resolved_card_id: Option<&str>,
        cmd: &str,
        arg: &str,
        now_unix: u64,
    ) {
        match cmd {
            "/start" | "/help" => {
                let p = cfg.permissions;
                let forum_line = cfg
                    .forum
                    .as_ref()
                    .map(|f| format!("Forum: <b>{}</b>", escape_html(&f.title)))
                    .unwrap_or_else(|| "Forum: not linked (run /linkforum in a 2-member Forum group)".into());
                let html = format!(
                    "<b>SUPER DESKTOP Bot</b>\n{forum_line}\n\n\
                     <b>Commands:</b>\n\
                     /app — open Mini Web App (live ANSI terminal)\n\
                     /cards — list active cards\n\
                     /dashboard — live workspace &amp; usage\n\
                     /usage — weekly allowance &amp; provider progress\n\
                     /status — selected card status\n\
                     /screen — current screen\n\
                     /history [N] — scrollback (20–300)\n\
                     /live — stream live screen output\n\
                     /keys — quick keys &amp; slash shortcuts\n\
                     /files — referenced images, files &amp; links\n\
                     /new — launch harness card\n\
                     /close — close selected card\n\
                     /ctrlc /enter /esc — send control key\n\
                     /sync — sync Forum Topics\n\
                     /linkforum · /unlinkforum · /unlink\n\n\
                     <b>Permissions:</b> alerts={} · output={} · prompts={} · controls={} · shell={} · launch={} · close={} · files={} · redact={}",
                    on_off(p.notifications),
                    on_off(p.read_output),
                    on_off(p.prompts),
                    on_off(p.controls),
                    on_off(p.shell_input),
                    on_off(p.launch),
                    on_off(p.close),
                    on_off(p.attachments),
                    on_off(p.redact_secrets),
                );
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, None);
            }
            "/dashboard" | "/dash" => {
                let cards = backend.list_cards();
                let usage = backend.usage_summary();
                if is_linked_forum && thread_id.is_none() && cfg.forum.is_some() {
                    topics::sync_dashboard(api, cfg, runtime, &cards, &usage, now_unix, true);
                } else {
                    let html = format_dashboard_html(&cards, &usage);
                    let app_url = if is_linked_forum {
                        cfg.mini_app.dashboard_deep_link(cfg.bot_username.as_deref())
                    } else {
                        None
                    };
                    let kb = dashboard_keyboard(cfg.permissions, cfg.forum.is_some(), app_url.as_deref());
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        &html,
                        None,
                        Some(&kb),
                        None,
                    );
                }
            }
            "/app" => {
                if !cfg.mini_app.enabled {
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        "Mini Web App is disabled. Turn on <b>Enable Telegram Mini Web App</b> in <b>Settings → Telegram</b>.",
                        None,
                        None,
                        None,
                    );
                    return;
                }
                let Some(base_url) = cfg.mini_app.normalized_url() else {
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        "Mini Web App HTTPS URL is not set yet. Configure Tailscale Serve and paste the <code>https://…ts.net:8443</code> URL in <b>Settings → Telegram</b>.",
                        None,
                        None,
                        None,
                    );
                    return;
                };
                if is_linked_forum {
                    let dl = resolved_card_id
                        .and_then(|cid| cfg.mini_app.card_deep_link(cfg.bot_username.as_deref(), cid))
                        .or_else(|| cfg.mini_app.dashboard_deep_link(cfg.bot_username.as_deref()));
                    if let Some(link) = dl {
                        let kb = InlineKeyboardMarkup {
                            inline_keyboard: vec![vec![InlineButton::url("📱 Open SUPER DESKTOP", link)]],
                        };
                        self.reply_and_track(
                            api,
                            runtime,
                            chat_id,
                            thread_id,
                            "📱 Launch live ANSI terminal &amp; Files:",
                            None,
                            Some(&kb),
                            resolved_card_id,
                        );
                    } else {
                        self.reply_and_track(
                            api,
                            runtime,
                            chat_id,
                            thread_id,
                            "Open the Mini App from the <b>⚡ Open App</b> menu button in our private 1:1 chat, or add a <code>@BotFather</code> <code>/newapp</code> short name in <b>Settings → Telegram</b> for one-tap buttons inside Forum Topics.",
                            None,
                            None,
                            None,
                        );
                    }
                } else {
                    let launch_url = match resolved_card_id {
                        Some(cid) => format!("{base_url}#card={cid}"),
                        None => base_url,
                    };
                    let kb = InlineKeyboardMarkup {
                        inline_keyboard: vec![vec![InlineButton::web_app(
                            "📱 Open SUPER DESKTOP",
                            launch_url,
                        )]],
                    };
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        "⚡ <b>SUPER DESKTOP Mini App</b>\nLive ANSI terminal, Weekly Usage &amp; Files over your private Tailscale connection:",
                        None,
                        Some(&kb),
                        resolved_card_id,
                    );
                }
            }
            "/usage" => {
                let usage = backend.usage_summary();
                let html = format_usage_html(&usage);
                let kb = usage_keyboard();
                self.reply_and_track(
                    api,
                    runtime,
                    chat_id,
                    thread_id,
                    &html,
                    None,
                    Some(&kb),
                    None,
                );
            }
            "/cards" | "/list" => {
                let cards = backend.list_cards();
                if cards.is_empty() {
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        "No running terminal cards.",
                        None,
                        None,
                        None,
                    );
                    return;
                }
                let mut lines = Vec::new();
                let mut rows = Vec::new();
                for card in &cards {
                    let active_mark = if runtime.active_card_id.as_deref() == Some(card.id.as_str()) {
                        " ◀"
                    } else {
                        ""
                    };
                    let title_suffix = card
                        .title
                        .as_deref()
                        .map(|t| format!(" — <i>{}</i>", escape_html(&truncate_chars(t, 40))))
                        .unwrap_or_default();
                    lines.push(format!(
                        "{} <b>{}</b> · <code>{}</code> [{}]{}{}",
                        escape_html(&card.icon),
                        escape_html(&card.agent_name),
                        escape_html(&card.directory_display),
                        escape_html(&card.label),
                        title_suffix,
                        active_mark,
                    ));
                    let btn_label = truncate_chars(
                        &format!("{} {} · {}", card.icon, card.agent_name, card.directory_display),
                        34,
                    );
                    let mut row = vec![InlineButton::callback(btn_label, format!("sel:{}", card.id))];
                    if cfg.permissions.read_output {
                        row.push(InlineButton::callback("📷", format!("scr:{}", card.id)));
                        row.push(InlineButton::callback("📎", format!("fil:{}", card.id)));
                    }
                    rows.push(row);
                }
                let kb = InlineKeyboardMarkup { inline_keyboard: rows };
                self.reply_and_track(
                    api,
                    runtime,
                    chat_id,
                    thread_id,
                    &lines.join("\n"),
                    None,
                    Some(&kb),
                    None,
                );
            }
            "/status" => {
                let cards = backend.list_cards();
                if let Some(card) = resolved_card_id.and_then(|id| cards.iter().find(|c| c.id == id)) {
                    let kb = card_action_keyboard(card, cfg.permissions);
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        &format_card_header_html(card),
                        None,
                        kb.as_ref(),
                        Some(&card.id),
                    );
                } else {
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        &format!("Active cards: <b>{}</b>. Send /cards to select one.", cards.len()),
                        None,
                        None,
                        None,
                    );
                }
            }
            "/screen" | "/s" => {
                let cards = backend.list_cards();
                let Some(card) = resolved_card_id.and_then(|id| cards.iter().find(|c| c.id == id)) else {
                    self.reply_and_track(api, runtime, chat_id, thread_id, "No card selected. Send /cards first.", None, None, None);
                    return;
                };
                self.send_capture(api, backend, cfg, runtime, chat_id, thread_id, card, 0, 2);
            }
            "/history" => {
                let cards = backend.list_cards();
                let Some(card) = resolved_card_id.and_then(|id| cards.iter().find(|c| c.id == id)) else {
                    self.reply_and_track(api, runtime, chat_id, thread_id, "No card selected. Send /cards first.", None, None, None);
                    return;
                };
                let lines = arg.parse::<u32>().unwrap_or(80).clamp(20, 300);
                self.send_capture(api, backend, cfg, runtime, chat_id, thread_id, card, lines, 3);
            }
            "/live" => {
                let cards = backend.list_cards();
                let Some(card) = resolved_card_id.and_then(|id| cards.iter().find(|c| c.id == id)) else {
                    self.reply_and_track(api, runtime, chat_id, thread_id, "No card selected. Send /cards first.", None, None, None);
                    return;
                };
                self.start_live_stream(api, backend, cfg, runtime, chat_id, thread_id, card, now_unix);
            }
            "/keys" => {
                let cards = backend.list_cards();
                let Some(card) = resolved_card_id.and_then(|id| cards.iter().find(|c| c.id == id)) else {
                    self.reply_and_track(api, runtime, chat_id, thread_id, "No card selected. Send /cards first.", None, None, None);
                    return;
                };
                self.send_keys_panel(api, cfg, runtime, chat_id, thread_id, card);
            }
            "/files" => {
                let cards = backend.list_cards();
                let Some(card) = resolved_card_id.and_then(|id| cards.iter().find(|c| c.id == id)) else {
                    self.reply_and_track(api, runtime, chat_id, thread_id, "No card selected. Send /cards first.", None, None, None);
                    return;
                };
                self.send_files_list(api, backend, cfg, runtime, chat_id, thread_id, card, 'a', 0, None);
            }
            "/ctrlc" | "/esc" | "/enter" => {
                let cards = backend.list_cards();
                let Some(card) = resolved_card_id.and_then(|id| cards.iter().find(|c| c.id == id)) else {
                    self.reply_and_track(api, runtime, chat_id, thread_id, "No card selected. Send /cards first.", None, None, None);
                    return;
                };
                let key = match cmd {
                    "/ctrlc" => NamedKey::CtrlC,
                    "/esc" => NamedKey::Escape,
                    _ => NamedKey::Enter,
                };
                self.send_key_for_card(api, backend, cfg, runtime, chat_id, thread_id, card, key);
            }
            "/new" => {
                self.send_new_wizard_folders(api, backend, cfg, runtime, chat_id, thread_id);
            }
            "/close" => {
                if !cfg.permissions.close {
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        "Closing cards from Telegram is disabled. Turn on <b>Close cards</b> in <b>Settings → Telegram</b>.",
                        None,
                        None,
                        None,
                    );
                    return;
                }
                let cards = backend.list_cards();
                let Some(card) = resolved_card_id.and_then(|id| cards.iter().find(|c| c.id == id)) else {
                    self.reply_and_track(api, runtime, chat_id, thread_id, "No card selected. Send /cards first.", None, None, None);
                    return;
                };
                let kb = InlineKeyboardMarkup {
                    inline_keyboard: vec![vec![
                        InlineButton::callback("Confirm close", format!("clc:{}", card.id)),
                        InlineButton::callback("Cancel", format!("clx:{}", card.id)),
                    ]],
                };
                let html = format!(
                    "Close {} <b>{}</b> · <code>{}</code>?",
                    escape_html(&card.icon),
                    escape_html(&card.agent_name),
                    escape_html(&card.directory_display)
                );
                self.reply_and_track(
                    api,
                    runtime,
                    chat_id,
                    thread_id,
                    &html,
                    None,
                    Some(&kb),
                    Some(&card.id),
                );
            }
            "/linkforum" => {
                if let Ok(disk_cfg) = super::config::load_config_at(&state_dir.join("config.json")) {
                    if disk_cfg.allowed_user.is_some() {
                        *cfg = disk_cfg;
                    }
                }
                match pairing::link_forum(api, state_dir, cfg, runtime, chat_id, self.bot_id, now_unix) {
                    Ok(title) => {
                        let cards = backend.list_cards();
                        topics::sync_topics(api, cfg, runtime, &cards, now_unix, true);
                        let active = runtime
                            .topics
                            .iter()
                            .filter(|t| !t.closed && !t.deleted_in_telegram)
                            .count();
                        let html = format!(
                            "✅ Linked Forum group <b>{}</b>.\nSynced <b>{active}</b> card topic(s). Every card now gets its own topic.",
                            escape_html(&title)
                        );
                        self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, None);
                    }
                    Err(err) => {
                        let html = format!("Could not link Forum group:\n{}", escape_html(&err));
                        self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, None);
                    }
                }
            }
            "/unlinkforum" => {
                if let Ok(disk_cfg) = super::config::load_config_at(&state_dir.join("config.json")) {
                    if disk_cfg.allowed_user.is_some() {
                        *cfg = disk_cfg;
                    }
                }
                cfg.forum = None;
                runtime.topics.clear();
                runtime.dashboard_message_id = None;
                runtime.dashboard_pinned = false;
                runtime.last_dashboard_card = None;
                runtime.last_dashboard_edit_at = 0;
                runtime.live_streams.clear();
                let _ = save_config_at(&state_dir.join("config.json"), cfg);
                self.reply_and_track(
                    api,
                    runtime,
                    chat_id,
                    thread_id,
                    "Forum group unlinked. Card topics will no longer be synced.",
                    None,
                    None,
                    None,
                );
            }
            "/sync" => {
                if cfg.forum.is_none() {
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        "No Forum group is linked. Create a 2-member Forum group and send /linkforum inside General.",
                        None,
                        None,
                        None,
                    );
                    return;
                }
                let cards = backend.list_cards();
                topics::sync_topics(api, cfg, runtime, &cards, now_unix, true);
                topics::sync_dashboard(
                    api,
                    cfg,
                    runtime,
                    &cards,
                    &backend.usage_summary(),
                    now_unix,
                    true,
                );
                let active = runtime
                    .topics
                    .iter()
                    .filter(|t| !t.closed && !t.deleted_in_telegram)
                    .count();
                self.reply_and_track(
                    api,
                    runtime,
                    chat_id,
                    thread_id,
                    &format!("🔄 Topics synced ({active} active)."),
                    None,
                    None,
                    None,
                );
            }
            "/unlink" => {
                if !is_linked_forum || thread_id.is_none() {
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        "Run /unlink inside a card's Forum Topic to unlink that topic without closing the card.",
                        None,
                        None,
                        None,
                    );
                    return;
                }
                let tid = thread_id.unwrap();
                if let Some(binding) = runtime.topics.iter_mut().find(|t| t.thread_id == tid) {
                    binding.deleted_in_telegram = true;
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        "Topic unlinked from card (the card keeps running on your PC). Use /sync to recreate it.",
                        None,
                        None,
                        None,
                    );
                } else {
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        "This topic is not linked to any card.",
                        None,
                        None,
                        None,
                    );
                }
            }
            _ => {
                self.reply_and_track(
                    api,
                    runtime,
                    chat_id,
                    thread_id,
                    "Unknown command. Send /help for available commands.",
                    None,
                    None,
                    None,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn send_capture(
        &self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        card: &CardSummary,
        history_lines: u32,
        max_chunks: usize,
    ) {
        if !cfg.permissions.read_output {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                "Reading terminal output is disabled. Turn on <b>Read terminal output</b> in <b>Settings → Telegram</b>.",
                None,
                None,
                Some(&card.id),
            );
            return;
        }
        match backend.capture_screen(&card.id, history_lines) {
            Ok(raw) => {
                let clean = sanitize_terminal_text(
                    &raw,
                    &cfg.bot_token,
                    cfg.permissions.redact_secrets,
                );
                let trimmed = clean.trim();
                if trimmed.is_empty() {
                    let kb = card_action_keyboard(card, cfg.permissions);
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        "(empty screen)",
                        None,
                        kb.as_ref(),
                        Some(&card.id),
                    );
                    return;
                }
                let chunks = split_chunks(trimmed, DEFAULT_CHUNK_CHARS);
                let start = chunks.len().saturating_sub(max_chunks);
                let slice = &chunks[start..];
                for (i, chunk) in slice.iter().enumerate() {
                    let is_last = i + 1 == slice.len();
                    let kb = if is_last {
                        card_action_keyboard(card, cfg.permissions)
                    } else {
                        None
                    };
                    let header = if i == 0 {
                        format!(
                            "<b>{} {}</b> · <code>{}</code>\n",
                            escape_html(&card.icon),
                            escape_html(&card.agent_name),
                            escape_html(&card.directory_display)
                        )
                    } else {
                        String::new()
                    };
                    let html = format!("{header}<pre><code>{}</code></pre>", escape_html(chunk));
                    self.reply_and_track(
                        api,
                        runtime,
                        chat_id,
                        thread_id,
                        &html,
                        None,
                        kb.as_ref(),
                        Some(&card.id),
                    );
                }
            }
            Err(err) => {
                let html = format!("Capture failed: <code>{}</code>", escape_html(&err));
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&card.id));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn send_files_list(
        &mut self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        card: &CardSummary,
        filter: char,
        page: usize,
        edit_message_id: Option<i64>,
    ) {
        if !cfg.permissions.read_output {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                "Reading referenced files is disabled. Turn on <b>Read terminal output</b> in <b>Settings → Telegram</b>.",
                None,
                None,
                Some(&card.id),
            );
            return;
        }
        let mut refs = match backend.list_references(&card.id) {
            Ok(refs) => refs,
            Err(err) => {
                let html = format!("Could not inspect files: <code>{}</code>", escape_html(&err));
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&card.id));
                return;
            }
        };

        if refs.assets.is_empty() && refs.links.is_empty() {
            let html = format!(
                "📎 No referenced files or links found in <b>{} {}</b> · <code>{}</code> yet.\n\n<i>Images, PDFs, code/text paths, and web links mentioned in terminal output appear here.</i>",
                escape_html(&card.icon),
                escape_html(&card.agent_name),
                escape_html(&card.directory_display)
            );
            let kb = InlineKeyboardMarkup {
                inline_keyboard: vec![vec![InlineButton::callback("🔄 Refresh", format!("flr:a:0:{}", card.id))]],
            };
            if let Some(mid) = edit_message_id {
                let _ = api.edit_message_text(chat_id, mid, &html, Some(&kb));
            } else {
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, Some(&kb), Some(&card.id));
            }
            return;
        }

        // Preserve relative discovery order within display groups: images, PDFs, markdown/text.
        refs.assets.sort_by_key(|a| match a.kind.as_str() {
            "image" => 0u8,
            "pdf" => 1,
            "markdown" | "text" => 2,
            _ => 3,
        });

        let img_count = refs.assets.iter().filter(|a| a.kind == "image").count();
        let file_count = refs.assets.iter().filter(|a| a.kind != "image").count();
        let link_count = refs.links.len();
        let filter = match filter {
            'i' | 'f' | 'l' => filter,
            _ => 'a',
        };

        let mut rows: Vec<Vec<InlineButton>> = Vec::new();
        let non_empty_categories = usize::from(img_count > 0)
            + usize::from(file_count > 0)
            + usize::from(link_count > 0);
        if non_empty_categories > 1 {
            let tab = |code: char, label: String| {
                let text = if filter == code {
                    format!("• {label} •")
                } else {
                    label
                };
                InlineButton::callback(text, format!("flr:{code}:0:{}", card.id))
            };
            let mut tabs = vec![tab('a', "All".into())];
            if img_count > 0 {
                tabs.push(tab('i', format!("🖼 {img_count}")));
            }
            if file_count > 0 {
                tabs.push(tab('f', format!("📄 {file_count}")));
            }
            if link_count > 0 {
                tabs.push(tab('l', format!("🔗 {link_count}")));
            }
            rows.push(tabs);
        }

        let mut lines = vec![format!(
            "📎 <b>Files &amp; links</b> · {} {} · <code>{}</code>\n{} image(s) · {} doc(s) · {} link(s)",
            escape_html(&card.icon),
            escape_html(&card.agent_name),
            escape_html(&card.directory_display),
            img_count,
            file_count,
            link_count,
        )];

        let total_pages: usize;
        let current_page: usize;
        if filter == 'l' {
            total_pages = link_count.max(1).div_ceil(FILES_PAGE_SIZE);
            current_page = page.min(total_pages - 1);
            let start = current_page * FILES_PAGE_SIZE;
            let slice = &refs.links[start..(start + FILES_PAGE_SIZE).min(link_count)];
            lines.push(String::new());
            lines.push(format!("<b>Links ({}/{}):</b>", current_page + 1, total_pages));
            for link in slice {
                let shown = escape_html(&truncate_chars(link, 90));
                if is_telegram_button_url(link) {
                    lines.push(format!("• <a href=\"{}\">{}</a>", escape_html(link), shown));
                    let btn_text = format!(
                        "🔗 {}",
                        truncate_chars(
                            link.trim_start_matches("https://").trim_start_matches("http://"),
                            42
                        )
                    );
                    rows.push(vec![InlineButton::url(btn_text, link.clone())]);
                } else {
                    lines.push(format!("• <code>{}</code>", shown));
                }
            }
        } else {
            let filtered_assets: Vec<&CardAsset> = refs
                .assets
                .iter()
                .filter(|a| match filter {
                    'i' => a.kind == "image",
                    'f' => a.kind != "image",
                    _ => true,
                })
                .collect();
            total_pages = filtered_assets.len().max(1).div_ceil(FILES_PAGE_SIZE);
            current_page = page.min(total_pages - 1);
            let start = current_page * FILES_PAGE_SIZE;
            let slice = &filtered_assets[start..(start + FILES_PAGE_SIZE).min(filtered_assets.len())];
            for asset in slice {
                let pick_id = self.cache_file_pick(CachedFilePick {
                    card_id: card.id.clone(),
                    asset_id: asset.id.clone(),
                    relative_path: asset.relative_path.clone(),
                });
                let label = format!(
                    "{} {} · {}",
                    asset_icon(asset),
                    truncate_chars(&asset.name, 34),
                    human_bytes(asset.size)
                );
                rows.push(vec![InlineButton::callback(label, format!("fop:{pick_id}"))]);
            }
            if filter == 'a' && current_page == 0 && !refs.links.is_empty() {
                lines.push(String::new());
                lines.push("<b>Links:</b>".into());
                let mut url_buttons = 0usize;
                for link in &refs.links {
                    let shown = escape_html(&truncate_chars(link, 90));
                    if is_telegram_button_url(link) {
                        lines.push(format!("• <a href=\"{}\">{}</a>", escape_html(link), shown));
                        if url_buttons < 4 {
                            let btn_text = format!(
                                "🔗 {}",
                                truncate_chars(
                                    link.trim_start_matches("https://").trim_start_matches("http://"),
                                    42
                                )
                            );
                            rows.push(vec![InlineButton::url(btn_text, link.clone())]);
                            url_buttons += 1;
                        }
                    } else {
                        lines.push(format!("• <code>{}</code>", shown));
                    }
                }
            }
        }

        if total_pages > 1 {
            let mut nav = Vec::new();
            if current_page > 0 {
                nav.push(InlineButton::callback(
                    "◀ Prev",
                    format!("flr:{filter}:{}:{}", current_page - 1, card.id),
                ));
            }
            nav.push(InlineButton::callback(
                format!("{}/{}", current_page + 1, total_pages),
                format!("flr:{filter}:{current_page}:{}", card.id),
            ));
            if current_page + 1 < total_pages {
                nav.push(InlineButton::callback(
                    "Next ▶",
                    format!("flr:{filter}:{}:{}", current_page + 1, card.id),
                ));
            }
            rows.push(nav);
        }

        rows.push(vec![InlineButton::callback(
            "🔄 Refresh",
            format!("flr:{filter}:{current_page}:{}", card.id),
        )]);
        let kb = InlineKeyboardMarkup { inline_keyboard: rows };
        let html = lines.join("\n");
        if let Some(mid) = edit_message_id {
            let _ = api.edit_message_text(chat_id, mid, &html, Some(&kb));
        } else {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                &html,
                None,
                Some(&kb),
                Some(&card.id),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn open_file_pick(
        &self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        pick_id: u64,
        pick: &CachedFilePick,
    ) {
        if !cfg.permissions.read_output {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                "Reading referenced files is disabled. Turn on <b>Read terminal output</b> in <b>Settings → Telegram</b>.",
                None,
                None,
                Some(&pick.card_id),
            );
            return;
        }
        let (asset, bytes) = match backend.read_asset(&pick.card_id, &pick.asset_id) {
            Ok(pair) => pair,
            Err(err) => {
                let html = format!(
                    "Could not open <code>{}</code>: <code>{}</code> (tap 📎 Files to refresh).",
                    escape_html(&pick.relative_path),
                    escape_html(&err)
                );
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&pick.card_id));
                return;
            }
        };

        let caption = format!(
            "📎 <code>{}</code> ({})",
            escape_html(&truncate_chars(&asset.relative_path, 200)),
            human_bytes(asset.size)
        );
        let result = if asset.mime_type == "image/gif" {
            send_media_with_doc_fallback(
                api,
                chat_id,
                thread_id,
                &asset.name,
                &asset.mime_type,
                &bytes,
                &caption,
                true,
            )
        } else if asset.kind == "image" {
            if bytes.len() <= 10 * 1024 * 1024 {
                send_media_with_doc_fallback(
                    api,
                    chat_id,
                    thread_id,
                    &asset.name,
                    &asset.mime_type,
                    &bytes,
                    &caption,
                    false,
                )
            } else {
                api.send_document(
                    chat_id,
                    thread_id,
                    &asset.name,
                    &asset.mime_type,
                    &bytes,
                    Some(&caption),
                    None,
                )
            }
        } else if asset.kind == "pdf" {
            let kb = InlineKeyboardMarkup {
                inline_keyboard: vec![vec![InlineButton::callback(
                    "🖼 Preview page 1",
                    format!("ppg:{pick_id}:1"),
                )]],
            };
            api.send_document(
                chat_id,
                thread_id,
                &asset.name,
                &asset.mime_type,
                &bytes,
                Some(&caption),
                Some(&kb),
            )
        } else if asset.kind == "markdown" || asset.kind == "text" {
            let clean = sanitize_terminal_text(
                &String::from_utf8_lossy(&bytes),
                &cfg.bot_token,
                cfg.permissions.redact_secrets,
            );
            if clean.chars().count() <= 3400 {
                let html = format!(
                    "📄 <code>{}</code>\n<pre>{}</pre>",
                    escape_html(&asset.relative_path),
                    escape_html(&clean)
                );
                api.send_message(chat_id, thread_id, &html, None, None)
            } else {
                self.send_text_chunks_page(
                    api,
                    runtime,
                    chat_id,
                    thread_id,
                    pick_id,
                    &pick.card_id,
                    &asset.relative_path,
                    &clean,
                    0,
                    None,
                );
                return;
            }
        } else {
            api.send_document(
                chat_id,
                thread_id,
                &asset.name,
                &asset.mime_type,
                &bytes,
                Some(&caption),
                None,
            )
        };

        match result {
            Ok(msg_id) => runtime.record_message_card(chat_id, msg_id, &pick.card_id),
            Err(err) => {
                let html = format!(
                    "Could not send <code>{}</code>: <code>{}</code>",
                    escape_html(&asset.name),
                    escape_html(&err.to_string())
                );
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&pick.card_id));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn send_text_chunks_page(
        &self,
        api: &dyn TelegramApi,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        pick_id: u64,
        card_id: &str,
        relative_path: &str,
        clean_text: &str,
        page: usize,
        edit_message_id: Option<i64>,
    ) {
        let chunks = split_chunks(clean_text, 3200);
        let total = chunks.len().max(1);
        let page = page.min(total - 1);
        let html = format!(
            "📄 <code>{}</code> (Page {}/{})\n<pre>{}</pre>",
            escape_html(relative_path),
            page + 1,
            total,
            escape_html(&chunks[page])
        );
        let mut nav = Vec::new();
        if page > 0 {
            nav.push(InlineButton::callback("◀ Prev", format!("tpg:{pick_id}:{}", page - 1)));
        }
        if page + 1 < total {
            nav.push(InlineButton::callback("Next ▶", format!("tpg:{pick_id}:{}", page + 1)));
        }
        let mut rows = Vec::new();
        if !nav.is_empty() {
            rows.push(nav);
        }
        rows.push(vec![InlineButton::callback(
            "📥 Download file",
            format!("tdl:{pick_id}"),
        )]);
        let kb = InlineKeyboardMarkup { inline_keyboard: rows };
        if let Some(mid) = edit_message_id {
            let _ = api.edit_message_text(chat_id, mid, &html, Some(&kb));
        } else {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                &html,
                None,
                Some(&kb),
                Some(card_id),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn open_text_page(
        &self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        pick_id: u64,
        pick: &CachedFilePick,
        page: usize,
        edit_message_id: Option<i64>,
    ) {
        if !cfg.permissions.read_output {
            return;
        }
        match backend.read_asset(&pick.card_id, &pick.asset_id) {
            Ok((asset, bytes)) => {
                let clean = sanitize_terminal_text(
                    &String::from_utf8_lossy(&bytes),
                    &cfg.bot_token,
                    cfg.permissions.redact_secrets,
                );
                self.send_text_chunks_page(
                    api,
                    runtime,
                    chat_id,
                    thread_id,
                    pick_id,
                    &pick.card_id,
                    &asset.relative_path,
                    &clean,
                    page,
                    edit_message_id,
                );
            }
            Err(err) => {
                let html = format!(
                    "Could not open <code>{}</code>: <code>{}</code>",
                    escape_html(&pick.relative_path),
                    escape_html(&err)
                );
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&pick.card_id));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn download_text_pick(
        &self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        pick: &CachedFilePick,
    ) {
        if !cfg.permissions.read_output {
            return;
        }
        match backend.read_asset(&pick.card_id, &pick.asset_id) {
            Ok((asset, bytes)) => {
                let clean = sanitize_terminal_text(
                    &String::from_utf8_lossy(&bytes),
                    &cfg.bot_token,
                    cfg.permissions.redact_secrets,
                );
                let caption = format!(
                    "📎 <code>{}</code> ({})",
                    escape_html(&truncate_chars(&asset.relative_path, 200)),
                    human_bytes(asset.size)
                );
                if let Ok(mid) = api.send_document(
                    chat_id,
                    thread_id,
                    &asset.name,
                    "text/plain",
                    clean.as_bytes(),
                    Some(&caption),
                    None,
                ) {
                    runtime.record_message_card(chat_id, mid, &pick.card_id);
                }
            }
            Err(err) => {
                let html = format!("Download failed: <code>{}</code>", escape_html(&err));
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&pick.card_id));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn open_pdf_page(
        &self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        pick_id: u64,
        pick: &CachedFilePick,
        page: u32,
    ) {
        if !cfg.permissions.read_output || page == 0 {
            return;
        }
        match backend.render_pdf_page(&pick.card_id, &pick.asset_id, page) {
            Ok((asset, png_bytes)) => {
                let mut nav = Vec::new();
                if page > 1 {
                    nav.push(InlineButton::callback(
                        format!("◀ Page {}", page - 1),
                        format!("ppg:{pick_id}:{}", page - 1),
                    ));
                }
                nav.push(InlineButton::callback(
                    format!("Page {} ▶", page + 1),
                    format!("ppg:{pick_id}:{}", page + 1),
                ));
                let kb = InlineKeyboardMarkup {
                    inline_keyboard: vec![nav],
                };
                let caption = format!(
                    "📕 <code>{}</code> · Page {page}",
                    escape_html(&truncate_chars(&asset.relative_path, 180))
                );
                let fname = format!("{}-page-{page}.png", asset.name);
                match api.send_photo(
                    chat_id,
                    thread_id,
                    &fname,
                    "image/png",
                    &png_bytes,
                    Some(&caption),
                    Some(&kb),
                ) {
                    Ok(mid) => runtime.record_message_card(chat_id, mid, &pick.card_id),
                    Err(err) => {
                        let html = format!("PDF preview failed: <code>{}</code>", escape_html(&err.to_string()));
                        self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&pick.card_id));
                    }
                }
            }
            Err(err) => {
                let html = format!(
                    "Could not render page {page} of <code>{}</code>: <code>{}</code>",
                    escape_html(&pick.relative_path),
                    escape_html(&err)
                );
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&pick.card_id));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start_live_stream(
        &self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        card: &CardSummary,
        now_unix: u64,
    ) {
        if !cfg.permissions.read_output {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                "Reading terminal output is disabled. Turn on <b>Read terminal output</b> in <b>Settings → Telegram</b>.",
                None,
                None,
                Some(&card.id),
            );
            return;
        }
        let raw = match backend.capture_screen(&card.id, 0) {
            Ok(raw) => raw,
            Err(err) => {
                let html = format!("Capture failed: <code>{}</code>", escape_html(&err));
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&card.id));
                return;
            }
        };
        let snippet = tail_output_snippet(&raw, &cfg.bot_token, cfg.permissions.redact_secrets, 30, 3200)
            .unwrap_or_else(|| "(empty screen)".to_string());
        let html = format_live_message_html(card, &snippet);
        let kb = live_stream_keyboard(card, cfg.permissions);
        runtime.live_streams.retain(|s| s.card_id != card.id);
        while runtime.live_streams.len() >= MAX_LIVE_STREAMS {
            runtime.live_streams.remove(0);
        }
        if let Ok(msg_id) = api.send_message(chat_id, thread_id, &html, None, Some(&kb)) {
            runtime.record_message_card(chat_id, msg_id, &card.id);
            runtime.live_streams.push(LiveStream {
                card_id: card.id.clone(),
                chat_id,
                thread_id,
                message_id: msg_id,
                last_text: html,
                started_at: now_unix,
                last_edited_at: now_unix,
                idle_since: None,
            });
        }
    }

    fn send_keys_panel(
        &self,
        api: &dyn TelegramApi,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        card: &CardSummary,
    ) {
        let Some(kb) = quick_keys_keyboard(card, cfg.permissions) else {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                "Quick keys require <b>Control keys</b> or <b>Submit prompts</b> in <b>Settings → Telegram</b>.",
                None,
                None,
                Some(&card.id),
            );
            return;
        };
        let dot = status_emoji(&card.status);
        let html = format!(
            "⌨ <b>Quick keys</b> · {dot} {} <b>{}</b> · <code>{}</code>",
            escape_html(&card.icon),
            escape_html(&card.agent_name),
            escape_html(&card.directory_display)
        );
        self.reply_and_track(
            api,
            runtime,
            chat_id,
            thread_id,
            &html,
            None,
            Some(&kb),
            Some(&card.id),
        );
    }

    fn send_new_wizard_folders(
        &mut self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
    ) {
        if !cfg.permissions.launch {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                "Launching cards from Telegram is disabled. Turn on <b>Launch new cards</b> in <b>Settings → Telegram</b>.",
                None,
                None,
                None,
            );
            return;
        }
        match backend.workspace_choices() {
            Ok(ws) => {
                let mut folders = Vec::new();
                folders.push(ws.current);
                for dir in ws.recent {
                    if !folders.contains(&dir) && folders.len() < 12 {
                        folders.push(dir);
                    }
                }
                self.new_wizard_folders = folders;
                let rows: Vec<Vec<InlineButton>> = self
                    .new_wizard_folders
                    .iter()
                    .enumerate()
                    .map(|(idx, dir)| {
                        vec![InlineButton::callback(
                            truncate_chars(&crate::state::display_dir(dir), 48),
                            format!("nwd:{idx}"),
                        )]
                    })
                    .collect();
                let kb = InlineKeyboardMarkup { inline_keyboard: rows };
                self.reply_and_track(
                    api,
                    runtime,
                    chat_id,
                    thread_id,
                    "Choose a project folder for the new card:",
                    None,
                    Some(&kb),
                    None,
                );
            }
            Err(err) => {
                let html = format!("Cannot list folders: <code>{}</code>", escape_html(&err));
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, None);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn send_key_for_card(
        &self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        cfg: &Config,
        runtime: &mut RuntimeState,
        chat_id: i64,
        thread_id: Option<i64>,
        card: &CardSummary,
        key: NamedKey,
    ) {
        if !cfg.permissions.controls || (card.is_shell && !cfg.permissions.shell_input) {
            self.reply_and_track(
                api,
                runtime,
                chat_id,
                thread_id,
                "Sending control keys is disabled. Check <b>Control keys</b> (and <b>Allow shell input</b> for shells) in <b>Settings → Telegram</b>.",
                None,
                None,
                Some(&card.id),
            );
            return;
        }
        match backend.send_key(&card.id, key) {
            Ok(()) => {
                let html = format!(
                    "✓ Sent <code>{}</code> to {} <b>{}</b>",
                    key.label(),
                    escape_html(&card.icon),
                    escape_html(&card.agent_name)
                );
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&card.id));
            }
            Err(err) => {
                let html = format!("Key failed: <code>{}</code>", escape_html(&err));
                self.reply_and_track(api, runtime, chat_id, thread_id, &html, None, None, Some(&card.id));
            }
        }
    }

    fn handle_callback(
        &mut self,
        api: &dyn TelegramApi,
        backend: &dyn DesktopBackend,
        state_dir: &Path,
        cfg: &Config,
        runtime: &mut RuntimeState,
        cq: &CallbackQuery,
        now_unix: u64,
    ) {
        let Some(owner) = cfg.allowed_user.as_ref() else {
            return;
        };
        if cq.from.id != owner.user_id {
            return;
        }
        let Some(msg) = cq.message.as_ref() else {
            let _ = api.answer_callback_query(&cq.id, None);
            return;
        };
        let is_private = msg.chat.id == owner.chat_id && msg.chat.chat_type == "private";
        let is_linked_forum = cfg.forum.as_ref().is_some_and(|f| f.chat_id == msg.chat.id);
        if !is_private && !is_linked_forum {
            return;
        }
        let thread_id = if is_linked_forum && msg.is_topic_message == Some(true) {
            msg.message_thread_id.filter(|&id| id > 1)
        } else {
            None
        };
        let data = cq.data.as_deref().unwrap_or("");
        let cards = backend.list_cards();

        if let Some(card_id) = data.strip_prefix("sel:") {
            let _ = api.answer_callback_query(&cq.id, Some("Card selected"));
            if let Some(card) = cards.iter().find(|c| c.id == card_id) {
                runtime.active_card_id = Some(card.id.clone());
                let kb = card_action_keyboard(card, cfg.permissions);
                self.reply_and_track(
                    api,
                    runtime,
                    msg.chat.id,
                    thread_id,
                    &format_card_header_html(card),
                    None,
                    kb.as_ref(),
                    Some(&card.id),
                );
            } else {
                self.reply_and_track(api, runtime, msg.chat.id, thread_id, "Card is no longer running.", None, None, None);
            }
        } else if let Some(card_id) = data.strip_prefix("scr:") {
            let _ = api.answer_callback_query(&cq.id, None);
            if let Some(card) = cards.iter().find(|c| c.id == card_id) {
                self.send_capture(api, backend, cfg, runtime, msg.chat.id, thread_id, card, 0, 2);
            } else {
                self.reply_and_track(api, runtime, msg.chat.id, thread_id, "Card is no longer running.", None, None, None);
            }
        } else if let Some(card_id) = data.strip_prefix("his:") {
            let _ = api.answer_callback_query(&cq.id, None);
            if let Some(card) = cards.iter().find(|c| c.id == card_id) {
                self.send_capture(api, backend, cfg, runtime, msg.chat.id, thread_id, card, 80, 3);
            } else {
                self.reply_and_track(api, runtime, msg.chat.id, thread_id, "Card is no longer running.", None, None, None);
            }
        } else if data.starts_with("dsh:") {
            let _ = api.answer_callback_query(&cq.id, Some("Refreshed"));
            let usage = backend.usage_summary();
            let html = format_dashboard_html(&cards, &usage);
            let app_url = if is_linked_forum {
                cfg.mini_app.dashboard_deep_link(cfg.bot_username.as_deref())
            } else {
                None
            };
            let kb = dashboard_keyboard(cfg.permissions, cfg.forum.is_some(), app_url.as_deref());
            let _ = api.edit_message_text(msg.chat.id, msg.message_id, &html, Some(&kb));
        } else if data.starts_with("usg:") {
            let _ = api.answer_callback_query(&cq.id, Some("Refreshed"));
            let usage = backend.usage_summary();
            let html = format_usage_html(&usage);
            let kb = usage_keyboard();
            let _ = api.edit_message_text(msg.chat.id, msg.message_id, &html, Some(&kb));
        } else if data.starts_with("nwo:") {
            let _ = api.answer_callback_query(&cq.id, None);
            self.send_new_wizard_folders(api, backend, cfg, runtime, msg.chat.id, thread_id);
        } else if data.starts_with("syn:") {
            let _ = api.answer_callback_query(&cq.id, Some("Synced"));
            if cfg.forum.is_some() {
                topics::sync_topics(api, cfg, runtime, &cards, now_unix, true);
                topics::sync_dashboard(
                    api,
                    cfg,
                    runtime,
                    &cards,
                    &backend.usage_summary(),
                    now_unix,
                    true,
                );
            }
        } else if let Some(card_id) = data.strip_prefix("liv:") {
            let _ = api.answer_callback_query(&cq.id, None);
            if let Some(card) = cards.iter().find(|c| c.id == card_id) {
                self.start_live_stream(api, backend, cfg, runtime, msg.chat.id, thread_id, card, now_unix);
            } else {
                self.reply_and_track(api, runtime, msg.chat.id, thread_id, "Card is no longer running.", None, None, None);
            }
        } else if let Some(card_id) = data.strip_prefix("lst:") {
            let _ = api.answer_callback_query(&cq.id, Some("Live stopped"));
            runtime.live_streams.retain(|s| s.card_id != card_id);
            let kb = InlineKeyboardMarkup {
                inline_keyboard: vec![vec![
                    InlineButton::callback("📺 Resume live", format!("liv:{card_id}")),
                    InlineButton::callback("📷 Screen", format!("scr:{card_id}")),
                ]],
            };
            let _ = api.edit_message_text(
                msg.chat.id,
                msg.message_id,
                "📺 <b>Live paused</b>.",
                Some(&kb),
            );
        } else if let Some(card_id) = data.strip_prefix("kyp:") {
            let _ = api.answer_callback_query(&cq.id, None);
            if let Some(card) = cards.iter().find(|c| c.id == card_id) {
                self.send_keys_panel(api, cfg, runtime, msg.chat.id, thread_id, card);
            } else {
                self.reply_and_track(api, runtime, msg.chat.id, thread_id, "Card is no longer running.", None, None, None);
            }
        } else if let Some(rest) = data.strip_prefix("qc:") {
            if let Some((idx_str, card_id)) = rest.split_once(':') {
                if let (Ok(idx), Some(card)) = (
                    idx_str.parse::<usize>(),
                    cards.iter().find(|c| c.id == card_id),
                ) {
                    if !cfg.permissions.prompts || card.is_shell {
                        let _ = api.answer_callback_query(&cq.id, Some("Submit prompts disabled"));
                    } else if let Some(&cmd) = quick_slash_commands(&card.agent_type).get(idx) {
                        match backend.send_prompt(&card.id, cmd) {
                            Ok(()) => {
                                let _ = api.answer_callback_query(&cq.id, Some(&format!("Sent {cmd}")));
                            }
                            Err(err) => {
                                let _ = api.answer_callback_query(&cq.id, Some("Send failed"));
                                let html = format!("Could not send <code>{cmd}</code>: <code>{}</code>", escape_html(&err));
                                self.reply_and_track(api, runtime, msg.chat.id, thread_id, &html, None, None, Some(&card.id));
                            }
                        }
                    } else {
                        let _ = api.answer_callback_query(&cq.id, None);
                    }
                } else {
                    let _ = api.answer_callback_query(&cq.id, Some("Card unavailable"));
                }
            }
        } else if let Some(rest) = data.strip_prefix("fil:").or_else(|| data.strip_prefix("flr:")) {
            let edit_mid = data.starts_with("flr:").then_some(msg.message_id);
            let _ = api.answer_callback_query(&cq.id, None);
            let (filter, page, card_id) = parse_files_callback(rest);
            if let Some(card) = cards.iter().find(|c| c.id == card_id) {
                self.send_files_list(api, backend, cfg, runtime, msg.chat.id, thread_id, card, filter, page, edit_mid);
            } else {
                self.reply_and_track(api, runtime, msg.chat.id, thread_id, "Card is no longer running.", None, None, None);
            }
        } else if let Some(pick_str) = data.strip_prefix("fop:") {
            let parsed = pick_str.parse::<u64>().ok();
            let pick = parsed.and_then(|id| self.lookup_file_pick(id));
            if let (Some(pick_id), Some(pick)) = (parsed, pick) {
                let _ = api.answer_callback_query(&cq.id, Some("Sending…"));
                self.open_file_pick(api, backend, cfg, runtime, msg.chat.id, thread_id, pick_id, &pick);
            } else {
                let _ = api.answer_callback_query(&cq.id, Some("List expired — tap 📎 Files again"));
            }
        } else if let Some(rest) = data.strip_prefix("ppg:") {
            if let Some((pick_str, page_str)) = rest.split_once(':') {
                if let (Ok(pick_id), Ok(page)) = (pick_str.parse::<u64>(), page_str.parse::<u32>()) {
                    if let Some(pick) = self.lookup_file_pick(pick_id) {
                        let _ = api.answer_callback_query(&cq.id, Some(&format!("Rendering page {page}…")));
                        self.open_pdf_page(api, backend, cfg, runtime, msg.chat.id, thread_id, pick_id, &pick, page);
                    } else {
                        let _ = api.answer_callback_query(&cq.id, Some("List expired — tap 📎 Files again"));
                    }
                }
            }
        } else if let Some(rest) = data.strip_prefix("tpg:") {
            if let Some((pick_str, page_str)) = rest.split_once(':') {
                if let (Ok(pick_id), Ok(page)) = (pick_str.parse::<u64>(), page_str.parse::<usize>()) {
                    if let Some(pick) = self.lookup_file_pick(pick_id) {
                        let _ = api.answer_callback_query(&cq.id, None);
                        self.open_text_page(api, backend, cfg, runtime, msg.chat.id, thread_id, pick_id, &pick, page, Some(msg.message_id));
                    } else {
                        let _ = api.answer_callback_query(&cq.id, Some("List expired — tap 📎 Files again"));
                    }
                }
            }
        } else if let Some(pick_str) = data.strip_prefix("tdl:") {
            if let Some(pick) = pick_str.parse::<u64>().ok().and_then(|id| self.lookup_file_pick(id)) {
                let _ = api.answer_callback_query(&cq.id, Some("Uploading file…"));
                self.download_text_pick(api, backend, cfg, runtime, msg.chat.id, thread_id, &pick);
            } else {
                let _ = api.answer_callback_query(&cq.id, Some("List expired — tap 📎 Files again"));
            }
        } else if let Some(rest) = data.strip_prefix("key:") {
            if let Some((code, card_id)) = rest.split_once(':') {
                if let (Some(key), Some(card)) = (
                    NamedKey::from_code(code),
                    cards.iter().find(|c| c.id == card_id),
                ) {
                    let _ = api.answer_callback_query(&cq.id, Some(key.label()));
                    self.send_key_for_card(api, backend, cfg, runtime, msg.chat.id, thread_id, card, key);
                } else {
                    let _ = api.answer_callback_query(&cq.id, Some("Card unavailable"));
                }
            } else {
                let _ = api.answer_callback_query(&cq.id, None);
            }
        } else if let Some(card_id) = data.strip_prefix("cls:") {
            let _ = api.answer_callback_query(&cq.id, None);
            if !cfg.permissions.close {
                self.reply_and_track(api, runtime, msg.chat.id, thread_id, "Closing cards is disabled in Settings → Telegram.", None, None, None);
            } else if let Some(card) = cards.iter().find(|c| c.id == card_id) {
                let kb = InlineKeyboardMarkup {
                    inline_keyboard: vec![vec![
                        InlineButton::callback("Confirm close", format!("clc:{}", card.id)),
                        InlineButton::callback("Cancel", format!("clx:{}", card.id)),
                    ]],
                };
                let html = format!(
                    "Close {} <b>{}</b> · <code>{}</code>?",
                    escape_html(&card.icon),
                    escape_html(&card.agent_name),
                    escape_html(&card.directory_display)
                );
                self.reply_and_track(api, runtime, msg.chat.id, thread_id, &html, None, Some(&kb), Some(&card.id));
            }
        } else if let Some(card_id) = data.strip_prefix("clc:") {
            if !cfg.permissions.close {
                let _ = api.answer_callback_query(&cq.id, Some("Close disabled"));
            } else {
                let _ = api.answer_callback_query(&cq.id, Some("Closing card"));
                match backend.close_card(card_id) {
                    Ok(()) => {
                        let _ = api.edit_message_text(
                            msg.chat.id,
                            msg.message_id,
                            "🛑 Card closed.",
                            None,
                        );
                        if cfg.forum.is_some() {
                            let updated_cards = backend.list_cards();
                            topics::sync_topics(api, cfg, runtime, &updated_cards, now_unix, false);
                            topics::sync_dashboard(
                                api,
                                cfg,
                                runtime,
                                &updated_cards,
                                &backend.usage_summary(),
                                now_unix,
                                true,
                            );
                        }
                    }
                    Err(err) => {
                        let _ = api.edit_message_text(
                            msg.chat.id,
                            msg.message_id,
                            &format!("Could not close card: <code>{}</code>", escape_html(&err)),
                            None,
                        );
                    }
                }
            }
        } else if data.starts_with("clx:") {
            let _ = api.answer_callback_query(&cq.id, Some("Cancelled"));
            let _ = api.edit_message_text(msg.chat.id, msg.message_id, "Cancelled.", None);
        } else if let Some(idx_str) = data.strip_prefix("nwd:") {
            let _ = api.answer_callback_query(&cq.id, None);
            if !cfg.permissions.launch {
                self.reply_and_track(api, runtime, msg.chat.id, thread_id, "Launching cards is disabled in Settings → Telegram.", None, None, None);
            } else if let Some(dir_idx) = idx_str
                .parse::<usize>()
                .ok()
                .filter(|&i| i < self.new_wizard_folders.len())
            {
                self.new_wizard_harnesses = backend.available_harnesses();
                if self.new_wizard_harnesses.is_empty() {
                    self.reply_and_track(api, runtime, msg.chat.id, thread_id, "No installed harnesses detected on this PC.", None, None, None);
                } else {
                    let rows: Vec<Vec<InlineButton>> = self
                        .new_wizard_harnesses
                        .chunks(2)
                        .enumerate()
                        .map(|(chunk_idx, pair)| {
                            pair.iter()
                                .enumerate()
                                .map(|(offset, h)| {
                                    let h_idx = chunk_idx * 2 + offset;
                                    InlineButton::callback(
                                        format!("{} {}", h.icon, h.name),
                                        format!("nwh:{dir_idx}:{h_idx}"),
                                    )
                                })
                                .collect()
                        })
                        .collect();
                    let kb = InlineKeyboardMarkup { inline_keyboard: rows };
                    let dir_display = crate::state::display_dir(&self.new_wizard_folders[dir_idx]);
                    let html = format!("Choose a harness for <code>{}</code>:", escape_html(&dir_display));
                    let _ = api.edit_message_text(msg.chat.id, msg.message_id, &html, Some(&kb));
                }
            } else {
                self.reply_and_track(api, runtime, msg.chat.id, thread_id, "Folder choice expired. Run /new again.", None, None, None);
            }
        } else if let Some(rest) = data.strip_prefix("nwh:") {
            if !cfg.permissions.launch {
                let _ = api.answer_callback_query(&cq.id, Some("Launch disabled"));
            } else if let Some((d_str, h_str)) = rest.split_once(':') {
                let dir = d_str
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| self.new_wizard_folders.get(i))
                    .cloned();
                let harness = h_str
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| self.new_wizard_harnesses.get(i))
                    .cloned();
                match (dir, harness) {
                    (Some(dir), Some(harness)) => {
                        let _ = api.answer_callback_query(&cq.id, Some("Launching…"));
                        match backend.launch_card(&harness.id, &dir) {
                            Ok(new_id) => {
                                runtime.active_card_id = Some(new_id.clone());
                                if cfg.forum.is_some() {
                                    let updated_cards = backend.list_cards();
                                    topics::sync_topics(api, cfg, runtime, &updated_cards, now_unix, false);
                                }
                                let dir_display = crate::state::display_dir(&dir);
                                let html = format!(
                                    "✅ Launched {} <b>{}</b> in <code>{}</code>",
                                    escape_html(&harness.icon),
                                    escape_html(&harness.name),
                                    escape_html(&dir_display)
                                );
                                let _ = api.edit_message_text(msg.chat.id, msg.message_id, &html, None);
                                runtime.record_message_card(msg.chat.id, msg.message_id, &new_id);
                            }
                            Err(err) => {
                                let _ = api.edit_message_text(
                                    msg.chat.id,
                                    msg.message_id,
                                    &format!("Launch failed: <code>{}</code>", escape_html(&err)),
                                    None,
                                );
                            }
                        }
                    }
                    _ => {
                        let _ = api.answer_callback_query(&cq.id, Some("Expired — run /new again"));
                    }
                }
            }
        } else {
            let _ = api.answer_callback_query(&cq.id, None);
        }

        let _ = save_runtime_at(&state_dir.join("runtime.json"), runtime);
    }
}

fn on_off(val: bool) -> &'static str {
    if val { "on" } else { "off" }
}

pub(super) fn is_telegram_button_url(raw: &str) -> bool {
    if raw.len() > 2048 {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    let lower = host.to_ascii_lowercase();
    lower.contains('.')
        && lower != "localhost"
        && !lower.ends_with(".localhost")
        && !lower.ends_with(".local")
        && !lower.ends_with(".internal")
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", (bytes + 512) / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn asset_icon(asset: &CardAsset) -> &'static str {
    match asset.kind.as_str() {
        "image" => "🖼",
        "pdf" => "📕",
        "markdown" => "📝",
        _ => "📄",
    }
}

fn parse_files_callback(rest: &str) -> (char, usize, &str) {
    let mut parts = rest.splitn(3, ':');
    let first = parts.next().unwrap_or("");
    let second = parts.next();
    let third = parts.next();
    if let (Some(page_s), Some(card_id)) = (second, third) {
        let filter = first.chars().next().unwrap_or('a');
        let page = page_s.parse::<usize>().unwrap_or(0);
        return (filter, page, card_id);
    }
    ('a', 0, rest)
}

#[allow(clippy::too_many_arguments)]
fn send_media_with_doc_fallback(
    api: &dyn TelegramApi,
    chat_id: i64,
    thread_id: Option<i64>,
    filename: &str,
    mime_type: &str,
    bytes: &[u8],
    caption: &str,
    animation: bool,
) -> Result<i64, super::api::ApiError> {
    let first = if animation {
        api.send_animation(chat_id, thread_id, filename, mime_type, bytes, Some(caption), None)
    } else {
        api.send_photo(chat_id, thread_id, filename, mime_type, bytes, Some(caption), None)
    };
    match first {
        Err(super::api::ApiError::BadRequest(_)) => {
            api.send_document(chat_id, thread_id, filename, mime_type, bytes, Some(caption), None)
        }
        other => other,
    }
}

fn prepare_photo(api: &dyn TelegramApi, photos: &[PhotoSize]) -> Result<Attachment, String> {
    let chosen = photos
        .iter()
        .filter(|p| p.width <= 2048 && p.height <= 2048 && p.file_size.unwrap_or(0) <= MAX_IMAGE as u64)
        .max_by_key(|p| p.width * p.height)
        .or_else(|| photos.first())
        .ok_or_else(|| "Empty photo list.".to_string())?;
    let info = api.get_file(&chosen.file_id).map_err(|e| e.to_string())?;
    if info.file_size.is_some_and(|sz| sz > MAX_IMAGE as u64) {
        return Err("Photo exceeds 2 MiB image limit.".into());
    }
    let path = info
        .file_path
        .ok_or_else(|| "Telegram did not return a file path.".to_string())?;
    let bytes = api
        .download_file_bounded(&path, MAX_IMAGE)
        .map_err(|e| e.to_string())?;
    normalize_telegram_image(&bytes)
}

fn prepare_file(
    api: &dyn TelegramApi,
    file_id: &str,
    raw_name: &str,
    reported_size: Option<u64>,
) -> Result<Attachment, String> {
    if reported_size.is_some_and(|sz| sz > MAX_TOTAL as u64) {
        return Err("File exceeds 16 MiB attachment limit.".into());
    }
    let info = api.get_file(file_id).map_err(|e| e.to_string())?;
    if info.file_size.is_some_and(|sz| sz > MAX_TOTAL as u64) {
        return Err("File exceeds 16 MiB attachment limit.".into());
    }
    let path = info
        .file_path
        .ok_or_else(|| "Telegram did not return a file path.".to_string())?;
    let bytes = api
        .download_file_bounded(&path, MAX_TOTAL)
        .map_err(|e| e.to_string())?;
    Ok(Attachment {
        kind: Kind::File,
        name: safe_name(raw_name, Kind::File),
        bytes,
    })
}
