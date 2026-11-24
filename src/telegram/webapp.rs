//! Embedded Telegram Mini Web App server (loopback HTTP/WS for private `tailscale serve`).
//!
//! Security model:
//! - Listens strictly on `127.0.0.1:<port>`; exposed only inside the user's own tailnet via
//!   `tailscale serve --bg --https=8443 http://127.0.0.1:<port>` (never public Funnel).
//! - Authenticates via official Telegram `WebApp.initData` HMAC-SHA256 verified locally against
//!   the configured bot token and paired owner `user_id`, then issues short-lived Bearer tokens.
//! - Honors every Settings → Telegram permission toggle and invalidates tokens immediately on changes.
use super::config::{
    mask_token_in_text, unix_now, Config, DEFAULT_MINI_APP_HTTPS_PORT, DEFAULT_MINI_APP_PORT,
};
use super::desktop::{normalize_telegram_image, CardSummary, DesktopBackend, NamedKey};
use super::redact::{
    redact_ansi_preserving, redact_known_secrets, redact_link_text, sanitize_terminal_text,
};
use super::topics::quick_slash_commands;
use crate::prompt_attachments::{safe_name, Attachment, Kind, MAX_TOTAL};
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::hmac;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const UI_HTML: &str = include_str!("webapp_ui.html");
const INIT_DATA_MAX_AGE_SECS: u64 = 300;
const SESSION_TTL_SECS: u64 = 1800;
const SESSION_HARD_TTL_SECS: u64 = 3600;
const WS_TICKET_TTL_SECS: u64 = 15;
const MAX_WS_TICKETS: usize = 64;
const WS_MAX_LIFETIME_SECS: u64 = 1800;
const MAX_SESSIONS: usize = 32;
const MAX_CONNS: usize = 32;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_JSON_BODY_BYTES: usize = 64 * 1024;
const MAX_UPLOAD_BODY_BYTES: usize = 14 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedInitData {
    pub user_id: i64,
    pub first_name: String,
    pub username: Option<String>,
    pub start_param: Option<String>,
}

fn hex_decode_32(hex: &str) -> Result<[u8; 32], String> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return Err("Invalid Telegram initData hash length.".into());
    }
    let mut out = [0u8; 32];
    for (idx, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let s = std::str::from_utf8(chunk).map_err(|_| "Invalid hex byte".to_string())?;
        out[idx] = u8::from_str_radix(s, 16).map_err(|_| "Invalid hex hash".to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn decode_form_component(raw: &str) -> Result<String, String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                decoded.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err("Truncated percent-encoding in initData.".into());
                }
                let hi = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .map_err(|_| "Invalid percent-encoding".to_string())?;
                let val =
                    u8::from_str_radix(hi, 16).map_err(|_| "Invalid hex escape".to_string())?;
                decoded.push(val);
                i += 3;
            }
            other => {
                decoded.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8(decoded).map_err(|_| "Non-UTF-8 initData parameter.".into())
}

pub fn verify_init_data(
    init_data: &str,
    bot_token: &str,
    expected_user_id: i64,
    now_unix: u64,
    max_age_secs: u64,
) -> Result<VerifiedInitData, String> {
    let trimmed = init_data.trim();
    if trimmed.is_empty() || bot_token.trim().is_empty() {
        return Err("Missing Telegram initData or bot token.".into());
    }
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    let mut hash_hex: Option<String> = None;
    for pair in trimmed.split('&').filter(|s| !s.is_empty()) {
        let (raw_k, raw_v) = pair
            .split_once('=')
            .ok_or_else(|| "Malformed initData pair.".to_string())?;
        let key = decode_form_component(raw_k)?;
        let val = decode_form_component(raw_v)?;
        if key == "hash" {
            if hash_hex.is_some() {
                return Err("Duplicate hash in initData.".into());
            }
            hash_hex = Some(val);
        } else if map.insert(key, val).is_some() {
            return Err("Duplicate field in initData.".into());
        }
    }

    let hash_bytes = hex_decode_32(hash_hex.as_deref().ok_or("Missing initData hash.")?)?;
    let data_check_string = map
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n");

    let web_app_key = hmac::Key::new(hmac::HMAC_SHA256, b"WebAppData");
    let secret = hmac::sign(&web_app_key, bot_token.trim().as_bytes());
    let data_key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_ref());
    hmac::verify(&data_key, data_check_string.as_bytes(), &hash_bytes)
        .map_err(|_| "Invalid Telegram initData signature.".to_string())?;

    let auth_date = map
        .get("auth_date")
        .ok_or("Missing auth_date in initData.")?
        .parse::<u64>()
        .map_err(|_| "Invalid auth_date in initData.".to_string())?;
    if auth_date > now_unix.saturating_add(60) || now_unix > auth_date.saturating_add(max_age_secs)
    {
        return Err("Telegram Mini App session expired — reopen from Telegram.".into());
    }

    let user_raw = map.get("user").ok_or("Missing user in initData.")?;
    let user_val: Value =
        serde_json::from_str(user_raw).map_err(|_| "Invalid user JSON in initData.".to_string())?;
    let uid = user_val["id"]
        .as_i64()
        .ok_or("Missing user.id in initData.")?;
    if uid != expected_user_id {
        return Err("Telegram user is not authorized for this PC.".into());
    }

    Ok(VerifiedInitData {
        user_id: uid,
        first_name: user_val["first_name"]
            .as_str()
            .unwrap_or("Owner")
            .to_string(),
        username: user_val["username"].as_str().map(str::to_string),
        start_param: map
            .get("start_param")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
    })
}

