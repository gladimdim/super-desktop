//! Owner-control adapter for the same Telegram settings and pairing actions as GTK.
use crate::control::{Command, Reply, Request, TelegramAction};
use crate::telegram::{self, config, pairing};
use serde_json::{json, Value};

fn public_status(
    cfg: &config::Config,
    pending: Option<config::PendingRequest>,
    running: bool,
) -> Value {
    json!({"enabled":cfg.enabled,"tokenConfigured":!cfg.bot_token.is_empty(),"botUsername":cfg.bot_username,
                "workerRunning":running,"allowedUser":cfg.allowed_user,"pending":pending,
                "rejectedUsers":cfg.rejected_user_ids,"forum":cfg.forum,"permissions":cfg.permissions,
                "autoSyncTopics":cfg.auto_sync_topics,"archiveClosedTopics":cfg.archive_closed_topics,
                "mirrorAlertsToPrivate":cfg.mirror_alerts_to_private,"miniApp":cfg.mini_app})
}

pub(crate) fn execute(request: &Request) -> Reply {
    let id = &request.request_id;
    let dir = match pairing::default_state_dir() {
        Ok(dir) => dir,
        Err(e) => return Reply::failure(id, "unavailable", &e),
    };
    if let Command::TelegramRead { invitation } = &request.command {
        let read = || -> Result<Value, String> {
            let cfg = config::load_config().map_err(|e| e.to_string())?;
            if *invitation {
                let invite = config::load_invite()
                    .map_err(|e| e.to_string())?
                    .filter(|i| i.expires_at > config::unix_now());
                let link = invite.as_ref().and_then(|i| {
                    cfg.bot_username
                        .as_ref()
                        .map(|u| format!("https://t.me/{u}?start={}", i.secret))
                });
                return Ok(json!({"invite":invite,"link":link}));
            }
            let pending = config::load_pending_request()
                .map_err(|e| e.to_string())?
                .filter(|r| r.expires_at > config::unix_now());
            Ok(public_status(&cfg, pending, telegram::worker_running()))
        };
        return match read() {
            Ok(v) => Reply::success(id, v),
            Err(e) => Reply::failure(id, "unavailable", &e),
        };
    }
    let Command::TelegramEdit { action } = &request.command else {
        unreachable!()
    };
    crate::control_journal::execute(&crate::control_journal::root(), request, |_| {
        let apply = || -> Result<(), String> {
            match action {
                TelegramAction::Invite => {
                    pairing::start_invite(&dir)?;
                }
                TelegramAction::Approve { id, code } | TelegramAction::Reject { id, code } => {
                    let pending = config::load_pending_request()
                        .map_err(|e| e.to_string())?
                        .ok_or("No pending Telegram request.")?;
                    if pending.request_id != *id || pending.code != *code {
                        return Err("Pending request or comparison code changed.".into());
                    }
                    if matches!(action, TelegramAction::Approve { .. }) {
                        pairing::approve_pending(&dir, id)?;
                    } else {
                        pairing::reject_pending(&dir, id)?;
                    }
                }
                TelegramAction::Revoke => pairing::revoke_user(&dir)?,
                TelegramAction::ClearRejected => pairing::clear_rejected(&dir)?,
                TelegramAction::UnlinkForum => pairing::unlink_forum(&dir)?,
                TelegramAction::SyncTopics => {
                    config::request_topic_sync().map_err(|e| e.to_string())?
                }
                TelegramAction::SetupMiniApp { confirm } => {
                    if !confirm {
                        return Err("Confirm Tailscale Serve setup explicitly.".into());
                    }
                    let mut cfg = config::load_config().map_err(|e| e.to_string())?;
                    let port = cfg.mini_app.effective_port();
                    let url = telegram::webapp::configure_tailscale_serve(
                        port,
                        config::DEFAULT_MINI_APP_HTTPS_PORT,
                    )?;
                    cfg.mini_app.enabled = true;
                    cfg.mini_app.listen_port = port;
                    cfg.mini_app.public_url = url;
                    config::save_config(&cfg)
                        .map_err(|e| format!("Serve configuration may have changed. {e}"))?;
                }
                TelegramAction::Settings { key, value } => {
                    let mut cfg = config::load_config().map_err(|e| e.to_string())?;
                    let boolean = || value.as_bool().ok_or("This setting requires a boolean.");
                    match key.as_str() {
                        "botToken" => {
                            let token = value.as_str().ok_or("Supply a bot token string.")?.trim();
                            if token.is_empty() {
                                cfg.bot_token.clear();
                                cfg.bot_username = None;
                                cfg.bot_id = None;
                                cfg.allowed_user = None;
                                cfg.forum = None;
                                cfg.enabled = false;
                            } else {
                                let bot = telegram::verify_token(token)?;
                                cfg.bot_token = token.into();
                                cfg.bot_username = bot.username;
                                cfg.bot_id = Some(bot.id);
                                cfg.enabled = true;
                            }
                        }
                        "enabled" => {
                            cfg.enabled = boolean()?;
                            if cfg.enabled && cfg.bot_token.is_empty() {
                                return Err("Configure a verified bot token first.".into());
                            }
                        }
                        "autoSyncTopics" => cfg.auto_sync_topics = boolean()?,
                        "archiveClosedTopics" => cfg.archive_closed_topics = boolean()?,
                        "mirrorAlertsToPrivate" => cfg.mirror_alerts_to_private = boolean()?,
                        "miniApp.enabled" => cfg.mini_app.enabled = boolean()?,
                        "miniApp.publicUrl" => {
                            cfg.mini_app.public_url = value
                                .as_str()
                                .ok_or("Expected an HTTPS URL.")?
                                .trim()
                                .into();
                            if !cfg.mini_app.public_url.is_empty()
                                && cfg.mini_app.normalized_url().is_none()
                            {
                                return Err("Use an HTTPS URL.".into());
                            }
                        }
                        "miniApp.shortName" => {
                            cfg.mini_app.short_name = value
                                .as_str()
                                .ok_or("Expected a short name.")?
                                .trim()
                                .into();
                            if !cfg.mini_app.short_name.is_empty()
                                && cfg.mini_app.valid_short_name().is_none()
                            {
                                return Err("Invalid Mini App short name.".into());
                            }
                        }
                        key if key.starts_with("permissions.") => {
                            let mut permissions = json!(cfg.permissions);
                            let target = permissions
                                .get_mut(&key["permissions.".len()..])
                                .ok_or("Unknown permission.")?;
                            *target = json!(boolean()?);
                            cfg.permissions = serde_json::from_value(permissions)
                                .map_err(|_| "Invalid permission.")?;
                        }
                        _ => return Err("Unknown Telegram setting.".into()),
                    }
                    config::save_config(&cfg).map_err(|e| e.to_string())?;
                    if matches!(key.as_str(), "botToken" | "enabled") {
                        // Configuration is committed. A worker failure must not invite a blind retry.
                        if let Err(e) = telegram::stop_worker().and_then(|_| {
                            if cfg.should_run() {
                                telegram::start_worker()
                            } else {
                                Ok(())
                            }
                        }) {
                            return Err(format!(
                                "Configuration saved. Worker transition failed: {e}"
                            ));
                        }
                    }
                }
            }
            Ok(())
        };
        match apply() {
            Ok(()) => Reply::success(id, json!({"applied":true})),
            Err(e)
                if e.starts_with("Configuration saved.")
                    || matches!(action, TelegramAction::SetupMiniApp { confirm: true }) =>
            {
                let mut r = Reply::unknown(id);
                r.error.as_mut().unwrap().message = e;
                r
            }
            Err(e) => Reply::failure(id, "unavailable", &e),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_owner_status_reports_configuration_without_revealing_the_token() {
        let cfg = config::Config {
            bot_token: "private-bot-token".into(),
            ..Default::default()
        };
        let status = public_status(&cfg, None, true);
        assert_eq!(status["tokenConfigured"], true);
        assert_eq!(status["workerRunning"], true);
        assert!(!status.to_string().contains(&cfg.bot_token));
    }
}
