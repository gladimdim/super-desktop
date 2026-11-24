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
use super::redact::redact_ansi_preserving;
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
    token_fp: [u8; 32],
    last_seen: u64,
}

#[derive(Default)]
struct SessionStore {
    entries: VecDeque<SessionEntry>,
}

fn bot_token_fingerprint(bot_token: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bot_token.trim().as_bytes());
    h.finalize().into()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

impl SessionStore {
    fn issue(&mut self, cfg: &Config, user_id: i64, now_unix: u64) -> Result<String, String> {
        let token = super_desktop::control::random_hex(32).map_err(|e| e.to_string())?;
        let fp = bot_token_fingerprint(&cfg.bot_token);
        self.entries
            .retain(|e| now_unix.saturating_sub(e.last_seen) <= SESSION_TTL_SECS);
        while self.entries.len() >= MAX_SESSIONS {
            self.entries.pop_front();
        }
        self.entries.push_back(SessionEntry {
            token: token.clone(),
            user_id,
            token_fp: fp,
            last_seen: now_unix,
        });
        Ok(token)
    }

    fn validate(&mut self, token: &str, cfg: &Config, now_unix: u64) -> bool {
        if !cfg.should_run() || !cfg.mini_app.enabled {
            return false;
        }
        let Some(owner) = cfg.allowed_user.as_ref() else {
            return false;
        };
        let fp = bot_token_fingerprint(&cfg.bot_token);
        self.entries
            .retain(|e| now_unix.saturating_sub(e.last_seen) <= SESSION_TTL_SECS);
        for entry in &mut self.entries {
            if entry.user_id == owner.user_id
                && entry.token_fp == fp
                && ct_eq(entry.token.as_bytes(), token.as_bytes())
            {
                entry.last_seen = now_unix;
                return true;
            }
        }
        false
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
                            let _ = thread::Builder::new()
                                .name("tg-miniapp-conn".into())
                                .spawn(move || {
                                    handle_connection(
                                        stream,
                                        cfg_reader,
                                        backend,
                                        sessions,
                                        stop_conn,
                                    );
                                    conns.fetch_sub(1, Ordering::SeqCst);
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

struct ParsedHttp {
    method: String,
    path: String,
    query: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_http_request(stream: &mut TcpStream) -> Option<ParsedHttp> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = vec![0u8; MAX_HEADER_BYTES];
    let mut total = 0usize;
    let header_end = loop {
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
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let req_line = lines.next()?;
    let mut parts = req_line.split_whitespace();
    let method = parts.next()?.to_ascii_uppercase();
    let raw_target = parts.next()?;
    let (path, query) = match raw_target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (raw_target.to_string(), String::new()),
    };
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            if headers.insert(key, v.trim().to_string()).is_some() {
                return None;
            }
        }
    }
    if headers.contains_key("transfer-encoding") {
        return None;
    }
    let content_len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let max_body = if path.ends_with("/attachment") {
        MAX_UPLOAD_BODY_BYTES
    } else {
        MAX_JSON_BODY_BYTES
    };
    if content_len > max_body {
        return None;
    }
    let mut body = Vec::with_capacity(content_len);
    let initial = total.saturating_sub(header_end).min(content_len);
    body.extend_from_slice(&buf[header_end..header_end + initial]);
    while body.len() < content_len {
        let mut tmp = vec![0u8; (content_len - body.len()).min(16 * 1024)];
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some(ParsedHttp {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn write_http_response(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) {
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline' https://telegram.org; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' ws: wss:; frame-ancestors https://web.telegram.org https://*.telegram.org;\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
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

fn extract_bearer_or_query(req: &ParsedHttp) -> Option<String> {
    if let Some(auth) = req.headers.get("authorization") {
        if let Some(tok) = auth
            .strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("bearer "))
        {
            let trimmed = tok.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    query_param(&req.query, "token").filter(|t| !t.trim().is_empty())
}

fn origin_host(origin: &str) -> Option<&str> {
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))?;
    Some(rest.split('/').next().unwrap_or(""))
}

fn allowed_post_origin(req: &ParsedHttp, cfg: &Config) -> bool {
    let Some(origin) = req.headers.get("origin") else {
        return true;
    };
    let Some(o_host) = origin_host(origin) else {
        return false;
    };
    if req
        .headers
        .get("host")
        .is_some_and(|h| h.eq_ignore_ascii_case(o_host))
    {
        return true;
    }
    if req
        .headers
        .get("x-forwarded-host")
        .is_some_and(|h| h.eq_ignore_ascii_case(o_host))
    {
        return true;
    }
    if let Some(pub_url) = cfg.mini_app.normalized_url() {
        if origin_host(&pub_url).is_some_and(|h| h.eq_ignore_ascii_case(o_host)) {
            return true;
        }
    }
    false
}

fn card_json(card: &CardSummary) -> Value {
    let quick_cmds: Vec<&str> = if card.is_shell {
        Vec::new()
    } else {
        quick_slash_commands(&card.agent_type).to_vec()
    };
    json!({
        "id": card.id,
        "agentType": card.agent_type,
        "agentName": card.agent_name,
        "icon": card.icon,
        "status": card.status,
        "label": card.label,
        "model": card.model,
        "effort": card.effort,
        "title": card.title,
        "directoryDisplay": card.directory_display,
        "tag": card.tag,
        "isShell": card.is_shell,
        "quickSlashCommands": quick_cmds,
    })
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
        "attachment" => cfg.permissions.attachments,
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
    let cards: Vec<Value> = backend.list_cards().iter().map(card_json).collect();
    let usage: Vec<Value> = backend.usage_summary().iter().map(usage_item_json).collect();
    let workspaces = backend
        .workspace_choices()
        .map(|w| json!({ "current": w.current, "recent": w.recent }))
        .unwrap_or_else(|_| json!({ "current": "", "recent": [] }));
    let harnesses: Vec<Value> = backend
        .available_harnesses()
        .into_iter()
        .map(|h| json!({ "id": h.id, "name": h.name, "icon": h.icon }))
        .collect();
    let t = crate::theme::current_theme_fresh();
    let theme = json!({
        "name": t.name,
        "mode": t.mode,
        "background": t.background,
        "darkBackground": t.dark_background,
        "darkerBackground": t.darker_background,
        "lighterBackground": t.lighter_background,
        "selection": t.selection,
        "foreground": t.foreground,
        "muted": t.muted,
        "accent": t.accent,
        "green": t.green,
        "yellow": t.yellow,
        "red": t.red,
        "orange": t.orange,
        "cyan": t.cyan,
        "blue": t.blue,
        "magenta": t.magenta,
        "ansi16": [
            t.dark_background, t.red, t.green, t.yellow, t.blue, t.magenta, t.cyan, t.foreground,
            t.muted, t.bright_red, t.bright_green, t.bright_yellow, t.bright_blue, t.bright_magenta, t.bright_cyan, t.bright_foreground
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
    let Some(req) = read_http_request(&mut stream) else {
        return;
    };
    let cfg = cfg_reader();
    if req.method == "GET" && (req.path == "/" || req.path == "/index.html") {
        write_http_response(
            &mut stream,
            200,
            "OK",
            "text/html; charset=utf-8",
            UI_HTML.as_bytes(),
        );
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

    if req.method == "POST" && !allowed_post_origin(&req, &cfg) {
        reply_err(&mut stream, 403, "Forbidden", "Cross-origin POST rejected.");
        return;
    }

    if req.method == "POST" && req.path == "/api/auth" {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct AuthReq {
            init_data: String,
        }
        let Ok(body) = serde_json::from_slice::<AuthReq>(&req.body) else {
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
        let Ok(token) = sessions.lock().unwrap().issue(&cfg, verified.user_id, now) else {
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

    // All remaining routes require a valid session token.
    let now = unix_now();
    let Some(tok) = extract_bearer_or_query(&req) else {
        reply_err(&mut stream, 401, "Unauthorized", "Missing session token.");
        return;
    };
    if !sessions.lock().unwrap().validate(&tok, &cfg, now) {
        reply_err(
            &mut stream,
            401,
            "Unauthorized",
            "Session expired or revoked — reopen Mini App.",
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
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LaunchReq {
            agent_type: String,
            workspace: String,
        }
        let Ok(body) = serde_json::from_slice::<LaunchReq>(&req.body) else {
            reply_err(&mut stream, 400, "Bad Request", "Invalid launch payload.");
            return;
        };
        match backend.launch_card(&body.agent_type, &body.workspace) {
            Ok(id) => reply_json(&mut stream, 200, "OK", &json!({ "ok": true, "id": id })),
            Err(err) => reply_err(&mut stream, 400, "Bad Request", &err),
        }
        return;
    }

    if req.method == "GET" {
        if let Some(card_id) = req.path.strip_prefix("/ws/terminal/") {
            let Some(ws_key) = req.headers.get("sec-websocket-key").cloned() else {
                reply_err(&mut stream, 400, "Bad Request", "Missing Sec-WebSocket-Key.");
                return;
            };
            if !cfg.permissions.read_output {
                reply_err(&mut stream, 403, "Forbidden", "Read terminal output is disabled.");
                return;
            }
            if crate::ws::handshake(&mut stream, &ws_key).is_ok() {
                run_ws_stream(stream, card_id, &tok, cfg_reader, backend, sessions, stop);
            }
            return;
        }
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
                    reply_json(&mut stream, 200, "OK", &json!({ "ansi": ansi, "card": card_json(card) }));
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
                        .map(|l| mask_token_in_text(&l, &cfg.bot_token))
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
                    if asset.kind == "text" || asset.kind == "markdown" || asset.mime_type.starts_with("text/") {
                        let raw = String::from_utf8_lossy(&bytes);
                        let clean = redact_ansi_preserving(&raw, &cfg.bot_token, cfg.permissions.redact_secrets);
                        write_http_response(&mut stream, 200, "OK", "text/plain; charset=utf-8", clean.as_bytes());
                    } else {
                        write_http_response(&mut stream, 200, "OK", &asset.mime_type, &bytes);
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
            #[derive(Deserialize)]
            struct PromptReq {
                text: String,
            }
            let Ok(body) = serde_json::from_slice::<PromptReq>(&req.body) else {
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
            #[derive(Deserialize)]
            struct KeyReq {
                code: String,
            }
            let Ok(body) = serde_json::from_slice::<KeyReq>(&req.body) else {
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
            let Ok(body) = serde_json::from_slice::<AttachReq>(&req.body) else {
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

    let mut last_payload = String::new();
    let mut last_auth_check = Instant::now() - Duration::from_secs(10);
    while !stop.load(Ordering::SeqCst) {
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
                    if let Ok(val) = serde_json::from_str::<Value>(&txt) {
                        let cfg = cfg_reader();
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
            if !sessions.lock().unwrap().validate(token, &cfg, unix_now()) || !cfg.permissions.read_output {
                let _ = crate::ws::write_close(&mut stream, 4001, "unauthorized");
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
                "card": card_json(card),
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
