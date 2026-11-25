//! Settings → Telegram page: BYOB bot token, desktop pairing approval, Forum Topics, and permissions.
use crate::telegram::api::BotUser;
use crate::telegram::config::{
    self, unix_now, Config, RuntimeState,
};
use crate::telegram::pairing;
use crate::telegram::webapp;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, DrawingArea, Entry, Label, Orientation, Switch};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
struct Hooks {
    load_config: Arc<dyn Fn() -> std::io::Result<Config> + Send + Sync>,
    save_config: Arc<dyn Fn(&Config) -> std::io::Result<()> + Send + Sync>,
    load_runtime: Arc<dyn Fn() -> std::io::Result<RuntimeState> + Send + Sync>,
    load_invite: Arc<dyn Fn() -> std::io::Result<Option<config::PendingInvite>> + Send + Sync>,
    load_pending_request:
        Arc<dyn Fn() -> std::io::Result<Option<config::PendingRequest>> + Send + Sync>,
    request_sync: Arc<dyn Fn() -> std::io::Result<()> + Send + Sync>,
    verify_token: Arc<dyn Fn(&str) -> Result<BotUser, String> + Send + Sync>,
    start_worker: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    stop_worker: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    worker_running: Arc<dyn Fn() -> bool + Send + Sync>,
    start_invite:
        Arc<dyn Fn() -> Result<(config::PendingInvite, Option<String>), String> + Send + Sync>,
    approve_pending: Arc<dyn Fn(&str) -> Result<config::AllowedUser, String> + Send + Sync>,
    reject_pending: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
    revoke_user: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    clear_rejected: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    unlink_forum: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    detect_tailscale: Arc<dyn Fn(u16) -> webapp::TailscaleInfo + Send + Sync>,
    setup_tailscale: Arc<dyn Fn(u16, u16) -> Result<String, String> + Send + Sync>,
}

impl Hooks {
    fn live() -> Self {
        Self {
            load_config: Arc::new(config::load_config),
            save_config: Arc::new(config::save_config),
            load_runtime: Arc::new(config::load_runtime),
            load_invite: Arc::new(config::load_invite),
            load_pending_request: Arc::new(config::load_pending_request),
            request_sync: Arc::new(config::request_topic_sync),
            verify_token: Arc::new(crate::telegram::verify_token),
            start_worker: Arc::new(crate::telegram::start_worker),
            stop_worker: Arc::new(crate::telegram::stop_worker),
            worker_running: Arc::new(crate::telegram::worker_running),
            start_invite: Arc::new(pairing::start_invite_default),
            approve_pending: Arc::new(|id| {
                let dir = pairing::default_state_dir()?;
                pairing::approve_pending(&dir, id)
            }),
            reject_pending: Arc::new(|id| {
                let dir = pairing::default_state_dir()?;
                pairing::reject_pending(&dir, id)
            }),
            revoke_user: Arc::new(|| {
                let dir = pairing::default_state_dir()?;
                pairing::revoke_user(&dir)
            }),
            clear_rejected: Arc::new(|| {
                let dir = pairing::default_state_dir()?;
                pairing::clear_rejected(&dir)
            }),
            unlink_forum: Arc::new(|| {
                let dir = pairing::default_state_dir()?;
                pairing::unlink_forum(&dir)
            }),
            detect_tailscale: Arc::new(webapp::detect_tailscale_info),
            setup_tailscale: Arc::new(webapp::configure_tailscale_serve),
        }
    }
}

pub fn home_status_summary() -> &'static str {
    let Ok(cfg) = config::load_config() else {
        return "Err";
    };
    if !cfg.should_run() {
        return "Off";
    }
    if let Ok(Some(req)) = config::load_pending_request() {
        if req.expires_at >= unix_now() {
            return "Review";
        }
    }
    if let Ok(rt) = config::load_runtime() {
        if rt.worker_status.state == "error" {
            return "Error";
        }
    }
    if crate::telegram::worker_running() {
        if cfg.allowed_user.is_some() {
            "Online"
        } else {
            "Pair"
        }
    } else {
        "Starting"
    }
}

fn text(body: &Box, value: &str) -> Label {
    let label = Label::new(Some(value));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_max_width_chars(62);
    body.append(&label);
    label
}

fn switch_row(body: &Box, name: &str, title: &str, description: &str) -> Switch {
    let row = Box::new(Orientation::Horizontal, 12);
    let words = Box::new(Orientation::Vertical, 4);
    words.set_hexpand(true);
    text(&words, title).add_css_class("settings-entry-title");
    text(&words, description).add_css_class("launcher-note");
    let toggle = Switch::new();
    toggle.set_widget_name(name);
    toggle.set_valign(Align::Center);
    toggle.update_property(&[gtk4::accessible::Property::Label(title)]);
    row.append(&words);
    row.append(&toggle);
    body.append(&row);
    toggle
}

pub fn build(root: &Box) -> Rc<dyn Fn()> {
    build_with_hooks(root, Hooks::live())
}

