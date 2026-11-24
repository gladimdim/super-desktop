//! Comprehensive offline tests for Telegram config, redaction, pairing, Forum Topics,
//! permissions gating, notifications, and HTTP TelegramClient against a local mock server.
use super::api::*;
use super::config::*;
use super::desktop::{
    extract_terminal_links, CardAsset, CardReferences, CardSummary, DesktopBackend, HarnessChoice,
    NamedKey, ProviderUsageLimit, ProviderUsageSummary, WorkspaceInfo,
};
use super::notifier::NotifierState;
use super::pairing;
use super::redact::*;
use super::router::Router;
use super::topics;
use crate::prompt_attachments::Attachment;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn scratch_dir(tag: &str) -> PathBuf {
    let nonce = super_desktop::control::random_hex(6).unwrap();
    let dir = std::env::temp_dir().join(format!("sd-tg-{tag}-{nonce}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

#[derive(Clone, Debug)]
struct SentMsg {
    chat_id: i64,
    thread_id: Option<i64>,
    html: String,
    keyboard: Option<InlineKeyboardMarkup>,
}

#[derive(Clone, Debug)]
struct SentMedia {
    chat_id: i64,
    thread_id: Option<i64>,
    filename: String,
    mime_type: String,
    bytes: Vec<u8>,
    caption: Option<String>,
}

#[derive(Default)]
struct MockApiState {
    next_msg_id: i64,
    next_thread_id: i64,
    sent: Vec<SentMsg>,
    sent_photos: Vec<SentMedia>,
    sent_animations: Vec<SentMedia>,
    sent_documents: Vec<SentMedia>,
    fail_photo_bad_request: bool,
    fail_reactions: bool,
    fail_delete_topic_bad_request: bool,
    chat_actions: Vec<(i64, Option<i64>, String)>,
    reactions: Vec<(i64, i64, String)>,
    edited_msgs: Vec<(i64, i64, String, Option<InlineKeyboardMarkup>)>,
    pinned_msgs: Vec<(i64, i64, bool)>,
    answered_callbacks: Vec<(String, Option<String>)>,
    created_topics: Vec<(i64, String, u32, i64)>,
    renamed_topics: Vec<(i64, i64, String)>,
    closed_topics: Vec<(i64, i64)>,
    deleted_topics: Vec<(i64, i64)>,
    reopened_topics: Vec<(i64, i64)>,
    deleted_threads: std::collections::HashSet<i64>,
    deleted_messages: std::collections::HashSet<i64>,
    chat_info: Option<ChatInfo>,
    chat_member: Option<ChatMember>,
    chat_member_count: u64,
    files: HashMap<String, (FileInfo, Vec<u8>)>,
}

struct MockApi {
    state: Mutex<MockApiState>,
}

impl MockApi {
    fn new() -> Self {
        Self {
            state: Mutex::new(MockApiState {
                next_msg_id: 100,
                next_thread_id: 500,
                chat_member_count: 2,
                ..Default::default()
            }),
        }
    }
}

impl TelegramApi for MockApi {
    fn get_me(&self) -> Result<BotUser, ApiError> {
        Ok(BotUser {
            id: 999,
            is_bot: true,
            first_name: "SD Bot".into(),
            username: Some("sd_test_bot".into()),
        })
    }

    fn get_updates(&self, _offset: i64, _timeout_secs: u64) -> Result<Vec<Update>, ApiError> {
        Ok(Vec::new())
    }

    fn send_message(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        html: &str,
        _reply_to: Option<i64>,
        keyboard: Option<&InlineKeyboardMarkup>,
    ) -> Result<i64, ApiError> {
        let mut st = self.state.lock().unwrap();
        if let Some(tid) = thread_id {
            if st.deleted_threads.contains(&tid) {
                return Err(ApiError::TopicNotFound);
            }
        }
        st.next_msg_id += 1;
        let id = st.next_msg_id;
        st.sent.push(SentMsg {
            chat_id,
            thread_id,
            html: html.to_string(),
            keyboard: keyboard.cloned(),
        });
        Ok(id)
    }

    fn send_photo(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        caption_html: Option<&str>,
        _keyboard: Option<&InlineKeyboardMarkup>,
    ) -> Result<i64, ApiError> {
        let mut st = self.state.lock().unwrap();
        if st.fail_photo_bad_request {
            return Err(ApiError::BadRequest("PHOTO_INVALID_DIMENSIONS".into()));
        }
        st.next_msg_id += 1;
        let id = st.next_msg_id;
        st.sent_photos.push(SentMedia {
            chat_id,
            thread_id,
            filename: filename.to_string(),
            mime_type: mime_type.to_string(),
            bytes: bytes.to_vec(),
            caption: caption_html.map(str::to_string),
        });
        Ok(id)
    }

    fn send_animation(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        caption_html: Option<&str>,
        _keyboard: Option<&InlineKeyboardMarkup>,
    ) -> Result<i64, ApiError> {
        let mut st = self.state.lock().unwrap();
        st.next_msg_id += 1;
        let id = st.next_msg_id;
        st.sent_animations.push(SentMedia {
            chat_id,
            thread_id,
            filename: filename.to_string(),
            mime_type: mime_type.to_string(),
            bytes: bytes.to_vec(),
            caption: caption_html.map(str::to_string),
        });
        Ok(id)
    }

    fn send_document(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        caption_html: Option<&str>,
        _keyboard: Option<&InlineKeyboardMarkup>,
    ) -> Result<i64, ApiError> {
        let mut st = self.state.lock().unwrap();
        st.next_msg_id += 1;
        let id = st.next_msg_id;
        st.sent_documents.push(SentMedia {
            chat_id,
            thread_id,
            filename: filename.to_string(),
            mime_type: mime_type.to_string(),
            bytes: bytes.to_vec(),
            caption: caption_html.map(str::to_string),
        });
        Ok(id)
    }

    fn send_chat_action(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        action: &str,
    ) -> Result<(), ApiError> {
        self.state
            .lock()
            .unwrap()
            .chat_actions
            .push((chat_id, thread_id, action.to_string()));
        Ok(())
    }

    fn set_message_reaction(
        &self,
        chat_id: i64,
        message_id: i64,
        emoji: &str,
    ) -> Result<(), ApiError> {
        let mut st = self.state.lock().unwrap();
        if st.fail_reactions {
            return Err(ApiError::BadRequest("REACTION_INVALID".into()));
        }
        st.reactions.push((chat_id, message_id, emoji.to_string()));
        Ok(())
    }

    fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        html: &str,
        keyboard: Option<&InlineKeyboardMarkup>,
    ) -> Result<(), ApiError> {
        let mut st = self.state.lock().unwrap();
        if st.deleted_messages.contains(&message_id) {
            return Err(ApiError::MessageNotFound);
        }
        st.edited_msgs
            .push((chat_id, message_id, html.to_string(), keyboard.cloned()));
        Ok(())
    }

    fn pin_chat_message(
        &self,
        chat_id: i64,
        message_id: i64,
        disable_notification: bool,
    ) -> Result<(), ApiError> {
        let mut st = self.state.lock().unwrap();
        if st.deleted_messages.contains(&message_id) {
            return Err(ApiError::MessageNotFound);
        }
        st.pinned_msgs
            .push((chat_id, message_id, disable_notification));
        Ok(())
    }

    fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<(), ApiError> {
        self.state
            .lock()
            .unwrap()
            .answered_callbacks
            .push((callback_id.to_string(), text.map(str::to_string)));
        Ok(())
    }

    fn set_my_commands(&self, _commands: &[(&str, &str)]) -> Result<(), ApiError> {
        Ok(())
    }

    fn get_chat(&self, chat_id: i64) -> Result<ChatInfo, ApiError> {
        let st = self.state.lock().unwrap();
        Ok(st.chat_info.clone().unwrap_or(ChatInfo {
            id: chat_id,
            chat_type: "supergroup".into(),
            title: Some("SD Topics".into()),
            is_forum: Some(true),
        }))
    }

    fn get_chat_member(&self, _chat_id: i64, _user_id: i64) -> Result<ChatMember, ApiError> {
        let st = self.state.lock().unwrap();
        Ok(st.chat_member.clone().unwrap_or(ChatMember {
            status: "administrator".into(),
            can_manage_topics: Some(true),
        }))
    }

    fn get_chat_member_count(&self, _chat_id: i64) -> Result<u64, ApiError> {
        Ok(self.state.lock().unwrap().chat_member_count)
    }

    fn create_forum_topic(
        &self,
        chat_id: i64,
        name: &str,
        icon_color: u32,
    ) -> Result<ForumTopic, ApiError> {
        let mut st = self.state.lock().unwrap();
        st.next_thread_id += 1;
        let tid = st.next_thread_id;
        st.created_topics
            .push((chat_id, name.to_string(), icon_color, tid));
        Ok(ForumTopic {
            message_thread_id: tid,
            name: name.to_string(),
            icon_color,
        })
    }

    fn edit_forum_topic(&self, chat_id: i64, thread_id: i64, name: &str) -> Result<(), ApiError> {
        let mut st = self.state.lock().unwrap();
        if st.deleted_threads.contains(&thread_id) {
            return Err(ApiError::TopicNotFound);
        }
        st.renamed_topics
            .push((chat_id, thread_id, name.to_string()));
        Ok(())
    }

    fn close_forum_topic(&self, chat_id: i64, thread_id: i64) -> Result<(), ApiError> {
        let mut st = self.state.lock().unwrap();
        if st.deleted_threads.contains(&thread_id) {
            return Err(ApiError::TopicNotFound);
        }
        st.closed_topics.push((chat_id, thread_id));
        Ok(())
    }

    fn delete_forum_topic(&self, chat_id: i64, thread_id: i64) -> Result<(), ApiError> {
        let mut st = self.state.lock().unwrap();
        if st.deleted_threads.contains(&thread_id) {
            return Err(ApiError::TopicNotFound);
        }
        if st.fail_delete_topic_bad_request {
            return Err(ApiError::BadRequest("not enough rights".into()));
        }
        st.deleted_topics.push((chat_id, thread_id));
        st.deleted_threads.insert(thread_id);
        Ok(())
    }

    fn reopen_forum_topic(&self, chat_id: i64, thread_id: i64) -> Result<(), ApiError> {
        let mut st = self.state.lock().unwrap();
        if st.deleted_threads.contains(&thread_id) {
            return Err(ApiError::TopicNotFound);
        }
        st.reopened_topics.push((chat_id, thread_id));
        Ok(())
    }

    fn get_file(&self, file_id: &str) -> Result<FileInfo, ApiError> {
        let st = self.state.lock().unwrap();
        st.files
            .get(file_id)
            .map(|(info, _)| info.clone())
            .ok_or_else(|| ApiError::BadRequest("file not found".into()))
    }

    fn download_file_bounded(&self, file_path: &str, max_bytes: usize) -> Result<Vec<u8>, ApiError> {
        let st = self.state.lock().unwrap();
        let (_, bytes) = st
            .files
            .values()
            .find(|(info, _)| info.file_path.as_deref() == Some(file_path))
            .ok_or_else(|| ApiError::BadRequest("missing file".into()))?;
        if bytes.len() > max_bytes {
            return Err(ApiError::BadRequest("file too large".into()));
        }
        Ok(bytes.clone())
    }
}

struct MockBackend {
    cards: Mutex<Vec<CardSummary>>,
    usage: Mutex<Vec<ProviderUsageSummary>>,
    screens: Mutex<HashMap<String, String>>,
    references: Mutex<HashMap<String, CardReferences>>,
    assets: Mutex<HashMap<(String, String), (CardAsset, Vec<u8>)>>,
    pdf_pages: Mutex<HashMap<(String, String, u32), Vec<u8>>>,
    prompts_sent: Mutex<Vec<(String, String)>>,
    keys_sent: Mutex<Vec<(String, NamedKey)>>,
    attachments_sent: Mutex<Vec<(String, String, String, Vec<u8>)>>,
    launched: Mutex<Vec<(String, String)>>,
    closed: Mutex<Vec<String>>,
    workspaces: Mutex<WorkspaceInfo>,
    harnesses: Mutex<Vec<HarnessChoice>>,
}

impl MockBackend {
    fn new(cards: Vec<CardSummary>) -> Self {
        Self {
            cards: Mutex::new(cards),
            usage: Mutex::new(Vec::new()),
            screens: Mutex::new(HashMap::new()),
            references: Mutex::new(HashMap::new()),
            assets: Mutex::new(HashMap::new()),
            pdf_pages: Mutex::new(HashMap::new()),
            prompts_sent: Mutex::new(Vec::new()),
            keys_sent: Mutex::new(Vec::new()),
            attachments_sent: Mutex::new(Vec::new()),
            launched: Mutex::new(Vec::new()),
            closed: Mutex::new(Vec::new()),
            workspaces: Mutex::new(WorkspaceInfo {
                current: "/home/user/proj".into(),
                recent: vec!["/home/user/other".into()],
            }),
            harnesses: Mutex::new(vec![
                HarnessChoice {
                    id: "claude".into(),
                    name: "Claude Code".into(),
                    icon: "⚡".into(),
                },
                HarnessChoice {
                    id: "custom-very-long-harness-identifier-that-exceeds-50-chars-easily".into(),
                    name: "Custom".into(),
                    icon: "🚀".into(),
                },
            ]),
        }
    }
}

impl DesktopBackend for MockBackend {
    fn list_cards(&self) -> Vec<CardSummary> {
        self.cards.lock().unwrap().clone()
    }

    fn usage_summary(&self) -> Vec<ProviderUsageSummary> {
        self.usage.lock().unwrap().clone()
    }

    fn capture_screen(&self, card_id: &str, _history_lines: u32) -> Result<String, String> {
        Ok(self
            .screens
            .lock()
            .unwrap()
            .get(card_id)
            .cloned()
            .unwrap_or_default())
    }

    fn list_references(&self, card_id: &str) -> Result<CardReferences, String> {
        Ok(self
            .references
            .lock()
            .unwrap()
            .get(card_id)
            .cloned()
            .unwrap_or_default())
    }

    fn read_asset(&self, card_id: &str, asset_id: &str) -> Result<(CardAsset, Vec<u8>), String> {
        self.assets
            .lock()
            .unwrap()
            .get(&(card_id.to_string(), asset_id.to_string()))
            .cloned()
            .ok_or_else(|| "asset_expired_refresh_list".to_string())
    }

    fn render_pdf_page(
        &self,
        card_id: &str,
        asset_id: &str,
        page: u32,
    ) -> Result<(CardAsset, Vec<u8>), String> {
        let (asset, _) = self.read_asset(card_id, asset_id)?;
        let bytes = self
            .pdf_pages
            .lock()
            .unwrap()
            .get(&(card_id.to_string(), asset_id.to_string(), page))
            .cloned()
            .ok_or_else(|| "page out of range".to_string())?;
        Ok((asset, bytes))
    }

    fn send_prompt(&self, card_id: &str, text: &str) -> Result<(), String> {
        self.prompts_sent
            .lock()
            .unwrap()
            .push((card_id.to_string(), text.to_string()));
        Ok(())
    }

    fn send_key(&self, card_id: &str, key: NamedKey) -> Result<(), String> {
        self.keys_sent
            .lock()
            .unwrap()
            .push((card_id.to_string(), key));
        Ok(())
    }

    fn send_attachment(
        &self,
        card_id: &str,
        _owner: &str,
        caption: &str,
        attachment: Attachment,
    ) -> Result<(), String> {
        self.attachments_sent.lock().unwrap().push((
            card_id.to_string(),
            caption.to_string(),
            attachment.name,
            attachment.bytes,
        ));
        Ok(())
    }

    fn workspace_choices(&self) -> Result<WorkspaceInfo, String> {
        Ok(self.workspaces.lock().unwrap().clone())
    }

    fn available_harnesses(&self) -> Vec<HarnessChoice> {
        self.harnesses.lock().unwrap().clone()
    }

    fn launch_card(&self, agent_type: &str, workspace: &str) -> Result<String, String> {
        let ws = self.workspaces.lock().unwrap();
        if workspace != ws.current && !ws.recent.iter().any(|r| r == workspace) {
            return Err("Workspace not allowed".into());
        }
        self.launched
            .lock()
            .unwrap()
            .push((agent_type.to_string(), workspace.to_string()));
        let new_id = "sd_term_launched_1".to_string();
        self.cards.lock().unwrap().push(sample_card(&new_id, agent_type, false));
        Ok(new_id)
    }

    fn close_card(&self, card_id: &str) -> Result<(), String> {
        self.closed.lock().unwrap().push(card_id.to_string());
        self.cards.lock().unwrap().retain(|c| c.id != card_id);
        Ok(())
    }
}

fn sample_card(id: &str, agent: &str, is_shell: bool) -> CardSummary {
    CardSummary {
        id: id.to_string(),
        agent_type: agent.to_string(),
        agent_name: if is_shell { "Shell" } else { "Claude Code" }.to_string(),
        icon: if is_shell { "💻" } else { "⚡" }.to_string(),
        status: "IDLE".to_string(),
        label: "Idle".to_string(),
        model: None,
        effort: None,
        title: Some("Fix login bug".to_string()),
        last_prompt: Some("Fix login bug".to_string()),
        directory: "/home/user/super-desktop".to_string(),
        directory_display: "~/super-desktop".to_string(),
        tag: 2,
        is_shell,
        completion_supported: true,
        completion_state: "unknown".to_string(),
        completion_id: None,
    }
}

fn user(id: i64) -> User {
    User {
        id,
        is_bot: false,
        first_name: "Owner".into(),
        username: Some("owner".into()),
    }
}

fn text_msg(chat_id: i64, chat_type: &str, from_id: i64, msg_id: i64, text: &str, now: u64) -> Message {
    Message {
        message_id: msg_id,
        message_thread_id: None,
        is_topic_message: None,
        from: Some(user(from_id)),
        chat: Chat {
            id: chat_id,
            chat_type: chat_type.to_string(),
            title: None,
            is_forum: None,
        },
        date: now,
        text: Some(text.to_string()),
        caption: None,
        reply_to_message: None,
        photo: None,
        document: None,
        voice: None,
        audio: None,
    }
}

#[test]
fn telegram_config_permissions_symlink_and_token_masking() {
    let dir = scratch_dir("cfg");
    let cfg_path = dir.join("config.json");
    assert_eq!(load_config_at(&cfg_path).unwrap(), Config::default());

    let mut cfg = Config::default();
    cfg.enabled = true;
    cfg.bot_token = "123456789:SECRET_BOT_TOKEN_ABC".into();
    save_config_at(&cfg_path, &cfg).unwrap();
    assert_eq!(
        std::fs::metadata(&cfg_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(load_config_at(&cfg_path).unwrap(), cfg);

    // Loose file permissions fail closed.
    std::fs::set_permissions(&cfg_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(load_config_at(&cfg_path).is_err());

    // Symlink fails closed.
    std::fs::remove_file(&cfg_path).unwrap();
    std::os::unix::fs::symlink(dir.join("missing"), &cfg_path).unwrap();
    assert!(load_config_at(&cfg_path).is_err());

    // Corrupt JSON fails closed.
    std::fs::remove_file(&cfg_path).unwrap();
    std::fs::write(&cfg_path, b"{bad json").unwrap();
    std::fs::set_permissions(&cfg_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(load_config_at(&cfg_path).is_err());

    // Token masking removes raw token from errors.
    let err = mask_token_in_text(
        "POST https://api.telegram.org/bot123456789:SECRET_BOT_TOKEN_ABC/getUpdates failed",
        &cfg.bot_token,
    );
    assert!(!err.contains("SECRET_BOT_TOKEN_ABC"));
    assert!(err.contains("[REDACTED_TOKEN]"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn telegram_redaction_chunking_and_topic_formatting() {
    let token = "123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZ123456";
    let raw = format!(
        "\x1b[32mok\x1b[0m\n\
         OPENAI_API_KEY=sk-ant-api03-1234567890abcdefghijklmnop\n\
         MY_SECRET=super_secret_value\n\
         github=ghp_1234567890abcdef123456\n\
         aws=AKIA1234567890ABCDEF\n\
         Authorization: Bearer abcdefghij1234567890xyz\n\
         -----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----\n\
         bot={token}\n\
         other_bot=987654321:ZYXWVUTSRQPONMLKJIHGFEDCBA987654\n"
    );
    let cleaned = sanitize_terminal_text(&raw, token, true);
    assert!(!cleaned.contains('\x1b'));
    assert!(!cleaned.contains("sk-ant-"));
    assert!(!cleaned.contains("super_secret_value"));
    assert!(!cleaned.contains("ghp_123456"));
    assert!(!cleaned.contains("AKIA1234567890ABCDEF"));
    assert!(!cleaned.contains("abcdefghij1234567890xyz"));
    assert!(!cleaned.contains("MIIEowIBAAKCAQEA"));
    assert!(!cleaned.contains(token));
    assert!(!cleaned.contains("987654321:ZYXWVUTSRQPONMLKJIHGFEDCBA987654"));
    assert!(cleaned.contains("[REDACTED_PRIVATE_KEY]"));
    assert!(cleaned.contains("[REDACTED_TOKEN]"));
    assert!(cleaned.contains("[REDACTED_BOT_TOKEN]"));

    assert_eq!(escape_html("a < b & c > d"), "a &lt; b &amp; c &gt; d");

    let long_text = (0..100).map(|i| format!("line {i} with content\n")).collect::<String>();
    let chunks = split_chunks(&long_text, 300);
    assert!(chunks.len() > 2);
    assert!(chunks.iter().all(|c| c.chars().count() <= 300));

    let long_title = "Very long prompt ".repeat(20);
    let topic = topic_title("WORKING", "⚡", "Claude Code", "~/GitHub/super-desktop", Some(&long_title));
    assert!(topic.chars().count() <= MAX_TOPIC_NAME_CHARS);
    assert!(!topic.contains('\n'));
    assert!(topic.starts_with("🟢 ⚡ Claude Code · super-desktop"));
    assert!(topic_title("WAITING", "⚡", "Claude Code", "~/p", None).starts_with("🟡 "));
    assert!(topic_title("FINISHED", "⚡", "Claude Code", "~/p", None).starts_with("✅ "));
    assert!(topic_title("ERROR", "⚡", "Claude Code", "~/p", None).starts_with("🔴 "));
    assert!(topic_title("EXITED", "⚡", "Claude Code", "~/p", None).starts_with("⚫ "));
    assert!(topic_title("IDLE", "⚡", "Claude Code", "~/p", None).starts_with("⚪ "));

    for tag in 0..=8 {
        assert!(TELEGRAM_TOPIC_COLORS.contains(&tag_to_topic_color(tag)));
    }
}

#[test]
fn telegram_pairing_authorization_and_stale_guard() {
    let dir = scratch_dir("pair");
    let api = MockApi::new();
    let backend = MockBackend::new(vec![sample_card("sd_term_1", "claude", false)]);
    let now = unix_now();

    let (invite, link) = {
        let mut cfg = Config::default();
        cfg.enabled = true;
        cfg.bot_token = "123:tok".into();
        cfg.bot_username = Some("sd_test_bot".into());
        save_config_at(&dir.join("config.json"), &cfg).unwrap();
        pairing::start_invite(&dir).unwrap()
    };
    assert!(link.unwrap().contains(&invite.secret));

    let mut cfg = load_config_at(&dir.join("config.json")).unwrap();
    let mut rt = load_runtime_at(&dir.join("runtime.json")).unwrap();
    let mut router = Router::new(999, Some("sd_test_bot".into()), now);

    // Non-private /start cannot pair.
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 1,
            message: Some(text_msg(-1001, "supergroup", 777, 1, &format!("/start {}", invite.secret), now)),
            callback_query: None,
        },
        now,
    );
    assert!(load_pending_request_at(&dir).unwrap().is_none());

    // Wrong secret does not create pending request.
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 2,
            message: Some(text_msg(777, "private", 777, 2, "/start wrong_secret", now)),
            callback_query: None,
        },
        now,
    );
    assert!(load_pending_request_at(&dir).unwrap().is_none());

    // Valid secret in private chat creates 6-digit pending request & consumes invite.
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 3,
            message: Some(text_msg(777, "private", 777, 3, &format!("/start {}", invite.secret), now)),
            callback_query: None,
        },
        now,
    );
    let req = load_pending_request_at(&dir)
        .unwrap()
        .expect("pending request created");
    assert_eq!(req.user_id, 777);
    assert_eq!(req.code.len(), 6);
    assert!(load_invite_at(&dir).unwrap().is_none());

    // Approve on PC.
    let approved = pairing::approve_pending(&dir, &req.request_id).unwrap();
    assert_eq!(approved.user_id, 777);
    assert!(load_pending_request_at(&dir).unwrap().is_none());
    cfg = load_config_at(&dir.join("config.json")).unwrap();
    rt = load_runtime_at(&dir.join("runtime.json")).unwrap();
    cfg.permissions.prompts = true;
    rt.active_card_id = Some("sd_term_1".into());

    // Stranger is silently ignored (even with active card).
    let sent_before = api.state.lock().unwrap().sent.len();
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 4,
            message: Some(text_msg(888, "private", 888, 4, "rm -rf /", now)),
            callback_query: None,
        },
        now,
    );
    assert!(backend.prompts_sent.lock().unwrap().is_empty());
    assert_eq!(api.state.lock().unwrap().sent.len(), sent_before);

    // Stale message (>120s old) is ignored.
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 5,
            message: Some(text_msg(777, "private", 777, 5, "old stale command", now.saturating_sub(150))),
            callback_query: None,
        },
        now,
    );
    assert!(backend.prompts_sent.lock().unwrap().is_empty());

    // Fresh owner message works.
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 6,
            message: Some(text_msg(777, "private", 777, 6, "fix tests", now)),
            callback_query: None,
        },
        now,
    );
    assert_eq!(backend.prompts_sent.lock().unwrap().len(), 1);

    // Reject flow blocks future invite attempts from that user.
    let (invite2, _) = pairing::start_invite(&dir).unwrap();
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 7,
            message: Some(text_msg(555, "private", 555, 7, &format!("/start {}", invite2.secret), now)),
            callback_query: None,
        },
        now,
    );
    let req2 = load_pending_request_at(&dir).unwrap().unwrap();
    pairing::reject_pending(&dir, &req2.request_id).unwrap();
    cfg = load_config_at(&dir.join("config.json")).unwrap();
    assert!(cfg.rejected_user_ids.contains(&555));

    // Revoke wipes allowed_user and forum.
    pairing::revoke_user(&dir).unwrap();
    cfg = load_config_at(&dir.join("config.json")).unwrap();
    assert!(cfg.allowed_user.is_none());
    assert!(cfg.forum.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn telegram_pairing_mailbox_survives_concurrent_runtime_saves() {
    let dir = scratch_dir("mailbox");
    let api = MockApi::new();
    let backend = MockBackend::new(vec![]);
    let now = unix_now();

    let cfg_init = Config {
        enabled: true,
        bot_token: "123:tok".into(),
        bot_username: Some("sd_test_bot".into()),
        ..Config::default()
    };
    save_config_at(&dir.join("config.json"), &cfg_init).unwrap();

    // 1. Settings writes invite.json.
    let (invite, _) = pairing::start_invite(&dir).unwrap();
    assert!(load_invite_at(&dir).unwrap().is_some());

    // 2. Worker saves runtime.json concurrently (e.g. long-poll offset bump).
    let mut worker_rt = RuntimeState {
        update_offset: 99,
        ..RuntimeState::default()
    };
    save_runtime_at(&dir.join("runtime.json"), &worker_rt).unwrap();

    // 3. invite.json must survive and /start <secret> must create pending_request.json.
    assert!(load_invite_at(&dir).unwrap().is_some());
    let mut cfg = load_config_at(&dir.join("config.json")).unwrap();
    let mut router = Router::new(999, Some("sd_test_bot".into()), now);
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut worker_rt,
        &Update {
            update_id: 100,
            message: Some(text_msg(777, "private", 777, 1, &format!("/start {}", invite.secret), now)),
            callback_query: None,
        },
        now,
    );
    let req = load_pending_request_at(&dir).unwrap().expect("pending_request.json written");
    assert!(load_invite_at(&dir).unwrap().is_none());

    // 4. Settings approves request (clears pending_request.json, updates config.json).
    pairing::approve_pending(&dir, &req.request_id).unwrap();
    assert!(load_pending_request_at(&dir).unwrap().is_none());

    // 5. Worker saves runtime.json again; pending_request.json stays gone (no resurrection).
    worker_rt.update_offset = 101;
    save_runtime_at(&dir.join("runtime.json"), &worker_rt).unwrap();
    assert!(load_pending_request_at(&dir).unwrap().is_none());
    assert_eq!(
        load_config_at(&dir.join("config.json"))
            .unwrap()
            .allowed_user
            .unwrap()
            .user_id,
        777
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn telegram_forum_topics_linking_sync_routing_and_lifecycle() {
    let dir = scratch_dir("topics");
    let api = MockApi::new();
    let card1 = sample_card("sd_term_1", "claude", false);
    let card2 = sample_card("sd_term_2", "codex", false);
    let backend = MockBackend::new(vec![card1.clone(), card2.clone()]);
    let now = 1_700_000_000;

    let mut cfg = Config {
        enabled: true,
        bot_token: "123:tok".into(),
        bot_id: Some(999),
        allowed_user: Some(AllowedUser {
            user_id: 777,
            chat_id: 777,
            username: Some("owner".into()),
            first_name: "Owner".into(),
            paired_at: now,
        }),
        permissions: Permissions {
            prompts: true,
            read_output: true,
            ..Permissions::default()
        },
        ..Config::default()
    };
    let mut rt = RuntimeState::default();
    let mut router = Router::new(999, Some("sd_test_bot".into()), now);

    // Reject linking group with > 2 members (privacy guard!).
    api.state.lock().unwrap().chat_member_count = 3;
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 1,
            message: Some(text_msg(-100999, "supergroup", 777, 1, "/linkforum", now)),
            callback_query: None,
        },
        now,
    );
    assert!(cfg.forum.is_none());

    // Reject non-forum supergroup.
    api.state.lock().unwrap().chat_member_count = 2;
    api.state.lock().unwrap().chat_info = Some(ChatInfo {
        id: -100999,
        chat_type: "supergroup".into(),
        title: Some("Normal Group".into()),
        is_forum: Some(false),
    });
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 2,
            message: Some(text_msg(-100999, "supergroup", 777, 2, "/linkforum", now)),
            callback_query: None,
        },
        now,
    );
    assert!(cfg.forum.is_none());

    // Succeed with 2-member forum supergroup + admin with can_manage_topics.
    api.state.lock().unwrap().chat_info = Some(ChatInfo {
        id: -100999,
        chat_type: "supergroup".into(),
        title: Some("My PC Topics".into()),
        is_forum: Some(true),
    });
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 3,
            message: Some(text_msg(-100999, "supergroup", 777, 3, "/linkforum", now)),
            callback_query: None,
        },
        now,
    );
    assert_eq!(cfg.forum.as_ref().unwrap().chat_id, -100999);
    assert_eq!(rt.topics.len(), 2);
    let tid1 = rt.topic_for_card("sd_term_1").unwrap().thread_id;
    let tid2 = rt.topic_for_card("sd_term_2").unwrap().thread_id;
    assert_ne!(tid1, tid2);

    // Message written inside Topic 2 routes directly to sd_term_2 without /select.
    let mut topic_msg = text_msg(-100999, "supergroup", 777, 10, "run cargo check", now);
    topic_msg.is_topic_message = Some(true);
    topic_msg.message_thread_id = Some(tid2);
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 4,
            message: Some(topic_msg),
            callback_query: None,
        },
        now,
    );
    assert_eq!(
        backend.prompts_sent.lock().unwrap().as_slice(),
        [("sd_term_2".to_string(), "run cargo check".to_string())]
    );

    // Rename throttled (<60s skipped for topic name, >=60s applied; silent header card updates immediately).
    {
        let mut cards = backend.cards.lock().unwrap();
        cards[0].title = Some("Refactored router".into());
    }
    let updated = backend.list_cards();
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &updated, now + 10, false));
    assert_eq!(api.state.lock().unwrap().renamed_topics.len(), 0);
    assert_eq!(api.state.lock().unwrap().edited_msgs.len(), 1);
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &updated, now + 65, false));
    assert_eq!(api.state.lock().unwrap().renamed_topics.len(), 1);

    // Closing card deletes topic via deleteForumTopic and drops its binding; returning card creates a fresh topic.
    backend.cards.lock().unwrap().retain(|c| c.id != "sd_term_2");
    assert!(topics::sync_topics(
        &api,
        &cfg,
        &mut rt,
        &backend.list_cards(),
        now + 70,
        false
    ));
    assert!(rt.topic_for_card("sd_term_2").is_none());
    assert_eq!(api.state.lock().unwrap().deleted_topics, vec![(-100999, tid2)]);

    backend.cards.lock().unwrap().push(card2.clone());
    assert!(topics::sync_topics(
        &api,
        &cfg,
        &mut rt,
        &backend.list_cards(),
        now + 75,
        false
    ));
    let tid2_new = rt.topic_for_card("sd_term_2").unwrap().thread_id;
    assert_ne!(tid2_new, tid2);

    // Closing via clc: callback immediately deletes the bound topic.
    cfg.permissions.close = true;
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 5,
            message: None,
            callback_query: Some(CallbackQuery {
                id: "cq_close".into(),
                from: user(777),
                message: Some(text_msg(-100999, "supergroup", 999, 250, "Close?", now + 80)),
                data: Some("clc:sd_term_2".into()),
            }),
        },
        now + 80,
    );
    assert!(rt.topic_for_card("sd_term_2").is_none());
    assert!(api.state.lock().unwrap().deleted_topics.contains(&(-100999, tid2_new)));

    // Fallback when deleteForumTopic is denied: archives topic once and reopens if card returns.
    backend.cards.lock().unwrap().push(card2.clone());
    assert!(topics::sync_topics(
        &api,
        &cfg,
        &mut rt,
        &backend.list_cards(),
        now + 85,
        false
    ));
    let tid2_fb = rt.topic_for_card("sd_term_2").unwrap().thread_id;
    api.state.lock().unwrap().fail_delete_topic_bad_request = true;
    backend.cards.lock().unwrap().retain(|c| c.id != "sd_term_2");
    assert!(topics::sync_topics(
        &api,
        &cfg,
        &mut rt,
        &backend.list_cards(),
        now + 90,
        false
    ));
    let fb_binding = rt.topic_for_card("sd_term_2").unwrap();
    assert!(fb_binding.closed);
    assert!(fb_binding.delete_failed);
    assert_eq!(api.state.lock().unwrap().closed_topics, vec![(-100999, tid2_fb)]);
    // Subsequent sync tick does not retry delete_forum_topic while closed & delete_failed.
    assert!(!topics::sync_topics(
        &api,
        &cfg,
        &mut rt,
        &backend.list_cards(),
        now + 95,
        false
    ));

    backend.cards.lock().unwrap().push(card2);
    assert!(topics::sync_topics(
        &api,
        &cfg,
        &mut rt,
        &backend.list_cards(),
        now + 100,
        false
    ));
    let reopened = rt.topic_for_card("sd_term_2").unwrap();
    assert!(!reopened.closed);
    assert!(!reopened.delete_failed);
    assert_eq!(api.state.lock().unwrap().reopened_topics, vec![(-100999, tid2_fb)]);
    api.state.lock().unwrap().fail_delete_topic_bad_request = false;

    // Deleted topic in Telegram marks deleted_in_telegram and does not spam recreate until force_recreate.
    api.state.lock().unwrap().deleted_threads.insert(tid1);
    {
        let mut cards = backend.cards.lock().unwrap();
        cards[0].title = Some("Another title".into());
    }
    assert!(topics::sync_topics(
        &api,
        &cfg,
        &mut rt,
        &backend.list_cards(),
        now + 200,
        false
    ));
    assert!(rt.topic_for_card("sd_term_1").unwrap().deleted_in_telegram);
    let created_before = api.state.lock().unwrap().created_topics.len();
    assert!(!topics::sync_topics(
        &api,
        &cfg,
        &mut rt,
        &backend.list_cards(),
        now + 210,
        false
    ));
    assert_eq!(api.state.lock().unwrap().created_topics.len(), created_before);

    // /sync (force_recreate_deleted = true) recreates deleted topic.
    assert!(topics::sync_topics(
        &api,
        &cfg,
        &mut rt,
        &backend.list_cards(),
        now + 220,
        true
    ));
    assert!(!rt.topic_for_card("sd_term_1").unwrap().deleted_in_telegram);
    assert_eq!(api.state.lock().unwrap().created_topics.len(), created_before + 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn telegram_permissions_gating_and_new_wizard_callbacks() {
    let dir = scratch_dir("perms");
    let api = MockApi::new();
    let ai_card = sample_card("sd_term_ai", "claude", false);
    let shell_card = sample_card("sd_term_sh", "shell", true);
    let backend = MockBackend::new(vec![ai_card, shell_card]);
    backend
        .screens
        .lock()
        .unwrap()
        .insert("sd_term_ai".into(), "hello from screen".into());
    let now = 1_700_000_000;

    let mut cfg = Config {
        enabled: true,
        bot_token: "123:tok".into(),
        bot_id: Some(999),
        allowed_user: Some(AllowedUser {
            user_id: 777,
            chat_id: 777,
            username: Some("owner".into()),
            first_name: "Owner".into(),
            paired_at: now,
        }),
        permissions: Permissions::default(), // everything off except notifications + redact_secrets
        ..Config::default()
    };
    let mut rt = RuntimeState {
        active_card_id: Some("sd_term_ai".into()),
        ..RuntimeState::default()
    };
    let mut router = Router::new(999, Some("sd_test_bot".into()), now);

    // Disabled permissions refuse cleanly.
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 1,
            message: Some(text_msg(777, "private", 777, 1, "/screen", now)),
            callback_query: None,
        },
        now,
    );
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 2,
            message: Some(text_msg(777, "private", 777, 2, "run tests", now)),
            callback_query: None,
        },
        now,
    );
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 3,
            message: Some(text_msg(777, "private", 777, 3, "/new", now)),
            callback_query: None,
        },
        now,
    );
    assert!(backend.prompts_sent.lock().unwrap().is_empty());
    assert!(backend.launched.lock().unwrap().is_empty());

    // Enabling prompts allows AI card, while shell card still requires shell_input!
    cfg.permissions.prompts = true;
    cfg.permissions.controls = true;
    rt.active_card_id = Some("sd_term_sh".into());
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 4,
            message: Some(text_msg(777, "private", 777, 4, "ls -la", now)),
            callback_query: None,
        },
        now,
    );
    assert!(backend.prompts_sent.lock().unwrap().is_empty());

    // /new 2-step wizard uses bounded callback_data <= 64 bytes even for long custom harness IDs.
    cfg.permissions.launch = true;
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 5,
            message: Some(text_msg(777, "private", 777, 5, "/new", now)),
            callback_query: None,
        },
        now,
    );
    let sent = api.state.lock().unwrap().sent.last().unwrap().clone();
    let kb = sent.keyboard.unwrap();
    assert!(kb
        .inline_keyboard
        .iter()
        .flatten()
        .all(|b| b.callback_data.as_deref().unwrap_or("").len() <= 64));
    let folder_cb = kb.inline_keyboard[0][0].callback_data.clone().unwrap();

    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 6,
            message: None,
            callback_query: Some(CallbackQuery {
                id: "cq1".into(),
                from: user(777),
                message: Some(text_msg(777, "private", 999, 50, "choose", now)),
                data: Some(folder_cb),
            }),
        },
        now,
    );
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 7,
            message: None,
            callback_query: Some(CallbackQuery {
                id: "cq2".into(),
                from: user(777),
                message: Some(text_msg(777, "private", 999, 50, "choose harness", now)),
                data: Some("nwh:0:1".into()),
            }),
        },
        now,
    );
    assert_eq!(backend.launched.lock().unwrap().len(), 1);
    assert_eq!(rt.active_card_id.as_deref(), Some("sd_term_launched_1"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn telegram_notifier_seeds_quietly_and_alerts_into_forum_topics() {
    let api = MockApi::new();
    let mut cfg = Config {
        enabled: true,
        bot_token: "123:tok".into(),
        allowed_user: Some(AllowedUser {
            user_id: 777,
            chat_id: 777,
            username: None,
            first_name: "Owner".into(),
            paired_at: 100,
        }),
        forum: Some(ForumConfig {
            chat_id: -100555,
            title: "Forum".into(),
            linked_at: 100,
        }),
        permissions: Permissions {
            notifications: true,
            controls: true,
            read_output: true,
            ..Permissions::default()
        },
        ..Config::default()
    };
    let mut rt = RuntimeState {
        topics: vec![TopicBinding {
            card_id: "sd_term_1".into(),
            thread_id: 77,
            topic_name: "Claude".into(),
            icon_color: 7_322_096,
            last_renamed_at: 100,
            ..TopicBinding::default()
        }],
        ..RuntimeState::default()
    };

    let mut card = sample_card("sd_term_1", "claude", false);
    card.completion_state = "completed".into();
    card.completion_id = Some("cid_1".into());
    card.status = "WAITING".into();

    let backend = MockBackend::new(vec![card.clone()]);
    backend.screens.lock().unwrap().insert(
        "sd_term_1".into(),
        "Allow running tests?\nOPENAI_API_KEY=sk-ant-api03-1234567890abcdefghijklmnop\n1) Yes  2) No"
            .into(),
    );

    let mut notifier = NotifierState::default();
    // Initial seed emits zero notifications even for already-completed / waiting cards.
    assert!(!notifier.tick(&api, &backend, &cfg, &mut rt, &[card.clone()]));
    assert!(api.state.lock().unwrap().sent.is_empty());

    // Single tick of WAITING on another state doesn't spam; wait for 2nd tick.
    card.status = "WORKING".into();
    assert!(!notifier.tick(&api, &backend, &cfg, &mut rt, &[card.clone()]));

    card.status = "WAITING".into();
    assert!(!notifier.tick(&api, &backend, &cfg, &mut rt, &[card.clone()]));
    assert!(api.state.lock().unwrap().sent.is_empty());

    // Second consecutive WAITING tick sends approval buttons + redacted screen snippet into topic!
    assert!(notifier.tick(&api, &backend, &cfg, &mut rt, &[card.clone()]));
    {
        let st = api.state.lock().unwrap();
        assert_eq!(st.sent.len(), 1);
        assert_eq!(st.sent[0].chat_id, -100555);
        assert_eq!(st.sent[0].thread_id, Some(77));
        assert!(st.sent[0].html.contains("Waiting for input"));
        assert!(st.sent[0].html.contains("Allow running tests?"));
        assert!(st.sent[0].html.contains("[REDACTED]"));
        assert!(!st.sent[0].html.contains("sk-ant-"));
        assert!(st.sent[0].keyboard.is_some());
    }

    // New completion_id emits Finished alert with snippet when read_output=true, and omits output when read_output=false.
    card.status = "FINISHED".into();
    card.completion_state = "completed".into();
    card.completion_id = Some("cid_2".into());
    assert!(notifier.tick(&api, &backend, &cfg, &mut rt, &[card.clone()]));
    {
        let st = api.state.lock().unwrap();
        assert_eq!(st.sent.len(), 2);
        assert!(st.sent[1].html.contains("Finished"));
        assert!(st.sent[1].html.contains("Allow running tests?"));
        assert_eq!(st.sent[1].thread_id, Some(77));
    }

    cfg.permissions.read_output = false;
    card.completion_id = Some("cid_3".into());
    assert!(notifier.tick(&api, &backend, &cfg, &mut rt, &[card]));
    let st = api.state.lock().unwrap();
    assert_eq!(st.sent.len(), 3);
    assert!(st.sent[2].html.contains("Finished"));
    assert!(!st.sent[2].html.contains("Allow running tests?"));
}

#[test]
fn telegram_forum_topics_live_status_header_pin_and_debounced_title_updates() {
    let api = MockApi::new();
    let cfg = Config {
        enabled: true,
        bot_token: "123:tok".into(),
        bot_id: Some(999),
        allowed_user: Some(AllowedUser {
            user_id: 777,
            chat_id: 777,
            username: None,
            first_name: "Owner".into(),
            paired_at: 1_700_000_000,
        }),
        forum: Some(ForumConfig {
            chat_id: -100555,
            title: "Forum".into(),
            linked_at: 1_700_000_000,
        }),
        permissions: Permissions {
            notifications: true,
            controls: true,
            read_output: true,
            ..Permissions::default()
        },
        ..Config::default()
    };
    let mut rt = RuntimeState::default();
    let t0 = 1_700_000_000;
    let mut card = sample_card("sd_term_live", "claude", false);

    // 1. Creation silently pins status_message_id and prefixes topic title with ⚪ for IDLE.
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0, false));
    let binding = rt.topic_for_card("sd_term_live").unwrap().clone();
    let status_msg_id = binding.status_message_id.expect("status header created");
    assert!(binding.status_pinned);
    assert!(binding.topic_name.starts_with("⚪ ⚡ Claude Code"));
    assert_eq!(
        api.state.lock().unwrap().pinned_msgs,
        vec![(-100555, status_msg_id, true)]
    );

    // 2. Brief 1-tick flicker (IDLE -> WORKING at t+2, back to IDLE at t+4) updates the silent
    // header card immediately, without renaming the Forum topic.
    card.status = "WORKING".into();
    card.label = "● WORKING".into();
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0 + 2, false));
    assert_eq!(api.state.lock().unwrap().renamed_topics.len(), 0);
    assert_eq!(api.state.lock().unwrap().edited_msgs.len(), 1);
    assert!(api.state.lock().unwrap().edited_msgs[0].2.contains("🟢"));

    card.status = "IDLE".into();
    card.label = "● IDLE".into();
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0 + 4, false));
    assert_eq!(api.state.lock().unwrap().renamed_topics.len(), 0);
    assert_eq!(api.state.lock().unwrap().edited_msgs.len(), 2);
    assert!(rt.topic_for_card("sd_term_live").unwrap().pending_status.is_none());

    // 3. Sustained transition (IDLE -> WAITING at t+10..t+14): header updates at t+10 with
    // waiting_keyboard (Yes/No/1/2), topic title renames at t+14 (>= 4s debounce).
    card.status = "WAITING".into();
    card.label = "◌ WAITING".into();
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0 + 10, false));
    {
        let st = api.state.lock().unwrap();
        assert_eq!(st.renamed_topics.len(), 0);
        let (_, mid, ref html, ref kb) = st.edited_msgs.last().unwrap();
        assert_eq!(*mid, status_msg_id);
        assert!(html.contains("🟡"));
        let labels: Vec<&str> = kb
            .as_ref()
            .unwrap()
            .inline_keyboard
            .iter()
            .flatten()
            .map(|b| b.text.as_str())
            .collect();
        assert!(labels.contains(&"Yes") && labels.contains(&"No"));
    }
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0 + 14, false));
    {
        let st = api.state.lock().unwrap();
        assert_eq!(st.renamed_topics.len(), 1);
        assert!(st.renamed_topics[0].2.starts_with("🟡 "));
    }

    // 4. Cooldown (10s) holds next topic rename until t+24 while header edits immediately at t+16.
    card.status = "FINISHED".into();
    card.label = "✓ FINISHED".into();
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0 + 16, false));
    {
        let st = api.state.lock().unwrap();
        assert_eq!(st.renamed_topics.len(), 1);
        let (_, _, ref html, ref kb) = st.edited_msgs.last().unwrap();
        assert!(html.contains("✅"));
        let labels: Vec<&str> = kb
            .as_ref()
            .unwrap()
            .inline_keyboard
            .iter()
            .flatten()
            .map(|b| b.text.as_str())
            .collect();
        assert!(!labels.contains(&"Yes") && labels.contains(&"📷 Screen"));
    }
    assert!(!topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0 + 20, false));
    assert_eq!(api.state.lock().unwrap().renamed_topics.len(), 1);
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0 + 24, false));
    {
        let st = api.state.lock().unwrap();
        assert_eq!(st.renamed_topics.len(), 2);
        assert!(st.renamed_topics[1].2.starts_with("✅ "));
    }

    // 5. Explicit /sync (force_recreate_deleted = true) bypasses debounce and cooldown immediately.
    card.status = "ERROR".into();
    card.label = "⚠ ERROR".into();
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0 + 25, true));
    assert_eq!(api.state.lock().unwrap().renamed_topics.len(), 3);
    assert!(api.state.lock().unwrap().renamed_topics[2].2.starts_with("🔴 "));

    // 6. If deleteForumTopic is denied, fallback edits status header to Closed on PC before archiving;
    //    default deletion deletes the topic cleanly when the card closes.
    api.state.lock().unwrap().fail_delete_topic_bad_request = true;
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[], t0 + 30, false));
    {
        let st = api.state.lock().unwrap();
        let (_, mid, ref html, ref kb) = st.edited_msgs.last().unwrap();
        assert_eq!(*mid, status_msg_id);
        assert!(html.contains("Closed on PC"));
        assert!(kb.is_none());
    }
    assert!(rt.topic_for_card("sd_term_live").unwrap().closed);
    assert!(rt.topic_for_card("sd_term_live").unwrap().delete_failed);

    card.status = "WORKING".into();
    card.label = "● WORKING".into();
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0 + 40, false));
    assert!(!rt.topic_for_card("sd_term_live").unwrap().closed);
    assert!(!rt.topic_for_card("sd_term_live").unwrap().delete_failed);
    assert!(api.state.lock().unwrap().edited_msgs.last().unwrap().2.contains("🟢"));

    // Manually deleted status header message clears status_message_id cleanly without failing later syncs.
    api.state.lock().unwrap().deleted_messages.insert(status_msg_id);
    card.status = "IDLE".into();
    card.label = "● IDLE".into();
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], t0 + 42, false));
    assert!(rt.topic_for_card("sd_term_live").unwrap().status_message_id.is_none());
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[card], t0 + 60, false));

    // Default deletion removes topic and binding completely.
    api.state.lock().unwrap().fail_delete_topic_bad_request = false;
    assert!(topics::sync_topics(&api, &cfg, &mut rt, &[], t0 + 70, false));
    assert!(rt.topic_for_card("sd_term_live").is_none());
    assert!(!api.state.lock().unwrap().deleted_topics.is_empty());
}