#[cfg(test)]
pub fn sign_init_data_for_test(params: &[(&str, &str)], bot_token: &str) -> String {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in params {
        map.insert((*k).to_string(), (*v).to_string());
    }
    let check = map
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n");
    let secret = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, b"WebAppData"),
        bot_token.trim().as_bytes(),
    );
    let sig = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, secret.as_ref()),
        check.as_bytes(),
    );
    let pct = |s: &str| -> String {
        let mut out = String::new();
        for b in s.as_bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(*b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    };
    let mut encoded: Vec<String> = map
        .iter()
        .map(|(k, v)| format!("{}={}", pct(k), pct(v)))
        .collect();
    encoded.push(format!("hash={}", hex_encode(sig.as_ref())));
    encoded.join("&")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TailscaleInfo {
    pub installed: bool,
    pub running: bool,
    pub dns_name: Option<String>,
    pub serve_url: Option<String>,
    pub suggested_url: Option<String>,
    pub serve_command: String,
    pub funnel_enabled: bool,
}

fn has_funnel_enabled(val: &Value) -> bool {
    match val {
        Value::Object(map) => map.iter().any(|(k, v)| {
            (k.eq_ignore_ascii_case("AllowFunnel")
                && match v {
                    Value::Bool(b) => *b,
                    Value::Object(inner) => inner.values().any(|iv| iv.as_bool() == Some(true)),
                    _ => false,
                })
                || has_funnel_enabled(v)
        }),
        Value::Array(arr) => arr.iter().any(has_funnel_enabled),
        _ => false,
    }
}

fn find_serve_url(val: &Value, listen_port: u16) -> Option<String> {
    let target_ip = format!("127.0.0.1:{listen_port}");
    let target_lh = format!("localhost:{listen_port}");
    let web = val.get("Web")?.as_object()?;
    for (host_port, entry) in web {
        let ser = serde_json::to_string(entry).unwrap_or_default();
        if ser.contains(&target_ip) || ser.contains(&target_lh) {
            let clean = host_port.trim_end_matches(":443");
            return Some(format!("https://{clean}"));
        }
    }
    None
}

pub fn detect_tailscale_info(listen_port: u16) -> TailscaleInfo {
    let port = if listen_port >= 1024 {
        listen_port
    } else {
        DEFAULT_MINI_APP_PORT
    };
    let serve_command = format!(
        "tailscale serve --bg --https={DEFAULT_MINI_APP_HTTPS_PORT} http://127.0.0.1:{port}"
    );
    let Ok(status_out) = Command::new("tailscale").args(["status", "--json"]).output() else {
        return TailscaleInfo {
            serve_command,
            ..TailscaleInfo::default()
        };
    };
    let mut info = TailscaleInfo {
        installed: true,
        serve_command,
        ..TailscaleInfo::default()
    };
    if let Ok(val) = serde_json::from_slice::<Value>(&status_out.stdout) {
        info.running = val["BackendState"].as_str() == Some("Running");
        if let Some(dns) = val["Self"]["DNSName"]
            .as_str()
            .map(|s| s.trim().trim_end_matches('.'))
            .filter(|s| !s.is_empty())
        {
            info.dns_name = Some(dns.to_string());
            info.suggested_url = Some(format!("https://{dns}:{DEFAULT_MINI_APP_HTTPS_PORT}"));
        }
    }
    if info.running {
        if let Ok(serve_out) = Command::new("tailscale")
            .args(["serve", "status", "--json"])
            .output()
        {
            if let Ok(val) = serde_json::from_slice::<Value>(&serve_out.stdout) {
                info.serve_url = find_serve_url(&val, port);
                info.funnel_enabled = has_funnel_enabled(&val);
            }
        }
    }
    info
}

pub fn configure_tailscale_serve(listen_port: u16, https_port: u16) -> Result<String, String> {
    let port = if listen_port >= 1024 {
        listen_port
    } else {
        DEFAULT_MINI_APP_PORT
    };
    let info = detect_tailscale_info(port);
    if !info.installed {
        return Err("`tailscale` binary was not found on PATH.".into());
    }
    if !info.running {
        return Err("Tailscale is not running on this PC. Run `tailscale up` first.".into());
    }
    let out = Command::new("tailscale")
        .args([
            "serve",
            "--bg",
            &format!("--https={https_port}"),
            &format!("http://127.0.0.1:{port}"),
        ])
        .output()
        .map_err(|e| format!("Could not run tailscale serve: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "tailscale serve failed. Ensure MagicDNS and HTTPS Certificates are enabled in Tailscale Admin → DNS.".into()
        } else {
            stderr
        });
    }
    let refreshed = detect_tailscale_info(port);
    if let Some(url) = refreshed.serve_url {
        return Ok(url);
    }
    refreshed
        .dns_name
        .map(|dns| {
            if https_port == 443 {
                format!("https://{dns}")
            } else {
                format!("https://{dns}:{https_port}")
            }
        })
        .ok_or_else(|| "Tailscale serve started, but DNSName was not reported. Enable MagicDNS in Tailscale Admin.".into())
}

pub fn ai_agent_setup_prompt(listen_port: u16, https_port: u16) -> String {
    let port = if listen_port >= 1024 {
        listen_port
    } else {
        DEFAULT_MINI_APP_PORT
    };
    format!(
        r#"Configure the SUPER DESKTOP Telegram Mini Web App on this Linux PC using Tailscale Serve (never Tailscale Funnel).

Rules:
- Never print, echo, or commit my bot token (`botToken`) or any secrets from config.json.
- Use `tailscale serve` (private to my tailnet only), never `tailscale funnel`.
- Keep all existing Telegram settings (`enabled`, `botToken`, `allowedUser`, `forum`, `permissions`) intact.

Steps:
1. Check Tailscale status with `tailscale status --json` and ensure it is Running with a `Self.DNSName`.
   - If MagicDNS or HTTPS Certificates are disabled, stop and tell me what toggle to enable in https://login.tailscale.com/admin/dns.
2. Expose only SUPER DESKTOP's local loopback Mini App server privately via Tailscale Serve:
   `tailscale serve --bg --https={https_port} http://127.0.0.1:{port}`
   and record `URL="https://<DNSName_without_trailing_dot>:{https_port}"`.
3. Inspect `${{XDG_STATE_HOME:-$HOME/.local/state}}/super-desktop/telegram/config.json`:
   - If the Telegram bot is not yet configured (`enabled: false` or empty `botToken` or no `allowedUser`), stop and guide me through Settings → Telegram (`@BotFather` token + account pairing) first.
   - Otherwise update `config.json` atomically (`chmod 600`) to set:
     `miniApp.enabled = true`, `miniApp.listenPort = {port}`, `miniApp.publicUrl = "$URL"` (preserving `miniApp.shortName` and all other fields).
4. Signal/verify the worker cleanly:
   - Check `curl -fsS http://127.0.0.1:{port}/health` and `curl -fsS "$URL/health"`.
   - If the `super-desktop telegram-bot` process isn't listening on `127.0.0.1:{port}` yet, wait a couple seconds after updating `config.json` (the daemon hot-reloads it) or restart SUPER DESKTOP with `super-desktop status`.
5. Report back concisely:
   - The private Mini App `$URL`
   - Whether `/health` succeeded locally and via Tailscale HTTPS
   - Next steps on my phone (keep Tailscale ON, tap ⚡ Open App in 1:1 chat with the bot or run `/app`; optionally register `/newapp` in `@BotFather` and paste the short name into Settings → Telegram → Mini Web App for Forum Topic deep links)."#
    )
}

#[derive(Clone)]
struct SessionEntry {
    token: String,
    user_id: i64,
    paired_at: u64,
    token_fp: [u8; 32],
    created_at: u64,
    last_seen: u64,
}

#[derive(Clone)]
struct WsTicketEntry {
    ticket: String,
    session_token: String,
    card_id: String,
    expires_at: u64,
}

