//! Detached `super-desktop telegram-bot` worker process: long polling + topic/notification sync.
use super::api::{ApiError, BotUser, TelegramApi, TelegramClient};
use super::config::{
    ensure_private_dir, file_stamp, load_config_at, load_runtime_at, lock_path,
    mask_token_in_text, save_config_at, save_runtime_at, state_dir, take_topic_sync_request_at,
    unix_now, Config, FileStamp, RuntimeState,
};
use super::desktop::{DesktopBackend, LiveDesktopBackend, ProviderUsageSummary};
use super::notifier::NotifierState;
use super::router::{Router, BOT_COMMANDS};
use super::topics;
use super::webapp::MiniAppServer;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

static SIGNAL_STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_: libc::c_int) {
    SIGNAL_STOP.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, handle_signal as *const () as libc::sighandler_t);
    }
}

pub fn verify_token(token: &str) -> Result<BotUser, String> {
    let trimmed = token.trim();
    if trimmed.is_empty() || !trimmed.contains(':') {
        return Err("Enter a valid @BotFather token (looks like 123456789:ABC...).".into());
    }
    let client = TelegramClient::new(trimmed).map_err(|e| mask_token_in_text(&e.to_string(), trimmed))?;
    client
        .get_me()
        .map_err(|e| mask_token_in_text(&e.to_string(), trimmed))
}

fn acquire_worker_lock(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent, "Telegram state").map_err(|e| e.to_string())?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("Cannot open Telegram worker lock: {e}"))?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err("Another telegram-bot worker is already running.".into());
    }
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    let _ = file.sync_all();
    Ok(file)
}

struct SharedState {
    cfg: Config,
    cfg_snapshot: Arc<RwLock<Config>>,
    runtime: RuntimeState,
    router: Router,
    notifier: NotifierState,
    config_stamp: Option<FileStamp>,
}

fn sync_menu_button(client: &TelegramClient, cfg: &Config) {
    let Some(owner) = cfg.allowed_user.as_ref() else {
        return;
    };
    let web_app_url = if cfg.mini_app.enabled {
        cfg.mini_app.normalized_url()
    } else {
        None
    };
    let arg = web_app_url.as_deref().map(|u| ("⚡ Open App", u));
    let _ = client.set_chat_menu_button(Some(owner.chat_id), arg);
}

fn reconcile_mini_app_server(
    server: &mut Option<MiniAppServer>,
    cfg: &Config,
    cfg_snapshot: &Arc<RwLock<Config>>,
) {
    let want_port = (cfg.should_run() && cfg.mini_app.enabled).then(|| cfg.mini_app.effective_port());
    if server.as_ref().map(|s| s.listen_port) == want_port {
        return;
    }
    if let Some(mut old) = server.take() {
        old.stop();
    }
    if let Some(port) = want_port {
        let snap = Arc::clone(cfg_snapshot);
        let cfg_reader: Arc<dyn Fn() -> Config + Send + Sync> = Arc::new(move || {
            snap.read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        });
        let backend: Arc<dyn DesktopBackend + Send + Sync> = Arc::new(LiveDesktopBackend);
        match MiniAppServer::spawn(port, cfg_reader, backend) {
            Ok(srv) => *server = Some(srv),
            Err(err) => eprintln!("SUPER DESKTOP telegram mini-app: {err}"),
        }
    }
}