#[test]
fn telegram_files_and_links_discovery_photo_send_fallback_and_redaction() {
    let dir = scratch_dir("files");
    let api = MockApi::new();
    let card = sample_card("sd_term_files", "claude", false);
    let backend = MockBackend::new(vec![card.clone()]);
    let now = 1_700_000_000;

    // 1. Link extractor validates HTTP(S) links and rejects unsafe schemes / credentials.
    let extracted = extract_terminal_links(
        "Check [docs](https://example.org/docs?a=1#top) and http://localhost:3000/app, \
         duplicate https://example.org/docs?a=1#top, bad file:///etc/passwd javascript:alert(1) \
         https://user:pass@example.org/secret",
    );
    assert_eq!(
        extracted,
        vec![
            "https://example.org/docs?a=1#top".to_string(),
            "http://localhost:3000/app".to_string()
        ]
    );

    let img_id = "a".repeat(64);
    let gif_id = "b".repeat(64);
    let pdf_id = "c".repeat(64);
    let md_small_id = "d".repeat(64);
    let txt_large_id = "e".repeat(64);

    let assets = vec![
        CardAsset {
            id: md_small_id.clone(),
            name: "NOTES.md".into(),
            relative_path: "NOTES.md".into(),
            mime_type: "text/markdown".into(),
            kind: "markdown".into(),
            size: 64,
        },
        CardAsset {
            id: img_id.clone(),
            name: "screen.png".into(),
            relative_path: "out/screen.png".into(),
            mime_type: "image/png".into(),
            kind: "image".into(),
            size: 16,
        },
        CardAsset {
            id: gif_id.clone(),
            name: "demo.gif".into(),
            relative_path: "out/demo.gif".into(),
            mime_type: "image/gif".into(),
            kind: "image".into(),
            size: 16,
        },
        CardAsset {
            id: pdf_id.clone(),
            name: "report.pdf".into(),
            relative_path: "out/report.pdf".into(),
            mime_type: "application/pdf".into(),
            kind: "pdf".into(),
            size: 16,
        },
        CardAsset {
            id: txt_large_id.clone(),
            name: "huge.log".into(),
            relative_path: "logs/huge.log".into(),
            mime_type: "text/plain".into(),
            kind: "text".into(),
            size: 5000,
        },
    ];
    backend.references.lock().unwrap().insert(
        card.id.clone(),
        CardReferences {
            assets: assets.clone(),
            links: extracted,
        },
    );
    {
        let mut map = backend.assets.lock().unwrap();
        map.insert((card.id.clone(), img_id.clone()), (assets[1].clone(), b"\x89PNG\r\n\x1a\npngdata".to_vec()));
        map.insert((card.id.clone(), gif_id.clone()), (assets[2].clone(), b"GIF89adata".to_vec()));
        map.insert((card.id.clone(), pdf_id.clone()), (assets[3].clone(), b"%PDF-1.7data".to_vec()));
        map.insert(
            (card.id.clone(), md_small_id.clone()),
            (assets[0].clone(), b"OPENAI_API_KEY=sk-ant-api03-1234567890abcdefghijklmnop\n# Hello".to_vec()),
        );
        let large_body = format!(
            "MY_SECRET=super_secret_value\n{}",
            "log line\n".repeat(500)
        );
        map.insert((card.id.clone(), txt_large_id.clone()), (assets[4].clone(), large_body.into_bytes()));
    }

    {
        let mut pages = backend.pdf_pages.lock().unwrap();
        pages.insert((card.id.clone(), pdf_id.clone(), 1), b"\x89PNGpage1".to_vec());
        pages.insert((card.id.clone(), pdf_id.clone(), 2), b"\x89PNGpage2".to_vec());
    }

    let mut cfg = Config {
        enabled: true,
        bot_token: "123:tok".into(),
        bot_id: Some(999),
        allowed_user: Some(AllowedUser {
            user_id: 777,
            chat_id: 777,
            username: Some("owner".into()),
            first_name: "Owner".into(),
            paired_at: now,
        }),
        forum: Some(ForumConfig {
            chat_id: -100555,
            title: "Forum".into(),
            linked_at: now,
        }),
        permissions: Permissions::default(), // read_output = false initially
        ..Config::default()
    };
    let mut rt = RuntimeState::default();
    topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], now, false);
    let tid = rt.topic_for_card(&card.id).unwrap().thread_id;
    let mut router = Router::new(999, Some("sd_test_bot".into()), now);

    // 2. read_output = false refuses /files and fil: cleanly.
    let mut msg = text_msg(-100555, "supergroup", 777, 1, "/files", now);
    msg.is_topic_message = Some(true);
    msg.message_thread_id = Some(tid);
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 1,
            message: Some(msg.clone()),
            callback_query: None,
        },
        now,
    );
    assert!(api.state.lock().unwrap().sent.last().unwrap().html.contains("disabled"));

    // 3. Enable read_output: fil:<card_id> renders sorted files (images first), public URL button only, compact callback_data.
    cfg.permissions.read_output = true;
    msg.message_id = 2;
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 2,
            message: Some(msg),
            callback_query: None,
        },
        now,
    );
    let list_msg = api.state.lock().unwrap().sent.last().unwrap().clone();
    assert_eq!(list_msg.thread_id, Some(tid));
    assert!(list_msg.html.contains("http://localhost:3000/app"));
    let kb = list_msg.keyboard.unwrap();
    let all_btns: Vec<&InlineButton> = kb.inline_keyboard.iter().flatten().collect();
    assert!(all_btns.iter().all(|b| b.callback_data.as_deref().unwrap_or("").len() <= 64));
    // Public link has InlineButton::url; localhost is never emitted as a Telegram URL button.
    let url_btns: Vec<&str> = all_btns.iter().filter_map(|b| b.url.as_deref()).collect();
    assert_eq!(url_btns, vec!["https://example.org/docs?a=1#top"]);
    // Images sorted before PDFs and Markdown.
    let fop_buttons: Vec<(&str, &str)> = all_btns
        .iter()
        .filter_map(|b| Some((b.text.as_str(), b.callback_data.as_deref()?)))
        .filter(|(_, d)| d.starts_with("fop:"))
        .collect();
    assert_eq!(fop_buttons.len(), 5);
    assert!(fop_buttons[0].0.contains("screen.png"));
    assert!(fop_buttons[1].0.contains("demo.gif"));
    assert!(fop_buttons[2].0.contains("report.pdf"));

    let trigger_fop = |router: &mut Router, cfg: &mut Config, rt: &mut RuntimeState, uid: i64, data: &str| {
        let mut cq_msg = text_msg(-100555, "supergroup", 999, 50, "list", now);
        cq_msg.is_topic_message = Some(true);
        cq_msg.message_thread_id = Some(tid);
        router.handle_update(
            &api,
            &backend,
            &dir,
            cfg,
            rt,
            &Update {
                update_id: uid,
                message: None,
                callback_query: Some(CallbackQuery {
                    id: format!("cq_{uid}"),
                    from: user(777),
                    message: Some(cq_msg),
                    data: Some(data.to_string()),
                }),
            },
            now,
        );
    };

    // 4. Image sends as photo into the exact Forum topic; BadRequest falls back to document.
    trigger_fop(&mut router, &mut cfg, &mut rt, 3, fop_buttons[0].1);
    {
        let st = api.state.lock().unwrap();
        assert_eq!(st.sent_photos.len(), 1);
        assert_eq!(st.sent_photos[0].chat_id, -100555);
        assert_eq!(st.sent_photos[0].thread_id, Some(tid));
        assert_eq!(st.sent_photos[0].filename, "screen.png");
        assert_eq!(st.sent_photos[0].mime_type, "image/png");
        assert!(st.sent_photos[0].caption.as_deref().unwrap_or("").contains("screen.png"));
    }

    api.state.lock().unwrap().fail_photo_bad_request = true;
    trigger_fop(&mut router, &mut cfg, &mut rt, 4, fop_buttons[0].1);
    assert_eq!(api.state.lock().unwrap().sent_documents.len(), 1);
    assert_eq!(api.state.lock().unwrap().sent_documents[0].filename, "screen.png");
    api.state.lock().unwrap().fail_photo_bad_request = false;

    // 5. GIF -> sendAnimation, PDF -> sendDocument.
    trigger_fop(&mut router, &mut cfg, &mut rt, 5, fop_buttons[1].1);
    assert_eq!(api.state.lock().unwrap().sent_animations.len(), 1);
    assert_eq!(api.state.lock().unwrap().sent_animations[0].filename, "demo.gif");

    trigger_fop(&mut router, &mut cfg, &mut rt, 6, fop_buttons[2].1);
    assert_eq!(api.state.lock().unwrap().sent_documents.len(), 2);
    assert_eq!(api.state.lock().unwrap().sent_documents[1].filename, "report.pdf");
    // Preview page 1 of PDF as photo via ppg:
    let pdf_pick_num = fop_buttons[2].1.strip_prefix("fop:").unwrap();
    trigger_fop(&mut router, &mut cfg, &mut rt, 60, &format!("ppg:{pdf_pick_num}:1"));
    assert_eq!(api.state.lock().unwrap().sent_photos.len(), 2);
    assert!(api.state.lock().unwrap().sent_photos[1].filename.contains("page-1"));

    // 6. Small Markdown/text sends inline redacted; large text starts inline multi-page preview with Next/Download buttons.
    trigger_fop(&mut router, &mut cfg, &mut rt, 7, fop_buttons[3].1);
    let inline_md = api.state.lock().unwrap().sent.last().unwrap().clone();
    assert!(inline_md.html.contains("NOTES.md"));
    assert!(inline_md.html.contains("[REDACTED]"));
    assert!(!inline_md.html.contains("sk-ant-"));

    trigger_fop(&mut router, &mut cfg, &mut rt, 8, fop_buttons[4].1);
    {
        let st = api.state.lock().unwrap();
        let paged = st.sent.last().unwrap();
        assert!(paged.html.contains("Page 1/2"));
        assert!(paged.html.contains("[REDACTED]"));
        assert!(!paged.html.contains("super_secret_value"));
    }
    let text_pick_num = fop_buttons[4].1.strip_prefix("fop:").unwrap();
    trigger_fop(&mut router, &mut cfg, &mut rt, 80, &format!("tpg:{text_pick_num}:1"));
    assert!(api.state.lock().unwrap().edited_msgs.last().unwrap().2.contains("Page 2/2"));

    trigger_fop(&mut router, &mut cfg, &mut rt, 81, &format!("tdl:{text_pick_num}"));
    let st = api.state.lock().unwrap();
    assert_eq!(st.sent_documents.len(), 3);
    let doc_text = String::from_utf8_lossy(&st.sent_documents[2].bytes);
    assert!(!doc_text.contains("super_secret_value"));
    assert!(doc_text.contains("[REDACTED]"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn telegram_dashboard_typing_reactions_live_stream_and_quick_keys() {
    let dir = scratch_dir("ui");
    let api = MockApi::new();
    let mut card = sample_card("sd_term_ui", "claude", false);
    card.status = "WORKING".into();
    card.label = "● WORKING".into();
    let backend = MockBackend::new(vec![card.clone()]);
    *backend.usage.lock().unwrap() = vec![ProviderUsageSummary {
        id: "claude".into(),
        name: "Claude".into(),
        tier: "Max 5x".into(),
        ready: true,
        limits: vec![ProviderUsageLimit {
            label: "Weekly (7-day)".into(),
            percent_used: 0.18,
            resets_at: String::new(),
            used: None,
            allowance: None,
        }],
        ..ProviderUsageSummary::default()
    }];
    backend
        .screens
        .lock()
        .unwrap()
        .insert(card.id.clone(), "step 1: building...".into());

    let now = 1_700_000_000;
    let mut cfg = Config {
        enabled: true,
        bot_token: "123:tok".into(),
        bot_id: Some(999),
        allowed_user: Some(AllowedUser {
            user_id: 777,
            chat_id: 777,
            username: Some("owner".into()),
            first_name: "Owner".into(),
            paired_at: now,
        }),
        forum: Some(ForumConfig {
            chat_id: -100555,
            title: "Forum".into(),
            linked_at: now,
        }),
        permissions: Permissions {
            notifications: true,
            read_output: true,
            prompts: true,
            controls: true,
            ..Permissions::default()
        },
        ..Config::default()
    };
    let mut rt = RuntimeState::default();
    topics::sync_topics(&api, &cfg, &mut rt, &[card.clone()], now, false);
    let tid = rt.topic_for_card(&card.id).unwrap().thread_id;

    // 1. General Dashboard creates & pins with card status + usage, and throttles edits (<6s skipped, >=6s applied).
    let usage = backend.usage_summary();
    assert!(topics::sync_dashboard(&api, &cfg, &mut rt, &[card.clone()], &usage, now, false));
    let dash_id = rt.dashboard_message_id.expect("dashboard created");
    assert!(rt.dashboard_pinned);
    {
        let st = api.state.lock().unwrap();
        let dash = st.sent.iter().find(|m| m.thread_id.is_none() && m.html.contains("SUPER DESKTOP")).unwrap();
        assert!(dash.html.contains("Claude"));
        assert!(dash.html.contains("18%"));
        assert!(dash.html.contains("82% left"));
        assert!(dash.html.contains("█"));
        assert!(dash.html.contains("░"));
        assert!(st.pinned_msgs.contains(&(-100555, dash_id, true)));
    }
    card.status = "FINISHED".into();
    card.label = "✓ FINISHED".into();
    *backend.cards.lock().unwrap() = vec![card.clone()];
    assert!(!topics::sync_dashboard(&api, &cfg, &mut rt, &[card.clone()], &usage, now + 2, false));
    assert!(topics::sync_dashboard(&api, &cfg, &mut rt, &[card.clone()], &usage, now + 8, false));
    assert!(api
        .state
        .lock()
        .unwrap()
        .edited_msgs
        .iter()
        .any(|(_, mid, html, _)| *mid == dash_id && html.contains("✅")));

    // 2. Typing indicator fires for WORKING cards with >=4s cadence.
    card.status = "WORKING".into();
    card.label = "● WORKING".into();
    *backend.cards.lock().unwrap() = vec![card.clone()];
    let mut last_typing = HashMap::new();
    topics::tick_typing_actions(&api, &cfg, &rt, &[card.clone()], now, &mut last_typing);
    topics::tick_typing_actions(&api, &cfg, &rt, &[card.clone()], now + 2, &mut last_typing);
    topics::tick_typing_actions(&api, &cfg, &rt, &[card.clone()], now + 5, &mut last_typing);
    assert_eq!(api.state.lock().unwrap().chat_actions.len(), 2);
    assert_eq!(api.state.lock().unwrap().chat_actions[0], (-100555, Some(tid), "typing".into()));

    // 3. Forum topic prompts acknowledge silently with 👍 reaction without redundant text reply; falls back on error.
    let mut router = Router::new(999, Some("sd_test_bot".into()), now);
    let mut prompt_msg = text_msg(-100555, "supergroup", 777, 300, "check status", now);
    prompt_msg.is_topic_message = Some(true);
    prompt_msg.message_thread_id = Some(tid);
    let sent_before = api.state.lock().unwrap().sent.len();
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 10,
            message: Some(prompt_msg.clone()),
            callback_query: None,
        },
        now,
    );
    assert_eq!(api.state.lock().unwrap().reactions, vec![(-100555, 300, "👍".into())]);
    assert_eq!(api.state.lock().unwrap().sent.len(), sent_before);

    api.state.lock().unwrap().fail_reactions = true;
    prompt_msg.message_id = 301;
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 11,
            message: Some(prompt_msg),
            callback_query: None,
        },
        now,
    );
    assert_eq!(api.state.lock().unwrap().sent.len(), sent_before + 1);

    // 4. Quick keys panel sends Tab/arrows + quick slash commands gated cleanly.
    let mut keys_cmd = text_msg(-100555, "supergroup", 777, 302, "/keys", now);
    keys_cmd.is_topic_message = Some(true);
    keys_cmd.message_thread_id = Some(tid);
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 12,
            message: Some(keys_cmd),
            callback_query: None,
        },
        now,
    );
    let keys_kb = api.state.lock().unwrap().sent.last().unwrap().keyboard.clone().unwrap();
    let key_labels: Vec<&str> = keys_kb
        .inline_keyboard
        .iter()
        .flatten()
        .map(|b| b.text.as_str())
        .collect();
    assert!(key_labels.contains(&"Tab") && key_labels.contains(&"/compact"));

    let mut cq_msg = text_msg(-100555, "supergroup", 999, 303, "keys", now);
    cq_msg.is_topic_message = Some(true);
    cq_msg.message_thread_id = Some(tid);
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 13,
            message: None,
            callback_query: Some(CallbackQuery {
                id: "cq_tab".into(),
                from: user(777),
                message: Some(cq_msg.clone()),
                data: Some(format!("key:t:{}", card.id)),
            }),
        },
        now,
    );
    assert_eq!(backend.keys_sent.lock().unwrap().as_slice(), [(card.id.clone(), NamedKey::Tab)]);

    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 14,
            message: None,
            callback_query: Some(CallbackQuery {
                id: "cq_slash".into(),
                from: user(777),
                message: Some(cq_msg),
                data: Some(format!("qc:0:{}", card.id)),
            }),
        },
        now,
    );
    assert!(backend.prompts_sent.lock().unwrap().iter().any(|(_, p)| p == "/compact"));

    // 5. Live stream starts, edits on output change every >=4s, and auto-stops after idle threshold.
    let mut live_cmd = text_msg(-100555, "supergroup", 777, 304, "/live", now);
    live_cmd.is_topic_message = Some(true);
    live_cmd.message_thread_id = Some(tid);
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 15,
            message: Some(live_cmd),
            callback_query: None,
        },
        now,
    );
    assert_eq!(rt.live_streams.len(), 1);
    backend.screens.lock().unwrap().insert(card.id.clone(), "step 2: all tests passing".into());
    assert!(!topics::tick_live_streams(&api, &backend, &cfg, &mut rt, &[card.clone()], now + 2));
    assert!(topics::tick_live_streams(&api, &backend, &cfg, &mut rt, &[card.clone()], now + 5));
    assert!(api.state.lock().unwrap().edited_msgs.last().unwrap().2.contains("step 2: all tests passing"));

    card.status = "FINISHED".into();
    card.label = "✓ FINISHED".into();
    assert!(topics::tick_live_streams(&api, &backend, &cfg, &mut rt, &[card.clone()], now + 7));
    assert_eq!(rt.live_streams.len(), 1);
    assert!(topics::tick_live_streams(&api, &backend, &cfg, &mut rt, &[card], now + 30));
    assert!(rt.live_streams.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn telegram_usage_command_shows_weekly_allowance_progress_and_refreshes() {
    let dir = scratch_dir("usage_cmd");
    let api = MockApi::new();
    let backend = MockBackend::new(vec![]);
    let now = 1_700_000_000;
    let mut cfg = Config {
        enabled: true,
        bot_token: "123:tok".into(),
        bot_id: Some(999),
        allowed_user: Some(AllowedUser {
            user_id: 777,
            chat_id: 777,
            username: Some("owner".into()),
            first_name: "Owner".into(),
            paired_at: now,
        }),
        ..Config::default()
    };
    let mut rt = RuntimeState::default();

    *backend.usage.lock().unwrap() = vec![
        ProviderUsageSummary {
            id: "codex".into(),
            name: "Codex".into(),
            tier: "Pro 100".into(),
            ready: true,
            limits: vec![ProviderUsageLimit {
                label: "Weekly (7-day)".into(),
                percent_used: 0.07,
                resets_at: "2099-01-01T00:00:00Z".into(),
                used: None,
                allowance: None,
            }],
            today_tokens: 37_762_105,
            ..ProviderUsageSummary::default()
        },
        ProviderUsageSummary {
            id: "claude".into(),
            name: "Claude".into(),
            tier: "Max <5x>".into(),
            ready: true,
            limits: vec![
                ProviderUsageLimit {
                    label: "Session (5-hour)".into(),
                    percent_used: 0.0,
                    resets_at: String::new(),
                    used: None,
                    allowance: None,
                },
                ProviderUsageLimit {
                    label: "Weekly (7-day)".into(),
                    percent_used: 1.0,
                    resets_at: String::new(),
                    used: None,
                    allowance: None,
                },
                ProviderUsageLimit {
                    label: "Fable Weekly".into(),
                    percent_used: 0.96,
                    resets_at: String::new(),
                    used: None,
                    allowance: None,
                },
            ],
            total_prompts: 2362,
            total_sessions: 190,
            ..ProviderUsageSummary::default()
        },
        ProviderUsageSummary {
            id: "antigravity".into(),
            name: "Antigravity".into(),
            tier: "Google".into(),
            ready: true,
            limits: vec![ProviderUsageLimit {
                label: "Thinking Models Quota".into(),
                percent_used: 0.77,
                resets_at: String::new(),
                used: Some(77),
                allowance: Some(100),
            }],
            ..ProviderUsageSummary::default()
        },
        ProviderUsageSummary {
            id: "agy".into(),
            name: "Gemini".into(),
            tier: "Ultra".into(),
            ready: true,
            limits: vec![],
            today_tokens: 138_468_542,
            today_prompts: 26,
            today_sessions: 3,
            ..ProviderUsageSummary::default()
        },
        ProviderUsageSummary {
            id: "opencode".into(),
            name: "OpenCode".into(),
            ready: true,
            total_prompts: 133,
            total_sessions: 40,
            ..ProviderUsageSummary::default()
        },
    ];

    let mut router = Router::new(999, Some("sd_test_bot".into()), now);
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 1,
            message: Some(text_msg(777, "private", 777, 10, "/usage", now)),
            callback_query: None,
        },
        now,
    );

    let sent = api.state.lock().unwrap().sent.last().unwrap().clone();
    assert!(sent.html.contains("Codex"));
    assert!(sent.html.contains("Claude"));
    assert!(sent.html.contains("Max &lt;5x&gt;"));
    assert!(sent.html.contains("Antigravity"));
    assert!(sent.html.contains("Gemini"));
    assert!(sent.html.contains("OpenCode"));
    // Weekly limits are ordered ahead of Session (5-hour) for Claude.
    let claude_weekly_pos = sent.html.find("Weekly (7-day)</b>\n<code>██████████</code>").unwrap();
    let fable_weekly_pos = sent.html.find("Fable Weekly</b>\n<code>█████████░</code>").unwrap();
    let claude_session_pos = sent.html.find("Session (5-hour)").unwrap();
    assert!(claude_weekly_pos < claude_session_pos);
    assert!(fable_weekly_pos < claude_session_pos);
    assert!(sent.html.contains("77%</b> used (77/100) · 23% left"));
    assert!(sent.html.contains("No weekly allowance reported"));
    assert!(sent.html.contains("138.5M tokens"));
    assert!(sent.html.contains("133 prompts · 40 sessions"));

    let kb = sent.keyboard.unwrap();
    assert_eq!(kb.inline_keyboard[0][0].callback_data.as_deref(), Some("usg:0"));

    // Update provider usage and trigger usg:0 callback to refresh in place.
    backend.usage.lock().unwrap()[0].limits[0].percent_used = 0.50;
    router.handle_update(
        &api,
        &backend,
        &dir,
        &mut cfg,
        &mut rt,
        &Update {
            update_id: 2,
            message: None,
            callback_query: Some(CallbackQuery {
                id: "cq_usg".into(),
                from: user(777),
                message: Some(text_msg(777, "private", 999, 100, "usage", now)),
                data: Some("usg:0".into()),
            }),
        },
        now + 5,
    );

    let st = api.state.lock().unwrap();
    let edited = st.edited_msgs.last().unwrap();
    assert_eq!(edited.1, 100);
    assert!(edited.2.contains("<code>█████░░░░░</code> <b>50%</b> used · 50% left"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn telegram_http_client_against_loopback_mock_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let responses = Arc::new(Mutex::new(VecDeque::from(vec![
        (200u16, "application/json", json!({"ok":true,"result":{"id":42,"is_bot":true,"first_name":"Mock","username":"mock_bot"}}).to_string().into_bytes()),
        (200u16, "application/json", json!({"ok":true,"result":{"message_thread_id":301,"name":"Topic","icon_color":7322096}}).to_string().into_bytes()),
        (400u16, "application/json", json!({"ok":false,"error_code":400,"description":"Bad Request: TOPIC_NOT_MODIFIED"}).to_string().into_bytes()),
        (200u16, "application/json", json!({"ok":true,"result":true}).to_string().into_bytes()),
        (400u16, "application/json", json!({"ok":false,"error_code":400,"description":"Bad Request: message to edit not found"}).to_string().into_bytes()),
        (200u16, "application/json", json!({"ok":true,"result":{"message_id":777,"chat":{"id":-1001,"type":"supergroup"},"date":1700000000}}).to_string().into_bytes()),
        (200u16, "application/json", json!({"ok":true,"result":true}).to_string().into_bytes()),
        (200u16, "application/json", json!({"ok":true,"result":true}).to_string().into_bytes()),
        (200u16, "application/json", json!({"ok":true,"result":true}).to_string().into_bytes()),
        (400u16, "application/json", json!({"ok":false,"error_code":400,"description":"Bad Request: TOPIC_ID_INVALID"}).to_string().into_bytes()),
        (429u16, "application/json", json!({"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":7}}).to_string().into_bytes()),
        (200u16, "application/octet-stream", b"hello-bytes".to_vec()),
        (200u16, "application/octet-stream", vec![b'x'; 200]),
    ])));

    let server_resps = Arc::clone(&responses);
    let recorded_reqs = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let reqs_clone = Arc::clone(&recorded_reqs);

    let handle = std::thread::spawn(move || {
        for stream in listener.incoming().take(13) {
            let mut stream = stream.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = stream.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let hdr = String::from_utf8_lossy(&buf[..pos]);
                    let content_len = hdr
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim().eq_ignore_ascii_case("Content-Length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if buf.len() >= pos + 4 + content_len {
                        break;
                    }
                }
            }
            let raw = String::from_utf8_lossy(&buf).into_owned();
            let first_line = raw.lines().next().unwrap_or("").to_string();
            reqs_clone.lock().unwrap().push((first_line, raw));

            let (status, ctype, body) = server_resps.lock().unwrap().pop_front().unwrap();
            let header = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });

    let client = TelegramClient::with_base_url("123456:TESTTOKEN", &format!("http://{addr}")).unwrap();
    let me = client.get_me().unwrap();
    assert_eq!(me.id, 42);
    assert_eq!(me.username.as_deref(), Some("mock_bot"));

    let topic = client.create_forum_topic(-1001, "Topic", 7_322_096).unwrap();
    assert_eq!(topic.message_thread_id, 301);

    // TOPIC_NOT_MODIFIED treated as Ok(())
    client.edit_forum_topic(-1001, 301, "Topic").unwrap();

    // pinChatMessage succeeds silently
    client.pin_chat_message(-1001, 10, true).unwrap();

    // Deleted status message returns MessageNotFound
    assert_eq!(
        client.edit_message_text(-1001, 10, "hi", None).unwrap_err(),
        ApiError::MessageNotFound
    );

    // sendPhoto multipart upload succeeds and delivers filename + raw bytes
    let sent_photo_id = client
        .send_photo(-1001, Some(301), "shot.png", "image/png", b"PNGRAWDATA", Some("cap"), None)
        .unwrap();
    assert_eq!(sent_photo_id, 777);

    // sendChatAction & setMessageReaction succeed
    client.send_chat_action(-1001, Some(301), "typing").unwrap();
    client.set_message_reaction(-1001, 777, "👍").unwrap();

    // deleteForumTopic succeeds and maps TOPIC_ID_INVALID to TopicNotFound
    client.delete_forum_topic(-1001, 301).unwrap();
    assert_eq!(
        client.delete_forum_topic(-1001, 999).unwrap_err(),
        ApiError::TopicNotFound
    );

    // 429 classified cleanly as RateLimited
    let err = client.get_updates(0, 1).unwrap_err();
    assert_eq!(err, ApiError::RateLimited { retry_after: 7 });

    // Bounded download succeeds under limit and rejects over limit
    let bytes = client.download_file_bounded("photos/a.png", 64).unwrap();
    assert_eq!(bytes, b"hello-bytes");
    let big_err = client.download_file_bounded("photos/big.bin", 64).unwrap_err();
    assert!(matches!(big_err, ApiError::BadRequest(_)));

    handle.join().unwrap();
    let seen = recorded_reqs.lock().unwrap();
    assert!(seen[0].0.contains("/bot123456:TESTTOKEN/getMe"));
    assert!(seen[3].0.contains("/bot123456:TESTTOKEN/pinChatMessage"));
    assert!(seen[5].0.contains("/bot123456:TESTTOKEN/sendPhoto"));
    assert!(seen[5]
        .1
        .to_ascii_lowercase()
        .contains("content-type: multipart/form-data; boundary="));
    assert!(seen[5].1.contains("filename=\"shot.png\""));
    assert!(seen[5].1.contains("PNGRAWDATA"));
    assert!(seen[6].0.contains("/bot123456:TESTTOKEN/sendChatAction"));
    assert!(seen[7].0.contains("/bot123456:TESTTOKEN/setMessageReaction"));
    assert!(seen[8].0.contains("/bot123456:TESTTOKEN/deleteForumTopic"));
    assert!(seen[8].1.contains("\"message_thread_id\":301"));
    assert!(seen[11].0.contains("/file/bot123456:TESTTOKEN/photos/a.png"));
}