#[derive(Default)]
struct SessionStore {
    entries: VecDeque<SessionEntry>,
    ws_tickets: VecDeque<WsTicketEntry>,
}

fn bot_token_fingerprint(bot_token: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bot_token.trim().as_bytes());
    h.finalize().into()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8;
    let max_len = a.len().max(b.len());
    for i in 0..max_len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0 && a.len() == b.len()
}

impl SessionStore {
    fn prune_expired(&mut self, now_unix: u64) {
        self.entries.retain(|e| {
            now_unix.saturating_sub(e.last_seen) <= SESSION_TTL_SECS
                && now_unix.saturating_sub(e.created_at) <= SESSION_HARD_TTL_SECS
        });
        self.ws_tickets.retain(|t| now_unix <= t.expires_at);
    }

    fn issue(
        &mut self,
        cfg: &Config,
        user_id: i64,
        paired_at: u64,
        now_unix: u64,
    ) -> Result<String, String> {
        let token = super_desktop::control::random_hex(32).map_err(|e| e.to_string())?;
        let fp = bot_token_fingerprint(&cfg.bot_token);
        self.prune_expired(now_unix);
        while self.entries.len() >= MAX_SESSIONS {
            self.entries.pop_front();
        }
        self.entries.push_back(SessionEntry {
            token: token.clone(),
            user_id,
            paired_at,
            token_fp: fp,
            created_at: now_unix,
            last_seen: now_unix,
        });
        Ok(token)
    }

    fn validate(&mut self, token: &str, cfg: &Config, now_unix: u64) -> bool {
        if !cfg.should_run() || !cfg.mini_app.enabled {
            self.entries.clear();
            self.ws_tickets.clear();
            return false;
        }
        let Some(owner) = cfg.allowed_user.as_ref() else {
            self.entries.clear();
            self.ws_tickets.clear();
            return false;
        };
        let fp = bot_token_fingerprint(&cfg.bot_token);
        self.prune_expired(now_unix);
        for entry in &mut self.entries {
            if entry.user_id == owner.user_id
                && entry.paired_at == owner.paired_at
                && entry.token_fp == fp
                && ct_eq(entry.token.as_bytes(), token.as_bytes())
            {
                entry.last_seen = now_unix;
                return true;
            }
        }
        false
    }

    fn issue_ws_ticket(
        &mut self,
        session_token: &str,
        card_id: &str,
        now_unix: u64,
    ) -> Result<String, String> {
        let ticket = super_desktop::control::random_hex(24).map_err(|e| e.to_string())?;
        self.prune_expired(now_unix);
        while self.ws_tickets.len() >= MAX_WS_TICKETS {
            self.ws_tickets.pop_front();
        }
        self.ws_tickets.push_back(WsTicketEntry {
            ticket: ticket.clone(),
            session_token: session_token.to_string(),
            card_id: card_id.to_string(),
            expires_at: now_unix.saturating_add(WS_TICKET_TTL_SECS),
        });
        Ok(ticket)
    }

    fn consume_ws_ticket(
        &mut self,
        ticket: &str,
        card_id: &str,
        cfg: &Config,
        now_unix: u64,
    ) -> Option<String> {
        self.prune_expired(now_unix);
        let pos = self.ws_tickets.iter().position(|t| {
            t.card_id == card_id
                && now_unix <= t.expires_at
                && ct_eq(t.ticket.as_bytes(), ticket.as_bytes())
        })?;
        let entry = self.ws_tickets.remove(pos)?;
        self.validate(&entry.session_token, cfg, now_unix)
            .then_some(entry.session_token)
    }
}

