//! Minimal blocking Telegram Bot API client over `reqwest` + `rustls`.
use super::config::mask_token_in_text;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

pub const DEFAULT_API_BASE: &str = "https://api.telegram.org";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    Unauthorized,
    Conflict,
    RateLimited { retry_after: u64 },
    TopicNotFound,
    TopicNotModified,
    MessageNotFound,
    MessageNotModified,
    BadRequest(String),
    Network(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "Bot token rejected by Telegram (401 Unauthorized)"),
            Self::Conflict => write!(
                f,
                "Another process or PC is polling updates for this same bot token (409 Conflict)"
            ),
            Self::RateLimited { retry_after } => {
                write!(f, "Telegram rate limit (retry after {retry_after}s)")
            }
            Self::TopicNotFound => write!(f, "Forum topic was deleted or not found"),
            Self::TopicNotModified => write!(f, "Forum topic is unchanged"),
            Self::MessageNotFound => write!(f, "Message to edit was deleted or not found"),
            Self::MessageNotModified => write!(f, "Message is unchanged"),
            Self::BadRequest(msg) => write!(f, "Telegram API error: {msg}"),
            Self::Network(msg) => write!(f, "Telegram network error: {msg}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct BotUser {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    pub first_name: String,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    pub first_name: String,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub is_forum: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Document {
    pub file_id: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Voice {
    pub file_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Audio {
    pub file_id: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ReplyMessage {
    pub message_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Message {
    pub message_id: i64,
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    #[serde(default)]
    pub is_topic_message: Option<bool>,
    #[serde(default)]
    pub from: Option<User>,
    pub chat: Chat,
    #[serde(default)]
    pub date: u64,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub reply_to_message: Option<ReplyMessage>,
    #[serde(default)]
    pub photo: Option<Vec<PhotoSize>>,
    #[serde(default)]
    pub document: Option<Document>,
    #[serde(default)]
    pub voice: Option<Voice>,
    #[serde(default)]
    pub audio: Option<Audio>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub data: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ChatInfo {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub is_forum: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ChatMember {
    pub status: String,
    #[serde(default)]
    pub can_manage_topics: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ForumTopic {
    pub message_thread_id: i64,
    pub name: String,
    #[serde(default)]
    pub icon_color: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct FileInfo {
    pub file_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub file_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineButton {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl InlineButton {
    pub fn callback(text: impl Into<String>, callback_data: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            callback_data: Some(callback_data.into()),
            url: None,
        }
    }

    pub fn url(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            callback_data: None,
            url: Some(url.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineButton>>,
}

pub trait TelegramApi {
    fn get_me(&self) -> Result<BotUser, ApiError>;
    fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Update>, ApiError>;
    fn send_message(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        html: &str,
        reply_to: Option<i64>,
        keyboard: Option<&InlineKeyboardMarkup>,
    ) -> Result<i64, ApiError>;
    fn send_photo(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        caption_html: Option<&str>,
    ) -> Result<i64, ApiError>;
    fn send_animation(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        caption_html: Option<&str>,
    ) -> Result<i64, ApiError>;
    fn send_document(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        caption_html: Option<&str>,
    ) -> Result<i64, ApiError>;
    fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        html: &str,
        keyboard: Option<&InlineKeyboardMarkup>,
    ) -> Result<(), ApiError>;
    fn pin_chat_message(
        &self,
        chat_id: i64,
        message_id: i64,
        disable_notification: bool,
    ) -> Result<(), ApiError>;
    fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<(), ApiError>;
    fn set_my_commands(&self, commands: &[(&str, &str)]) -> Result<(), ApiError>;
    fn get_chat(&self, chat_id: i64) -> Result<ChatInfo, ApiError>;
    fn get_chat_member(&self, chat_id: i64, user_id: i64) -> Result<ChatMember, ApiError>;
    fn get_chat_member_count(&self, chat_id: i64) -> Result<u64, ApiError>;
    fn create_forum_topic(
        &self,
        chat_id: i64,
        name: &str,
        icon_color: u32,
    ) -> Result<ForumTopic, ApiError>;
    fn edit_forum_topic(&self, chat_id: i64, thread_id: i64, name: &str) -> Result<(), ApiError>;
    fn close_forum_topic(&self, chat_id: i64, thread_id: i64) -> Result<(), ApiError>;
    fn reopen_forum_topic(&self, chat_id: i64, thread_id: i64) -> Result<(), ApiError>;
    fn get_file(&self, file_id: &str) -> Result<FileInfo, ApiError>;
    fn download_file_bounded(&self, file_path: &str, max_bytes: usize) -> Result<Vec<u8>, ApiError>;
}

#[derive(Deserialize)]
struct ApiEnvelopeRaw {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    error_code: Option<u16>,
    #[serde(default)]
    parameters: Option<ApiParameters>,
}

#[derive(Deserialize)]
struct ApiParameters {
    #[serde(default)]
    retry_after: Option<u64>,
}

fn ensure_rustls_provider() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub fn validate_api_base(base: &str) -> Result<String, ApiError> {
    let trimmed = base.trim().trim_end_matches('/');
    let url = reqwest::Url::parse(trimmed)
        .map_err(|_| ApiError::BadRequest("Invalid Telegram API base URL".into()))?;
    let is_loopback = matches!(url.host_str(), Some("127.0.0.1" | "::1" | "[::1]"));
    match url.scheme() {
        "https" => Ok(trimmed.to_string()),
        "http" if is_loopback => Ok(trimmed.to_string()),
        _ => Err(ApiError::BadRequest(
            "Telegram API base URL must use HTTPS (or loopback HTTP for tests)".into(),
        )),
    }
}

pub struct TelegramClient {
    http: reqwest::blocking::Client,
    api_base: String,
    token: String,
}

impl TelegramClient {
    pub fn new(token: &str) -> Result<Self, ApiError> {
        let base = std::env::var("SD_TELEGRAM_API_BASE_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
        Self::with_base_url(token, &base)
    }

    pub fn with_base_url(token: &str, base_url: &str) -> Result<Self, ApiError> {
        let token = token.trim();
        if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(ApiError::BadRequest("Invalid bot token format".into()));
        }
        let api_base = validate_api_base(base_url)?;
        ensure_rustls_provider();
        let allow_http_loopback = api_base.starts_with("http://");
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(!allow_http_loopback)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .build()
            .map_err(|e| ApiError::Network(mask_token_in_text(&e.to_string(), token)))?;
        Ok(Self {
            http,
            api_base,
            token: token.to_string(),
        })
    }

    fn parse_response<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        resp: reqwest::blocking::Response,
    ) -> Result<T, ApiError> {
        let status = resp.status().as_u16();
        let mut bytes = Vec::new();
        resp.take(4 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| ApiError::Network(mask_token_in_text(&e.to_string(), &self.token)))?;
        let env: ApiEnvelopeRaw = serde_json::from_slice(&bytes).map_err(|_| {
            ApiError::Network(format!("Invalid JSON response from Telegram (HTTP {status})"))
        })?;
        if env.ok {
            let result = env.result.ok_or_else(|| {
                ApiError::BadRequest(format!("Telegram returned ok=true without result ({method})"))
            })?;
            return serde_json::from_value(result).map_err(|_| {
                ApiError::Network(format!("Unexpected Telegram result shape ({method})"))
            });
        }
        let code = env.error_code.unwrap_or(status);
        let desc = mask_token_in_text(
            env.description.as_deref().unwrap_or("Unknown Telegram error"),
            &self.token,
        );
        Err(classify_error(code, &desc, env.parameters.and_then(|p| p.retry_after)))
    }

    fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: &Value,
    ) -> Result<T, ApiError> {
        let url = format!("{}/bot{}/{}", self.api_base, self.token, method);
        let resp = self
            .http
            .post(&url)
            .json(body)
            .send()
            .map_err(|e| ApiError::Network(mask_token_in_text(&e.to_string(), &self.token)))?;
        self.parse_response(method, resp)
    }

    #[allow(clippy::too_many_arguments)]
    fn call_multipart<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        fields: &[(&str, String)],
        file_field: &str,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
    ) -> Result<T, ApiError> {
        let boundary = format!(
            "----sd-tg-boundary-{}",
            super_desktop::control::random_hex(12)
                .unwrap_or_else(|_| format!("{:x}", std::process::id()))
        );
        let safe_filename: String = filename
            .chars()
            .filter(|&c| !c.is_control() && c != '"' && c != '\\' && c != '/' && c != '\r' && c != '\n')
            .collect();
        let safe_filename = if safe_filename.is_empty() {
            "file.bin".to_string()
        } else {
            safe_filename
        };
        let safe_mime: String = mime_type
            .chars()
            .filter(|&c| !c.is_control() && c != '\r' && c != '\n')
            .collect();
        let safe_mime = if safe_mime.is_empty() {
            "application/octet-stream".to_string()
        } else {
            safe_mime
        };

        let mut payload = Vec::with_capacity(bytes.len() + 1024);
        for (name, val) in fields {
            payload.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            payload.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            );
            payload.extend_from_slice(val.as_bytes());
            payload.extend_from_slice(b"\r\n");
        }
        payload.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        payload.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{file_field}\"; filename=\"{safe_filename}\"\r\n\
                 Content-Type: {safe_mime}\r\n\r\n"
            )
            .as_bytes(),
        );
        payload.extend_from_slice(bytes);
        payload.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let url = format!("{}/bot{}/{}", self.api_base, self.token, method);
        let resp = self
            .http
            .post(&url)
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(payload)
            .send()
            .map_err(|e| ApiError::Network(mask_token_in_text(&e.to_string(), &self.token)))?;
        self.parse_response(method, resp)
    }

    #[allow(clippy::too_many_arguments)]
    fn send_media(
        &self,
        method: &str,
        file_field: &str,
        chat_id: i64,
        thread_id: Option<i64>,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        caption_html: Option<&str>,
    ) -> Result<i64, ApiError> {
        let mut fields = vec![("chat_id", chat_id.to_string())];
        if let Some(tid) = thread_id.filter(|&id| id > 1) {
            fields.push(("message_thread_id", tid.to_string()));
        }
        if let Some(cap) = caption_html.filter(|c| !c.is_empty()) {
            fields.push(("caption", cap.to_string()));
            fields.push(("parse_mode", "HTML".to_string()));
        }
        let sent: Message =
            self.call_multipart(method, &fields, file_field, filename, mime_type, bytes)?;
        Ok(sent.message_id)
    }
}

fn classify_error(code: u16, desc: &str, retry_after: Option<u64>) -> ApiError {
    let lower = desc.to_ascii_lowercase();
    if code == 401 || lower.contains("unauthorized") {
        ApiError::Unauthorized
    } else if code == 409 {
        ApiError::Conflict
    } else if code == 429 {
        ApiError::RateLimited {
            retry_after: retry_after.unwrap_or(5).max(1),
        }
    } else if lower.contains("message thread not found") || lower.contains("topic_deleted") {
        ApiError::TopicNotFound
    } else if lower.contains("topic_not_modified") || lower.contains("topic_closed") {
        ApiError::TopicNotModified
    } else if lower.contains("message is not modified") {
        ApiError::MessageNotModified
    } else if lower.contains("message to edit not found")
        || lower.contains("message_id_invalid")
        || lower.contains("message to pin not found")
    {
        ApiError::MessageNotFound
    } else {
        ApiError::BadRequest(desc.to_string())
    }
}

impl TelegramApi for TelegramClient {
    fn get_me(&self) -> Result<BotUser, ApiError> {
        self.call("getMe", &json!({}))
    }

    fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Update>, ApiError> {
        self.call(
            "getUpdates",
            &json!({
                "offset": offset,
                "timeout": timeout_secs,
                "allowed_updates": ["message", "callback_query"]
            }),
        )
    }

    fn send_message(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        html: &str,
        reply_to: Option<i64>,
        keyboard: Option<&InlineKeyboardMarkup>,
    ) -> Result<i64, ApiError> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": html,
            "parse_mode": "HTML",
            "disable_web_page_preview": true
        });
        if let Some(tid) = thread_id.filter(|&id| id > 1) {
            body["message_thread_id"] = json!(tid);
        }
        if let Some(reply_id) = reply_to {
            body["reply_parameters"] = json!({ "message_id": reply_id, "allow_sending_without_reply": true });
        }
        if let Some(kb) = keyboard {
            body["reply_markup"] = serde_json::to_value(kb).unwrap_or(Value::Null);
        }
        let sent: Message = self.call("sendMessage", &body)?;
        Ok(sent.message_id)
    }

    fn send_photo(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        caption_html: Option<&str>,
    ) -> Result<i64, ApiError> {
        self.send_media(
            "sendPhoto",
            "photo",
            chat_id,
            thread_id,
            filename,
            mime_type,
            bytes,
            caption_html,
        )
    }

    fn send_animation(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        caption_html: Option<&str>,
    ) -> Result<i64, ApiError> {
        self.send_media(
            "sendAnimation",
            "animation",
            chat_id,
            thread_id,
            filename,
            mime_type,
            bytes,
            caption_html,
        )
    }

    fn send_document(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        caption_html: Option<&str>,
    ) -> Result<i64, ApiError> {
        self.send_media(
            "sendDocument",
            "document",
            chat_id,
            thread_id,
            filename,
            mime_type,
            bytes,
            caption_html,
        )
    }

    fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        html: &str,
        keyboard: Option<&InlineKeyboardMarkup>,
    ) -> Result<(), ApiError> {
        let mut body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": html,
            "parse_mode": "HTML",
            "disable_web_page_preview": true
        });
        if let Some(kb) = keyboard {
            body["reply_markup"] = serde_json::to_value(kb).unwrap_or(Value::Null);
        }
        match self.call::<Value>("editMessageText", &body) {
            Ok(_) | Err(ApiError::MessageNotModified) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn pin_chat_message(
        &self,
        chat_id: i64,
        message_id: i64,
        disable_notification: bool,
    ) -> Result<(), ApiError> {
        let _: Value = self.call(
            "pinChatMessage",
            &json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "disable_notification": disable_notification
            }),
        )?;
        Ok(())
    }

    fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<(), ApiError> {
        let mut body = json!({ "callback_query_id": callback_id });
        if let Some(t) = text {
            body["text"] = json!(t);
        }
        let _: Value = self.call("answerCallbackQuery", &body)?;
        Ok(())
    }

    fn set_my_commands(&self, commands: &[(&str, &str)]) -> Result<(), ApiError> {
        let items: Vec<Value> = commands
            .iter()
            .map(|(cmd, desc)| json!({ "command": cmd, "description": desc }))
            .collect();
        let _: Value = self.call("setMyCommands", &json!({ "commands": items }))?;
        Ok(())
    }

    fn get_chat(&self, chat_id: i64) -> Result<ChatInfo, ApiError> {
        self.call("getChat", &json!({ "chat_id": chat_id }))
    }

    fn get_chat_member(&self, chat_id: i64, user_id: i64) -> Result<ChatMember, ApiError> {
        self.call(
            "getChatMember",
            &json!({ "chat_id": chat_id, "user_id": user_id }),
        )
    }

    fn get_chat_member_count(&self, chat_id: i64) -> Result<u64, ApiError> {
        self.call("getChatMemberCount", &json!({ "chat_id": chat_id }))
    }

    fn create_forum_topic(
        &self,
        chat_id: i64,
        name: &str,
        icon_color: u32,
    ) -> Result<ForumTopic, ApiError> {
        self.call(
            "createForumTopic",
            &json!({
                "chat_id": chat_id,
                "name": name,
                "icon_color": icon_color
            }),
        )
    }

    fn edit_forum_topic(&self, chat_id: i64, thread_id: i64, name: &str) -> Result<(), ApiError> {
        match self.call::<Value>(
            "editForumTopic",
            &json!({
                "chat_id": chat_id,
                "message_thread_id": thread_id,
                "name": name
            }),
        ) {
            Ok(_) | Err(ApiError::TopicNotModified) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn close_forum_topic(&self, chat_id: i64, thread_id: i64) -> Result<(), ApiError> {
        match self.call::<Value>(
            "closeForumTopic",
            &json!({
                "chat_id": chat_id,
                "message_thread_id": thread_id
            }),
        ) {
            Ok(_) | Err(ApiError::TopicNotModified) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn reopen_forum_topic(&self, chat_id: i64, thread_id: i64) -> Result<(), ApiError> {
        match self.call::<Value>(
            "reopenForumTopic",
            &json!({
                "chat_id": chat_id,
                "message_thread_id": thread_id
            }),
        ) {
            Ok(_) | Err(ApiError::TopicNotModified) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn get_file(&self, file_id: &str) -> Result<FileInfo, ApiError> {
        self.call("getFile", &json!({ "file_id": file_id }))
    }

    fn download_file_bounded(&self, file_path: &str, max_bytes: usize) -> Result<Vec<u8>, ApiError> {
        if file_path.is_empty() || file_path.contains("..") || file_path.starts_with('/') {
            return Err(ApiError::BadRequest("Invalid Telegram file path".into()));
        }
        let url = format!("{}/file/bot{}/{}", self.api_base, self.token, file_path);
        let resp = self
            .http
            .get(&url)
            .send()
            .map_err(|e| ApiError::Network(mask_token_in_text(&e.to_string(), &self.token)))?;
        if !resp.status().is_success() {
            return Err(ApiError::BadRequest(format!(
                "Telegram file download failed (HTTP {})",
                resp.status().as_u16()
            )));
        }
        if resp
            .content_length()
            .is_some_and(|len| len > max_bytes as u64)
        {
            return Err(ApiError::BadRequest(format!(
                "Telegram file exceeds {} byte limit",
                max_bytes
            )));
        }
        let mut bytes = Vec::new();
        resp.take((max_bytes + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| ApiError::Network(mask_token_in_text(&e.to_string(), &self.token)))?;
        if bytes.len() > max_bytes {
            return Err(ApiError::BadRequest(format!(
                "Telegram file exceeds {} byte limit",
                max_bytes
            )));
        }
        Ok(bytes)
    }
}
