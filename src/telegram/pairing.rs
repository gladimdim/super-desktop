//! Account pairing, desktop approval, revocation, and private 2-member Forum linking.
use super::api::{TelegramApi, User};
use super::config::{
    clear_invite_at, clear_pending_request_at, config_path, load_config_at, load_invite_at,
    load_pending_request_at, save_config_at, save_invite_at, save_pending_request_at,
    save_runtime_at, unix_now, AllowedUser, Config, ForumConfig, PendingInvite, PendingRequest,
    RuntimeState, MAX_REJECTED_USERS,
};
use std::path::Path;
use std::process::{Command, Stdio};

pub const INVITE_TTL_SECS: u64 = 300;
pub const REQUEST_TTL_SECS: u64 = 180;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingReply {
    pub reply_html: Option<String>,
    pub pending_created: bool,
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8;
    let max_len = a.len().max(b.len());
    for i in 0..max_len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0 && a.len() == b.len()
}

pub fn format_code(code: &str) -> String {
    if code.len() == 6 && code.is_ascii() {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

fn random_six_digit_code() -> Result<String, String> {
    let hex = super_desktop::control::random_hex(4).map_err(|e| e.to_string())?;
    let val = u32::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
    Ok(format!("{:06}", val % 1_000_000))
}

pub fn start_invite(state_dir: &Path) -> Result<(PendingInvite, Option<String>), String> {
    let cfg_path = state_dir.join("config.json");
    let cfg = load_config_at(&cfg_path).map_err(|e| e.to_string())?;
    if !cfg.should_run() {
        return Err("Enable the Telegram bot and save a valid token first.".into());
    }
    let now = unix_now();
    let secret = super_desktop::control::random_hex(24).map_err(|e| e.to_string())?;
    let invite = PendingInvite {
        secret: secret.clone(),
        created_at: now,
        expires_at: now + INVITE_TTL_SECS,
    };
    save_invite_at(state_dir, &invite).map_err(|e| e.to_string())?;
    let _ = clear_pending_request_at(state_dir);

    let link = cfg
        .bot_username
        .as_deref()
        .filter(|u| !u.trim().is_empty())
        .map(|username| format!("https://t.me/{username}?start={secret}"));
    Ok((invite, link))
}

pub fn start_invite_default() -> Result<(PendingInvite, Option<String>), String> {
    let dir = default_state_dir()?;
    start_invite(&dir)
}

pub fn has_active_invite(state_dir: &Path, now: u64) -> bool {
    load_invite_at(state_dir)
        .ok()
        .flatten()
        .is_some_and(|inv| now <= inv.expires_at)
}

pub fn handle_pairing_start(
    state_dir: &Path,
    cfg: &Config,
    from: &User,
    chat_id: i64,
    chat_type: &str,
    secret: &str,
    now: u64,
) -> PairingReply {
    if chat_type != "private" || from.is_bot {
        return PairingReply {
            reply_html: None,
            pending_created: false,
        };
    }
    if cfg.rejected_user_ids.contains(&from.id) {
        return PairingReply {
            reply_html: None,
            pending_created: false,
        };
    }
    let Some(invite) = load_invite_at(state_dir).ok().flatten() else {
        return PairingReply {
            reply_html: Some(
                "No active pairing invitation on this PC. Open <b>Settings → Telegram</b> and click <b>Pair Telegram account</b>."
                    .to_string(),
            ),
            pending_created: false,
        };
    };
    if now > invite.expires_at {
        let _ = clear_invite_at(state_dir);
        return PairingReply {
            reply_html: Some(
                "That pairing invitation expired. Generate a fresh one in <b>Settings → Telegram</b>."
                    .to_string(),
            ),
            pending_created: false,
        };
    }
    if !constant_time_eq(invite.secret.as_bytes(), secret.trim().as_bytes()) {
        return PairingReply {
            reply_html: Some(
                "Invalid pairing invitation. Generate a fresh one in <b>Settings → Telegram</b>."
                    .to_string(),
            ),
            pending_created: false,
        };
    }

    let Ok(request_id) = super_desktop::control::random_hex(16) else {
        return PairingReply {
            reply_html: Some("Could not generate pairing request on PC.".to_string()),
            pending_created: false,
        };
    };
    let Ok(code) = random_six_digit_code() else {
        return PairingReply {
            reply_html: Some("Could not generate verification code on PC.".to_string()),
            pending_created: false,
        };
    };

    // Single-use invitation is consumed immediately on valid match.
    if clear_invite_at(state_dir).is_err() {
        return PairingReply {
            reply_html: Some("Could not consume pairing invitation safely on PC.".to_string()),
            pending_created: false,
        };
    }
    let req = PendingRequest {
        request_id,
        user_id: from.id,
        chat_id,
        username: from.username.clone(),
        first_name: from.first_name.clone(),
        code: code.clone(),
        requested_at: now,
        expires_at: now + REQUEST_TTL_SECS,
    };
    if save_pending_request_at(state_dir, &req).is_err() {
        return PairingReply {
            reply_html: Some("Could not save pairing request on PC.".to_string()),
            pending_created: false,
        };
    }

    notify_desktop_pairing_request();

    PairingReply {
        reply_html: Some(format!(
            "Pairing request sent to your PC.\n\nComparison code: <code>{}</code>\nOpen <b>Settings → Telegram</b> on your PC and click <b>Approve</b> if this code matches.",
            format_code(&code)
        )),
        pending_created: true,
    }
}

fn notify_desktop_pairing_request() {
    if cfg!(test) {
        return;
    }
    // Constant text only — never interpolate untrusted Telegram profile strings.
    let _ = Command::new("notify-send")
        .args([
            "SUPER DESKTOP",
            "Telegram pairing request — open Settings → Telegram to review",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

pub fn approve_pending(state_dir: &Path, request_id: &str) -> Result<AllowedUser, String> {
    let cfg_path = state_dir.join("config.json");
    let mut cfg = load_config_at(&cfg_path).map_err(|e| e.to_string())?;
    let now = unix_now();
    let req = load_pending_request_at(state_dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No pending Telegram pairing request.".to_string())?;
    if req.request_id != request_id {
        return Err("Pairing request no longer matches; refresh and try again.".into());
    }
    if now > req.expires_at {
        let _ = clear_pending_request_at(state_dir);
        return Err("Pairing request expired. Start pairing again.".into());
    }
    let allowed = AllowedUser {
        user_id: req.user_id,
        chat_id: req.chat_id,
        username: req.username,
        first_name: req.first_name,
        paired_at: now,
    };
    if cfg
        .allowed_user
        .as_ref()
        .is_some_and(|prev| prev.user_id != allowed.user_id)
    {
        cfg.forum = None;
    }
    cfg.allowed_user = Some(allowed.clone());
    cfg.rejected_user_ids.retain(|&id| id != allowed.user_id);
    save_config_at(&cfg_path, &cfg).map_err(|e| e.to_string())?;
    let _ = clear_pending_request_at(state_dir);
    Ok(allowed)
}

pub fn reject_pending(state_dir: &Path, request_id: &str) -> Result<(), String> {
    let cfg_path = state_dir.join("config.json");
    let mut cfg = load_config_at(&cfg_path).map_err(|e| e.to_string())?;
    let req = load_pending_request_at(state_dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No pending Telegram pairing request.".to_string())?;
    if req.request_id != request_id {
        return Err("Pairing request no longer matches; refresh and try again.".into());
    }
    if !cfg.rejected_user_ids.contains(&req.user_id) {
        cfg.rejected_user_ids.push(req.user_id);
        while cfg.rejected_user_ids.len() > MAX_REJECTED_USERS {
            cfg.rejected_user_ids.remove(0);
        }
    }
    save_config_at(&cfg_path, &cfg).map_err(|e| e.to_string())?;
    let _ = clear_pending_request_at(state_dir);
    Ok(())
}

pub fn revoke_user(state_dir: &Path) -> Result<(), String> {
    let cfg_path = state_dir.join("config.json");
    let mut cfg = load_config_at(&cfg_path).map_err(|e| e.to_string())?;
    cfg.allowed_user = None;
    cfg.forum = None;
    save_config_at(&cfg_path, &cfg).map_err(|e| e.to_string())?;
    let _ = clear_invite_at(state_dir);
    let _ = clear_pending_request_at(state_dir);
    Ok(())
}

pub fn clear_rejected(state_dir: &Path) -> Result<(), String> {
    let cfg_path = state_dir.join("config.json");
    let mut cfg = load_config_at(&cfg_path).map_err(|e| e.to_string())?;
    cfg.rejected_user_ids.clear();
    save_config_at(&cfg_path, &cfg).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn unlink_forum(state_dir: &Path) -> Result<(), String> {
    let cfg_path = state_dir.join("config.json");
    let mut cfg = load_config_at(&cfg_path).map_err(|e| e.to_string())?;
    cfg.forum = None;
    save_config_at(&cfg_path, &cfg).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn link_forum(
    api: &dyn TelegramApi,
    state_dir: &Path,
    cfg: &mut Config,
    runtime: &mut RuntimeState,
    chat_id: i64,
    bot_id: i64,
    now: u64,
) -> Result<String, String> {
    let chat = api.get_chat(chat_id).map_err(|e| e.to_string())?;
    if chat.chat_type != "supergroup" || chat.is_forum != Some(true) {
        return Err(
            "This chat is not a Forum Supergroup. Create a private group, enable Topics in Group Settings, and run /linkforum again."
                .into(),
        );
    }
    if chat.username.as_deref().is_some_and(|u| !u.trim().is_empty()) {
        return Err(
            "Refused: this Forum Supergroup has a public @username. Create a private supergroup without a public link."
                .into(),
        );
    }
    let member = api
        .get_chat_member(chat_id, bot_id)
        .map_err(|e| e.to_string())?;
    let is_admin = member.status == "administrator" || member.status == "creator";
    if !is_admin || member.can_manage_topics != Some(true) {
        return Err(
            "Promote this bot to Administrator with the \"Manage Topics\" permission first, then send /linkforum."
                .into(),
        );
    }
    let count = api.get_chat_member_count(chat_id).map_err(|e| e.to_string())?;
    if count > 2 {
        return Err(format!(
            "Refused: this group has {count} members. For privacy, Forum Topics only link to a private 2-member group (just you + your bot)."
        ));
    }
    let title = chat
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "SUPER DESKTOP Forum".to_string());
    let owner_id = cfg
        .allowed_user
        .as_ref()
        .map(|u| u.user_id)
        .ok_or_else(|| "Pair your Telegram account in Settings → Telegram first.".to_string())?;
    let cfg_path = state_dir.join("config.json");
    let mut fresh = load_config_at(&cfg_path).map_err(|e| e.to_string())?;
    if !fresh.should_run() || fresh.allowed_user.as_ref().map(|u| u.user_id) != Some(owner_id) {
        return Err("Telegram owner changed or was revoked; /linkforum aborted.".into());
    }
    fresh.forum = Some(ForumConfig {
        chat_id,
        title: title.clone(),
        linked_at: now,
    });
    runtime.topics.clear();
    save_config_at(&cfg_path, &fresh).map_err(|e| e.to_string())?;
    *cfg = fresh;
    save_runtime_at(&state_dir.join("runtime.json"), runtime).map_err(|e| e.to_string())?;
    Ok(title)
}

pub fn default_state_dir() -> Result<std::path::PathBuf, String> {
    config_path()
        .map_err(|e| e.to_string())?
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "Invalid Telegram state path".to_string())
}