struct ConnGuard(Arc<AtomicUsize>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct MiniAppServer {
    pub listen_port: u16,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl MiniAppServer {
    pub fn spawn(
        listen_port: u16,
        cfg_reader: Arc<dyn Fn() -> Config + Send + Sync>,
        backend: Arc<dyn DesktopBackend + Send + Sync>,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", listen_port))
            .map_err(|e| format!("Could not bind Mini App server on 127.0.0.1:{listen_port}: {e}"))?;
        let bound_port = listener
            .local_addr()
            .map_err(|e| e.to_string())?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("Could not configure Mini App listener: {e}"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_loop = Arc::clone(&stop);
        let sessions = Arc::new(Mutex::new(SessionStore::default()));
        let active_conns = Arc::new(AtomicUsize::new(0));

        let join = thread::Builder::new()
            .name("tg-miniapp".into())
            .spawn(move || {
                while !stop_loop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if active_conns.load(Ordering::SeqCst) >= MAX_CONNS {
                                drop(stream);
                                continue;
                            }
                            let _ = stream.set_nonblocking(false);
                            let conns = Arc::clone(&active_conns);
                            let cfg_reader = Arc::clone(&cfg_reader);
                            let backend = Arc::clone(&backend);
                            let sessions = Arc::clone(&sessions);
                            let stop_conn = Arc::clone(&stop_loop);
                            conns.fetch_add(1, Ordering::SeqCst);
                            let conn_guard = ConnGuard(conns);
                            let _ = thread::Builder::new()
                                .name("tg-miniapp-conn".into())
                                .spawn(move || {
                                    let _conn_guard = conn_guard;
                                    handle_connection(
                                        stream,
                                        cfg_reader,
                                        backend,
                                        sessions,
                                        stop_conn,
                                    );
                                });
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(_) => {
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            })
            .map_err(|e| format!("Could not start Mini App server thread: {e}"))?;

        Ok(Self {
            listen_port: bound_port,
            stop,
            join: Some(join),
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for MiniAppServer {
    fn drop(&mut self) {
        self.stop();
    }
}

struct ParsedHttpHead {
    method: String,
    path: String,
    query: String,
    headers: BTreeMap<String, String>,
    content_len: usize,
    initial_body: Vec<u8>,
}

fn valid_http_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'))
}

fn read_http_headers(stream: &mut TcpStream) -> Option<ParsedHttpHead> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = vec![0u8; MAX_HEADER_BYTES];
    let mut total = 0usize;
    let header_end = loop {
        let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
            return None;
        };
        let _ = stream.set_read_timeout(Some(rem));
        let n = stream.read(&mut buf[total..]).ok()?;
        if n == 0 {
            return None;
        }
        total += n;
        if let Some(pos) = buf[..total].windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if total >= MAX_HEADER_BYTES {
            return None;
        }
    };
    let head = std::str::from_utf8(&buf[..header_end - 4]).ok()?;
    let mut lines = head.split("\r\n");
    let req_line = lines.next()?;
    let mut parts = req_line.split(' ');
    let method = parts.next()?.to_ascii_uppercase();
    let raw_target = parts.next()?;
    let version = parts.next()?;
    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return None;
    }
    let (path, query) = match raw_target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (raw_target.to_string(), String::new()),
    };
    let mut headers = BTreeMap::new();
    for line in lines {
        let (k, v) = line.split_once(':')?;
        if !valid_http_header_name(k) {
            return None;
        }
        let key = k.to_ascii_lowercase();
        if headers.insert(key, v.trim().to_string()).is_some() {
            return None;
        }
    }
    if headers.contains_key("transfer-encoding") {
        return None;
    }
    let content_len: usize = match headers.get("content-length") {
        Some(v) => v.trim().parse::<usize>().ok()?,
        None => 0,
    };
    Some(ParsedHttpHead {
        method,
        path,
        query,
        headers,
        content_len,
        initial_body: buf[header_end..total].to_vec(),
    })
}

fn read_http_body(
    stream: &mut TcpStream,
    head: &ParsedHttpHead,
    max_body: usize,
    deadline_secs: u64,
) -> Option<Vec<u8>> {
    if head.content_len > max_body {
        return None;
    }
    let mut body = Vec::with_capacity(head.content_len.min(64 * 1024));
    let initial = head.initial_body.len().min(head.content_len);
    body.extend_from_slice(&head.initial_body[..initial]);
    let deadline = Instant::now() + Duration::from_secs(deadline_secs);
    while body.len() < head.content_len {
        let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
            return None;
        };
        let _ = stream.set_read_timeout(Some(rem));
        let mut tmp = vec![0u8; (head.content_len - body.len()).min(16 * 1024)];
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some(body)
}

fn write_http_response_with_headers(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    content_type: &str,
    csp: &str,
    extra_headers: &str,
    body: &[u8],
) {
    let clean_ct: String = content_type
        .chars()
        .filter(|&c| c != '\r' && c != '\n')
        .collect();
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: {clean_ct}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: {csp}\r\n\
         {extra_headers}\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn write_http_response(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) {
    write_http_response_with_headers(
        stream,
        code,
        reason,
        content_type,
        "default-src 'none'; frame-ancestors 'none';",
        "",
        body,
    );
}

fn serve_ui_html(stream: &mut TcpStream) {
    let Ok(nonce) = super_desktop::control::random_hex(16) else {
        reply_err(stream, 500, "Internal Server Error", "RNG unavailable.");
        return;
    };
    let html = UI_HTML.replace("<script>", &format!("<script nonce=\"{nonce}\">"));
    let csp = format!(
        "default-src 'self'; script-src 'nonce-{nonce}' https://telegram.org; \
         style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
         connect-src 'self' ws: wss:; base-uri 'none'; object-src 'none'; \
         form-action 'none'; frame-ancestors https://web.telegram.org https://*.telegram.org;"
    );
    write_http_response_with_headers(
        stream,
        200,
        "OK",
        "text/html; charset=utf-8",
        &csp,
        "",
        html.as_bytes(),
    );
}

fn reply_json(stream: &mut TcpStream, code: u16, reason: &str, value: &Value) {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    write_http_response(stream, code, reason, "application/json; charset=utf-8", &bytes);
}

fn reply_err(stream: &mut TcpStream, code: u16, reason: &str, message: &str) {
    reply_json(stream, code, reason, &json!({ "error": message }));
}

fn query_param(query: &str, wanted: &str) -> Option<String> {
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if decode_form_component(k).ok().as_deref() == Some(wanted) {
            return decode_form_component(v).ok();
        }
    }
    None
}

fn extract_bearer_token(headers: &BTreeMap<String, String>) -> Option<String> {
    let auth = headers.get("authorization")?;
    let tok = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))?;
    let trimmed = tok.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn split_origin(origin: &str) -> Option<(&str, &str)> {
    if let Some(rest) = origin.strip_prefix("https://") {
        (!rest.contains('/')).then_some(("https", rest))
    } else if let Some(rest) = origin.strip_prefix("http://") {
        (!rest.contains('/')).then_some(("http", rest))
    } else {
        None
    }
}

fn allowed_origin(headers: &BTreeMap<String, String>, cfg: &Config) -> bool {
    if let Some(sfs) = headers.get("sec-fetch-site") {
        if sfs.eq_ignore_ascii_case("cross-site") {
            return false;
        }
    }
    let Some(origin) = headers.get("origin") else {
        return true;
    };
    let Some((o_scheme, o_host)) = split_origin(origin) else {
        return false;
    };
    if let Some(pub_url) = cfg.mini_app.normalized_url() {
        if let Some((p_scheme, p_host)) = split_origin(&pub_url) {
            if o_scheme.eq_ignore_ascii_case(p_scheme) && o_host.eq_ignore_ascii_case(p_host) {
                return true;
            }
        }
    }
    headers
        .get("host")
        .is_some_and(|h| h.eq_ignore_ascii_case(o_host))
}

fn redact_card_field(raw: &str, cfg: &Config) -> String {
    let masked = mask_token_in_text(raw, &cfg.bot_token);
    if cfg.permissions.redact_secrets {
        redact_known_secrets(&masked)
    } else {
        masked
    }
}

fn card_json(card: &CardSummary, cfg: &Config) -> Value {
    let quick_cmds: Vec<&str> = if card.is_shell {
        Vec::new()
    } else {
        quick_slash_commands(&card.agent_type).to_vec()
    };
    let clean_title = card.title.as_deref().map(|t| redact_card_field(t, cfg));
    json!({
        "id": card.id,
        "agentType": card.agent_type,
        "agentName": card.agent_name,
        "icon": card.icon,
        "status": card.status,
        "label": card.label,
        "model": card.model,
        "effort": card.effort,
        "title": clean_title,
        "directoryDisplay": card.directory_display,
        "tag": card.tag,
        "isShell": card.is_shell,
        "quickSlashCommands": quick_cmds,
    })
}

fn valid_hex_color(raw: &str, fallback: &str) -> String {
    let s = raw.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if (hex.len() == 6 || hex.len() == 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return s.to_string();
        }
    }
    fallback.to_string()
}

fn usage_item_json(p: &super::desktop::ProviderUsageSummary) -> Value {
    let limits: Vec<Value> = {
        let mut ordered: Vec<&super::desktop::ProviderUsageLimit> =
            p.limits.iter().filter(|l| l.is_weekly()).collect();
        ordered.extend(p.limits.iter().filter(|l| !l.is_weekly()));
        ordered
            .into_iter()
            .map(|l| {
                json!({
                    "label": l.label,
                    "isWeekly": l.is_weekly(),
                    "usedPercent": l.used_percent_int(),
                    "leftPercent": l.left_percent_int(),
                    "resetsIn": crate::usage::format_reset_in(&l.resets_at),
                    "used": l.used,
                    "allowance": l.allowance,
                })
            })
            .collect()
    };
    let activity = if p.today_tokens > 0 || p.today_prompts > 0 || p.today_sessions > 0 {
        let mut parts = Vec::new();
        if p.today_tokens > 0 {
            parts.push(format!("{} tokens", crate::usage::format_tokens(p.today_tokens)));
        }
        if p.today_prompts > 0 {
            parts.push(format!("{} prompts", p.today_prompts));
        }
        if p.today_sessions > 0 {
            parts.push(format!("{} sessions", p.today_sessions));
        }
        format!("Today: {}", parts.join(" · "))
    } else if p.total_prompts > 0 || p.total_sessions > 0 {
        format!("Lifetime: {} prompts · {} sessions", p.total_prompts, p.total_sessions)
    } else {
        String::new()
    };
    json!({
        "id": p.id,
        "name": p.name,
        "tier": p.tier,
        "ready": p.ready,
        "limits": limits,
        "statusText": p.status_text,
        "activity": activity,
    })
}

fn check_card_input_perm(cfg: &Config, card: &CardSummary, need: &str) -> Result<(), &'static str> {
    let allowed = match need {
        "prompt" => cfg.permissions.prompts,
        "control" => cfg.permissions.controls,
        "attachment" => cfg.permissions.attachments && cfg.permissions.prompts,
        _ => false,
    };
    if !allowed {
        return Err("Action is disabled in Settings → Telegram.");
    }
    if card.is_shell && !cfg.permissions.shell_input {
        return Err("Allow shell input is disabled in Settings → Telegram.");
    }
    Ok(())
}