/// Reload `config.json` immediately whenever Settings changed it on disk.
/// Returns `true` if `runtime` changed (e.g. cleared topics/active card on revocation).
fn reload_config_if_changed(
    guard: &mut SharedState,
    cfg_path: &Path,
    token: &str,
    client: &TelegramClient,
    stop: &AtomicBool,
) -> bool {
    let new_stamp = file_stamp(cfg_path);
    if new_stamp == guard.config_stamp {
        return false;
    }
    guard.config_stamp = new_stamp;
    let Ok(new_cfg) = load_config_at(cfg_path) else {
        return false;
    };
    if !new_cfg.should_run() || new_cfg.bot_token.trim() != token {
        stop.store(true, Ordering::SeqCst);
        return false;
    }
    let mut runtime_dirty = false;
    let prev_owner = guard.cfg.allowed_user.clone();
    let prev_user = prev_owner.as_ref().map(|u| u.user_id);
    let new_user = new_cfg.allowed_user.as_ref().map(|u| u.user_id);
    if prev_user.is_none() {
        if let Some(owner) = new_cfg.allowed_user.as_ref() {
            let _ = client.send_message(
                owner.chat_id,
                None,
                "✅ <b>Paired with SUPER DESKTOP!</b>\nSend /cards to see your terminals, /new to launch a card, or /help for commands.",
                None,
                None,
            );
        }
    }
    if guard.cfg.forum.is_some() && new_cfg.forum.is_none() {
        guard.runtime.topics.clear();
        guard.runtime.dashboard_message_id = None;
        guard.runtime.dashboard_pinned = false;
        guard.runtime.last_dashboard_card = None;
        guard.runtime.last_dashboard_edit_at = 0;
        guard.runtime.live_streams.clear();
        runtime_dirty = true;
    }
    if new_cfg.allowed_user.is_none() || prev_user != new_user {
        guard.runtime.active_card_id = None;
        guard.runtime.topics.clear();
        guard.runtime.message_card_map.clear();
        guard.runtime.dashboard_message_id = None;
        guard.runtime.dashboard_pinned = false;
        guard.runtime.last_dashboard_card = None;
        guard.runtime.last_dashboard_edit_at = 0;
        guard.runtime.live_streams.clear();
        runtime_dirty = true;
        if let Some(prev) = prev_owner {
            if new_cfg
                .allowed_user
                .as_ref()
                .is_none_or(|n| n.chat_id != prev.chat_id)
            {
                let _ = client.set_chat_menu_button(Some(prev.chat_id), None);
            }
        }
    }
    if guard.cfg.allowed_user != new_cfg.allowed_user || guard.cfg.mini_app != new_cfg.mini_app {
        sync_menu_button(client, &new_cfg);
    }
    guard.cfg = new_cfg.clone();
    *guard.cfg_snapshot.write().unwrap_or_else(|e| e.into_inner()) = new_cfg;
    runtime_dirty
}

fn persist_status(
    rt_path: &Path,
    runtime: &mut RuntimeState,
    state: &str,
    bot_username: Option<String>,
    last_error: Option<String>,
) {
    runtime.worker_status.state = state.to_string();
    runtime.worker_status.pid = (state == "running" || state == "starting").then(std::process::id);
    if bot_username.is_some() {
        runtime.worker_status.bot_username = bot_username;
    }
    runtime.worker_status.last_poll_at = unix_now();
    runtime.worker_status.last_error = last_error;
    let _ = save_runtime_at(rt_path, runtime);
}

