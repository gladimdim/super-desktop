//! Owner-only configuration and runtime state for the Telegram worker.
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_CONFIG_BYTES: usize = 256 * 1024;
pub const MAX_RUNTIME_BYTES: usize = 512 * 1024;
pub const MAX_MESSAGE_CARD_MAP: usize = 256;
pub const MAX_REJECTED_USERS: usize = 256;
pub const MAX_TOPICS: usize = 256;
pub const MAX_LIVE_STREAMS: usize = 3;
pub const DEFAULT_MINI_APP_PORT: u16 = 7789;
pub const DEFAULT_MINI_APP_HTTPS_PORT: u16 = 8443;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MiniAppConfig {
    pub enabled: bool,
    pub listen_port: u16,
    pub public_url: String,
    pub short_name: String,
}

impl Default for MiniAppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_port: DEFAULT_MINI_APP_PORT,
            public_url: String::new(),
            short_name: String::new(),
        }
    }
}

impl MiniAppConfig {
    pub fn effective_port(&self) -> u16 {
        if self.listen_port >= 1024 {
            self.listen_port
        } else {
            DEFAULT_MINI_APP_PORT
        }
    }

    pub fn normalized_url(&self) -> Option<String> {
        let trimmed = self.public_url.trim().trim_end_matches('/');
        if !trimmed.starts_with("https://")
            || trimmed.len() <= "https://".len()
            || trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return None;
        }
        Some(trimmed.to_string())
    }

    pub fn valid_short_name(&self) -> Option<&str> {
        let s = self.short_name.trim();
        if s.is_empty()
            || s.len() > 64
            || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return None;
        }
        Some(s)
    }

    pub fn card_deep_link(&self, bot_username: Option<&str>, card_id: &str) -> Option<String> {
        if !self.enabled || self.normalized_url().is_none() {
            return None;
        }
        let bot = bot_username.map(str::trim).filter(|b| !b.is_empty())?;
        let short = self.valid_short_name()?;
        Some(format!("https://t.me/{bot}/{short}?startapp=card_{card_id}"))
    }

    pub fn dashboard_deep_link(&self, bot_username: Option<&str>) -> Option<String> {
        if !self.enabled || self.normalized_url().is_none() {
            return None;
        }
        let bot = bot_username.map(str::trim).filter(|b| !b.is_empty())?;
        let short = self.valid_short_name()?;
        Some(format!("https://t.me/{bot}/{short}"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Permissions {
    pub notifications: bool,
    pub read_output: bool,
    pub prompts: bool,
    pub controls: bool,
    pub shell_input: bool,
    pub launch: bool,
    pub close: bool,
    pub attachments: bool,
    pub redact_secrets: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            notifications: true,
            read_output: false,
            prompts: false,
            controls: false,
            shell_input: false,
            launch: false,
            close: false,
            attachments: false,
            redact_secrets: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowedUser {
    pub user_id: i64,
    pub chat_id: i64,
    #[serde(default)]
    pub username: Option<String>,
    pub first_name: String,
    pub paired_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForumConfig {
    pub chat_id: i64,
    pub title: String,
    pub linked_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub enabled: bool,
    pub bot_token: String,
    pub bot_id: Option<i64>,
    pub bot_username: Option<String>,
    pub allowed_user: Option<AllowedUser>,
    pub rejected_user_ids: Vec<i64>,
    pub forum: Option<ForumConfig>,
    pub auto_sync_topics: bool,
    pub archive_closed_topics: bool,
    pub mirror_alerts_to_private: bool,
    pub permissions: Permissions,
    #[serde(default)]
    pub mini_app: MiniAppConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            bot_id: None,
            bot_username: None,
            allowed_user: None,
            rejected_user_ids: Vec::new(),
            forum: None,
            auto_sync_topics: true,
            archive_closed_topics: true,
            mirror_alerts_to_private: false,
            permissions: Permissions::default(),
            mini_app: MiniAppConfig::default(),
        }
    }
}

impl Config {
    pub fn should_run(&self) -> bool {
        self.enabled && !self.bot_token.trim().is_empty()
    }

    pub fn disabled() -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicBinding {
    pub card_id: String,
    pub thread_id: i64,
    pub topic_name: String,
    pub icon_color: u32,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub delete_failed: bool,
    #[serde(default)]
    pub next_delete_at: u64,
    #[serde(default)]
    pub deleted_in_telegram: bool,
    #[serde(default)]
    pub last_renamed_at: u64,
    #[serde(default)]
    pub status_message_id: Option<i64>,
    #[serde(default)]
    pub last_status: String,
    #[serde(default)]
    pub last_status_card: Option<String>,
    #[serde(default)]
    pub status_pinned: bool,
    #[serde(default)]
    pub next_rename_allowed_at: u64,
    #[serde(default)]
    pub pending_status: Option<String>,
    #[serde(default)]
    pub pending_status_since: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInvite {
    pub secret: String,
    pub created_at: u64,
    pub expires_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequest {
    pub request_id: String,
    pub user_id: i64,
    pub chat_id: i64,
    #[serde(default)]
    pub username: Option<String>,
    pub first_name: String,
    pub code: String,
    pub requested_at: u64,
    pub expires_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkerStatus {
    pub state: String,
    pub pid: Option<u32>,
    pub bot_username: Option<String>,
    pub last_poll_at: u64,
    pub last_error: Option<String>,
}

impl Default for WorkerStatus {
    fn default() -> Self {
        Self {
            state: "stopped".to_string(),
            pid: None,
            bot_username: None,
            last_poll_at: 0,
            last_error: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageCardRef {
    pub chat_id: i64,
    pub message_id: i64,
    pub card_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LiveStream {
    pub card_id: String,
    pub chat_id: i64,
    #[serde(default)]
    pub thread_id: Option<i64>,
    pub message_id: i64,
    #[serde(default)]
    pub last_text: String,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub last_edited_at: u64,
    #[serde(default)]
    pub idle_since: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RuntimeState {
    pub update_offset: i64,
    pub active_card_id: Option<String>,
    pub topics: Vec<TopicBinding>,
    pub message_card_map: Vec<MessageCardRef>,
    pub worker_status: WorkerStatus,
    #[serde(default)]
    pub dashboard_message_id: Option<i64>,
    #[serde(default)]
    pub dashboard_pinned: bool,
    #[serde(default)]
    pub last_dashboard_card: Option<String>,
    #[serde(default)]
    pub last_dashboard_edit_at: u64,
    #[serde(default)]
    pub live_streams: Vec<LiveStream>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRequest {
    pub requested_at: u64,
}

impl RuntimeState {
    pub fn record_message_card(&mut self, chat_id: i64, message_id: i64, card_id: &str) {
        if let Some(existing) = self
            .message_card_map
            .iter_mut()
            .find(|entry| entry.chat_id == chat_id && entry.message_id == message_id)
        {
            existing.card_id = card_id.to_string();
            return;
        }
        self.message_card_map.push(MessageCardRef {
            chat_id,
            message_id,
            card_id: card_id.to_string(),
        });
        while self.message_card_map.len() > MAX_MESSAGE_CARD_MAP {
            self.message_card_map.remove(0);
        }
    }

    pub fn card_for_message(&self, chat_id: i64, message_id: i64) -> Option<&str> {
        self.message_card_map
            .iter()
            .rev()
            .find(|entry| entry.chat_id == chat_id && entry.message_id == message_id)
            .map(|entry| entry.card_id.as_str())
    }

    #[cfg(test)]
    pub fn topic_for_card(&self, card_id: &str) -> Option<&TopicBinding> {
        self.topics.iter().find(|topic| topic.card_id == card_id)
    }

    pub fn card_for_thread(&self, thread_id: i64) -> Option<&str> {
        self.topics
            .iter()
            .find(|topic| topic.thread_id == thread_id && !topic.deleted_in_telegram)
            .map(|topic| topic.card_id.as_str())
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn state_dir() -> io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("SD_TELEGRAM_STATE_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Ok(dir);
    }
    #[cfg(test)]
    {
        Ok(std::env::temp_dir().join(format!("sd-telegram-test-{}", std::process::id())))
    }
    #[cfg(not(test))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join(".local/state"))
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No user state directory"))?;
        Ok(base.join("super-desktop/telegram"))
    }
}

pub fn config_path() -> io::Result<PathBuf> {
    Ok(state_dir()?.join("config.json"))
}

pub fn runtime_path() -> io::Result<PathBuf> {
    Ok(state_dir()?.join("runtime.json"))
}

pub fn lock_path() -> io::Result<PathBuf> {
    Ok(state_dir()?.join("worker.lock"))
}

pub fn log_path() -> io::Result<PathBuf> {
    Ok(state_dir()?.join("telegram.log"))
}

pub fn load_config() -> io::Result<Config> {
    load_config_at(&config_path()?)
}

pub fn save_config(config: &Config) -> io::Result<()> {
    save_config_at(&config_path()?, config)
}

pub fn load_runtime() -> io::Result<RuntimeState> {
    load_runtime_at(&runtime_path()?)
}

pub fn load_invite() -> io::Result<Option<PendingInvite>> {
    load_invite_at(&state_dir()?)
}

pub fn load_pending_request() -> io::Result<Option<PendingRequest>> {
    load_pending_request_at(&state_dir()?)
}

pub fn request_topic_sync() -> io::Result<()> {
    request_topic_sync_at(&state_dir()?, unix_now())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileStamp {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

pub fn file_stamp(path: &Path) -> Option<FileStamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).ok()?;
    Some(FileStamp {
        dev: meta.dev(),
        ino: meta.ino(),
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

pub fn should_run_from_disk() -> bool {
    let Ok(path) = config_path() else {
        return false;
    };
    if !path.exists() {
        return false;
    }
    load_config_at(&path).is_ok_and(|cfg| cfg.should_run())
}

pub fn load_config_at(path: &Path) -> io::Result<Config> {
    load_json_at(path, MAX_CONFIG_BYTES, "Telegram settings")
}

pub fn save_config_at(path: &Path, config: &Config) -> io::Result<()> {
    save_json_at(path, "tg-cfg", config, "Telegram settings")
}

pub fn load_runtime_at(path: &Path) -> io::Result<RuntimeState> {
    load_json_at(path, MAX_RUNTIME_BYTES, "Telegram runtime state")
}

pub fn save_runtime_at(path: &Path, runtime: &RuntimeState) -> io::Result<()> {
    save_json_at(path, "tg-rt", runtime, "Telegram runtime state")
}

pub fn load_invite_at(dir: &Path) -> io::Result<Option<PendingInvite>> {
    load_optional_json_at(&dir.join("invite.json"), 4096, "Telegram invite")
}

pub fn save_invite_at(dir: &Path, invite: &PendingInvite) -> io::Result<()> {
    save_json_at(&dir.join("invite.json"), "tg-inv", invite, "Telegram invite")
}

pub fn clear_invite_at(dir: &Path) -> io::Result<()> {
    remove_file_if_present(&dir.join("invite.json"))
}

pub fn load_pending_request_at(dir: &Path) -> io::Result<Option<PendingRequest>> {
    load_optional_json_at(&dir.join("pending_request.json"), 8192, "Telegram pairing request")
}

pub fn save_pending_request_at(dir: &Path, req: &PendingRequest) -> io::Result<()> {
    save_json_at(
        &dir.join("pending_request.json"),
        "tg-req",
        req,
        "Telegram pairing request",
    )
}

pub fn clear_pending_request_at(dir: &Path) -> io::Result<()> {
    remove_file_if_present(&dir.join("pending_request.json"))
}

pub fn request_topic_sync_at(dir: &Path, now: u64) -> io::Result<()> {
    save_json_at(
        &dir.join("sync_request.json"),
        "tg-sync",
        &SyncRequest { requested_at: now },
        "Telegram sync request",
    )
}

pub fn take_topic_sync_request_at(dir: &Path) -> bool {
    let path = dir.join("sync_request.json");
    let found = load_optional_json_at::<SyncRequest>(&path, 1024, "Telegram sync request")
        .ok()
        .flatten()
        .is_some();
    if found {
        let _ = remove_file_if_present(&path);
    }
    found
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn check_owner_dir(parent: &Path, label: &str) -> io::Result<()> {
    match fs::symlink_metadata(parent) {
        Ok(metadata)
            if !metadata.is_dir()
                || metadata.uid() != unsafe { libc::getuid() }
                || metadata.mode() & 0o022 != 0 =>
        {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Unsafe {label} directory"),
            ))
        }
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub fn ensure_private_dir(dir: &Path, label: &str) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::getuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{label} directory must be owner-only (0700)"),
        ));
    }
    Ok(())
}

fn load_json_at<T: DeserializeOwned + Default>(
    path: &Path,
    max_bytes: usize,
    label: &str,
) -> io::Result<T> {
    Ok(load_optional_json_at(path, max_bytes, label)?.unwrap_or_default())
}

fn load_optional_json_at<T: DeserializeOwned>(
    path: &Path,
    max_bytes: usize,
    label: &str,
) -> io::Result<Option<T>> {
    if let Some(parent) = path.parent() {
        check_owner_dir(parent, label)?;
    }
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::getuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{label} must be an owner-only regular file"),
        ));
    }
    let mut bytes = Vec::new();
    file.take((max_bytes + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} file is too large"),
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("Invalid {label} JSON")))
}

fn save_json_at<T: Serialize>(
    path: &Path,
    prefix: &str,
    value: &T,
    label: &str,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid path"))?;
    ensure_private_dir(parent, label)?;
    let nonce = super_desktop::control::random_hex(12)?;
    let temp = parent.join(format!(".{prefix}-{nonce}.tmp"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    let _ = fs::remove_file(temp);
    result
}

/// Mask a bot token out of arbitrary error or diagnostic strings.
pub fn mask_token_in_text(text: &str, token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        text.to_string()
    } else {
        text.replace(trimmed, "[REDACTED_TOKEN]")
    }
}