fn build_bootstrap_json(cfg: &Config, backend: &dyn DesktopBackend) -> Value {
    let cards: Vec<Value> = backend
        .list_cards()
        .iter()
        .map(|c| card_json(c, cfg))
        .collect();
    let usage: Vec<Value> = backend.usage_summary().iter().map(usage_item_json).collect();
    let (workspaces, harnesses) = if cfg.permissions.launch {
        let ws = backend
            .workspace_choices()
            .map(|w| json!({ "current": w.current, "recent": w.recent }))
            .unwrap_or_else(|_| json!({ "current": "", "recent": [] }));
        let hs: Vec<Value> = backend
            .available_harnesses()
            .into_iter()
            .map(|h| json!({ "id": h.id, "name": h.name, "icon": h.icon }))
            .collect();
        (ws, hs)
    } else {
        (json!({ "current": "", "recent": [] }), Vec::new())
    };
    let t = crate::theme::current_theme_fresh();
    let dark_bg = valid_hex_color(&t.dark_background, "#1c1b1a");
    let bg = valid_hex_color(&t.background, "#10100f");
    let fg = valid_hex_color(&t.foreground, "#ece8e5");
    let theme = json!({
        "name": t.name,
        "mode": if t.mode == "light" { "light" } else { "dark" },
        "background": bg,
        "darkBackground": dark_bg,
        "darkerBackground": valid_hex_color(&t.darker_background, &dark_bg),
        "lighterBackground": valid_hex_color(&t.lighter_background, &bg),
        "selection": valid_hex_color(&t.selection, "#22201d"),
        "foreground": fg,
        "muted": valid_hex_color(&t.muted, "#8f8a84"),
        "accent": valid_hex_color(&t.accent, "#5ca3ad"),
        "green": valid_hex_color(&t.green, "#88ad7c"),
        "yellow": valid_hex_color(&t.yellow, "#dbb471"),
        "red": valid_hex_color(&t.red, "#d95750"),
        "orange": valid_hex_color(&t.orange, "#d47844"),
        "cyan": valid_hex_color(&t.cyan, "#5ca3ad"),
        "blue": valid_hex_color(&t.blue, "#6b93b8"),
        "magenta": valid_hex_color(&t.magenta, "#a688b5"),
        "ansi16": [
            valid_hex_color(&t.dark_background, "#1c1b1a"),
            valid_hex_color(&t.red, "#d95750"),
            valid_hex_color(&t.green, "#88ad7c"),
            valid_hex_color(&t.yellow, "#dbb471"),
            valid_hex_color(&t.blue, "#6b93b8"),
            valid_hex_color(&t.magenta, "#a688b5"),
            valid_hex_color(&t.cyan, "#5ca3ad"),
            valid_hex_color(&t.foreground, "#d1cdc9"),
            valid_hex_color(&t.muted, "#736f6b"),
            valid_hex_color(&t.bright_red, "#ea6861"),
            valid_hex_color(&t.bright_green, "#9ec292"),
            valid_hex_color(&t.bright_yellow, "#e8c484"),
            valid_hex_color(&t.bright_blue, "#84acd2"),
            valid_hex_color(&t.bright_magenta, "#ba9dca"),
            valid_hex_color(&t.bright_cyan, "#72bac5"),
            valid_hex_color(&t.bright_foreground, "#ece8e5"),
        ],
    });
    let p = cfg.permissions;
    json!({
        "cards": cards,
        "usage": usage,
        "workspaces": workspaces,
        "harnesses": harnesses,
        "theme": theme,
        "permissions": {
            "readOutput": p.read_output,
            "prompts": p.prompts,
            "controls": p.controls,
            "shellInput": p.shell_input,
            "launch": p.launch,
            "close": p.close,
            "attachments": p.attachments,
            "redactSecrets": p.redact_secrets,
        }
    })
}