fn build_with_hooks(root: &Box, hooks: Hooks) -> Rc<dyn Fn()> {
    let painting = Rc::new(Cell::new(false));

    // --- Section 1: Bot setup (BYOB) ---
    let (_, bot_body) = crate::launcher_settings::section_card(root, "", "Bot setup (BYOB)");
    text(
        &bot_body,
        "Create your own bot in Telegram via @BotFather (/newbot) and paste its token here. Your PC connects directly to api.telegram.org — no relay server ever sees your prompts or terminal output.",
    );

    let bot_status_row = Box::new(Orientation::Horizontal, 8);
    let bot_chip = crate::launcher_settings::chip("Stopped");
    bot_chip.set_widget_name("telegram-status-chip");
    let bot_status_label = Label::new(Some("Not configured"));
    bot_status_label.add_css_class("launcher-note");
    bot_status_label.set_xalign(0.0);
    bot_status_label.set_wrap(true);
    bot_status_label.set_hexpand(true);
    bot_status_row.append(&bot_chip);
    bot_status_row.append(&bot_status_label);
    bot_body.append(&bot_status_row);

    let token_row = Box::new(Orientation::Horizontal, 8);
    let token_entry = Entry::new();
    token_entry.set_widget_name("telegram-token-entry");
    token_entry.add_css_class("ws-entry");
    token_entry.set_visibility(false);
    token_entry.set_hexpand(true);
    token_entry.set_placeholder_text(Some("Paste @BotFather token (e.g. 123456789:ABC...)"));
    token_row.append(&token_entry);

    let btn_save_token = Button::with_label("Save & verify");
    btn_save_token.set_widget_name("telegram-save-token");
    btn_save_token.add_css_class("launcher-btn");
    btn_save_token.add_css_class("launcher-btn-primary");
    token_row.append(&btn_save_token);

    let btn_clear_token = Button::with_label("Clear");
    btn_clear_token.set_widget_name("telegram-clear-token");
    btn_clear_token.add_css_class("launcher-btn");
    token_row.append(&btn_clear_token);
    bot_body.append(&token_row);

    let token_feedback = text(&bot_body, "");
    token_feedback.add_css_class("launcher-note");
    token_feedback.set_visible(false);

    let enable_toggle = switch_row(
        &bot_body,
        "telegram-enabled",
        "Enable Telegram bot",
        "Run the background Telegram worker on this PC while SUPER DESKTOP is running (desktop or headless).",
    );

    // --- Section 2: Account pairing ---
    let (_, pair_body) = crate::launcher_settings::section_card(root, "", "Account pairing");
    text(
        &pair_body,
        "Only one Telegram account can control this PC. Every pairing link is single-use and requires your approval here.",
    );

    let owner_label = text(&pair_body, "No Telegram account paired yet.");
    owner_label.add_css_class("settings-entry-title");

    let pair_actions = Box::new(Orientation::Horizontal, 8);
    let btn_pair = Button::with_label("Pair Telegram account");
    btn_pair.set_widget_name("telegram-pair-btn");
    btn_pair.add_css_class("launcher-btn");
    btn_pair.add_css_class("launcher-btn-primary");
    pair_actions.append(&btn_pair);

    let btn_revoke = Button::with_label("Revoke access");
    btn_revoke.set_widget_name("telegram-revoke-btn");
    btn_revoke.add_css_class("launcher-btn");
    btn_revoke.add_css_class("launcher-btn-danger");
    pair_actions.append(&btn_revoke);

    let btn_clear_rejected = Button::with_label("Clear rejected");
    btn_clear_rejected.set_widget_name("telegram-clear-rejected");
    btn_clear_rejected.add_css_class("launcher-btn");
    pair_actions.append(&btn_clear_rejected);
    pair_body.append(&pair_actions);

    let invite_box = Box::new(Orientation::Vertical, 6);
    invite_box.set_visible(false);
    let qr_code: Rc<RefCell<Option<qrcode::QrCode>>> = Rc::new(RefCell::new(None));
    let qr = DrawingArea::new();
    qr.set_content_width(180);
    qr.set_content_height(180);
    qr.set_halign(Align::Start);
    qr.add_css_class("invite-qr");
    qr.set_draw_func({
        let qr_code = Rc::clone(&qr_code);
        move |_, cr, width, height| {
            let Some(code) = qr_code.borrow().clone() else {
                return;
            };
            let size = code.width();
            let unit = (width.min(height) as f64 / (size + 8) as f64).floor().max(1.0);
            let offset_x = (width as f64 - (size + 8) as f64 * unit) / 2.0;
            let offset_y = (height as f64 - (size + 8) as f64 * unit) / 2.0;
            cr.set_source_rgb(1.0, 1.0, 1.0);
            let _ = cr.paint();
            cr.set_source_rgb(0.0, 0.0, 0.0);
            for y in 0..size {
                for x in 0..size {
                    if code[(x, y)] == qrcode::Color::Dark {
                        cr.rectangle(
                            offset_x + (x + 4) as f64 * unit,
                            offset_y + (y + 4) as f64 * unit,
                            unit,
                            unit,
                        );
                    }
                }
            }
            let _ = cr.fill();
        }
    });
    invite_box.append(&qr);

    let invite_link_row = Box::new(Orientation::Horizontal, 8);
    let invite_link_entry = Entry::new();
    invite_link_entry.add_css_class("ws-entry");
    invite_link_entry.set_editable(false);
    invite_link_entry.set_hexpand(true);
    let btn_copy_invite = Button::with_label("Copy link");
    btn_copy_invite.set_widget_name("telegram-copy-invite");
    btn_copy_invite.add_css_class("launcher-btn");
    btn_copy_invite.add_css_class("launcher-btn-primary");
    invite_link_row.append(&invite_link_entry);
    invite_link_row.append(&btn_copy_invite);
    invite_box.append(&invite_link_row);

    btn_copy_invite.connect_clicked({
        let invite_link_entry = invite_link_entry.clone();
        let token_feedback = token_feedback.clone();
        move |button| {
            button.clipboard().set_text(&invite_link_entry.text());
            token_feedback.set_text("Pairing link copied to clipboard.");
            token_feedback.set_visible(true);
        }
    });

    let invite_hint = text(
        &invite_box,
        "Open the link in Telegram (or send /start <secret> to your bot), then approve the matching code below.",
    );
    invite_hint.add_css_class("launcher-hint");
    pair_body.append(&invite_box);

    let pending_box = Box::new(Orientation::Vertical, 6);
    pending_box.set_widget_name("telegram-pending-box");
    pending_box.set_visible(false);
    let pending_info = text(&pending_box, "");
    pending_info.add_css_class("settings-entry-title");
    let pending_code_label = text(&pending_box, "");
    pending_code_label.add_css_class("monospace");
    let pending_btns = Box::new(Orientation::Horizontal, 8);
    let btn_approve = Button::with_label("Approve");
    btn_approve.set_widget_name("telegram-approve-btn");
    btn_approve.add_css_class("launcher-btn");
    btn_approve.add_css_class("launcher-btn-primary");
    let btn_reject = Button::with_label("Reject");
    btn_reject.set_widget_name("telegram-reject-btn");
    btn_reject.add_css_class("launcher-btn");
    btn_reject.add_css_class("launcher-btn-danger");
    pending_btns.append(&btn_approve);
    pending_btns.append(&btn_reject);
    pending_box.append(&pending_btns);
    pair_body.append(&pending_box);

    // --- Section 3: Forum Topics ---
    let (_, forum_body) = crate::launcher_settings::section_card(root, "", "Forum Topics");
    text(
        &forum_body,
        "Optional 1-card-1-topic mode: create a private group with just you and your bot, enable Topics in Group Settings, promote the bot to Admin (Manage Topics and Delete Messages), and send /linkforum inside General.",
    );
    let forum_status = text(&forum_body, "Not linked to a Forum group.");
    forum_status.add_css_class("launcher-note");

    let forum_actions = Box::new(Orientation::Horizontal, 8);
    let btn_sync_topics = Button::with_label("Sync topics now");
    btn_sync_topics.set_widget_name("telegram-sync-topics");
    btn_sync_topics.add_css_class("launcher-btn");
    let btn_unlink_forum = Button::with_label("Unlink forum");
    btn_unlink_forum.set_widget_name("telegram-unlink-forum");
    btn_unlink_forum.add_css_class("launcher-btn");
    forum_actions.append(&btn_sync_topics);
    forum_actions.append(&btn_unlink_forum);
    forum_body.append(&forum_actions);

    let auto_topics_toggle = switch_row(
        &forum_body,
        "telegram-auto-topics",
        "Auto-create a topic for each card",
        "Automatically open a Forum Topic when a terminal card starts.",
    );
    let archive_topics_toggle = switch_row(
        &forum_body,
        "telegram-archive-topics",
        "Delete topic when a card closes",
        "Requires Delete Messages in the group. If Telegram denies deletion, the bot archives the topic and retries.",
    );

    // --- Section 3b: Mini Web App (Tailscale) ---
    let (_, miniapp_body) = crate::launcher_settings::section_card(root, "", "Mini Web App (Tailscale)");
    text(
        &miniapp_body,
        "Open an Android-like live terminal, weekly usage bars, Quick Keys, and Files inside Telegram using your own Tailscale Serve HTTPS URL (stays inside your private tailnet).",
    );
    let miniapp_status = text(&miniapp_body, "Tailscale status: checking…");
    miniapp_status.add_css_class("launcher-note");
    let miniapp_toggle = switch_row(
        &miniapp_body,
        "telegram-miniapp-enabled",
        "Enable Telegram Mini Web App",
        "Serve the embedded Android-like web UI on 127.0.0.1 for private Tailscale Serve HTTPS access.",
    );

    let url_row = Box::new(Orientation::Horizontal, 8);
    let miniapp_url_entry = Entry::new();
    miniapp_url_entry.set_widget_name("telegram-miniapp-url");
    miniapp_url_entry.add_css_class("ws-entry");
    miniapp_url_entry.set_hexpand(true);
    miniapp_url_entry.set_placeholder_text(Some("https://<pc>.<tailnet>.ts.net:8443"));
    url_row.append(&miniapp_url_entry);
    miniapp_body.append(&url_row);

    let short_row = Box::new(Orientation::Horizontal, 8);
    let miniapp_short_entry = Entry::new();
    miniapp_short_entry.set_widget_name("telegram-miniapp-shortname");
    miniapp_short_entry.add_css_class("ws-entry");
    miniapp_short_entry.set_hexpand(true);
    miniapp_short_entry.set_placeholder_text(Some("Optional @BotFather /newapp short name (for Forum Topic buttons)"));
    short_row.append(&miniapp_short_entry);
    miniapp_body.append(&short_row);

    let miniapp_actions = Box::new(Orientation::Horizontal, 8);
    let btn_miniapp_save = Button::with_label("Save Mini App");
    btn_miniapp_save.set_widget_name("telegram-miniapp-save");
    btn_miniapp_save.add_css_class("launcher-btn");
    btn_miniapp_save.add_css_class("launcher-btn-primary");
    miniapp_actions.append(&btn_miniapp_save);

    let btn_miniapp_ts = Button::with_label("Auto-setup Tailscale Serve");
    btn_miniapp_ts.set_widget_name("telegram-miniapp-tailscale");
    btn_miniapp_ts.add_css_class("launcher-btn");
    miniapp_actions.append(&btn_miniapp_ts);

    let btn_miniapp_prompt = Button::with_label("Copy AI setup prompt");
    btn_miniapp_prompt.set_widget_name("telegram-miniapp-copy-prompt");
    btn_miniapp_prompt.add_css_class("launcher-btn");
    miniapp_actions.append(&btn_miniapp_prompt);
    miniapp_body.append(&miniapp_actions);

    let miniapp_feedback = text(&miniapp_body, "");
    miniapp_feedback.add_css_class("launcher-note");
    miniapp_feedback.set_visible(false);

    // --- Section 4: Permissions ---
    let (_, perm_body) = crate::launcher_settings::section_card(root, "", "Permissions");
    text(
        &perm_body,
        "Choose what your paired Telegram account is allowed to inspect or trigger. Note: Telegram Bot API messages are processed by Telegram servers (not end-to-end encrypted).",
    );
    let perm_specs = [
        ("telegram-notifications", "Notifications", "Push alerts when an agent finishes, waits for input, or hits an error."),
        ("telegram-read-output", "Read terminal output", "Allow /screen and /history screen captures. Output may contain sensitive text."),
        ("telegram-prompts", "Submit prompts", "Allow sending text prompts to AI harness cards."),
        ("telegram-controls", "Control keys", "Allow Yes/No/1/2/Enter/Esc/Ctrl-C actions."),
        ("telegram-shell-input", "Allow shell input", "Allow typing text or control keys into plain Shell cards (higher risk)."),
        ("telegram-launch", "Launch new cards", "Allow /new to start harnesses in remembered project folders."),
        ("telegram-close", "Close cards", "Allow closing cards from Telegram after confirmation."),
        ("telegram-attachments", "Receive attachments", "Allow photos, files, and voice clips to be staged privately for harnesses."),
        ("telegram-redact", "Redact common secrets", "Best-effort masking of API keys, tokens, and PEM blocks before sending output."),
    ];
    let mut perm_switches = Vec::with_capacity(perm_specs.len());
    for (name, title, desc) in perm_specs {
        perm_switches.push(switch_row(&perm_body, name, title, desc));
    }

    let pending_req_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let token_loaded = Rc::new(Cell::new(false));
    let miniapp_loaded = Rc::new(Cell::new(false));
    let verifying = Rc::new(Cell::new(false));
    let cached_ts: Rc<RefCell<Option<webapp::TailscaleInfo>>> = Rc::new(RefCell::new(None));

    let paint: Rc<
        dyn Fn(
            &Config,
            &RuntimeState,
            Option<&config::PendingInvite>,
            Option<&config::PendingRequest>,
            bool,
        ),
    > = Rc::new({
        let painting = Rc::clone(&painting);
        let token_loaded = Rc::clone(&token_loaded);
        let miniapp_loaded = Rc::clone(&miniapp_loaded);
        let verifying = Rc::clone(&verifying);
        let cached_ts = Rc::clone(&cached_ts);
        let hooks = hooks.clone();
        let bot_chip = bot_chip.clone();
        let bot_status_label = bot_status_label.clone();
        let token_entry = token_entry.clone();
        let enable_toggle = enable_toggle.clone();
        let owner_label = owner_label.clone();
        let btn_pair = btn_pair.clone();
        let btn_revoke = btn_revoke.clone();
        let btn_clear_rejected = btn_clear_rejected.clone();
        let invite_box = invite_box.clone();
        let invite_link_entry = invite_link_entry.clone();
        let qr_code = Rc::clone(&qr_code);
        let qr = qr.clone();
        let pending_box = pending_box.clone();
        let pending_info = pending_info.clone();
        let pending_code_label = pending_code_label.clone();
        let pending_req_id = Rc::clone(&pending_req_id);
        let forum_status = forum_status.clone();
        let btn_sync_topics = btn_sync_topics.clone();
        let btn_unlink_forum = btn_unlink_forum.clone();
        let auto_topics_toggle = auto_topics_toggle.clone();
        let archive_topics_toggle = archive_topics_toggle.clone();
        let miniapp_status = miniapp_status.clone();
        let miniapp_toggle = miniapp_toggle.clone();
        let miniapp_url_entry = miniapp_url_entry.clone();
        let miniapp_short_entry = miniapp_short_entry.clone();
        let perm_switches = perm_switches.clone();
        move |cfg: &Config,
              rt: &RuntimeState,
              invite: Option<&config::PendingInvite>,
              pending_req: Option<&config::PendingRequest>,
              alive: bool| {
            painting.set(true);
            let now = unix_now();

            if !token_loaded.get() && !verifying.get() {
                token_entry.set_text(&cfg.bot_token);
                token_loaded.set(true);
            }
            enable_toggle.set_active(cfg.enabled);
            enable_toggle.set_sensitive(!cfg.bot_token.trim().is_empty());

            let (chip_txt, status_txt) = if !cfg.should_run() {
                ("Stopped", "Bot is disabled or has no token.".to_string())
            } else if rt.worker_status.state == "error" {
                (
                    "Error",
                    rt.worker_status
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "Telegram worker error.".to_string()),
                )
            } else if alive {
                let handle = cfg
                    .bot_username
                    .as_deref()
                    .map(|u| format!("@{u}"))
                    .unwrap_or_else(|| "bot".to_string());
                ("Running", format!("Polling api.telegram.org as {handle}."))
            } else {
                ("Starting", "Waiting for Telegram worker…".to_string())
            };
            bot_chip.set_text(chip_txt);
            bot_status_label.set_text(&status_txt);

            if let Some(owner) = cfg.allowed_user.as_ref() {
                let handle = owner
                    .username
                    .as_deref()
                    .map(|u| format!(" (@{u})"))
                    .unwrap_or_default();
                owner_label.set_text(&format!(
                    "Paired with {}{} · ID {}",
                    owner.first_name, handle, owner.user_id
                ));
                btn_revoke.set_visible(true);
                btn_pair.set_label("Re-pair account");
            } else {
                owner_label.set_text("No Telegram account paired yet.");
                btn_revoke.set_visible(false);
                btn_pair.set_label("Pair Telegram account");
            }
            btn_pair.set_sensitive(cfg.should_run());
            btn_clear_rejected.set_visible(!cfg.rejected_user_ids.is_empty());
            btn_clear_rejected.set_label(&format!(
                "Clear rejected ({})",
                cfg.rejected_user_ids.len()
            ));

            if let Some(invite) = invite.filter(|i| i.expires_at >= now) {
                let link = cfg
                    .bot_username
                    .as_deref()
                    .filter(|u| !u.is_empty())
                    .map(|u| format!("https://t.me/{u}?start={}", invite.secret))
                    .unwrap_or_else(|| format!("/start {}", invite.secret));
                invite_link_entry.set_text(&link);
                *qr_code.borrow_mut() = qrcode::QrCode::new(link.as_bytes()).ok();
                qr.set_visible(qr_code.borrow().is_some());
                qr.queue_draw();
                invite_box.set_visible(true);
            } else {
                invite_box.set_visible(false);
            }

            if let Some(req) = pending_req.filter(|r| r.expires_at >= now) {
                let handle = req
                    .username
                    .as_deref()
                    .map(|u| format!(" (@{u})"))
                    .unwrap_or_default();
                pending_info.set_text(&format!(
                    "Pairing request from {}{} · ID {}",
                    req.first_name, handle, req.user_id
                ));
                pending_code_label.set_text(&format!(
                    "Comparison code: {}",
                    pairing::format_code(&req.code)
                ));
                *pending_req_id.borrow_mut() = Some(req.request_id.clone());
                pending_box.set_visible(true);
            } else {
                *pending_req_id.borrow_mut() = None;
                pending_box.set_visible(false);
            }

            if let Some(forum) = cfg.forum.as_ref() {
                let active_topics = rt
                    .topics
                    .iter()
                    .filter(|t| !t.closed && !t.deleted_in_telegram)
                    .count();
                forum_status.set_text(&format!(
                    "Linked to \"{}\" · {active_topics} active topic(s)",
                    forum.title
                ));
                btn_sync_topics.set_visible(true);
                btn_unlink_forum.set_visible(true);
            } else {
                forum_status.set_text("Not linked to a Forum group.");
                btn_sync_topics.set_visible(false);
                btn_unlink_forum.set_visible(false);
            }
            auto_topics_toggle.set_active(cfg.auto_sync_topics);
            archive_topics_toggle.set_active(cfg.archive_closed_topics);

            let port = cfg.mini_app.effective_port();
            if cached_ts.borrow().is_none() {
                *cached_ts.borrow_mut() = Some((hooks.detect_tailscale)(port));
            }
            if !miniapp_loaded.get() {
                let ts_url = cached_ts
                    .borrow()
                    .as_ref()
                    .and_then(|ts| ts.serve_url.clone().or_else(|| ts.suggested_url.clone()));
                let initial_url = if cfg.mini_app.public_url.trim().is_empty() {
                    ts_url.unwrap_or_default()
                } else {
                    cfg.mini_app.public_url.clone()
                };
                miniapp_url_entry.set_text(&initial_url);
                miniapp_short_entry.set_text(&cfg.mini_app.short_name);
                miniapp_loaded.set(true);
            }
            miniapp_toggle.set_active(cfg.mini_app.enabled);
            if let Some(ts) = cached_ts.borrow().as_ref() {
                let status_line = if !ts.installed {
                    format!("Tailscale CLI not found. Loopback port 127.0.0.1:{port}. Or paste your own HTTPS URL.")
                } else if !ts.running {
                    format!("Tailscale installed (stopped). Run `tailscale up`, then Auto-setup Tailscale Serve (port 127.0.0.1:{port}).")
                } else if let Some(serve) = ts.serve_url.as_deref() {
                    format!("Tailscale Serve active: {serve} -> http://127.0.0.1:{port}")
                } else if let Some(suggested) = ts.suggested_url.as_deref() {
                    format!("Tailscale running ({suggested}). Click Auto-setup Tailscale Serve to proxy port {port}.")
                } else {
                    format!("Tailscale running. Loopback port 127.0.0.1:{port}.")
                };
                miniapp_status.set_text(&status_line);
            }

            let p = cfg.permissions;
            for (idx, val) in [
                p.notifications,
                p.read_output,
                p.prompts,
                p.controls,
                p.shell_input,
                p.launch,
                p.close,
                p.attachments,
                p.redact_secrets,
            ]
            .into_iter()
            .enumerate()
            {
                perm_switches[idx].set_active(val);
                perm_switches[idx].set_sensitive(cfg.enabled);
            }
            painting.set(false);
        }
    });

    let refresh: Rc<dyn Fn()> = Rc::new({
        let hooks = hooks.clone();
        let paint = Rc::clone(&paint);
        let token_feedback = token_feedback.clone();
        move || match ((hooks.load_config)(), (hooks.load_runtime)()) {
            (Ok(cfg), Ok(rt)) => {
                let invite = (hooks.load_invite)().ok().flatten();
                let pending = (hooks.load_pending_request)().ok().flatten();
                let alive = (hooks.worker_running)();
                paint(&cfg, &rt, invite.as_ref(), pending.as_ref(), alive);
            }
            _ => {
                paint(&Config::disabled(), &RuntimeState::default(), None, None, false);
                token_feedback.set_text("Telegram settings could not be read; access is blocked.");
                token_feedback.set_visible(true);
            }
        }
    });

    // Save & verify token on a background thread.
    btn_save_token.connect_clicked({
        let hooks = hooks.clone();
        let token_entry = token_entry.clone();
        let token_feedback = token_feedback.clone();
        let btn_save_token = btn_save_token.clone();
        let verifying = Rc::clone(&verifying);
        let token_loaded = Rc::clone(&token_loaded);
        let refresh = Rc::clone(&refresh);
        move |_| {
            let token = token_entry.text().trim().to_string();
            if token.is_empty() {
                token_feedback.set_text("Paste a bot token from @BotFather first.");
                token_feedback.set_visible(true);
                return;
            }
            verifying.set(true);
            btn_save_token.set_sensitive(false);
            token_feedback.set_text("Verifying token with api.telegram.org…");
            token_feedback.set_visible(true);
            let hooks = hooks.clone();
            let btn_save_token = btn_save_token.clone();
            let token_feedback = token_feedback.clone();
            let verifying = Rc::clone(&verifying);
            let token_loaded = Rc::clone(&token_loaded);
            let refresh = Rc::clone(&refresh);
            glib::MainContext::default().spawn_local(async move {
                let res = gtk4::gio::spawn_blocking(move || -> Result<BotUser, String> {
                    let me = (hooks.verify_token)(&token)?;
                    let mut cfg = (hooks.load_config)().map_err(|e| e.to_string())?;
                    cfg.bot_token = token;
                    cfg.bot_id = Some(me.id);
                    cfg.bot_username = me.username.clone();
                    cfg.enabled = true;
                    (hooks.save_config)(&cfg).map_err(|e| e.to_string())?;
                    let _ = (hooks.stop_worker)();
                    let _ = (hooks.start_worker)();
                    Ok(me)
                })
                .await;
                verifying.set(false);
                btn_save_token.set_sensitive(true);
                match res {
                    Ok(Ok(me)) => {
                        token_loaded.set(false);
                        let name = me.username.as_deref().unwrap_or(&me.first_name);
                        token_feedback.set_text(&format!("Saved and verified @{name}."));
                    }
                    Ok(Err(err)) => {
                        token_feedback.set_text(&err);
                    }
                    Err(_) => {
                        token_feedback.set_text("Verification task failed.");
                    }
                }
                token_feedback.set_visible(true);
                refresh();
            });
        }
    });

    btn_clear_token.connect_clicked({
        let hooks = hooks.clone();
        let token_entry = token_entry.clone();
        let token_loaded = Rc::clone(&token_loaded);
        let token_feedback = token_feedback.clone();
        let refresh = Rc::clone(&refresh);
        move |_| {
            let mut cfg = (hooks.load_config)().unwrap_or_default();
            cfg.bot_token.clear();
            cfg.bot_id = None;
            cfg.bot_username = None;
            cfg.enabled = false;
            cfg.allowed_user = None;
            cfg.forum = None;
            if let Err(err) = (hooks.save_config)(&cfg) {
                token_feedback.set_text(&format!("Could not clear token: {err}"));
                token_feedback.set_visible(true);
            } else {
                token_entry.set_text("");
                token_loaded.set(true);
                token_feedback.set_text("Token cleared and Telegram bot stopped.");
                token_feedback.set_visible(true);
                let stop = hooks.stop_worker.clone();
                let _ = gtk4::gio::spawn_blocking(move || {
                    let _ = stop();
                });
            }
            refresh();
        }
    });

    enable_toggle.connect_active_notify({
        let hooks = hooks.clone();
        let painting = Rc::clone(&painting);
        let token_feedback = token_feedback.clone();
        let refresh = Rc::clone(&refresh);
        move |toggle| {
            if painting.get() {
                return;
            }
            let mut cfg = (hooks.load_config)().unwrap_or_default();
            cfg.enabled = toggle.is_active();
            if (hooks.save_config)(&cfg).is_err() {
                token_feedback.set_text("Could not save Telegram settings.");
                token_feedback.set_visible(true);
                refresh();
                return;
            }
            let start = hooks.start_worker.clone();
            let stop = hooks.stop_worker.clone();
            let enable = cfg.enabled;
            let refresh = Rc::clone(&refresh);
            glib::MainContext::default().spawn_local(async move {
                let _ = gtk4::gio::spawn_blocking(move || {
                    if enable {
                        let _ = start();
                    } else {
                        let _ = stop();
                    }
                })
                .await;
                refresh();
            });
        }
    });

    btn_pair.connect_clicked({
        let hooks = hooks.clone();
        let token_feedback = token_feedback.clone();
        let refresh = Rc::clone(&refresh);
        move |_| {
            match (hooks.start_invite)() {
                Ok(_) => token_feedback.set_visible(false),
                Err(err) => {
                    token_feedback.set_text(&err);
                    token_feedback.set_visible(true);
                }
            }
            refresh();
        }
    });

    btn_approve.connect_clicked({
        let hooks = hooks.clone();
        let pending_req_id = Rc::clone(&pending_req_id);
        let token_feedback = token_feedback.clone();
        let refresh = Rc::clone(&refresh);
        move |_| {
            let Some(req_id) = pending_req_id.borrow().clone() else {
                return;
            };
            if let Err(err) = (hooks.approve_pending)(&req_id) {
                token_feedback.set_text(&err);
                token_feedback.set_visible(true);
            }
            refresh();
        }
    });

    btn_reject.connect_clicked({
        let hooks = hooks.clone();
        let pending_req_id = Rc::clone(&pending_req_id);
        let token_feedback = token_feedback.clone();
        let refresh = Rc::clone(&refresh);
        move |_| {
            let Some(req_id) = pending_req_id.borrow().clone() else {
                return;
            };
            if let Err(err) = (hooks.reject_pending)(&req_id) {
                token_feedback.set_text(&err);
                token_feedback.set_visible(true);
            }
            refresh();
        }
    });

    btn_revoke.connect_clicked({
        let hooks = hooks.clone();
        let token_feedback = token_feedback.clone();
        let refresh = Rc::clone(&refresh);
        move |_| {
            if let Err(err) = (hooks.revoke_user)() {
                token_feedback.set_text(&err);
                token_feedback.set_visible(true);
            }
            refresh();
        }
    });

    btn_clear_rejected.connect_clicked({
        let hooks = hooks.clone();
        let refresh = Rc::clone(&refresh);
        move |_| {
            let _ = (hooks.clear_rejected)();
            refresh();
        }
    });

    btn_sync_topics.connect_clicked({
        let hooks = hooks.clone();
        let token_feedback = token_feedback.clone();
        let refresh = Rc::clone(&refresh);
        move |_| {
            if (hooks.request_sync)().is_ok() {
                token_feedback.set_text("Topic sync requested.");
                token_feedback.set_visible(true);
            }
            refresh();
        }
    });

    btn_unlink_forum.connect_clicked({
        let hooks = hooks.clone();
        let refresh = Rc::clone(&refresh);
        move |_| {
            let _ = (hooks.unlink_forum)();
            refresh();
        }
    });

    auto_topics_toggle.connect_active_notify({
        let hooks = hooks.clone();
        let painting = Rc::clone(&painting);
        let refresh = Rc::clone(&refresh);
        move |toggle| {
            if painting.get() {
                return;
            }
            let mut cfg = (hooks.load_config)().unwrap_or_default();
            cfg.auto_sync_topics = toggle.is_active();
            let _ = (hooks.save_config)(&cfg);
            refresh();
        }
    });

    archive_topics_toggle.connect_active_notify({
        let hooks = hooks.clone();
        let painting = Rc::clone(&painting);
        let refresh = Rc::clone(&refresh);
        move |toggle| {
            if painting.get() {
                return;
            }
            let mut cfg = (hooks.load_config)().unwrap_or_default();
            cfg.archive_closed_topics = toggle.is_active();
            let _ = (hooks.save_config)(&cfg);
            refresh();
        }
    });

    miniapp_toggle.connect_active_notify({
        let hooks = hooks.clone();
        let painting = Rc::clone(&painting);
        let miniapp_url_entry = miniapp_url_entry.clone();
        let miniapp_short_entry = miniapp_short_entry.clone();
        let miniapp_feedback = miniapp_feedback.clone();
        let refresh = Rc::clone(&refresh);
        move |toggle| {
            if painting.get() {
                return;
            }
            let mut cfg = (hooks.load_config)().unwrap_or_default();
            cfg.mini_app.enabled = toggle.is_active();
            cfg.mini_app.listen_port = cfg.mini_app.effective_port();
            let typed_url = miniapp_url_entry.text().trim().to_string();
            if !typed_url.is_empty() {
                cfg.mini_app.public_url = typed_url;
            }
            cfg.mini_app.short_name = miniapp_short_entry.text().trim().to_string();
            if (hooks.save_config)(&cfg).is_err() {
                miniapp_feedback.set_text("Could not save Mini Web App settings.");
                miniapp_feedback.set_visible(true);
            } else {
                miniapp_feedback.set_visible(false);
            }
            refresh();
        }
    });

    btn_miniapp_save.connect_clicked({
        let hooks = hooks.clone();
        let miniapp_url_entry = miniapp_url_entry.clone();
        let miniapp_short_entry = miniapp_short_entry.clone();
        let miniapp_feedback = miniapp_feedback.clone();
        let refresh = Rc::clone(&refresh);
        move |_| {
            let url = miniapp_url_entry.text().trim().to_string();
            if !url.is_empty() && !url.starts_with("https://") {
                miniapp_feedback.set_text("Mini App URL must start with https:// (for example https://my-pc.tailnet.ts.net:8443).");
                miniapp_feedback.set_visible(true);
                return;
            }
            let short = miniapp_short_entry.text().trim().to_string();
            if !short.is_empty()
                && !short.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                miniapp_feedback.set_text("BotFather /newapp short name can only contain letters, digits, _ and -.");
                miniapp_feedback.set_visible(true);
                return;
            }
            let mut cfg = (hooks.load_config)().unwrap_or_default();
            cfg.mini_app.listen_port = cfg.mini_app.effective_port();
            cfg.mini_app.public_url = url;
            cfg.mini_app.short_name = short;
            if !cfg.mini_app.public_url.is_empty() {
                cfg.mini_app.enabled = true;
            }
            match (hooks.save_config)(&cfg) {
                Ok(()) => miniapp_feedback.set_text("Saved Mini Web App settings."),
                Err(e) => miniapp_feedback.set_text(&format!("Save failed: {e}")),
            }
            miniapp_feedback.set_visible(true);
            refresh();
        }
    });

    btn_miniapp_ts.connect_clicked({
        let hooks = hooks.clone();
        let cached_ts = Rc::clone(&cached_ts);
        let miniapp_loaded = Rc::clone(&miniapp_loaded);
        let miniapp_feedback = miniapp_feedback.clone();
        let btn_miniapp_ts = btn_miniapp_ts.clone();
        let refresh = Rc::clone(&refresh);
        move |_| {
            let port = (hooks.load_config)()
                .map(|c| c.mini_app.effective_port())
                .unwrap_or(config::DEFAULT_MINI_APP_PORT);
            btn_miniapp_ts.set_sensitive(false);
            miniapp_feedback.set_text("Configuring `tailscale serve --bg --https=8443`…");
            miniapp_feedback.set_visible(true);
            let hooks = hooks.clone();
            let cached_ts = Rc::clone(&cached_ts);
            let miniapp_loaded = Rc::clone(&miniapp_loaded);
            let miniapp_feedback = miniapp_feedback.clone();
            let btn_miniapp_ts = btn_miniapp_ts.clone();
            let refresh = Rc::clone(&refresh);
            glib::MainContext::default().spawn_local(async move {
                let res = gtk4::gio::spawn_blocking(move || {
                    let url = (hooks.setup_tailscale)(port, config::DEFAULT_MINI_APP_HTTPS_PORT)?;
                    let info = (hooks.detect_tailscale)(port);
                    let mut cfg = (hooks.load_config)().map_err(|e| e.to_string())?;
                    cfg.mini_app.enabled = true;
                    cfg.mini_app.listen_port = port;
                    cfg.mini_app.public_url = url.clone();
                    (hooks.save_config)(&cfg).map_err(|e| e.to_string())?;
                    Ok::<_, String>((url, info))
                })
                .await;
                btn_miniapp_ts.set_sensitive(true);
                match res {
                    Ok(Ok((url, info))) => {
                        *cached_ts.borrow_mut() = Some(info);
                        miniapp_loaded.set(false);
                        miniapp_feedback.set_text(&format!("Tailscale Serve active at {url} and Mini App enabled."));
                    }
                    Ok(Err(err)) => miniapp_feedback.set_text(&err),
                    Err(_) => miniapp_feedback.set_text("Tailscale Serve setup failed."),
                }
                miniapp_feedback.set_visible(true);
                refresh();
            });
        }
    });

    btn_miniapp_prompt.connect_clicked({
        let hooks = hooks.clone();
        let miniapp_feedback = miniapp_feedback.clone();
        move |btn| {
            let port = (hooks.load_config)()
                .map(|c| c.mini_app.effective_port())
                .unwrap_or(config::DEFAULT_MINI_APP_PORT);
            let prompt = webapp::ai_agent_setup_prompt(port, config::DEFAULT_MINI_APP_HTTPS_PORT);
            btn.clipboard().set_text(&prompt);
            miniapp_feedback.set_text("Copied AI agent setup prompt to clipboard — paste it into any AI agent.");
            miniapp_feedback.set_visible(true);
        }
    });

    for (index, toggle) in perm_switches.iter().enumerate() {
        let hooks = hooks.clone();
        let painting = Rc::clone(&painting);
        let token_feedback = token_feedback.clone();
        let refresh = Rc::clone(&refresh);
        toggle.connect_active_notify(move |toggle| {
            if painting.get() {
                return;
            }
            let mut cfg = (hooks.load_config)().unwrap_or_else(|_| Config::disabled());
            let v = toggle.is_active();
            match index {
                0 => cfg.permissions.notifications = v,
                1 => cfg.permissions.read_output = v,
                2 => cfg.permissions.prompts = v,
                3 => cfg.permissions.controls = v,
                4 => cfg.permissions.shell_input = v,
                5 => cfg.permissions.launch = v,
                6 => cfg.permissions.close = v,
                7 => cfg.permissions.attachments = v,
                _ => cfg.permissions.redact_secrets = v,
            }
            if (hooks.save_config)(&cfg).is_err() {
                token_feedback.set_text("Could not save Telegram permissions.");
                token_feedback.set_visible(true);
            }
            refresh();
        });
    }

    crate::launcher_settings::tick_while_mapped(
        &[root.clone().upcast()],
        Duration::from_millis(1500),
        {
            let refresh = Rc::clone(&refresh);
            move || refresh()
        },
    );

    refresh();
    refresh
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn telegram_settings_page_persists_toggles_and_pairing_ui() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process(
                "harness_settings::telegram_settings_page::tests::telegram_settings_page_persists_toggles_and_pairing_ui",
            );
            return;
        }
        gtk4::init().expect("private GTK display");
        let cfg_store = Arc::new(Mutex::new(Config {
            enabled: true,
            bot_token: "123456:TESTTOKEN".into(),
            bot_id: Some(123456),
            bot_username: Some("sd_test_bot".into()),
            ..Config::default()
        }));
        let rt_store = Arc::new(Mutex::new(RuntimeState::default()));
        let invite_store: Arc<Mutex<Option<config::PendingInvite>>> = Arc::new(Mutex::new(None));
        let req_store = Arc::new(Mutex::new(Some(config::PendingRequest {
            request_id: "req1".into(),
            user_id: 42,
            chat_id: 42,
            username: Some("dmytro".into()),
            first_name: "Dmytro".into(),
            code: "654321".into(),
            requested_at: unix_now(),
            expires_at: unix_now() + 180,
        })));
        let fail_save_flag = Arc::new(Mutex::new(false));

        let hooks = Hooks {
            load_config: {
                let s = Arc::clone(&cfg_store);
                Arc::new(move || Ok(s.lock().unwrap().clone()))
            },
            save_config: {
                let s = Arc::clone(&cfg_store);
                let fail = Arc::clone(&fail_save_flag);
                Arc::new(move |cfg| {
                    if *fail.lock().unwrap() {
                        Err(std::io::Error::other("disk error"))
                    } else {
                        *s.lock().unwrap() = cfg.clone();
                        Ok(())
                    }
                })
            },
            load_runtime: {
                let s = Arc::clone(&rt_store);
                Arc::new(move || Ok(s.lock().unwrap().clone()))
            },
            load_invite: {
                let s = Arc::clone(&invite_store);
                Arc::new(move || Ok(s.lock().unwrap().clone()))
            },
            load_pending_request: {
                let s = Arc::clone(&req_store);
                Arc::new(move || Ok(s.lock().unwrap().clone()))
            },
            request_sync: Arc::new(|| Ok(())),
            verify_token: Arc::new(|_| {
                Ok(BotUser {
                    id: 123456,
                    is_bot: true,
                    first_name: "SD".into(),
                    username: Some("sd_test_bot".into()),
                })
            }),
            start_worker: Arc::new(|| Ok(())),
            stop_worker: Arc::new(|| Ok(())),
            worker_running: Arc::new(|| true),
            start_invite: {
                let inv_store = Arc::clone(&invite_store);
                let req_store = Arc::clone(&req_store);
                Arc::new(move || {
                    let invite = config::PendingInvite {
                        secret: "sec123".into(),
                        created_at: unix_now(),
                        expires_at: unix_now() + 300,
                    };
                    *inv_store.lock().unwrap() = Some(invite.clone());
                    *req_store.lock().unwrap() = None;
                    Ok((
                        invite,
                        Some("https://t.me/sd_test_bot?start=sec123".into()),
                    ))
                })
            },
            approve_pending: {
                let c = Arc::clone(&cfg_store);
                let r = Arc::clone(&req_store);
                Arc::new(move |req_id| {
                    let req = r.lock().unwrap().take().unwrap();
                    assert_eq!(req.request_id, req_id);
                    let user = config::AllowedUser {
                        user_id: req.user_id,
                        chat_id: req.chat_id,
                        username: req.username,
                        first_name: req.first_name,
                        paired_at: unix_now(),
                    };
                    c.lock().unwrap().allowed_user = Some(user.clone());
                    Ok(user)
                })
            },
            reject_pending: Arc::new(|_| Ok(())),
            revoke_user: {
                let c = Arc::clone(&cfg_store);
                Arc::new(move || {
                    c.lock().unwrap().allowed_user = None;
                    Ok(())
                })
            },
            clear_rejected: Arc::new(|| Ok(())),
            unlink_forum: Arc::new(|| Ok(())),
            detect_tailscale: Arc::new(|port| webapp::TailscaleInfo {
                installed: true,
                running: true,
                dns_name: Some("my-pc.tailnet.ts.net".into()),
                serve_url: None,
                suggested_url: Some("https://my-pc.tailnet.ts.net:8443".into()),
                serve_command: format!("tailscale serve --bg --https=8443 http://127.0.0.1:{port}"),
                funnel_enabled: false,
            }),
            setup_tailscale: Arc::new(|_, https_port| {
                Ok(format!("https://my-pc.tailnet.ts.net:{https_port}"))
            }),
        };

        let root = Box::new(Orientation::Vertical, 10);
        let refresh = build_with_hooks(&root, hooks);
        let find_switch = |name: &str| -> Switch {
            crate::gtk_test::find_where(root.upcast_ref(), |w| w.widget_name() == name)
                .and_then(|w| w.downcast().ok())
                .unwrap()
        };
        let find_btn = |name: &str| -> Button {
            crate::gtk_test::find_where(root.upcast_ref(), |w| w.widget_name() == name)
                .and_then(|w| w.downcast().ok())
                .unwrap()
        };
        let token_entry: Entry = crate::gtk_test::find_where(root.upcast_ref(), |w| {
            w.widget_name() == "telegram-token-entry"
        })
        .and_then(|w| w.downcast().ok())
        .unwrap();

        // User typing into token_entry is NOT overwritten by refresh ticks.
        token_entry.set_text("typed-new-token");
        refresh();
        assert_eq!(token_entry.text(), "typed-new-token");

        // Copy link button exists on the invite box.
        let _copy_btn = find_btn("telegram-copy-invite");

        let read_out = find_switch("telegram-read-output");
        let prompts = find_switch("telegram-prompts");
        let shell_in = find_switch("telegram-shell-input");
        assert!(!read_out.is_active() && !prompts.is_active() && !shell_in.is_active());
        read_out.set_active(true);
        prompts.set_active(true);
        assert!(cfg_store.lock().unwrap().permissions.read_output);
        assert!(cfg_store.lock().unwrap().permissions.prompts);

        // Failed save reverts switch state.
        *fail_save_flag.lock().unwrap() = true;
        shell_in.set_active(true);
        assert!(!shell_in.is_active());
        assert!(!cfg_store.lock().unwrap().permissions.shell_input);
        *fail_save_flag.lock().unwrap() = false;

        // Pending pairing request is shown and Approve binds owner.
        let pending_box = crate::gtk_test::find_where(root.upcast_ref(), |w| {
            w.widget_name() == "telegram-pending-box"
        })
        .unwrap();
        assert!(pending_box.property::<bool>("visible"));
        find_btn("telegram-approve-btn").emit_clicked();
        assert_eq!(cfg_store.lock().unwrap().allowed_user.as_ref().unwrap().user_id, 42);
        assert!(!pending_box.property::<bool>("visible"));

        // Revoke clears allowed user.
        find_btn("telegram-revoke-btn").emit_clicked();
        assert!(cfg_store.lock().unwrap().allowed_user.is_none());
        refresh();

        // Mini App settings persist URL, short name, and enabled toggle.
        let miniapp_url: Entry = crate::gtk_test::find_where(root.upcast_ref(), |w| {
            w.widget_name() == "telegram-miniapp-url"
        })
        .and_then(|w| w.downcast().ok())
        .unwrap();
        let miniapp_short: Entry = crate::gtk_test::find_where(root.upcast_ref(), |w| {
            w.widget_name() == "telegram-miniapp-shortname"
        })
        .and_then(|w| w.downcast().ok())
        .unwrap();
        miniapp_url.set_text("https://devbox.tailnet.ts.net:8443/");
        miniapp_short.set_text("superdesktop");
        find_btn("telegram-miniapp-save").emit_clicked();
        {
            let guard = cfg_store.lock().unwrap();
            assert!(guard.mini_app.enabled);
            assert_eq!(
                guard.mini_app.normalized_url().as_deref(),
                Some("https://devbox.tailnet.ts.net:8443")
            );
            assert_eq!(guard.mini_app.valid_short_name(), Some("superdesktop"));
        }
        let _copy_prompt = find_btn("telegram-miniapp-copy-prompt");
    }

    #[test]
    fn telegram_settings_screenshots() {
        if std::env::var_os("SD_TELEGRAM_SHOTS").is_none() {
            return;
        }
        crate::gtk_test::run_in_child_process(
            "harness_settings::telegram_settings_page::tests::telegram_settings_screenshots_inner",
        );
    }

    #[test]
    fn telegram_settings_screenshots_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let Some(out) = std::env::var_os("SD_TELEGRAM_SHOTS").map(std::path::PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&out).unwrap();
        gtk4::init().unwrap();
        gtk4::Settings::default()
            .unwrap()
            .set_gtk_application_prefer_dark_theme(true);
        crate::styles::apply_styles();

        let cfg = Config {
            enabled: true,
            bot_token: "123456789:sample-bot-token-not-real".into(),
            bot_id: Some(123456789),
            bot_username: Some("my_superdesktop_bot".into()),
            allowed_user: None,
            forum: Some(config::ForumConfig {
                chat_id: -1001234567890,
                title: "My PC Agents".into(),
                linked_at: unix_now(),
            }),
            auto_sync_topics: true,
            archive_closed_topics: true,
            permissions: config::Permissions {
                notifications: true,
                redact_secrets: true,
                read_output: true,
                prompts: true,
                controls: true,
                ..config::Permissions::default()
            },
            mini_app: config::MiniAppConfig {
                enabled: true,
                listen_port: 7789,
                public_url: "https://workstation.tailnet.ts.net:8443".into(),
                short_name: "superdesktop".into(),
            },
            ..Config::default()
        };
        let mut rt = RuntimeState::default();
        rt.worker_status = config::WorkerStatus {
            state: "running".into(),
            pid: Some(4242),
            bot_username: Some("my_superdesktop_bot".into()),
            last_poll_at: unix_now(),
            last_error: None,
        };
        rt.topics.push(config::TopicBinding {
            card_id: "sd_term_1".into(),
            thread_id: 77,
            topic_name: "⚡ Claude Code · super-desktop".into(),
            ..config::TopicBinding::default()
        });
        let invite = config::PendingInvite {
            secret: "sampleInviteToken987".into(),
            created_at: unix_now(),
            expires_at: unix_now() + 300,
        };
        let req = config::PendingRequest {
            request_id: "req_sample".into(),
            user_id: 987654321,
            chat_id: 987654321,
            username: Some("alex".into()),
            first_name: "Alex".into(),
            code: "482915".into(),
            requested_at: unix_now(),
            expires_at: unix_now() + 180,
        };

        let cfg_store = Arc::new(Mutex::new(cfg));
        let hooks = Hooks {
            load_config: {
                let s = Arc::clone(&cfg_store);
                Arc::new(move || Ok(s.lock().unwrap().clone()))
            },
            save_config: {
                let s = Arc::clone(&cfg_store);
                Arc::new(move |next| {
                    *s.lock().unwrap() = next.clone();
                    Ok(())
                })
            },
            load_runtime: {
                let r = rt.clone();
                Arc::new(move || Ok(r.clone()))
            },
            load_invite: {
                let inv = invite.clone();
                Arc::new(move || Ok(Some(inv.clone())))
            },
            load_pending_request: {
                let req = req.clone();
                Arc::new(move || Ok(Some(req.clone())))
            },
            request_sync: Arc::new(|| Ok(())),
            verify_token: Arc::new(|_| {
                Ok(BotUser {
                    id: 123456789,
                    is_bot: true,
                    first_name: "My Super Desktop".into(),
                    username: Some("my_superdesktop_bot".into()),
                })
            }),
            start_worker: Arc::new(|| Ok(())),
            stop_worker: Arc::new(|| Ok(())),
            worker_running: Arc::new(|| true),
            start_invite: {
                let inv = invite.clone();
                Arc::new(move || {
                    Ok((
                        inv.clone(),
                        Some("https://t.me/my_superdesktop_bot?start=sampleInviteToken987".into()),
                    ))
                })
            },
            approve_pending: {
                let s = Arc::clone(&cfg_store);
                Arc::new(move |_| Ok(s.lock().unwrap().allowed_user.clone().unwrap()))
            },
            reject_pending: Arc::new(|_| Ok(())),
            revoke_user: Arc::new(|| Ok(())),
            clear_rejected: Arc::new(|| Ok(())),
            unlink_forum: Arc::new(|| Ok(())),
            detect_tailscale: Arc::new(|port| webapp::TailscaleInfo {
                installed: true,
                running: true,
                dns_name: Some("workstation.tailnet.ts.net".into()),
                serve_url: Some("https://workstation.tailnet.ts.net:8443".into()),
                suggested_url: Some("https://workstation.tailnet.ts.net:8443".into()),
                serve_command: format!("tailscale serve --bg --https=8443 http://127.0.0.1:{port}"),
                funnel_enabled: false,
            }),
            setup_tailscale: Arc::new(|_, https_port| {
                Ok(format!("https://workstation.tailnet.ts.net:{https_port}"))
            }),
        };

        let content = Box::new(Orientation::Vertical, 0);
        content.set_margin_start(16);
        content.set_margin_end(16);
        content.set_margin_top(12);
        content.set_margin_bottom(16);
        let refresh = build_with_hooks(&content, hooks);
        refresh();

        let scroller = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .child(&content)
            .build();

        let frame = Box::new(Orientation::Vertical, 0);
        frame.add_css_class("mini-terminal");
        frame.add_css_class("harness-panel");
        let header_badge = Label::new(Some("💬"));
        let header = crate::floating_panel::PanelHeader::new(
            &header_badge,
            "Telegram",
            "Bot · pairing · Forum Topics · Mini Web App",
            false,
        );
        frame.append(&header.widget);
        scroller.set_vexpand(true);
        frame.append(&scroller);

        let window = gtk4::Window::new();
        window.add_css_class("super-desktop");
        window.set_default_size(720, 760);
        window.set_child(Some(&frame));
        window.present();
        crate::gtk_test::pump(800);

        // Shot 1: Bot token + Running status + Account pairing QR & approval box.
        crate::gtk_test::save_png(&frame, &out.join("settings-bot-pair.png"));

        // Shot 2: Forum Topics & Mini Web App (Tailscale).
        let miniapp_anchor = crate::gtk_test::find_where(content.upcast_ref(), |w| {
            w.widget_name() == "telegram-sync-topics"
        });
        let adj = scroller.vadjustment();
        let forum_y = miniapp_anchor
            .and_then(|w| w.compute_bounds(&content))
            .map(|b| (f64::from(b.y()) - 120.0).max(0.0))
            .unwrap_or_else(|| (adj.upper() - adj.page_size()) * 0.45);
        adj.set_value(forum_y.min((adj.upper() - adj.page_size()).max(0.0)));
        crate::gtk_test::pump(300);
        crate::gtk_test::save_png(&frame, &out.join("settings-forum-miniapp.png"));

        // Shot 3: Permissions switches at bottom.
        adj.set_value((adj.upper() - adj.page_size()).max(0.0));
        crate::gtk_test::pump(300);
        crate::gtk_test::save_png(&frame, &out.join("settings-permissions.png"));
    }
}