pub fn serve() {
    install_signal_handlers();
    let dir: PathBuf = match state_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("SUPER DESKTOP telegram-bot: {e}");
            return;
        }
    };
    let cfg_path = dir.join("config.json");
    let rt_path = dir.join("runtime.json");
    let lk_path = lock_path().unwrap_or_else(|_| dir.join("worker.lock"));

    let _lock = match acquire_worker_lock(&lk_path) {
        Ok(lock) => lock,
        Err(msg) => {
            eprintln!("SUPER DESKTOP telegram-bot: {msg}");
            return;
        }
    };

    let mut cfg = match load_config_at(&cfg_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("SUPER DESKTOP telegram-bot: config unreadable ({e})");
            return;
        }
    };
    let mut runtime = match load_runtime_at(&rt_path) {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("SUPER DESKTOP telegram-bot: runtime state unreadable ({e})");
            return;
        }
    };

    if !cfg.should_run() {
        persist_status(&rt_path, &mut runtime, "stopped", cfg.bot_username.clone(), None);
        return;
    }

    let token = cfg.bot_token.trim().to_string();
    let client = match TelegramClient::new(&token) {
        Ok(c) => c,
        Err(e) => {
            let msg = mask_token_in_text(&e.to_string(), &token);
            persist_status(&rt_path, &mut runtime, "error", None, Some(msg));
            return;
        }
    };

    persist_status(&rt_path, &mut runtime, "starting", cfg.bot_username.clone(), None);

    let me = match client.get_me() {
        Ok(me) => me,
        Err(e) => {
            let msg = mask_token_in_text(&e.to_string(), &token);
            persist_status(&rt_path, &mut runtime, "error", None, Some(msg));
            return;
        }
    };

    if cfg.bot_id != Some(me.id) || cfg.bot_username != me.username {
        cfg.bot_id = Some(me.id);
        cfg.bot_username = me.username.clone();
        let _ = save_config_at(&cfg_path, &cfg);
    }
    let _ = client.set_my_commands(BOT_COMMANDS);
    sync_menu_button(&client, &cfg);

    let now = unix_now();
    let mut router = Router::new(me.id, me.username.clone(), now);
    let mut notifier = NotifierState::default();
    let backend = LiveDesktopBackend;
    let initial_cards = backend.list_cards();
    let mut cached_usage: Vec<ProviderUsageSummary> = backend.usage_summary();
    let mut last_usage_at = now;
    let _ = notifier.tick(&client, &backend, &cfg, &mut runtime, &initial_cards);
    if cfg.forum.is_some() {
        let _ = topics::sync_topics(&backend, &client, &cfg, &mut runtime, &initial_cards, now, false);
        let _ = topics::sync_dashboard(
            &client,
            &cfg,
            &mut runtime,
            &initial_cards,
            &cached_usage,
            now,
            false,
        );
    }
    router.bot_id = me.id;
    persist_status(&rt_path, &mut runtime, "running", cfg.bot_username.clone(), None);

    let stop = Arc::new(AtomicBool::new(false));
    let cfg_snapshot = Arc::new(RwLock::new(cfg.clone()));
    let shared = Arc::new(Mutex::new(SharedState {
        cfg: cfg.clone(),
        cfg_snapshot: Arc::clone(&cfg_snapshot),
        runtime,
        router,
        notifier,
        config_stamp: file_stamp(&cfg_path),
    }));
    let mut mini_app_server: Option<MiniAppServer> = None;
    reconcile_mini_app_server(&mut mini_app_server, &cfg, &cfg_snapshot);

    let poller_client = match TelegramClient::new(&token) {
        Ok(c) => c,
        Err(_) => return,
    };
    let poller_stop = Arc::clone(&stop);
    let poller_shared = Arc::clone(&shared);
    let poller_dir = dir.clone();
    let poller_cfg_path = cfg_path.clone();
    let poller_token = token.clone();
    let poller_handle = std::thread::Builder::new()
        .name("tg-poller".into())
        .spawn(move || {
            let backend = LiveDesktopBackend;
            let mut backoff_secs = 2u64;
            while !poller_stop.load(Ordering::SeqCst) && !SIGNAL_STOP.load(Ordering::SeqCst) {
                let offset = {
                    let guard = poller_shared.lock().unwrap();
                    guard.runtime.update_offset
                };
                match poller_client.get_updates(offset, 25) {
                    Ok(updates) => {
                        backoff_secs = 2;
                        let now = unix_now();
                        let mut guard = poller_shared.lock().unwrap();
                        if !updates.is_empty() {
                            reload_config_if_changed(
                                &mut guard,
                                &poller_cfg_path,
                                &poller_token,
                                &poller_client,
                                &poller_stop,
                            );
                            if poller_stop.load(Ordering::SeqCst) {
                                break;
                            }
                        }
                        if guard.runtime.worker_status.state != "running"
                            || guard.runtime.worker_status.last_error.is_some()
                        {
                            guard.runtime.worker_status.state = "running".to_string();
                            guard.runtime.worker_status.last_error = None;
                        }
                        guard.runtime.worker_status.last_poll_at = now;
                        for update in updates {
                            let next_offset = update.update_id + 1;
                            if next_offset > guard.runtime.update_offset {
                                guard.runtime.update_offset = next_offset;
                            }
                            let mut cfg_copy = guard.cfg.clone();
                            let mut rt_copy = guard.runtime.clone();
                            guard.router.handle_update(
                                &poller_client,
                                &backend,
                                &poller_dir,
                                &mut cfg_copy,
                                &mut rt_copy,
                                &update,
                                now,
                            );
                            guard.cfg = cfg_copy.clone();
                            *guard.cfg_snapshot.write().unwrap_or_else(|e| e.into_inner()) =
                                cfg_copy;
                            guard.runtime = rt_copy;
                        }
                        let _ = save_runtime_at(&poller_dir.join("runtime.json"), &guard.runtime);
                    }
                    Err(ApiError::Unauthorized) => {
                        let mut guard = poller_shared.lock().unwrap();
                        guard.runtime.worker_status.state = "error".to_string();
                        guard.runtime.worker_status.last_error =
                            Some(ApiError::Unauthorized.to_string());
                        let _ = save_runtime_at(&poller_dir.join("runtime.json"), &guard.runtime);
                        poller_stop.store(true, Ordering::SeqCst);
                        break;
                    }
                    Err(ApiError::Conflict) => {
                        let mut guard = poller_shared.lock().unwrap();
                        guard.runtime.worker_status.state = "error".to_string();
                        guard.runtime.worker_status.last_error =
                            Some(ApiError::Conflict.to_string());
                        let _ = save_runtime_at(&poller_dir.join("runtime.json"), &guard.runtime);
                        drop(guard);
                        sleep_interruptible(15, &poller_stop);
                    }
                    Err(ApiError::RateLimited { retry_after }) => {
                        let wait = retry_after.clamp(1, 60);
                        sleep_interruptible(wait, &poller_stop);
                    }
                    Err(err) => {
                        let clean = mask_token_in_text(&err.to_string(), &poller_token);
                        {
                            let mut guard = poller_shared.lock().unwrap();
                            guard.runtime.worker_status.last_error = Some(clean);
                            guard.runtime.worker_status.last_poll_at = unix_now();
                            let _ =
                                save_runtime_at(&poller_dir.join("runtime.json"), &guard.runtime);
                        }
                        sleep_interruptible(backoff_secs, &poller_stop);
                        backoff_secs = (backoff_secs * 2).min(30);
                    }
                }
            }
        });

    let sync_backend = LiveDesktopBackend;
    let mut last_typing: HashMap<String, u64> = HashMap::new();
    let mut last_forum_audit_at = now;
    while !stop.load(Ordering::SeqCst) && !SIGNAL_STOP.load(Ordering::SeqCst) {
        sleep_interruptible(2, &stop);
        if stop.load(Ordering::SeqCst) || SIGNAL_STOP.load(Ordering::SeqCst) {
            break;
        }

        let sync_requested = take_topic_sync_request_at(&dir);
        let cards = sync_backend.list_cards();
        let now = unix_now();
        if sync_requested || now.saturating_sub(last_usage_at) >= 10 {
            cached_usage = sync_backend.usage_summary();
            last_usage_at = now;
        }

        // Periodically verify linked Forum group still satisfies the 2-member + private supergroup guard.
        let audit_target = if now.saturating_sub(last_forum_audit_at) >= 120 {
            let guard = shared.lock().unwrap();
            guard
                .cfg
                .forum
                .clone()
                .map(|f| (f, guard.cfg.allowed_user.clone()))
        } else {
            None
        };
        if let Some((forum_cfg, owner_opt)) = audit_target {
            last_forum_audit_at = now;
            let mut violation: Option<&str> = None;
            if let Ok(count) = client.get_chat_member_count(forum_cfg.chat_id) {
                if count > 2 {
                    violation = Some("it now has more than 2 members (only you and your bot are allowed)");
                }
            }
            if violation.is_none() {
                if let Ok(info) = client.get_chat(forum_cfg.chat_id) {
                    if info.username.as_deref().is_some_and(|u| !u.trim().is_empty()) {
                        violation = Some("a public @username link was enabled on the group");
                    } else if info.is_forum != Some(true) {
                        violation = Some("Forum Topics were disabled on the group");
                    }
                }
            }
            if let Some(reason) = violation {
                if let Ok(mut disk_cfg) = load_config_at(&cfg_path) {
                    let same_owner = disk_cfg.allowed_user.as_ref().map(|u| u.user_id)
                        == owner_opt.as_ref().map(|o| o.user_id);
                    let same_forum = disk_cfg.forum.as_ref().map(|f| f.chat_id) == Some(forum_cfg.chat_id);
                    if same_owner && same_forum {
                        disk_cfg.forum = None;
                        if save_config_at(&cfg_path, &disk_cfg).is_ok() {
                            let mut guard = shared.lock().unwrap();
                            guard.cfg = disk_cfg.clone();
                            *guard.cfg_snapshot.write().unwrap_or_else(|e| e.into_inner()) =
                                disk_cfg;
                            guard.config_stamp = file_stamp(&cfg_path);
                            guard.runtime.topics.clear();
                            guard.runtime.dashboard_message_id = None;
                            guard.runtime.dashboard_pinned = false;
                            guard.runtime.last_dashboard_card = None;
                            guard.runtime.last_dashboard_edit_at = 0;
                            guard.runtime.live_streams.clear();
                            let _ = save_runtime_at(&rt_path, &guard.runtime);
                            if let Some(owner) = owner_opt {
                                let _ = client.send_message(
                                    owner.chat_id,
                                    None,
                                    &format!("⚠️ Linked Forum group was automatically unlinked because {reason}."),
                                    None,
                                    None,
                                );
                            }
                        }
                    }
                }
            }
        }

        let mut guard = shared.lock().unwrap();
        let mut runtime_dirty = reload_config_if_changed(&mut guard, &cfg_path, &token, &client, &stop);
        let latest_cfg = guard.cfg.clone();
        if stop.load(Ordering::SeqCst) {
            break;
        }
        reconcile_mini_app_server(&mut mini_app_server, &latest_cfg, &cfg_snapshot);

        if guard.cfg.forum.is_some() {
            let cfg_copy = guard.cfg.clone();
            let mut rt_copy = guard.runtime.clone();
            if topics::sync_topics(&sync_backend, &client, &cfg_copy, &mut rt_copy, &cards, now, sync_requested) {
                guard.runtime = rt_copy;
                runtime_dirty = true;
            }
            let mut rt_copy = guard.runtime.clone();
            if topics::sync_dashboard(
                &client,
                &cfg_copy,
                &mut rt_copy,
                &cards,
                &cached_usage,
                now,
                sync_requested,
            ) {
                guard.runtime = rt_copy;
                runtime_dirty = true;
            }
            topics::tick_typing_actions(
                &client,
                &cfg_copy,
                &guard.runtime,
                &cards,
                now,
                &mut last_typing,
            );
        }

        if !guard.runtime.live_streams.is_empty() {
            let cfg_copy = guard.cfg.clone();
            let mut rt_copy = guard.runtime.clone();
            if topics::tick_live_streams(&client, &sync_backend, &cfg_copy, &mut rt_copy, &cards, now)
            {
                guard.runtime = rt_copy;
                runtime_dirty = true;
            }
        }

        {
            let cfg_copy = guard.cfg.clone();
            let mut rt_copy = guard.runtime.clone();
            let mut notifier_copy = std::mem::take(&mut guard.notifier);
            let notified = notifier_copy.tick(&client, &sync_backend, &cfg_copy, &mut rt_copy, &cards);
            guard.notifier = notifier_copy;
            if notified {
                guard.runtime = rt_copy;
                runtime_dirty = true;
            }
        }

        if runtime_dirty {
            let _ = save_runtime_at(&rt_path, &guard.runtime);
        }
    }

    if let Some(mut srv) = mini_app_server.take() {
        srv.stop();
    }
    stop.store(true, Ordering::SeqCst);
    if let Ok(handle) = poller_handle {
        let _ = handle.join();
    }
    if let Ok(mut guard) = shared.lock() {
        let username = guard.cfg.bot_username.clone();
        persist_status(&rt_path, &mut guard.runtime, "stopped", username, None);
    };
}

fn sleep_interruptible(seconds: u64, stop: &AtomicBool) {
    for _ in 0..(seconds * 4) {
        if stop.load(Ordering::SeqCst) || SIGNAL_STOP.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