fn handle_connection(
    mut stream: TcpStream,
    cfg_reader: Arc<dyn Fn() -> Config + Send + Sync>,
    backend: Arc<dyn DesktopBackend + Send + Sync>,
    sessions: Arc<Mutex<SessionStore>>,
    stop: Arc<AtomicBool>,
) {
    let Some(req) = read_http_headers(&mut stream) else {
        return;
    };
    let cfg = cfg_reader();
    if req.method == "GET" && (req.path == "/" || req.path == "/index.html") {
        serve_ui_html(&mut stream);
        return;
    }
    if req.method == "GET" && req.path == "/health" {
        reply_json(&mut stream, 200, "OK", &json!({ "ok": true, "miniApp": true }));
        return;
    }

    if !cfg.should_run() || !cfg.mini_app.enabled {
        reply_err(
            &mut stream,
            503,
            "Service Unavailable",
            "Telegram Mini App is disabled in Settings → Telegram.",
        );
        return;
    }
    let Some(owner) = cfg.allowed_user.as_ref() else {
        reply_err(
            &mut stream,
            403,
            "Forbidden",
            "Pair your Telegram account in Settings → Telegram first.",
        );
        return;
    };

    if (req.method != "GET" && req.method != "HEAD") || req.path.starts_with("/ws/terminal/") {
        if !allowed_origin(&req.headers, &cfg) {
            reply_err(&mut stream, 403, "Forbidden", "Cross-origin request rejected.");
            return;
        }
    }

    if req.method == "POST" && req.path == "/api/auth" {
        let Some(body_bytes) = read_http_body(&mut stream, &req, MAX_JSON_BODY_BYTES, 10) else {
            reply_err(&mut stream, 400, "Bad Request", "Invalid or oversized auth body.");
            return;
        };
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct AuthReq {
            init_data: String,
        }
        let Ok(body) = serde_json::from_slice::<AuthReq>(&body_bytes) else {
            reply_err(&mut stream, 400, "Bad Request", "Expected JSON {initData}.");
            return;
        };
        let now = unix_now();
        let verified = match verify_init_data(
            &body.init_data,
            &cfg.bot_token,
            owner.user_id,
            now,
            INIT_DATA_MAX_AGE_SECS,
        ) {
            Ok(v) => v,
            Err(err) => {
                reply_err(&mut stream, 401, "Unauthorized", &err);
                return;
            }
        };
        let Ok(token) = sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .issue(&cfg, verified.user_id, owner.paired_at, now)
        else {
            reply_err(
                &mut stream,
                500,
                "Internal Server Error",
                "Could not issue session token.",
            );
            return;
        };
        reply_json(
            &mut stream,
            200,
            "OK",
            &json!({
                "token": token,
                "user": { "id": verified.user_id, "firstName": verified.first_name, "username": verified.username },
                "startParam": verified.start_param,
            }),
        );
        return;
    }

    let now = unix_now();

    // WebSocket terminal stream authenticates with a single-use short-lived ticket from POST /api/ws-ticket.
    if req.method == "GET" {
        if let Some(card_id) = req.path.strip_prefix("/ws/terminal/") {
            let is_upgrade = req
                .headers
                .get("upgrade")
                .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
                && req.headers.get("connection").is_some_and(|v| {
                    v.split(',')
                        .any(|tok| tok.trim().eq_ignore_ascii_case("upgrade"))
                });
            let Some(ws_key) = req.headers.get("sec-websocket-key").cloned() else {
                reply_err(&mut stream, 400, "Bad Request", "Missing Sec-WebSocket-Key.");
                return;
            };
            if !is_upgrade {
                reply_err(&mut stream, 400, "Bad Request", "Expected WebSocket upgrade.");
                return;
            }
            if !cfg.permissions.read_output {
                reply_err(&mut stream, 403, "Forbidden", "Read terminal output is disabled.");
                return;
            }
            let Some(ticket) = query_param(&req.query, "ticket").filter(|t| !t.trim().is_empty()) else {
                reply_err(&mut stream, 401, "Unauthorized", "Missing WebSocket ticket.");
                return;
            };
            let Some(tok) = sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .consume_ws_ticket(&ticket, card_id, &cfg, now)
            else {
                reply_err(&mut stream, 401, "Unauthorized", "Invalid or expired WebSocket ticket.");
                return;
            };
            if crate::ws::handshake(&mut stream, &ws_key).is_ok() {
                run_ws_stream(stream, card_id, &tok, cfg_reader, backend, sessions, stop);
            }
            return;
        }
    }

    // All remaining /api/* routes require a Bearer session token in the Authorization header.
    let Some(tok) = extract_bearer_token(&req.headers) else {
        reply_err(&mut stream, 401, "Unauthorized", "Missing session token.");
        return;
    };
    if !sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .validate(&tok, &cfg, now)
    {
        reply_err(
            &mut stream,
            401,
            "Unauthorized",
            "Session expired or revoked — reopen Mini App.",
        );
        return;
    }

    if req.method == "POST" && req.path == "/api/ws-ticket" {
        if !cfg.permissions.read_output {
            reply_err(&mut stream, 403, "Forbidden", "Read terminal output is disabled.");
            return;
        }
        let Some(body_bytes) = read_http_body(&mut stream, &req, MAX_JSON_BODY_BYTES, 10) else {
            reply_err(&mut stream, 400, "Bad Request", "Invalid or oversized body.");
            return;
        };
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct TicketReq {
            card_id: String,
        }
        let Ok(body) = serde_json::from_slice::<TicketReq>(&body_bytes) else {
            reply_err(&mut stream, 400, "Bad Request", "Expected JSON {cardId}.");
            return;
        };
        let cards = backend.list_cards();
        if !cards.iter().any(|c| c.id == body.card_id) {
            reply_err(&mut stream, 404, "Not Found", "Card is no longer running.");
            return;
        }
        let Ok(ticket) = sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .issue_ws_ticket(&tok, &body.card_id, now)
        else {
            reply_err(&mut stream, 500, "Internal Server Error", "Could not issue WebSocket ticket.");
            return;
        };
        reply_json(
            &mut stream,
            200,
            "OK",
            &json!({ "ticket": ticket, "expiresIn": WS_TICKET_TTL_SECS }),
        );
        return;
    }

    if req.method == "GET" && req.path == "/api/bootstrap" {
        let payload = build_bootstrap_json(&cfg, backend.as_ref());
        reply_json(&mut stream, 200, "OK", &payload);
        return;
    }

    if req.method == "POST" && req.path == "/api/cards/launch" {
        if !cfg.permissions.launch {
            reply_err(&mut stream, 403, "Forbidden", "Launch is disabled in Settings → Telegram.");
            return;
        }
        let Some(body_bytes) = read_http_body(&mut stream, &req, MAX_JSON_BODY_BYTES, 10) else {
            reply_err(&mut stream, 400, "Bad Request", "Invalid or oversized launch body.");
            return;
        };
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LaunchReq {
            agent_type: String,
            workspace: String,
        }
        let Ok(body) = serde_json::from_slice::<LaunchReq>(&body_bytes) else {
            reply_err(&mut stream, 400, "Bad Request", "Invalid launch payload.");
            return;
        };
        match backend.launch_card(&body.agent_type, &body.workspace) {
            Ok(id) => reply_json(&mut stream, 200, "OK", &json!({ "ok": true, "id": id })),
            Err(err) => reply_err(&mut stream, 400, "Bad Request", &err),
        }
        return;
    }

    let Some(rest) = req.path.strip_prefix("/api/cards/") else {
        reply_err(&mut stream, 404, "Not Found", "Unknown endpoint.");
        return;
    };

    if let Some((card_id, tail)) = rest.split_once('/') {
        let cards = backend.list_cards();
        let Some(card) = cards.iter().find(|c| c.id == card_id) else {
            reply_err(&mut stream, 404, "Not Found", "Card is no longer running.");
            return;
        };
        if req.method == "GET" && tail == "screen" {
            if !cfg.permissions.read_output {
                reply_err(&mut stream, 403, "Forbidden", "Read terminal output is disabled.");
                return;
            }
            match backend.capture_ansi_screen(card_id, 200) {
                Ok(raw) => {
                    let ansi = redact_ansi_preserving(&raw, &cfg.bot_token, cfg.permissions.redact_secrets);
                    reply_json(&mut stream, 200, "OK", &json!({ "ansi": ansi, "card": card_json(card, &cfg) }));
                }
                Err(err) => reply_err(&mut stream, 500, "Internal Server Error", &err),
            }
            return;
        }
        if req.method == "GET" && tail == "references" {
            if !cfg.permissions.read_output {
                reply_err(&mut stream, 403, "Forbidden", "Read terminal output is disabled.");
                return;
            }
            match backend.list_references(card_id) {
                Ok(refs) => {
                    let assets: Vec<Value> = refs.assets.into_iter().map(|a| json!({
                        "id": a.id, "name": a.name, "relativePath": a.relative_path,
                        "mimeType": a.mime_type, "kind": a.kind, "size": a.size,
                    })).collect();
                    let links: Vec<String> = refs.links.into_iter()
                        .map(|l| redact_link_text(&l, &cfg.bot_token, cfg.permissions.redact_secrets).0)
                        .collect();
                    reply_json(&mut stream, 200, "OK", &json!({ "assets": assets, "links": links }));
                }
                Err(err) => reply_err(&mut stream, 400, "Bad Request", &err),
            }
            return;
        }
        if let Some(asset_id) = (req.method == "GET").then(|| tail.strip_prefix("asset/")).flatten() {
            if !cfg.permissions.read_output {
                reply_err(&mut stream, 403, "Forbidden", "Read terminal output is disabled.");
                return;
            }
            match backend.read_asset(card_id, asset_id) {
                Ok((asset, bytes)) => {
                    let extra = "Content-Disposition: attachment; filename=\"download\"\r\n";
                    let sandbox_csp = "default-src 'none'; sandbox;";
                    if asset.kind == "pdf" || asset.mime_type.eq_ignore_ascii_case("application/pdf") {
                        reply_err(
                            &mut stream,
                            415,
                            "Unsupported Media Type",
                            "Use /pdf/<id>/<page> for PDF previews.",
                        );
                    } else if asset.kind == "text"
                        || asset.kind == "markdown"
                        || asset.mime_type.starts_with("text/")
                    {
                        let raw = String::from_utf8_lossy(&bytes);
                        let clean =
                            sanitize_terminal_text(&raw, &cfg.bot_token, cfg.permissions.redact_secrets);
                        write_http_response_with_headers(
                            &mut stream,
                            200,
                            "OK",
                            "text/plain; charset=utf-8",
                            sandbox_csp,
                            extra,
                            clean.as_bytes(),
                        );
                    } else {
                        write_http_response_with_headers(
                            &mut stream,
                            200,
                            "OK",
                            &asset.mime_type,
                            sandbox_csp,
                            extra,
                            &bytes,
                        );
                    }
                }
                Err(err) => reply_err(&mut stream, 404, "Not Found", &err),
            }
            return;
        }
        if let Some(pdf_rest) = (req.method == "GET").then(|| tail.strip_prefix("pdf/")).flatten() {
            if !cfg.permissions.read_output {
                reply_err(&mut stream, 403, "Forbidden", "Read terminal output is disabled.");
                return;
            }
            if let Some((asset_id, page_str)) = pdf_rest.split_once('/') {
                if let Ok(page) = page_str.parse::<u32>() {
                    match backend.render_pdf_page(card_id, asset_id, page.max(1)) {
                        Ok((_asset, png)) => write_http_response(&mut stream, 200, "OK", "image/png", &png),
                        Err(err) => reply_err(&mut stream, 404, "Not Found", &err),
                    }
                    return;
                }
            }
            reply_err(&mut stream, 400, "Bad Request", "Invalid PDF page request.");
            return;
        }
        if req.method == "POST" && tail == "prompt" {
            if let Err(msg) = check_card_input_perm(&cfg, card, "prompt") {
                reply_err(&mut stream, 403, "Forbidden", msg);
                return;
            }
            let Some(body_bytes) = read_http_body(&mut stream, &req, MAX_JSON_BODY_BYTES, 10) else {
                reply_err(&mut stream, 400, "Bad Request", "Invalid or oversized prompt body.");
                return;
            };
            #[derive(Deserialize)]
            struct PromptReq {
                text: String,
            }
            let Ok(body) = serde_json::from_slice::<PromptReq>(&body_bytes) else {
                reply_err(&mut stream, 400, "Bad Request", "Expected JSON {text}.");
                return;
            };
            match backend.send_prompt(card_id, body.text.trim()) {
                Ok(()) => reply_json(&mut stream, 200, "OK", &json!({ "ok": true })),
                Err(err) => reply_err(&mut stream, 400, "Bad Request", &err),
            }
            return;
        }
        if req.method == "POST" && tail == "key" {
            if let Err(msg) = check_card_input_perm(&cfg, card, "control") {
                reply_err(&mut stream, 403, "Forbidden", msg);
                return;
            }
            let Some(body_bytes) = read_http_body(&mut stream, &req, MAX_JSON_BODY_BYTES, 10) else {
                reply_err(&mut stream, 400, "Bad Request", "Invalid or oversized key body.");
                return;
            };
            #[derive(Deserialize)]
            struct KeyReq {
                code: String,
            }
            let Ok(body) = serde_json::from_slice::<KeyReq>(&body_bytes) else {
                reply_err(&mut stream, 400, "Bad Request", "Expected JSON {code}.");
                return;
            };
            let Some(key) = NamedKey::from_code(body.code.trim()) else {
                reply_err(&mut stream, 400, "Bad Request", "Unknown key code.");
                return;
            };
            match backend.send_key(card_id, key) {
                Ok(()) => reply_json(&mut stream, 200, "OK", &json!({ "ok": true })),
                Err(err) => reply_err(&mut stream, 400, "Bad Request", &err),
            }
            return;
        }
        if req.method == "POST" && tail == "attachment" {
            if let Err(msg) = check_card_input_perm(&cfg, card, "attachment") {
                reply_err(&mut stream, 403, "Forbidden", msg);
                return;
            }
            if req.content_len > MAX_UPLOAD_BODY_BYTES {
                reply_err(&mut stream, 413, "Payload Too Large", "Attachment exceeds size limit.");
                return;
            }
            let Some(body_bytes) = read_http_body(&mut stream, &req, MAX_UPLOAD_BODY_BYTES, 120) else {
                reply_err(&mut stream, 400, "Bad Request", "Incomplete or timed out upload body.");
                return;
            };
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct AttachReq {
                name: String,
                #[serde(default)]
                caption: String,
                data_base64: String,
                #[serde(default)]
                is_image: bool,
            }
            let Ok(body) = serde_json::from_slice::<AttachReq>(&body_bytes) else {
                reply_err(&mut stream, 400, "Bad Request", "Invalid attachment payload.");
                return;
            };
            let Ok(raw_bytes) = STANDARD.decode(body.data_base64.trim()) else {
                reply_err(&mut stream, 400, "Bad Request", "Invalid base64 payload.");
                return;
            };
            if raw_bytes.len() > MAX_TOTAL {
                reply_err(&mut stream, 413, "Payload Too Large", "Attachment exceeds size limit.");
                return;
            }
            let owner_key = format!("tg_{}", owner.user_id);
            let att_res = if body.is_image {
                normalize_telegram_image(&raw_bytes)
            } else {
                Ok(Attachment {
                    kind: Kind::File,
                    name: safe_name(&body.name, Kind::File),
                    bytes: raw_bytes,
                })
            };
            match att_res.and_then(|att| {
                backend.send_attachment(card_id, &owner_key, body.caption.trim(), att)
            }) {
                Ok(()) => reply_json(&mut stream, 200, "OK", &json!({ "ok": true })),
                Err(err) => reply_err(&mut stream, 400, "Bad Request", &err),
            }
            return;
        }
        if req.method == "POST" && tail == "close" {
            if !cfg.permissions.close {
                reply_err(&mut stream, 403, "Forbidden", "Closing cards is disabled in Settings → Telegram.");
                return;
            }
            match backend.close_card(card_id) {
                Ok(()) => reply_json(&mut stream, 200, "OK", &json!({ "ok": true })),
                Err(err) => reply_err(&mut stream, 400, "Bad Request", &err),
            }
            return;
        }
    }

    reply_err(&mut stream, 404, "Not Found", "Unknown card route.");
}

enum WsClientEvent {
    Frame(crate::ws::Frame),
    Closed,
}

fn run_ws_stream(
    mut stream: TcpStream,
    card_id: &str,
    token: &str,
    cfg_reader: Arc<dyn Fn() -> Config + Send + Sync>,
    backend: Arc<dyn DesktopBackend + Send + Sync>,
    sessions: Arc<Mutex<SessionStore>>,
    stop: Arc<AtomicBool>,
) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(mut reader_stream) = stream.try_clone() else {
        return;
    };
    let (tx, rx) = mpsc::sync_channel::<WsClientEvent>(32);
    let reader_stop = Arc::clone(&stop);
    let reader_handle = thread::spawn(move || {
        while !reader_stop.load(Ordering::SeqCst) {
            match crate::ws::read_frame(&mut reader_stream) {
                Ok(Some(frame)) => {
                    let is_close = matches!(frame, crate::ws::Frame::Close);
                    if tx.send(WsClientEvent::Frame(frame)).is_err() || is_close {
                        break;
                    }
                }
                Ok(None) | Err(_) => {
                    let _ = tx.send(WsClientEvent::Closed);
                    break;
                }
            }
        }
    });

    let stream_started = Instant::now();
    let mut last_ping = Instant::now();
    let mut last_payload = String::new();
    let mut last_auth_check = Instant::now() - Duration::from_secs(10);
    while !stop.load(Ordering::SeqCst) {
        if stream_started.elapsed() >= Duration::from_secs(WS_MAX_LIFETIME_SECS) {
            let _ = crate::ws::write_close(&mut stream, 1000, "session_limit");
            break;
        }

        let mut pending: Vec<WsClientEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            pending.push(ev);
        }
        if pending.is_empty() && !last_payload.is_empty() {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(120)) {
                pending.push(ev);
                while let Ok(more) = rx.try_recv() {
                    pending.push(more);
                }
            }
        }
        for ev in pending {
            match ev {
                WsClientEvent::Closed => {
                    let _ = stream.shutdown(Shutdown::Both);
                    let _ = reader_handle.join();
                    return;
                }
                WsClientEvent::Frame(crate::ws::Frame::Close) => {
                    let _ = crate::ws::write_close(&mut stream, 1000, "bye");
                    let _ = stream.shutdown(Shutdown::Both);
                    let _ = reader_handle.join();
                    return;
                }
                WsClientEvent::Frame(crate::ws::Frame::Ping(p)) => {
                    let _ = crate::ws::write_pong(&mut stream, &p);
                }
                WsClientEvent::Frame(crate::ws::Frame::Text(txt)) => {
                    let cfg = cfg_reader();
                    if !sessions
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .validate(token, &cfg, unix_now())
                        || !cfg.permissions.read_output
                    {
                        let _ = crate::ws::write_close(&mut stream, 4001, "unauthorized");
                        let _ = stream.shutdown(Shutdown::Both);
                        let _ = reader_handle.join();
                        return;
                    }
                    last_auth_check = Instant::now();
                    if let Ok(val) = serde_json::from_str::<Value>(&txt) {
                        let cards = backend.list_cards();
                        if let Some(card) = cards.iter().find(|c| c.id == card_id) {
                            match val["type"].as_str() {
                                Some("key") => {
                                    if check_card_input_perm(&cfg, card, "control").is_ok() {
                                        if let Some(key) = val["code"].as_str().and_then(NamedKey::from_code) {
                                            let _ = backend.send_key(card_id, key);
                                            last_payload.clear();
                                        }
                                    }
                                }
                                Some("prompt") => {
                                    if check_card_input_perm(&cfg, card, "prompt").is_ok() {
                                        if let Some(prompt) = val["text"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                                            let _ = backend.send_prompt(card_id, prompt);
                                            last_payload.clear();
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let cfg = cfg_reader();
        if last_auth_check.elapsed() >= Duration::from_secs(2) {
            last_auth_check = Instant::now();
            if !sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .validate(token, &cfg, unix_now())
                || !cfg.permissions.read_output
            {
                let _ = crate::ws::write_close(&mut stream, 4001, "unauthorized");
                break;
            }
        }
        if last_ping.elapsed() >= Duration::from_secs(30) {
            last_ping = Instant::now();
            if crate::ws::write_ping(&mut stream, b"kp").is_err() {
                break;
            }
        }

        let cards = backend.list_cards();
        let Some(card) = cards.iter().find(|c| c.id == card_id) else {
            let _ = crate::ws::write_close(&mut stream, 1000, "card_closed");
            break;
        };
        if let Ok(raw) = backend.capture_ansi_screen(card_id, 200) {
            let ansi = redact_ansi_preserving(&raw, &cfg.bot_token, cfg.permissions.redact_secrets);
            let frame = serde_json::to_string(&json!({
                "type": "frame",
                "ansi": ansi,
                "card": card_json(card, &cfg),
            }))
            .unwrap_or_default();
            if frame != last_payload {
                if crate::ws::write_text(&mut stream, &frame).is_err() {
                    break;
                }
                last_payload = frame;
            }
        }
    }

    let _ = stream.shutdown(Shutdown::Both);
    let _ = reader_handle.join();
}
