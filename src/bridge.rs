//! Harness bridge hosted by super-desktop (Rust-only backend).
//!
//! Exposes this machine's harnesses to the Android launcher and its local
//! workspace to paired SUPER DESKTOP PCs over LAN / Tailscale. The owning
//! daemon remains authoritative for card layout and lifecycle.
//!
//! Endpoints (see OmarchyAILauncher/PROTOCOL.md, wire v1):
//!   GET  /api/v1/ping
//!   GET  /api/v1/harnesses            (Bearer token)
//!   GET  /api/v1/theme                 (Bearer token)
//!   DELETE /api/v1/harnesses/<id>     (Bearer token)
//!   POST /api/v1/pair                 (open; requests desktop approval)
//!   POST /api/v1/pair/poll            (unguessable request capability)
//!   GET  /api/v1/pair/state           (owner-only Unix control socket)
//!   POST /api/v1/pair/approve, /deny   (owner-only; deny also blocks the device)
//!   GET  /api/v1/pair/rejected, POST /api/v1/pair/rejected/remove   (owner-only)

use serde::{Deserialize, Serialize};
#[path = "bridge_pairing.rs"]
mod pairing;
pub use pairing::{
    pending_requests, decide_request, paired_devices, revoke_device, pairing_invitation,
    rejected_devices, forget_rejected, pairing_requests,
};
#[path = "bridge_security.rs"]
mod security;
#[path = "bridge_lifecycle.rs"]
mod lifecycle;
pub use lifecycle::supervise as supervise_bridge;
use security::Connection;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::state::load_state;
use crate::tmux::{
    capture_pane_text, extract_composer_draft, get_agent_config,
    get_composer_draft, get_opencode_user_text_by_id, inspect_status,
    inspect_status_with_screen, resolve_own_opencode_id, resolve_workspace_dir,
    strip_terminal_escapes, SessionStatus,
};

pub const BRIDGE_PORT: u16 = 8759;
const SERVICE_NAME: &str = "Omarchy Harness Bridge";
const PROTOCOL_VERSION: u32 = 3;
#[path = "desktop_bridge.rs"]
mod desktop;
#[path = "desktop_events.rs"]
mod desktop_events;
#[path = "bridge_harness_list.rs"]
mod harness_list;
#[path = "bridge_terminal_stream.rs"]
mod terminal_stream;
#[path = "bridge_completions.rs"]
mod completions;

/// Phone input reached `session`: wake that session's terminal streams only.
fn wake_terminal_streams(session: &str) {
    terminal_stream::wake(session);
}
type LauncherSessionMeta = (String, String, Option<String>, u8, Option<String>);

fn utc_now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The active Omarchy palette, kept explicit so the wire format remains stable
/// if the desktop-side theme struct gains implementation-only fields.
fn theme_document() -> serde_json::Value {
    let t = crate::theme::current_theme_fresh();
    serde_json::json!({
        "name": t.name,
        "mode": t.mode,
        "accent": t.accent,
        "selection": t.selection,
        "muted": t.muted,
        "background": t.background,
        "darkBackground": t.dark_background,
        "darkerBackground": t.darker_background,
        "lighterBackground": t.lighter_background,
        "foreground": t.foreground,
        "darkForeground": t.dark_foreground,
        "lightForeground": t.light_foreground,
        "brightForeground": t.bright_foreground,
        "red": t.red,
        "yellow": t.yellow,
        "orange": t.orange,
        "green": t.green,
        "cyan": t.cyan,
        "blue": t.blue,
        "magenta": t.magenta,
        "brown": t.brown,
        "brightRed": t.bright_red,
        "brightYellow": t.bright_yellow,
        "brightGreen": t.bright_green,
        "brightCyan": t.bright_cyan,
        "brightBlue": t.bright_blue,
        "brightMagenta": t.bright_magenta,
        "fontFamily": t.font_family,
        "fontSize": t.font_size,
    })
}

/// The Android snapshot and its live stream share one document contract.
/// One `state.json` read and one tmux inventory per document.
fn harness_document() -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "timestamp": utc_now_iso(),
        "harnesses": collect_harnesses(),
        "usage": crate::usage::launcher_usage(),
        "theme": theme_document(),
    })
}

/// `(fetched_at, value)` for `tailscale_ip`; see the TTL there.
static TAILSCALE_CACHE: Mutex<Option<(f64, Option<String>)>> = Mutex::new(None);
static ADDRESSES_CACHE: Mutex<Option<(f64, Vec<serde_json::Value>)>> = Mutex::new(None);

fn interface_addresses(document: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    for interface in document.as_array().into_iter().flatten() {
        let name = interface["ifname"].as_str().unwrap_or("");
        for info in interface["addr_info"].as_array().into_iter().flatten() {
            let Some(address) = info["local"].as_str() else { continue };
            let Ok(ip) = address.parse::<std::net::IpAddr>() else { continue };
            if ip.is_unspecified() || ip.is_multicast() { continue; }
            let kind = if ip.is_loopback() { "loopback" }
                else if name.starts_with("tailscale") { "tailscale" }
                else { "lan" };
            result.push(serde_json::json!({"address":address,"interface":name,"kind":kind,
                // The HTTP listener currently serves IPv4. Show IPv6 addresses
                // too, but don't let clients select them as working endpoints.
                "connectable":ip.is_ipv4() && !ip.is_loopback() && info["scope"] != "link"}));
        }
    }
    result.sort_by_key(|v| (v["kind"] != "tailscale", v["connectable"] != true, v["address"].as_str().unwrap_or("").to_string()));
    result
}

fn bridge_addresses() -> Vec<serde_json::Value> {
    let mut cache = ADDRESSES_CACHE.lock().unwrap();
    if let Some((time, addresses)) = cache.as_ref() {
        if now_epoch() - time < 10.0 { return addresses.clone(); }
    }
    let addresses = Command::new("ip").args(["-j", "address", "show", "up"]).output().ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .map(|value| interface_addresses(&value)).unwrap_or_default();
    *cache = Some((now_epoch(), addresses.clone()));
    addresses
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ---------- session truth (reuses crate::tmux) ----------

pub(crate) fn last_user_text(
    session: &str,
    agent_type: &str,
    persisted: Option<&str>,
    _screen: &str,
) -> Option<String> {
    if is_regular_terminal(agent_type) {
        return crate::shell_title::last(session);
    }
    // A silent native adapter falls back to the typed prompt (same rule as
    // the card, see `reported_or_typed_prompt`).
    if let Some(prompt) = crate::card_status::reported_or_typed_prompt(
        crate::harness_metadata::inspect(session, agent_type).as_ref(),
        || crate::prompt_history::last(session),
    ) {
        return prompt;
    }
    if agent_type == "codex" {
        return crate::completion::last_user_prompt(session);
    }
    // Exact opencode DB text is resolved to the session OWNED by this pane
    // (own `--session` flag, else a claims-aware match), so a closed console's
    // prompt never leaks here. Composer drafts stay in their separate field.
    if agent_type == "opencode" {
        if let Some(id) = resolve_own_opencode_id(session, persisted) {
            if let Some(text) = get_opencode_user_text_by_id(&id) {
                return Some(text);
            }
        }
    }
    // Response text is never a fallback for AI harness titles.
    None
}

fn is_regular_terminal(agent_type: &str) -> bool {
    crate::shell_title::is_regular(agent_type)
}

pub(crate) fn session_title(session: &str, agent_type: &str, pane_pid: &str) -> Option<String> {
    if let Some(title) = crate::harness_metadata::title(session, agent_type) {
        return Some(title);
    }
    if agent_type != "codex" { return None; }
    crate::completion::session_title(pane_pid.parse().ok()?)
}

/// Pick the directory the launcher should describe. Harness rows show the
/// workspace they were launched in; regular terminals track the pane's live
/// cwd so `cd` is reflected immediately.
fn launcher_directory(
    agent_type: &str,
    harness_home: &str,
    live_cwd: &str,
) -> (String, &'static str) {
    if is_regular_terminal(agent_type) && !live_cwd.trim().is_empty() {
        (live_cwd.trim().to_string(), "cwd")
    } else if is_regular_terminal(agent_type) {
        (harness_home.to_string(), "cwd")
    } else {
        (harness_home.to_string(), "home")
    }
}

/// Keep the directory as the final preview line because the current Android
/// launcher renders the final three non-empty lines of this field.
fn preview_with_directory(screen: &str, kind: &str, display_dir: &str) -> String {
    let lines: Vec<&str> = screen
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(11);
    let mut preview = lines[start..].join("\n");
    if !preview.is_empty() {
        preview.push('\n');
    }
    preview.push_str(kind);
    preview.push_str(" · ");
    preview.push_str(display_dir);
    preview
}

/// Hex colour of a super-desktop group tag (0 = untagged).
///
/// One source of truth: the same palette the desktop dots use (`tag::TAG_COLORS`,
/// mirrored as `.tag-dot-N` in styles.rs).
fn tag_color(tag: u8) -> Option<&'static str> {
    let n = crate::tag::normalize_tag(tag);
    if n == crate::tag::TAG_NONE {
        None
    } else {
        crate::tag::TAG_COLORS.get(n as usize - 1).copied()
    }
}

pub fn collect_harnesses() -> Vec<serde_json::Value> {
    let state = load_state();
    let snapshot = crate::tmux::pane_snapshot();
    harness_list::collect(&state, snapshot.as_ref())
}

/// Read only recognized status-footer formats from the current pane bottom.
/// Never infer settings from global defaults or conversation history.
fn harness_model_effort(agent: &str, screen: &str) -> (Option<String>, Option<String>) {
    for line in screen.lines().rev().take(6).map(str::trim) {
        if let Some(rest) = line.strip_prefix("MODEL ") {
            if let Some((model, effort)) = rest.split_once("EFFORT ") {
                let model = model.trim();
                let effort = effort.split_whitespace().next().unwrap_or("");
                if !model.is_empty() && !effort.is_empty() {
                    return (Some(model.to_string()), Some(effort.to_string()));
                }
            }
        }
        if agent == "codex" {
            if let Some((settings, _)) = line.split_once(" · ") {
                let mut parts = settings.split_whitespace();
                if let (Some(model), Some(effort), None) = (parts.next(), parts.next(), parts.next()) {
                    if (model.starts_with("gpt-") || model.starts_with("o3") || model.starts_with("o4"))
                        && matches!(effort, "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra") {
                        return (Some(model.to_string()), Some(effort.to_string()));
                    }
                }
            }
        }
    }
    (None, None)
}

// ---------- pairing state ----------

#[derive(Debug, Serialize, Deserialize)]
struct BridgeConfig {
    #[serde(default = "new_bridge_id")]
    bridge_id: String,
    #[serde(default)]
    paired_tokens: Vec<String>,
    #[serde(default)]
    devices: Vec<PairedDevice>,
    /// Devices whose pairing request the owner rejected. Their later requests
    /// are refused without a desktop prompt until the owner removes them.
    #[serde(default)]
    rejected: Vec<RejectedDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairedDevice { id: String, name: String, token_hash: String, expires: f64,
    #[serde(default)]
    device_type: String,
}

/// Everything here was reported by the rejected device itself, so it is only
/// used to recognise that device again, never to grant anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RejectedDevice {
    /// Local handle for removing the entry; not the device's own identity.
    id: String,
    name: String,
    #[serde(default)]
    device_type: String,
    /// The installation ID a SUPER DESKTOP PC sends (its bridge ID). Empty for
    /// clients that do not send one.
    #[serde(default)]
    device_id: String,
    /// Source address of the rejected request.
    #[serde(default)]
    address: String,
    rejected_at: f64,
}

fn new_bridge_id() -> String { random_hex(16) }

/// Read identity without initializing or rewriting the bridge credential store.
pub fn own_bridge_id() -> Option<String> {
    let dir = std::env::var_os("SUPER_DESKTOP_BRIDGE_STATE_DIR").map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state/omarchy/harness-bridge")))?;
    let value: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("config.json")).ok()?).ok()?;
    value["bridge_id"].as_str().map(str::to_owned)
}

fn state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    // Unit tests exercise the pairing routes in-process; they must never
    // write the developer's real credential store.
    let default = if cfg!(test) {
        std::env::temp_dir().join(format!("sd-bridge-test-{}", std::process::id()))
    } else {
        PathBuf::from(home).join(".local/state/omarchy/harness-bridge")
    };
    let dir = std::env::var_os("SUPER_DESKTOP_BRIDGE_STATE_DIR").map(PathBuf::from)
        .unwrap_or(default);
    fs::create_dir_all(&dir).expect("Cannot create private bridge state directory");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("Cannot secure bridge state directory");
    dir.join("config.json")
}

/// Exactly `n` bytes from the CSPRNG. Never `fs::read` /dev/urandom —
/// it is an infinite stream and `read` would block forever.
fn urandom_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let ok = fs::File::open("/dev/urandom")
        .ok()
        .and_then(|mut f| {
            use std::io::Read as _;
            f.read_exact(&mut buf).ok()
        })
        .is_some();
    assert!(ok, "OS randomness unavailable; refusing to generate pairing credentials");
    buf
}

fn random_hex(bytes: usize) -> String {
    urandom_bytes(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

struct PairState {
    cfg: BridgeConfig,
    mtime: f64,
}

fn config_mtime() -> f64 {
    fs::metadata(state_path())
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs_f64())
        })
        .unwrap_or(0.0)
}

impl PairState {
    fn load() -> Self {
        let path = state_path();
        let cfg: BridgeConfig = fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_else(|| BridgeConfig {
                bridge_id: new_bridge_id(),
                paired_tokens: vec![],
                devices: vec![],
                rejected: vec![],
            });
        let mut state = Self {
            cfg,
            mtime: 0.0,
        };
        // Legacy credentials travelled over HTTP; never accept them under v3.
        state.cfg.paired_tokens.clear();
        state.save();
        let mtime = config_mtime();
        Self {
            cfg: state.cfg,
            mtime,
        }
    }

    fn save(&self) -> bool {
        use std::os::unix::fs::OpenOptionsExt;
        let path = state_path();
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        (|| -> std::io::Result<()> {
            let json = serde_json::to_vec_pretty(&self.cfg)?;
            let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
            file.write_all(&json)?;
            file.sync_all()?;
            fs::rename(temporary, path)
        })().is_ok()
    }

    /// Pick up config.json edits made by another process (e.g. a fresh PIN
    /// written from the overlay settings while the bridge keeps running).
    /// Tokens minted in-memory are unioned in so nothing is lost.
    fn refresh(&mut self) {
        let m = config_mtime();
        if m == 0.0 || m == self.mtime {
            return;
        }
        if let Some(disk) = fs::read_to_string(state_path())
            .ok()
            .and_then(|c| serde_json::from_str::<BridgeConfig>(&c).ok())
        {
            self.cfg = BridgeConfig {
                bridge_id: self.cfg.bridge_id.clone(),
                paired_tokens: vec![],
                devices: disk.devices,
                rejected: disk.rejected,
            };
        }
        self.mtime = config_mtime();
    }

    fn valid(&mut self, token: &str) -> bool {
        self.refresh();
        if token.is_empty() {
            return false;
        }
        let hash = security::digest(token.as_bytes());
        let now = now_epoch();
        self.cfg.devices.iter().any(|d| d.expires > now && security::equal(&d.token_hash, &hash))
    }
}

fn pair_state() -> &'static Mutex<PairState> {
    static CELL: OnceLock<Mutex<PairState>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(PairState::load()))
}

// ---------- network helpers ----------

pub fn lan_ip() -> String {
    UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|s| {
            s.connect("8.8.8.8:80").ok()?;
            s.local_addr().ok().map(|a| a.ip().to_string())
        })
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

pub fn hostname() -> String {
    // /proc read instead of forking `hostname`: the launcher page is filled on
    // the window-build path, where every fork/exec costs tens of milliseconds.
    if let Ok(raw) = fs::read_to_string("/proc/sys/kernel/hostname") {
        let name = raw.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
            None
        })
        .unwrap_or_else(|| "omarchy".to_string())
}

// ---------- minimal HTTP ----------

struct Request {
    method: String,
    path: String,
    /// Raw query string (after `?`), empty when absent.
    query: String,
    /// HTTP/1.1 without `Connection: close` (HTTP/1.0 never persists).
    keep_alive: bool,
    headers: HashMap<String, String>,
    body: String,
    _upload_slot: Option<crate::assets::Transfer>,
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn read_request(stream: &mut Connection, admission: Option<&security::Admission>) -> Option<Request> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    let mut buf = vec![0u8; 16384];
    let mut total = 0usize;
    let header_end = loop {
        let n = stream.read(&mut buf[total..]).ok()?;
        if n == 0 {
            if total == 0 {
                return None;
            }
            return None;
        }
        total += n;
        if let Some(pos) = find_subslice(&buf[..total], b"\r\n\r\n") {
            break pos + 4;
        }
        if total >= buf.len() {
            return None;
        }
    };
    let header_str = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = header_str.lines();
    let request_line = lines.next().unwrap_or("").to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_uppercase();
    let raw_path = parts.next().unwrap_or("/").to_string();
    let (path, query) = match raw_path.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (raw_path.clone(), String::new()),
    };
    let http11 = parts.next().is_some_and(|v| v == "HTTP/1.1");
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if headers.insert(k.trim().to_lowercase(), v.trim().to_string()).is_some() { return None; }
        }
    }
    let content_len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let keep_alive = http11 && !headers.get("connection").is_some_and(|v| {
        v.split(',').any(|token| token.trim().eq_ignore_ascii_case("close"))
    });
    // Prompt uploads alone may send a large body, each within its own limit
    // and deadline: (body limit, deadline, error when over the limit).
    let upload = match method.as_str() {
        "POST" if crate::prompt_image::route(&path).is_some() => {
            Some((crate::prompt_image::MAX_BODY, Duration::from_secs(30), "image_too_large"))
        }
        "POST" if crate::prompt_attachments::route(&path).is_some() => Some((
            crate::prompt_attachments::MAX_BODY,
            crate::prompt_attachments::UPLOAD_DEADLINE,
            "attachments_too_large",
        )),
        _ => None,
    };
    if headers.contains_key("transfer-encoding") { return None; }
    let mut upload_slot = None;
    if let Some((limit, deadline, too_large)) = upload {
        let head = Request { method: method.clone(), path: path.clone(), query: String::new(), keep_alive: false, headers: headers.clone(), body: String::new(), _upload_slot: None };
        if headers.contains_key("origin") || headers.contains_key("sec-fetch-site") {
            respond(stream, 403, "Forbidden", &serde_json::json!({"error":"browser_access_disabled"}));
            return None;
        }
        if !require_pairing(stream, &head, AuthReply::Plain) {
            return None;
        }
        if content_len > limit {
            respond(stream, 413, "Payload Too Large", &serde_json::json!({"error":too_large}));
            return None;
        }
        upload_slot = crate::assets::Transfer::acquire();
        if upload_slot.is_none() {
            respond(stream, 429, "Too Many Requests", &serde_json::json!({"error":"busy"}));
            return None;
        }
        let token = bearer(&headers);
        stream.credential(token);
        if let Some(guard) = admission { guard.identify(token); }
        stream.upload_deadline(deadline);
    } else if content_len > 16384 { return None; }
    // Bytes past this request's body belong to the next pipelined request.
    let body_end = (header_end + content_len).min(total);
    let mut body = buf[header_end..body_end].to_vec();
    stream.unread(&buf[body_end..total]);
    while body.len() < content_len {
        let mut chunk = vec![0u8; (content_len - body.len()).min(8192)];
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_len);
    Some(Request {
        method,
        path,
        query,
        keep_alive,
        headers,
        // Valid UTF-8 (every JSON body) is kept as is: an upload is not copied.
        body: String::from_utf8(body)
            .unwrap_or_else(|invalid| String::from_utf8_lossy(invalid.as_bytes()).into_owned()),
        _upload_slot: upload_slot,
    })
}

fn respond(stream: &mut Connection, code: u16, reason: &str, value: &serde_json::Value) {
    let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    respond_body(stream, code, reason, &body);
}

/// `respond` for an already serialized JSON body. The connection stays open
/// for another request only when `stream.persist` allows it.
fn respond_body(stream: &mut Connection, code: u16, reason: &str, body: &str) {
    let connection = if stream.persist { "keep-alive" } else { "close" };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n",
        body.len()
    );
    // One write, not head-then-body: a WebSocket client that rejects an
    // upgrade only keeps bytes already buffered past the headers (`tail`),
    // so a split write intermittently delivers headers without the error
    // body and the caller falls back to a status-only guess.
    let mut out = Vec::with_capacity(head.len() + body.len());
    out.extend_from_slice(head.as_bytes());
    out.extend_from_slice(body.as_bytes());
    let _ = stream.write_all(&out);
    let _ = stream.flush();
}

fn bearer(headers: &HashMap<String, String>) -> &str {
    let h = headers.get("authorization").map(String::as_str).unwrap_or("");
    if h.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("bearer ")) {
        h.get(7..).unwrap_or("").trim()
    } else {
        ""
    }
}

/// Protected Android and desktop routes require a registered bearer token.
/// Public ping and pairing, and owner-only Unix routes, are dispatched separately.
fn authorize(req: &Request) -> bool {
    pair_state()
            .lock()
            .map(|mut s| s.valid(bearer(&req.headers)))
            .unwrap_or(false)
}

#[derive(Clone, Copy)]
enum AuthReply {
    Plain,
    StatusEnvelope,
}

/// Keep the established `status` envelope on older Android routes while
/// sharing the token check. Other routes use the plain error document.
/// Every caller returns immediately on `false`.
fn require_pairing(stream: &mut Connection, req: &Request, reply: AuthReply) -> bool {
    if authorize(req) {
        return true;
    }
    let body = match reply {
        AuthReply::Plain => serde_json::json!({"error":"not_paired"}),
        AuthReply::StatusEnvelope => serde_json::json!({"status":"error","error":"not_paired"}),
    };
    respond(stream, 401, "Unauthorized", &body);
    false
}

/// Complete a WebSocket upgrade; `false` when this is not a WS request (the
/// caller then answers with plain HTTP).
fn ws_upgrade(stream: &mut Connection, req: &Request) -> bool {
    stream.streaming();
    let upgrade = req
        .headers
        .get("upgrade")
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    if !upgrade {
        return false;
    }
    let Some(key) = req.headers.get("sec-websocket-key") else {
        return false;
    };
    // The socket now belongs to the WebSocket, never to another HTTP request.
    stream.persist = false;
    crate::ws::handshake(stream, key).is_ok()
}

/// How long one push stream may live before the client is asked to reconnect.
const STREAM_MAX_SECS: u64 = 1800;

/// Drain any frames the client sent without blocking.
///
/// Returns false when the peer went away (EOF or a Close frame), true when it
/// is still there — including a Ping, which is answered so picky clients stay
/// happy. A read timeout just means "nothing to read".
fn ws_client_alive(stream: &mut Connection) -> bool {
    // Usually nothing is waiting: ask the socket rather than read with a 1 ms
    // timeout, which sleeps a whole kernel tick before every capture. A hang-up
    // polls as readable (EOF) and, like buffered plaintext, takes the path below.
    if !stream.has_buffered_input() {
        if let Some(fd) = stream.raw_fd() {
            let mut poll = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut poll, 1, 0) } == 0 {
                return true;
            }
        }
    }
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1)));
    // A timeout in the middle of read_exact would discard a partial frame.
    // Probe without consuming, then finish the frame or close the connection.
    match stream.peek(&mut [0u8; 1]) {
        Ok(0) => return false,
        Err(e) => return matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
        _ => {},
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    match crate::ws::read_frame(stream) {
        Ok(None) | Ok(Some(crate::ws::Frame::Close)) => false,
        Ok(Some(crate::ws::Frame::Ping(payload))) => crate::ws::write_pong(stream, &payload).is_ok(),
        Ok(Some(_)) => true,
        Err(_) => false,
    }
}

/// Ordered input on a persistent socket. Never replay an input after a lost
/// acknowledgement: the client must treat that outcome as uncertain.
fn stream_keys(stream: &mut Connection, id: &str) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let (agent, _) = session_meta(id);
    let Ok(mut control) = crate::tmux_control::Control::open(id) else {
        let _ = crate::ws::write_close(stream, 1011, "terminal unavailable");
        return;
    };
    loop {
        match crate::ws::read_frame(stream) {
            Ok(Some(crate::ws::Frame::Text(text))) => {
                let Ok(body) = serde_json::from_str::<serde_json::Value>(&text) else { break };
                let value = body["text"].as_str().unwrap_or("");
                let enter = body["enter"].as_bool().unwrap_or(false);
                let result = (|| -> Result<(), String> {
                    let _input = crate::prompt_image::input_guard(id)?;
                    if value.len() > 4096 { return Err("text_too_long".into()); }
                    if body["checkIdle"].as_bool().unwrap_or(false) {
                        if !matches!(inspect_status(id, &agent).status, "IDLE" | "FINISHED") {
                            return Err("Wait for the harness to become idle.".into());
                        }
                        if get_composer_draft(id).is_some_and(|s| !s.trim().is_empty()) {
                            return Err("Finish or clear the remote draft first.".into());
                        }
                    }
                    if !stream.still_authorized() { return Err("device_revoked".into()); }
                    // One failed tmux command poisons the client for good, and
                    // this socket can live for hours. Reopen it for this input;
                    // the input that failed was acknowledged as an error and is
                    // never replayed.
                    if !control.is_healthy() {
                        control = crate::tmux_control::Control::open(id)?;
                    }
                    control.send(value, false)?;
                    wake_terminal_streams(id);
                    // Codex paste detection needs settling, but it does not
                    // need another phone-to-bridge round trip.
                    if enter && !value.is_empty() && agent == "codex" {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    if !stream.still_authorized() { return Err("device_revoked".into()); }
                    if enter { control.send("", true)?; }
                    // The persistent phone input path bypasses tmux::send_keys,
                    // which normally records submitted composer text. Record
                    // only after Enter succeeds, never an unsent draft or error.
                    if enter {
                        crate::prompt_history::record(id, value);
                    }
                    wake_terminal_streams(id);
                    Ok(())
                })();
                let ack = serde_json::json!({"sequence": body["sequence"],
                    "ok": result.is_ok(), "error": result.err()});
                if crate::ws::write_text(stream, &ack.to_string()).is_err() { break; }
            }
            Ok(Some(crate::ws::Frame::Ping(payload))) => {
                if crate::ws::write_pong(stream, &payload).is_err() { break; }
            }
            Ok(Some(crate::ws::Frame::Pong(_))) => {},
            _ => break,
        }
    }
}

/// Agent type + group tag of a session, from the desktop state file.
fn session_meta(session: &str) -> (String, u8) {
    load_state()
        .terminals
        .iter()
        .find(|t| t.session_name == session)
        .map(|t| (t.agent_type.clone(), t.tag))
        .unwrap_or_else(|| ("shell".to_string(), 0))
}

/// `POST /api/v1/harnesses/<id>/keys` — type into a harness from the phone.
///
/// Body: `{"text": "ls -la", "enter": true}`. `text` is optional (so the phone
/// can send a bare Return, or a control byte such as `\u0003` for Ctrl-C).
fn handle_keys(stream: &mut Connection, req: &Request, id: &str) {
    if !require_pairing(stream, req, AuthReply::StatusEnvelope) {
        return;
    }
    if !crate::tmux::session_alive(id) {
        return respond(
            stream,
            404,
            "Not Found",
            &serde_json::json!({"status": "error", "error": "no_such_session"}),
        );
    }
    let body: serde_json::Value = match serde_json::from_str(&req.body) {
        Ok(value) => value,
        Err(_) => {
            return respond(
                stream,
                400,
                "Bad Request",
                &serde_json::json!({"status": "error", "error": "bad_json"}),
            )
        }
    };
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let enter = body.get("enter").and_then(|v| v.as_bool()).unwrap_or(true);
    if text.len() > 4096 {
        return respond(
            stream,
            413,
            "Payload Too Large",
            &serde_json::json!({"status": "error", "error": "text_too_long"}),
        );
    }
    let result = crate::prompt_image::input_guard(id).and_then(|_guard| crate::tmux::send_keys(id, text, enter));
    match result {
        Ok(()) => respond(stream, 200, "OK", &serde_json::json!({"status": "ok"})),
        Err(e) => respond(
            stream,
            500,
            "Internal Server Error",
            &serde_json::json!({"status": "error", "error": e}),
        ),
    }
}

/// `DELETE /api/v1/harnesses/<id>` — close one launcher-visible harness.
fn handle_close_harness(stream: &mut Connection, req: &Request, id: &str) {
    if !require_pairing(stream, req, AuthReply::StatusEnvelope) {
        return;
    }
    if !id.starts_with("sd_term_") {
        return respond(
            stream,
            404,
            "Not Found",
            &serde_json::json!({"status": "error", "error": "no_such_session"}),
        );
    }
    match crate::ipc_request(&format!("close-term {id}")) {
        crate::Ipc::Reply(reply) => {
            let result: serde_json::Value =
                serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
            if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                respond(stream, 200, "OK", &result)
            } else if result.get("error").and_then(|v| v.as_str()) == Some("no_such_session") {
                respond(stream, 404, "Not Found", &result)
            } else {
                respond(
                    stream,
                    500,
                    "Internal Server Error",
                    &serde_json::json!({"status": "error", "error": "close_failed"}),
                )
            }
        }
        crate::Ipc::NoDaemon => respond(
            stream,
            503,
            "Service Unavailable",
            &serde_json::json!({"status": "error", "error": "desktop_not_running"}),
        ),
        crate::Ipc::Stalled => respond(
            stream,
            504,
            "Gateway Timeout",
            &serde_json::json!({"status": "error", "error": "close_timeout_check_machine_before_retry"}),
        ),
    }
}

fn creation_command(agent: &str, body: &serde_json::Value) -> Option<String> {
    if let Some(value) = body.get("workspace") {
        let directory = value.as_str().and_then(crate::state::clean_dir)?;
        Some(format!("add-term-in {}", serde_json::json!({"agentType":agent,"workspace":directory})))
    } else {
        Some(format!("add-term {agent}"))
    }
}

/// Idle time a kept-alive connection waits for its next request.
const KEEP_ALIVE_IDLE: Duration = Duration::from_secs(30);
/// Requests served on one connection before it is closed.
const KEEP_ALIVE_MAX_REQUESTS: usize = 200;

/// Serve one connection: a single request, or several with HTTP/1.1
/// keep-alive. Every request is parsed, checked (browser origin, pairing,
/// revocation) and answered on its own; a WebSocket upgrade, a binary asset or
/// `Connection: close` ends the loop. The owner-only Unix socket is served
/// sequentially, so it never keeps a connection open.
fn serve_connection(mut stream: Connection, admission: Option<security::Admission>) {
    let mut served = 0;
    while handle_client(&mut stream, admission.as_ref()) {
        served += 1;
        if served >= KEEP_ALIVE_MAX_REQUESTS || !stream.await_next_request(KEEP_ALIVE_IDLE) {
            break;
        }
    }
}

/// Read and answer one request. Returns whether the connection may carry
/// another one.
fn handle_client(stream: &mut Connection, admission: Option<&security::Admission>) -> bool {
    stream.persist = false;
    stream.forget_credential();
    let Some(req) = read_request(stream, admission) else {
        return false;
    };
    stream.persist = req.keep_alive && !stream.is_local();
    route(stream, &req, admission);
    stream.persist
}

fn route(stream: &mut Connection, req: &Request, admission: Option<&security::Admission>) {
    let local = stream.is_local();
    if req.headers.contains_key("origin") || req.headers.contains_key("sec-fetch-site") {
        return respond(stream, 403, "Forbidden", &serde_json::json!({"error":"browser_access_disabled"}));
    }
    if let Some(guard) = admission { guard.identify(bearer(&req.headers)); }
    // Recheck on each stream I/O as well as closing the socket: TLS may already
    // have buffered input when a device is revoked.
    if authorize(req) {
        let token = bearer(&req.headers);
        security::note_activity(token);
        stream.credential(token);
    }
    let path = req.path.split('?').next().unwrap_or("").to_string();
    if req.method == "POST" {
        if let Some(session) = crate::prompt_image::route(&path) {
            if !require_pairing(stream, req, AuthReply::Plain) {
                return;
            }
            stream.streaming();
            // Uploads run under their own long deadline: never reuse the socket.
            stream.persist = false;
            let body = serde_json::from_str(&req.body).unwrap_or_default();
            let result = crate::prompt_image::submit(session, &security::digest(bearer(&req.headers).as_bytes()), &body, || stream.still_authorized());
            wake_terminal_streams(session);
            return match result {
                Ok(()) => respond(stream, 200, "OK", &serde_json::json!({"status":"submitted"})),
                Err(error) => respond(stream, 409, "Conflict", &serde_json::json!({"error":error})),
            };
        }
        if let Some(session) = crate::prompt_attachments::route(&path) {
            if !require_pairing(stream, req, AuthReply::Plain) {
                return;
            }
            stream.streaming();
            // Uploads run under their own long deadline: never reuse the socket.
            stream.persist = false;
            let owner = security::digest(bearer(&req.headers).as_bytes());
            let result = crate::prompt_attachments::parse(&req.body).and_then(|(text, request, attachments)| {
                crate::prompt_attachments::submit(session, &owner, &text, &request, &attachments, || stream.still_authorized())
            });
            wake_terminal_streams(session);
            return match result {
                Ok(()) => respond(stream, 200, "OK", &serde_json::json!({"status":"submitted"})),
                Err(error) => respond(stream, 409, "Conflict", &serde_json::json!({"error":error})),
            };
        }
    }

    // All pairing routes go through explicit desktop approval. In particular,
    // neither a legacy PIN nor an open window can mint a token any longer.
    if path == "/api/v1/pair" || path.starts_with("/api/v1/pair/") {
        return pairing::handle(stream, req, local, &path);
    }

    if let Some(rest) = path.strip_prefix("/api/v1/harnesses/") {
        let parts: Vec<_> = rest.split('/').collect();
        if parts.get(1) == Some(&"assets") {
            if !require_pairing(stream, req, AuthReply::Plain) {
                return;
            }
            // Authenticated asset work has its own bounded renderer timeout;
            // the initial TLS/header/body deadline no longer applies.
            stream.streaming();
            let Some(_permit) = crate::assets::Transfer::acquire() else {
                return respond(stream, 429, "Too Many Requests", &serde_json::json!({"error":"asset_transfer_busy"}));
            };
            if parts.len() == 2 && (req.method == "GET" || req.method == "POST") {
                let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
                let explicit = body["path"].as_str();
                if req.method == "POST" && explicit.is_none() {
                    return respond(stream, 400, "Bad Request", &serde_json::json!({"error":"missing_path"}));
                }
                return match crate::assets::list(parts[0], if req.method == "POST" { explicit } else { None }) {
                    Ok(items) => respond(stream, 200, "OK", &serde_json::json!({"assets":items,"maxFileBytes":crate::assets::MAX_FILE})),
                    Err(error) => respond(stream, 400, "Bad Request", &serde_json::json!({"error":error})),
                };
            }
            if parts.len() == 4 && parts[3] == "content" && req.method == "GET" {
                return match crate::assets::read(parts[0], parts[2]) {
                    Ok((asset, bytes)) => {
                        stream.persist = false;
                        let header = format!("HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nContent-Disposition: attachment\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", asset.mime_type, bytes.len());
                        if stream.write_all(header.as_bytes()).is_err() { return; }
                        // Connection checks revocation on every write, including buffered TLS.
                        for chunk in bytes.chunks(64 * 1024) {
                            if stream.write_all(chunk).is_err() { break; }
                        }
                    }
                    Err(error) => respond(stream, 404, "Not Found", &serde_json::json!({"error":error})),
                };
            }
            if parts.len() == 5 && parts[3] == "pages" && req.method == "GET" {
                let result = crate::assets::read(parts[0], parts[2]).and_then(|(asset, bytes)| {
                    if asset.kind != "pdf" { return Err("not_a_pdf".into()); }
                    let page = parts[4].parse().map_err(|_| "invalid_page")?;
                    crate::asset_pdf::page(bytes, page)
                });
                return match result {
                    Ok(bytes) => {
                        stream.persist = false;
                        let header = format!("HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n", bytes.len());
                        if stream.write_all(header.as_bytes()).is_err() { return; }
                        for chunk in bytes.chunks(64 * 1024) { if stream.write_all(chunk).is_err() { break; } }
                    }
                    Err(error) => respond(stream, 400, "Bad Request", &serde_json::json!({"error":error})),
                };
            }
            return respond(stream, 404, "Not Found", &serde_json::json!({"error":"unknown_asset_route"}));
        }
    }

    if path == "/api/v1/workspaces" || path == "/api/v1/harness-types" || (path == "/api/v1/harnesses" && req.method == "POST") {
        if !require_pairing(stream, req, AuthReply::Plain) {
            return;
        }
        if req.method == "GET" && path == "/api/v1/workspaces" {
            return match crate::ipc_request("workspace-choices") {
                crate::Ipc::Reply(reply) => match serde_json::from_str::<serde_json::Value>(&reply) {
                    Ok(value) => respond(stream, 200, "OK", &value),
                    Err(_) => respond(stream, 502, "Bad Gateway", &serde_json::json!({"error":"invalid_desktop_response"})),
                },
                _ => respond(stream, 503, "Service Unavailable", &serde_json::json!({"error":"desktop_not_running"})),
            };
        }
        if req.method == "GET" && path == "/api/v1/harness-types" {
            let state = load_state();
            let types: Vec<_> = crate::tmux::HARNESS_KEYS.iter().map(|key| {
                let config = get_agent_config(key);
                serde_json::json!({"id":key,"name":config.name,"icon":config.icon,
                    "available":crate::tmux::detect_harness_command(key).is_some()})
            }).chain(state.custom_harnesses.iter().map(|item| {
                serde_json::json!({"id":item.id,"name":item.name,"icon":item.icon,"available":item.available()})
            })).collect();
            return respond(stream, 200, "OK", &serde_json::json!({"types":types,
                "workspace":crate::state::effective_workspace_dir(&state)}));
        }
        if req.method == "POST" && path == "/api/v1/harnesses" {
            let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or(serde_json::Value::Null);
            let agent = body.get("agentType").and_then(|v| v.as_str()).unwrap_or("");
            let custom = load_state().custom_harnesses.into_iter().find(|item| item.id == agent);
            if !crate::tmux::HARNESS_KEYS.contains(&agent) && custom.is_none() {
                return respond(stream, 400, "Bad Request", &serde_json::json!({"error":"unsupported_harness"}));
            }
            let available = custom.as_ref().map_or_else(
                || crate::tmux::detect_harness_command(agent).is_some(),
                |item| item.validate().is_ok(),
            );
            if !available {
                return respond(stream, 409, "Conflict", &serde_json::json!({"error":"harness_not_installed"}));
            }
            let Some(command) = creation_command(agent, &body) else {
                return respond(stream, 400, "Bad Request", &serde_json::json!({"error":"invalid_workspace"}));
            };
            return match crate::ipc_request(&command) {
                crate::Ipc::Reply(reply) => {
                    let result: serde_json::Value = serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
                    if result["ok"] == true {
                        respond(stream, 201, "Created", &result)
                    } else if result["error"] == "invalid_workspace" {
                        respond(stream, 400, "Bad Request", &serde_json::json!({"error":"invalid_workspace"}))
                    } else {
                        respond(stream, 500, "Internal Server Error", &serde_json::json!({"error":"creation_failed"}))
                    }
                }
                crate::Ipc::NoDaemon => respond(stream, 503, "Service Unavailable", &serde_json::json!({"error":"desktop_not_running"})),
                crate::Ipc::Stalled => respond(stream, 504, "Gateway Timeout", &serde_json::json!({"error":"creation_timeout_check_machine_before_retry"})),
            };
        }
    }

    // Dynamic harness routes: /api/v1/harnesses/<id>/stream (WebSocket, live
    // pane output), /keys (phone → harness input), and DELETE (close harness).
    // Only our own `sd_term_*` sessions are addressable, so a paired phone
    // cannot type into unrelated tmux sessions.
    if let Some(rest) = path.strip_prefix("/api/v1/harnesses/") {
        if req.method == "DELETE" && !rest.contains('/') {
            return handle_close_harness(stream, req, rest);
        }
        if let Some((id, action)) = rest.rsplit_once('/') {
            if id.starts_with("sd_term_") {
                match (req.method.as_str(), action) {
                    ("GET", "input") => {
                        if !require_pairing(stream, req, AuthReply::Plain) {
                            return;
                        }
                        if !crate::tmux::session_alive(id) {
                            return respond(stream, 404, "Not Found", &serde_json::json!({"error":"no_such_session"}));
                        }
                        if ws_upgrade(stream, req) { return stream_keys(stream, id); }
                        return respond(stream, 400, "Bad Request", &serde_json::json!({"error":"expected_websocket"}));
                    }
                    ("GET", "stream") => {
                        if !require_pairing(stream, req, AuthReply::StatusEnvelope) {
                            return;
                        }
                        return match ws_upgrade(stream, req) {
                            true => terminal_stream::stream(stream, id, terminal_stream::ansi_only(&req.query)),
                            false => respond(
                                stream,
                                400,
                                "Bad Request",
                                &serde_json::json!({"status": "error", "error": "expected_websocket"}),
                            ),
                        };
                    }
                    ("POST", "keys") => return handle_keys(stream, req, id),
                    _ => {}
                }
            }
        }
    }

    // Live terminal bytes for one owned card: host output and viewer keystrokes
    // are WSS binary frames, plus bounded text control frames.
    if let Some(rest) = path.strip_prefix("/api/v1/desktop/terminals/") {
        if let Some(card_id) = rest.strip_suffix("/attach") {
            if req.method != "GET" || card_id.contains('/') {
                return respond(stream, 404, "Not Found", &serde_json::json!({"error":"not_found"}));
            }
            if !require_pairing(stream, req, AuthReply::Plain) {
                return;
            }
            // Ownership, session liveness, the host-owned grid and this
            // device's attachment budget are all settled before the upgrade, so
            // every failure is a status a viewer can show.
            let device = stream.credential_id().map(str::to_string);
            let target = match desktop::resolve_attach(card_id, device.as_deref()) {
                Ok(target) => target,
                Err(error) => {
                    return respond(stream, error.code, error.reason, &serde_json::json!({"error":error.error}))
                }
            };
            if !ws_upgrade(stream, req) {
                return respond(stream, 400, "Bad Request", &serde_json::json!({"error":"expected_websocket"}));
            }
            return desktop::attach_terminal(stream, target);
        }
    }

    // One typed command route for every mutation. The old per-card
    // `cards/<id>/position` route is gone: a move that cannot name the revision
    // it was based on is exactly the edit this route exists to refuse.
    if path == "/api/v1/desktop/commands" {
        // Authorization comes before the method check, like every other desktop
        // route: an unpaired caller learns nothing about the route's shape.
        if !require_pairing(stream, req, AuthReply::Plain) {
            return;
        }
        if req.method != "POST" {
            return respond(
                stream,
                405,
                "Method Not Allowed",
                &serde_json::json!({"error": "method_not_allowed"}),
            );
        }
        // Deduplication is per credential, so a revoked device cannot replay
        // another device's request id.
        let device = stream.credential_id().map(str::to_string).unwrap_or_default();
        return desktop::command(stream, &device, &req.body);
    }

    match (req.method.as_str(), path.as_str()) {
        ("GET", "/api/v1/desktop/capabilities") => {
            if !require_pairing(stream, req, AuthReply::Plain) {
                return;
            }
            let machine_id = pair_state().lock().unwrap().cfg.bridge_id.clone();
            let capabilities = crate::desktop_protocol::Capabilities::current(machine_id);
            respond(stream, 200, "OK", &serde_json::to_value(capabilities).unwrap());
        }
        ("GET", "/api/v1/desktop/workspace" | "/api/v1/desktop/events") => {
            if !require_pairing(stream, req, AuthReply::Plain) {
                return;
            }
            if path.ends_with("/events") {
                // The budget is settled before the upgrade, so a refusal is a
                // status a viewer can show, and it is released with the stream.
                let device = stream.credential_id().map(str::to_string).unwrap_or_default();
                let Some(subscription) = desktop_events::subscribe(&device) else {
                    return respond(stream, 429, "Too Many Requests", &serde_json::json!({"error":"subscription_limit"}));
                };
                if !ws_upgrade(stream, req) {
                    return respond(stream, 400, "Bad Request", &serde_json::json!({"error":"expected_websocket"}));
                }
                desktop_events::serve(stream, subscription);
            } else {
                desktop::get_workspace(stream);
            }
        }
        // The daemon's liveness probe (every 5 s, owner-only socket) needs no
        // address discovery: that ran `ip` whenever its cache had expired.
        // Older bridges ignore the query and answer the full ping.
        ("GET", "/api/v1/ping") if local && req.query.split('&').any(|pair| pair == HEALTH_QUERY) => {
            respond(stream, 200, "OK", &health_body())
        }
        ("GET", "/api/v1/ping") => {
            let bridge_id = pair_state().lock().unwrap().cfg.bridge_id.clone();
            respond(
            stream,
            200,
            "OK",
            &serde_json::json!({
                "status": "ok",
                "service": SERVICE_NAME,
                "protocolVersion": PROTOCOL_VERSION,
                "hostname": hostname(),
                "lanIp": lan_ip(),
                "bridgeId": bridge_id,
                "addresses": bridge_addresses(),
                "tailscaleIp": bridge_addresses().iter().find(|v| v["kind"] == "tailscale" && v["connectable"] == true).map(|v| v["address"].clone()),
                "port": BRIDGE_PORT,
                "time": utc_now_iso(),
            }),
        ) },
        ("GET", "/api/v1/harnesses") => {
            if !require_pairing(stream, req, AuthReply::StatusEnvelope) {
                return;
            }
            respond_body(stream, 200, "OK", &harness_list::current_document());
        }
        ("POST", "/api/v1/completions") => {
            if !require_pairing(stream, req, AuthReply::Plain) {
                return;
            }
            let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
            let ids: Option<Vec<String>> = body["sessions"].as_array().filter(|v| v.len() <= 32).and_then(|v| {
                v.iter().map(|id| id.as_str().filter(|s| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')).map(str::to_string)).collect()
            });
            let Some(ids) = ids else {
                return respond(stream, 400, "Bad Request", &serde_json::json!({"error":"invalid_sessions"}));
            };
            completions::handle(stream, &body, &ids);
        }
        ("GET", "/api/v1/theme") => {
            if !require_pairing(stream, req, AuthReply::StatusEnvelope) {
                return;
            }
            respond(stream, 200, "OK", &theme_document());
        }
        // PROTOCOL.md: full document on connect, then on every change (1s poll).
        ("GET", "/api/v1/harnesses/stream") => {
            if !require_pairing(stream, req, AuthReply::StatusEnvelope) {
                return;
            }
            if !ws_upgrade(stream, req) {
                return respond(
                    stream,
                    400,
                    "Bad Request",
                    &serde_json::json!({"status": "error", "error": "expected_websocket"}),
                );
            }
            harness_list::stream(stream);
        }
        _ => respond(
            stream,
            404,
            "Not Found",
            &serde_json::json!({"error": "not found"}),
        ),
    }
}

/// Blocking serve loop for `super-desktop harness-bridge`.
pub fn serve(port: u16) {
    let tls = security::tls_config().expect("Cannot load TLS identity; refusing insecure fallback");
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{SERVICE_NAME}: failed to bind 0.0.0.0:{port}: {e}");
            std::process::exit(1);
        }
    };
    security::serve_control().expect("Cannot start private desktop control socket");
    println!("{SERVICE_NAME} HTTPS on 0.0.0.0:{port} (lan {})", lan_ip());
    // Advertise for the launcher's NSD lookup (OmarchyAILauncher/PROTOCOL.md).
    // On a worker thread: confirming the record with `avahi-browse` takes a
    // moment, and discovery must never delay serving requests.
    std::thread::spawn(move || {
        if !publish_mdns(port) {
            eprintln!(
                "{SERVICE_NAME}: mDNS {MDNS_SERVICE_TYPE} not advertised (avahi-utils missing?) — \
                 the launcher can still be pointed at {}:{port} manually",
                lan_ip()
            );
        }
    });
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                if let Some(admission) = security::Admission::acquire(&s) {
                    let tls = tls.clone();
                    std::thread::spawn(move || {
                        if let Ok(stream) = Connection::tls(s, tls) { serve_connection(stream, Some(admission)); }
                    });
                }
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

/// mDNS service type the launcher browses for (OmarchyAILauncher/PROTOCOL.md).
pub const MDNS_SERVICE_TYPE: &str = "_omarchy-harness._tcp";

/// Protocol version advertised in the TXT record (`ver=1`).
const MDNS_PROTOCOL_TXT_VERSION: u32 = PROTOCOL_VERSION;

/// Publish `_omarchy-harness._tcp` through the system Avahi daemon.
///
/// Discovery is a convenience: without avahi-utils the bridge serves exactly as
/// before and the launcher can be pointed at the LAN/Tailscale address by hand,
/// so every failure here is logged, not fatal. Returns true once the record is
/// confirmed on the network.
///
/// The publisher's lifetime is tied to this process through a pipe: the wrapper
/// shell keeps the write end as its stdin and kills `avahi-publish-service`
/// when it closes, so the advertisement disappears with the bridge — including
/// a SIGKILL — instead of pointing at a dead port.
pub fn publish_mdns(port: u16) -> bool {
    let host = hostname();
    let name = format!("{SERVICE_NAME} on {host}");
    let txt_host = format!("host={host}");
    // Pass names as arguments, never interpolate them into a shell script.
    let script = format!(
        "avahi-publish-service -s \"$1\" \"$2\" \"$3\" \
         ver={MDNS_PROTOCOL_TXT_VERSION} \"$4\" approval=desktop & publisher=$!; \
         cat >/dev/null; kill $publisher 2>/dev/null; wait $publisher 2>/dev/null"
    );

    let mut command = Command::new("sh");
    command.arg("-c").arg(&script).arg("super-desktop-mdns")
        .arg(&name).arg(MDNS_SERVICE_TYPE).arg(port.to_string()).arg(&txt_host).stdin(Stdio::piped());
    match log_file_append().and_then(|file| file.try_clone().ok().map(|clone| (file, clone))) {
        Some((out, err)) => {
            command.stdout(Stdio::from(out)).stderr(Stdio::from(err));
        }
        None => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("{SERVICE_NAME}: cannot run avahi-publish-service: {e}");
            let _ = fs::remove_file(mdns_state_path());
            return false;
        }
    };
    // Deliberately leak the write end of the pipe (and the child handle): both
    // must outlive this call for as long as the bridge process lives.
    std::mem::forget(child.stdin.take());
    std::mem::forget(child);

    let advertised = mdns_advertised(port);
    if advertised {
        let state = serde_json::json!({
            "service": MDNS_SERVICE_TYPE,
            "port": port,
            "host": host,
            "ver": MDNS_PROTOCOL_TXT_VERSION,
        });
        let _ = fs::write(mdns_state_path(), state.to_string());
    } else {
        let _ = fs::remove_file(mdns_state_path());
    }
    advertised
}

/// Ask the local resolver whether our record is on the network yet.
///
/// `avahi-browse -rtp` prints one `=` line per resolved service; we look for one
/// with our service type and port. Missing avahi-browse is reported as "not
/// advertised" — the panel then tells the user to enter the IP manually.
fn mdns_advertised(port: u16) -> bool {
    // Avahi registers asynchronously, so poll briefly: checking once raced the
    // daemon and reported "not advertised" for a record that was about to
    // appear (which then kept the state file / panel out of sync).
    for _ in 0..6 {
        if browse_once(port) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Longest single `avahi-browse` sweep. Resolving another host's record on the
/// LAN has hung it for minutes, which held up the retries above and left
/// `mdns.json` stale.
const BROWSE_TIMEOUT: Duration = Duration::from_secs(3);

/// Single `avahi-browse -rtp` sweep for our own record. Stops at the first
/// line that shows it, so a sweep stuck on another record still counts ours.
fn browse_once(port: u16) -> bool {
    let mut command = Command::new("avahi-browse");
    command.args(["-rtp", MDNS_SERVICE_TYPE]);
    any_output_line(command, BROWSE_TIMEOUT, |line| browse_line_matches(line, port))
}

/// Run `command` until a line of its stdout satisfies `found` (true), or it
/// exits or `timeout` passes (false). The child is killed and reaped on every
/// path; lines printed before a hang are still examined.
fn any_output_line(mut command: Command, timeout: Duration, mut found: impl FnMut(&str) -> bool) -> bool {
    use std::os::fd::AsRawFd;
    struct Reaped(std::process::Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let spawned = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = spawned.map(Reaped) else { return false };
    let Some(mut stdout) = child.0.stdout.take() else { return false };
    let deadline = std::time::Instant::now() + timeout;
    let mut line = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        let mut poll = libc::pollfd { fd: stdout.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        match unsafe { libc::poll(&mut poll, 1, ms) } {
            0 => continue,
            ready if ready < 0 => {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return false;
            }
            _ => {}
        }
        let n = match stdout.read(&mut chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        };
        if n == 0 {
            return !line.is_empty() && found(&String::from_utf8_lossy(&line));
        }
        for &byte in &chunk[..n] {
            if byte != b'\n' {
                line.push(byte);
                continue;
            }
            if found(&String::from_utf8_lossy(&line)) {
                return true;
            }
            line.clear();
        }
        // No resolve line is this long; don't buffer a runaway one.
        if line.len() > 64 * 1024 {
            return false;
        }
    }
}

/// Does one `avahi-browse -p` line resolve OUR service on OUR port?
///
/// Resolve lines look like
/// `=;wlo1;IPv4;Name\032With\032Spaces;_omarchy-harness._tcp;local;host.local;192.168.50.219;8759;"ver=1"`.
fn browse_line_matches(line: &str, port: u16) -> bool {
    if !line.starts_with('=') {
        return false;
    }
    let fields: Vec<&str> = line.split(';').collect();
    // 4 = service type, 8 = port (6 = host, 7 = address, 9 = TXT).
    fields.get(4) == Some(&MDNS_SERVICE_TYPE)
        && fields.get(8).and_then(|p| p.parse::<u16>().ok()) == Some(port)
}

/// State file the 📱 panel reads to show whether discovery is live.
fn mdns_state_path() -> PathBuf {
    state_path().with_file_name("mdns.json")
}

/// `host:port` from the mDNS state file, when the bridge advertised itself.
fn mdns_advertised_port() -> Option<u16> {
    let raw = fs::read_to_string(mdns_state_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value.get("port")?.as_u64().map(|p| p as u16)
}

/// Line the launcher page shows for discovery.
pub fn mdns_summary(bridge_online: bool) -> String {
    if !bridge_online {
        return format!("{MDNS_SERVICE_TYPE} · bridge offline");
    }
    match mdns_advertised_port() {
        Some(port) => format!("{MDNS_SERVICE_TYPE} · advertised :{port}"),
        None => format!("{MDNS_SERVICE_TYPE} · not advertised — use the IP above"),
    }
}

/// Bridge log opened for append (the banner there is written at spawn time).
fn log_file_append() -> Option<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(bridge_log_path())
        .ok()
}

/// One-shot JSON dump for `super-desktop harnesses`.
pub fn print_once() {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "harnesses": collect_harnesses(),
            "usage": crate::usage::launcher_usage(),
        }))
        .unwrap_or_default()
    );
}

// ---------- overlay-facing helpers (used by the ⚙ Launcher settings panel) ----------

/// First IPv4 from `tailscale ip -4`, if Tailscale is up.
pub fn tailscale_ip() -> Option<String> {
    // `tailscale ip` asks tailscaled over its socket and takes ~100ms, and the
    // panel refresh runs while it is open: cache briefly.
    const TTL_SECS: f64 = 30.0;
    if let Ok(cache) = TAILSCALE_CACHE.lock() {
        if let Some((at, value)) = cache.as_ref() {
            if now_epoch() - at < TTL_SECS {
                return value.clone();
            }
        }
    }
    let value = tailscale_ip_uncached();
    if let Ok(mut cache) = TAILSCALE_CACHE.lock() {
        *cache = Some((now_epoch(), value.clone()));
    }
    value
}

fn tailscale_ip_uncached() -> Option<String> {
    let out = Command::new("tailscale").args(["ip", "-4"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .find(|l| !l.is_empty())
}

/// Query that turns a ping on the owner-only control socket into a health
/// probe; see `health_body`.
const HEALTH_QUERY: &str = "health=1";

/// Answer to the daemon's health probe: what `bridge_running` checks, without
/// the phone-facing ping's addresses, LAN IP or hostname (no subprocess).
fn health_body() -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "service": SERVICE_NAME,
        "protocolVersion": PROTOCOL_VERSION,
        "port": BRIDGE_PORT,
    })
}

/// True when a bridge answers on loopback (this laptop).
pub fn bridge_running(port: u16) -> bool {
    bridge_ping_body(port)
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .is_some_and(|body| body["status"] == "ok" && body["service"] == SERVICE_NAME)
}

fn bridge_ping_body(_port: u16) -> Option<String> {
    let mut s = std::os::unix::net::UnixStream::connect(security::control_path()).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(3))).ok()?;
    s.write_all(format!("GET /api/v1/ping?{HEALTH_QUERY} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes())
        .ok()?;
    let mut resp = String::new();
    s.read_to_string(&mut resp).ok()?;
    resp.split_once("\r\n\r\n").map(|(_, b)| b.to_string())
}

/// Start `super-desktop harness-bridge` detached.
///
/// Idempotent: a bridge that already answers counts as success (`Ok`), so the
/// panel's ▶ button never reports a healthy bridge as an error.
///
/// Self-healing: a bridge process can be alive but wedged (its ping no longer
/// answers) while still holding the port, which used to make every later start
/// fail on bind — silently, because the child's output went to /dev/null. Such
/// a process is replaced here (pkill only matches our own
/// `super-desktop harness-bridge`), and a port held by anything else is
/// reported by name.
///
/// The child's banner/errors are captured in `bridge.log` (next to the bridge
/// config) and its tail is folded into the error string, so the 📱 panel can
/// show WHY a start failed instead of doing nothing.
pub fn start_bridge() -> Result<(), String> {
    let mut lifecycle = lifecycle::LIFECYCLE.lock().unwrap();
    lifecycle.enable(true);
    start_bridge_inner()
}

fn start_bridge_inner() -> Result<(), String> {
    if bridge_running(BRIDGE_PORT) {
        return Ok(());
    }
    if port_taken(BRIDGE_PORT) {
        // Something is on our port: if it is a wedged bridge of ours, drop it
        // and take the port over; otherwise say who holds it.
        let _ = stop_bridge_inner();
        if bridge_running(BRIDGE_PORT) {
            return Ok(());
        }
        if port_taken(BRIDGE_PORT) {
            return Err(format!(
                "port {BRIDGE_PORT} is held by another process — check `ss -ltnp | grep {BRIDGE_PORT}`"
            ));
        }
    }

    let exe = bridge_exe()?;
    // The log is a diagnostic, never a precondition: on a read-only state dir
    // the bridge must still start (it would otherwise fail with a confusing
    // "Read-only file system" instead of serving).
    let log_path = bridge_log_path();
    // Keep the last run's diagnostics when recovering an unexpected exit.
    let _ = fs::rename(&log_path, log_path.with_file_name("bridge.previous.log"));
    let log = fs::File::create(log_path).ok();
    let log_err = log.as_ref().and_then(|f| f.try_clone().ok());

    let mut command = Command::new(&exe);
    command
        .arg("harness-bridge")
        // Stable argv[0]: `stop_bridge()` and a user's `pkill` match on
        // "super-desktop harness-bridge" whichever path we re-executed.
        .arg0("super-desktop")
        .process_group(0)
        .stdin(Stdio::null());
    match (log, log_err) {
        (Some(out), Some(err)) => {
            command.stdout(Stdio::from(out)).stderr(Stdio::from(err));
        }
        _ => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", exe.display()))?;
    std::thread::spawn(move || {
        // Reap recovered children and retain the exit reason in the daemon log.
        match child.wait() {
            Ok(status) => eprintln!("SUPER DESKTOP: bridge process exited: {status}"),
            Err(error) => eprintln!("SUPER DESKTOP: could not wait for bridge: {error}"),
        }
    });

    // The first start of a desktop session can be slow (cold binary, busy box).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if bridge_running(BRIDGE_PORT) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(match bridge_log_tail() {
        Some(line) => format!("bridge did not answer on :{BRIDGE_PORT} — {line}"),
        None => format!("bridge did not answer on :{BRIDGE_PORT}"),
    })
}

/// Path of a super-desktop binary able to re-exec `harness-bridge`.
///
/// `current_exe()` first, but checked for existence: a rebuild that replaced
/// the binary on disk leaves the running daemon pointing at a path that no
/// longer resolves, and spawning it would fail with a bare ENOENT nobody ever
/// sees. `/proc/self/exe` always resolves to the running image, so it is the
/// fallback that survives that case; PATH comes last.
fn bridge_exe() -> Result<PathBuf, String> {
    let proc_exe = PathBuf::from("/proc/self/exe");
    let path_var = std::env::var("PATH").unwrap_or_default();
    let candidates = std::env::current_exe()
        .ok()
        .into_iter()
        .chain([proc_exe])
        .chain(
            path_var
                .split(':')
                .filter(|dir| !dir.is_empty())
                .map(|dir| PathBuf::from(dir).join("super-desktop")),
        );
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err("cannot find a super-desktop binary to spawn the bridge with".to_string())
}

/// True when some process accepts connections on `port` (loopback).
///
/// Says nothing about whether it is *our* bridge — see `start_bridge`.
fn port_taken(port: u16) -> bool {
    // A connect to a free port in the ephemeral range can "succeed" as a TCP
    // self-connection when the kernel picks that same port as the source.
    // Nobody is listening then, so it does not count.
    TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(300),
    )
    .is_ok_and(|stream| match (stream.local_addr(), stream.peer_addr()) {
        (Ok(local), Ok(peer)) => local != peer,
        _ => true,
    })
}

/// Log the detached bridge writes its banner and bind errors to.
fn bridge_log_path() -> PathBuf {
    let mut path = state_path();
    path.set_file_name("bridge.log");
    path
}

/// Last non-empty line of the bridge log: its banner, or the bind error that
/// killed the process.
fn bridge_log_tail() -> Option<String> {
    let text = fs::read_to_string(bridge_log_path()).ok()?;
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
}

/// Stop a locally running bridge.
///
/// Escalates to SIGKILL: a wedged bridge can ignore (or be stopped for)
/// SIGTERM, and one that keeps holding the port is exactly what makes every
/// later start fail. Success means the port is free again, not merely "quiet".
pub fn stop_bridge() -> Result<(), String> {
    let mut lifecycle = lifecycle::LIFECYCLE.lock().unwrap();
    lifecycle.enable(false);
    stop_bridge_inner()
}

fn stop_bridge_inner() -> Result<(), String> {
    // pkill exits 1 when nothing matched — that means already stopped.
    if pkill_bridge(&["-f", "super-desktop harness-bridge"])? == 1 {
        return Ok(());
    }
    if wait_port_free() {
        return Ok(());
    }
    let _ = pkill_bridge(&["-9", "-f", "super-desktop harness-bridge"]);
    if wait_port_free() {
        return Ok(());
    }
    Err(format!(
        "port {BRIDGE_PORT} is still held after SIGKILL — check `ss -ltnp | grep {BRIDGE_PORT}`"
    ))
}

fn pkill_bridge(args: &[&str]) -> Result<i32, String> {
    let st = Command::new("pkill")
        .args(args)
        .status()
        .map_err(|e| e.to_string())?;
    if st.success() || st.code() == Some(1) {
        Ok(st.code().unwrap_or(0))
    } else {
        Err(format!("pkill exited {}", st.code().unwrap_or(-1)))
    }
}

/// Wait for the bridge port to be released (up to ~2s).
fn wait_port_free() -> bool {
    for _ in 0..20 {
        if !port_taken(BRIDGE_PORT) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

// ---------- firewall (UFW) ----------

fn firewall_marker_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".local/state/omarchy/harness-bridge/firewall_open")
}

/// (label, unlock_button_enabled). Non-root readable; falls back to our own
/// marker when the rules file can't be read.
pub fn firewall_summary() -> (String, bool) {
    let conf = fs::read_to_string("/etc/ufw/ufw.conf").unwrap_or_default();
    let enabled = conf.lines().any(|l| l.trim() == "ENABLED=yes");
    if !enabled {
        return ("Firewall off — nothing to unlock".to_string(), false);
    }
    match fs::read_to_string("/etc/ufw/user.rules") {
        Ok(rules) => {
            let open = rules
                .lines()
                .any(|l| l.contains("--dport 8759") && l.contains("ACCEPT"));
            if open {
                ("Port 8759/tcp open ✓".to_string(), false)
            } else {
                ("UFW is blocking port 8759".to_string(), true)
            }
        }
        Err(_) => {
            if let Ok(stamp) = fs::read_to_string(firewall_marker_path()) {
                (
                    format!("Port 8759 allowed ✓ ({})", stamp.trim()),
                    false,
                )
            } else {
                (
                    "Firewall state unknown — Unlock to be sure".to_string(),
                    true,
                )
            }
        }
    }
}

/// Allow 8759/tcp via a polkit password prompt. Writes our marker on success.
pub fn unlock_firewall() -> Result<String, String> {
    let out = Command::new("pkexec")
        .args(["ufw", "allow", "8759/tcp"])
        .output()
        .map_err(|e| format!("pkexec failed to start: {e}"))?;
    if out.status.success() {
        let _ = fs::write(firewall_marker_path(), utc_now_iso());
        Ok("Port 8759/tcp allowed ✓".to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if err.is_empty() {
            Err("unlock cancelled or failed".to_string())
        } else {
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn address_inventory_labels_tailscale_and_only_selects_served_addresses() {
        let addresses = super::interface_addresses(&serde_json::json!([
            {"ifname":"wlan0","addr_info":[{"local":"192.168.1.5","scope":"global"}]},
            {"ifname":"tailscale0","addr_info":[{"local":"100.64.0.5","scope":"global"},{"local":"fd7a::5","scope":"global"}]},
            {"ifname":"lo","addr_info":[{"local":"127.0.0.1","scope":"host"}]}
        ]));
        assert_eq!(addresses.len(), 4);
        assert_eq!(addresses[0]["address"], "100.64.0.5");
        assert_eq!(addresses[0]["connectable"], true);
        assert!(addresses.iter().filter(|a| a["address"] == "fd7a::5" || a["kind"] == "loopback").all(|a| a["connectable"] == false));
    }

    #[test]
    fn stable_bridge_identity_migrates_without_losing_tokens() {
        let old: super::BridgeConfig = serde_json::from_str(r#"{"paired_tokens":["existing"]}"#).unwrap();
        assert!(!old.bridge_id.is_empty());
        let restored: super::BridgeConfig = serde_json::from_str(&serde_json::to_string(&old).unwrap()).unwrap();
        assert_eq!(old.bridge_id, restored.bridge_id);
        assert_eq!(restored.paired_tokens, vec!["existing"]);
    }
    #[test]
    fn creation_workspace_is_explicit_and_invalid_paths_never_fall_back() {
        use serde_json::json;
        assert_eq!(super::creation_command("shell", &json!({})), Some("add-term shell".into()));
        for value in [json!(null), json!(42), json!(""), json!("/nonexistent/sd-workspace-test")] {
            assert!(super::creation_command("shell", &json!({"workspace":value})).is_none());
        }
        let directory = std::env::temp_dir().join(format!("sd bridge path with spaces {}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let command = super::creation_command("shell", &json!({"workspace":directory})).unwrap();
        let body: serde_json::Value = serde_json::from_str(command.strip_prefix("add-term-in ").unwrap()).unwrap();
        assert_eq!(body["workspace"], directory.canonicalize().unwrap().to_string_lossy().as_ref());
        assert_eq!(body["agentType"], "shell");
        std::fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn persistent_input_keeps_order_and_acknowledges_errors() {
        let id = format!("sd_perf_test_{}", std::process::id());
        struct Session(String);
        impl Drop for Session {
            fn drop(&mut self) { let _ = Command::new("tmux").args(["kill-session", "-t", &self.0]).output(); }
        }
        let result = Command::new("tmux").args(["new-session", "-d", "-s", &id, "cat"]).output().unwrap();
        assert!(result.status.success());
        let _session = Session(id.clone());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream_keys(&mut Connection::plain(stream), &id);
        });
        // Pipeline messages without waiting for each acknowledgement.
        ws_send(&mut client, serde_json::json!({"sequence":1,"text":"alpha","enter":true}));
        ws_send(&mut client, serde_json::json!({"sequence":2,"text":"beta","enter":true}));
        ws_send(&mut client, serde_json::json!({"sequence":3,"text":"x".repeat(4097)}));
        for sequence in 1..=3 {
            let ack = ws_receive(&mut client);
            assert_eq!(ack["sequence"], sequence);
            assert_eq!(ack["ok"], sequence < 3);
        }
        assert_eq!(last_user_text(&_session.0, "codex", None, "").as_deref(), Some("beta"));
        // Draft input and rejected submissions must not replace the last prompt.
        ws_send(&mut client, serde_json::json!({"sequence":4,"text":"draft","enter":false}));
        assert_eq!(ws_receive(&mut client)["ok"], true);
        ws_send(&mut client, serde_json::json!({"sequence":5,"text":"x".repeat(4097),"enter":true}));
        assert_eq!(ws_receive(&mut client)["ok"], false);
        assert_eq!(last_user_text(&_session.0, "codex", None, "").as_deref(), Some("beta"));
        ws_send(&mut client, serde_json::json!({"sequence":6,"text":"\nUnicode привіт ✓","enter":true}));
        assert_eq!(ws_receive(&mut client)["ok"], true);
        assert_eq!(last_user_text(&_session.0, "codex", None, "").as_deref(), Some("Unicode привіт ✓"));
        let screen = capture_pane_text(&_session.0).unwrap();
        assert!(screen.find("alpha").unwrap() < screen.find("beta").unwrap());
        let mut control = crate::tmux_control::Control::open(&_session.0).unwrap();
        assert_eq!(control.capture().unwrap(), crate::tmux::capture_pane_ansi(&_session.0).unwrap());
        control.send("literal ' ; $() \\ Ukrainian: привіт", true).unwrap();
        let expected = "literal ' ; $() \\ Ukrainian: привіт";
        let appeared = (0..20).any(|_| {
            if control.capture().unwrap().contains(expected) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
            false
        });
        assert!(appeared, "tmux never rendered the acknowledged input");
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn persistent_input_reopens_a_failed_tmux_connection() {
        // One failed tmux command used to poison the socket's control client,
        // so every later phone input was refused with "tmux connection must
        // be reopened" until the phone dropped the socket.
        let id = format!("sd_reopen_test_{}", std::process::id());
        struct Session(String);
        impl Drop for Session {
            fn drop(&mut self) { let _ = Command::new("tmux").args(["kill-session", "-t", &self.0]).output(); }
        }
        let result = Command::new("tmux").args(["new-session", "-d", "-s", &id, "cat"]).output().unwrap();
        assert!(result.status.success());
        let session = Session(id.clone());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream_keys(&mut Connection::plain(stream), &id);
        });
        ws_send(&mut client, serde_json::json!({"sequence":1,"text":"alpha","enter":true}));
        assert_eq!(ws_receive(&mut client)["ok"], true);

        // Drop the bridge's control client from under it, as a tmux hiccup would.
        let detached = Command::new("tmux").args(["detach-client", "-s", &format!("={}", session.0)]).output().unwrap();
        assert!(detached.status.success());
        ws_send(&mut client, serde_json::json!({"sequence":2,"text":"beta","enter":true}));
        let failed = ws_receive(&mut client);
        assert_eq!(failed["sequence"], 2);

        ws_send(&mut client, serde_json::json!({"sequence":3,"text":"gamma","enter":true}));
        let ack = ws_receive(&mut client);
        assert_eq!(ack["sequence"], 3);
        assert_eq!(ack["ok"], true, "input after a failed command must reopen tmux: {ack}");
        let appeared = (0..40).any(|_| {
            if capture_pane_text(&session.0).is_some_and(|screen| screen.contains("gamma")) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
            false
        });
        assert!(appeared, "the reopened client never delivered the input");
        if failed["ok"] == false {
            // A failed input is reported, never replayed on the new client.
            assert!(!capture_pane_text(&session.0).unwrap().contains("beta"));
        }
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn theme_document_contains_the_android_palette_contract() {
        let theme = super::theme_document();
        for key in [
            "name",
            "mode",
            "accent",
            "background",
            "darkBackground",
            "darkerBackground",
            "lighterBackground",
            "foreground",
            "darkForeground",
            "brightForeground",
            "brightRed",
            "brightYellow",
            "brightGreen",
            "brightCyan",
            "brightBlue",
            "brightMagenta",
        ] {
            assert!(theme.get(key).and_then(|v| v.as_str()).is_some(), "missing {key}");
        }
    }

    #[test]
    fn settings_come_from_live_footer_only() {
        assert_eq!(super::harness_model_effort("codex", "prompt\n  gpt-6-astra medium · ~/project · title"),
            (Some("gpt-6-astra".into()), Some("medium".into())));
        assert_eq!(super::harness_model_effort("reasonix", "MODEL deepseek-v4-flash   EFFORT auto\nstatus"),
            (Some("deepseek-v4-flash".into()), Some("auto".into())));
        assert_eq!(super::harness_model_effort("codex", &format!("gpt-6-astra high · old\n{}", "blank\n".repeat(8))), (None, None));
        assert_eq!(super::harness_model_effort("shell", "gpt-6-astra high · text"), (None, None));
    }
    use super::*;

    /// A masked client text frame, as the phone sends one.
    fn ws_send(client: &mut TcpStream, body: serde_json::Value) {
        let bytes = body.to_string().into_bytes();
        let mut frame = vec![0x81];
        if bytes.len() < 126 { frame.push(0x80 | bytes.len() as u8); }
        else { frame.push(0xfe); frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes()); }
        frame.extend_from_slice(&[0, 0, 0, 0]);
        frame.extend_from_slice(&bytes);
        client.write_all(&frame).unwrap();
    }

    /// One unmasked server text frame, parsed as JSON.
    fn ws_receive(client: &mut TcpStream) -> serde_json::Value {
        let mut header = [0u8; 2];
        client.read_exact(&mut header).unwrap();
        assert_eq!(header[0], 0x81);
        let mut size = usize::from(header[1]);
        if size == 126 {
            let mut extended = [0; 2]; client.read_exact(&mut extended).unwrap();
            size = usize::from(u16::from_be_bytes(extended));
        }
        let mut body = vec![0; size]; client.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Reads one HTTP response: (status, Connection header, body).
    fn read_response(client: &mut TcpStream) -> (u16, String, String) {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            client.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let header = |name: &str| head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim().to_string())
        });
        let length: usize = header("content-length").unwrap().parse().unwrap();
        let mut body = vec![0u8; length];
        client.read_exact(&mut body).unwrap();
        (head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            header("connection").unwrap(), String::from_utf8(body).unwrap())
    }

    #[test]
    fn keep_alive_serves_pipelined_requests_and_honors_close() {
        // Browser-origin requests are refused before any pairing lookup, so
        // this exercises the connection loop without bridge credentials.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            serve_connection(Connection::plain(socket), None);
        });
        // Two requests in one write; the first has a body that must not leak
        // into the second request's parse.
        client.write_all(b"POST /api/v1/completions HTTP/1.1\r\nOrigin: https://x\r\nContent-Length: 5\r\n\r\nhello\
GET /api/v1/ping HTTP/1.1\r\nOrigin: https://x\r\n\r\n").unwrap();
        for _ in 0..2 {
            let (status, connection, body) = read_response(&mut client);
            assert_eq!((status, connection.as_str()), (403, "keep-alive"));
            assert!(body.contains("browser_access_disabled"));
        }
        // A later request on the same socket, asking to close.
        client.write_all(b"GET /api/v1/ping HTTP/1.1\r\nOrigin: https://x\r\nConnection: close\r\n\r\n").unwrap();
        let (status, connection, _) = read_response(&mut client);
        assert_eq!((status, connection.as_str()), (403, "close"));
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "nothing after a close response");
        server.join().unwrap();
    }

    #[test]
    fn http10_requests_are_not_kept_alive() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            serve_connection(Connection::plain(socket), None);
        });
        client.write_all(b"GET /api/v1/ping HTTP/1.0\r\nOrigin: https://x\r\n\r\n").unwrap();
        assert_eq!(read_response(&mut client).1, "close");
        server.join().unwrap();
    }

    #[test]
    fn test_port_taken_follows_the_listener() {
        // `start_bridge` relies on this to tell "free" from "someone (maybe a
        // wedged bridge) is there". The port stays bound (reserved) for the
        // whole test and only its listening state changes: releasing it let a
        // parallel test's `bind(0)` take it, which made this test flaky.
        use std::os::fd::{FromRawFd, OwnedFd, AsRawFd};
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        assert!(fd >= 0);
        let socket = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        address.sin_family = libc::AF_INET as libc::sa_family_t;
        address.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();
        let size = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        assert_eq!(unsafe { libc::bind(socket.as_raw_fd(), (&address as *const libc::sockaddr_in).cast(), size) }, 0);
        let mut bound = address;
        let mut length = size;
        assert_eq!(unsafe { libc::getsockname(socket.as_raw_fd(), (&mut bound as *mut libc::sockaddr_in).cast(), &mut length) }, 0);
        let port = u16::from_be(bound.sin_port);
        assert!(!port_taken(port), "a bound but not listening port must read as free");
        assert_eq!(unsafe { libc::listen(socket.as_raw_fd(), 1) }, 0);
        assert!(port_taken(port), "listening port must read as taken");
    }

    #[test]
    fn test_bridge_exe_is_spawnable() {
        // Whatever we pick must exist: a rebuild that replaced the running
        // binary used to leave `current_exe()` pointing at a dead path.
        let exe = bridge_exe().expect("a super-desktop binary must be found");
        assert!(exe.is_file(), "{} is not a file", exe.display());
    }

    #[test]
    fn test_tag_color_follows_the_desktop_palette() {
        // The phone paints rows with the same swatch the desktop card shows.
        assert_eq!(tag_color(0), None, "untagged cards have no colour");
        assert_eq!(tag_color(9), None, "out-of-range tags fall back to none");
        assert_eq!(tag_color(5), Some("#22d3ee"), "cyan (the default tag)");
        assert_eq!(tag_color(1), Some("#f87171"));
        assert_eq!(tag_color(crate::tag::TAG_CYAN), Some("#22d3ee"));
        // Every palette entry resolves to a hex colour.
        for n in 1..=crate::tag::TAG_COUNT {
            assert!(tag_color(n).is_some_and(|c| c.starts_with('#') && c.len() == 7));
        }
    }

    #[test]
    fn test_launcher_directory_tracks_shell_cwd_and_harness_home() {
        assert_eq!(
            launcher_directory("shell", "/home/me", "/tmp/project"),
            ("/tmp/project".to_string(), "cwd")
        );
        assert_eq!(
            launcher_directory("codex", "/home/me/Github/app", "/tmp/other"),
            ("/home/me/Github/app".to_string(), "home")
        );
        assert_eq!(
            launcher_directory("terminal", "/home/me", ""),
            ("/home/me".to_string(), "cwd")
        );
    }

    #[test]
    fn test_launcher_preview_keeps_directory_visible_in_its_tail() {
        let screen = (0..20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let preview = preview_with_directory(&screen, "home", "~/Github/app");
        let lines: Vec<_> = preview.lines().collect();
        assert_eq!(lines.len(), 12);
        assert_eq!(lines.first(), Some(&"line 9"));
        assert_eq!(lines.last(), Some(&"home · ~/Github/app"));
        assert!(lines.iter().rev().take(3).any(|line| line.starts_with("home · ")));
    }

    #[test]
    fn agent_response_markers_are_not_prompt_fallbacks() {
        assert_eq!(last_user_text(
            "sd_term_missing_prompt_test", "shell", None,
            "Task output costs $5\n# Build summary\n> noisy output",
        ), None);
        assert_eq!(last_user_text(
            "sd_term_missing_prompt_test", "claude", None,
            "Answer costs $5\n# Summary\n> Last sentence of the response",
        ), None);
    }

    #[test]
    fn test_browse_lines_identify_our_service() {
        // Real `avahi-browse -rtp` output (spaces escaped in the name).
        let line = r#"=";tailscale0;IPv4;Harness\032Probe;_omarchy-harness._tcp;local;gladimdim-b9.local;100.67.193.30;8759;"ver=1" "host=gladimdim-b9""#;
        assert!(browse_line_matches(line, 8759));
        // Other ports, other services and the browse `+` lines do not count.
        assert!(!browse_line_matches(line, 8760));
        assert!(!browse_line_matches(
            "=;wlo1;IPv4;x;_other._tcp;local;h.local;10.0.0.1;8759;",
            8759
        ));
        assert!(!browse_line_matches(
            "+;wlo1;IPv4;x;_omarchy-harness._tcp;local",
            8759
        ));
    }

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    #[test]
    fn test_browse_is_bounded_and_uses_lines_printed_before_a_hang() {
        let ours = |line: &str| browse_line_matches(line, 8759);
        let record = "=;wlo1;IPv4;x;_omarchy-harness._tcp;local;h.local;10.0.0.1;8759;";
        // Our record resolved, then the sweep hangs on another host: found at
        // once, and the stuck child is killed (reaping it would otherwise
        // wait for the 30 s sleep).
        let start = std::time::Instant::now();
        let script = format!("echo '+;wlo1;IPv4;x;_omarchy-harness._tcp;local'; echo '{record}'; exec sleep 30");
        assert!(any_output_line(sh(&script), Duration::from_secs(10), ours));
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        // A sweep that hangs without our record gives up at the timeout.
        let start = std::time::Instant::now();
        assert!(!any_output_line(sh("echo '+;wlo1;IPv4;x;_other._tcp;local'; exec sleep 30"),
            Duration::from_millis(300), ours));
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(300) && took < Duration::from_secs(5), "{took:?}");
        // A sweep that finishes: every line counts, the last even without a newline.
        assert!(any_output_line(sh(&format!("echo other; printf '%s' '{record}'")), Duration::from_secs(10), ours));
        assert!(!any_output_line(sh("echo other"), Duration::from_secs(10), ours));
        // A missing avahi-browse is "not advertised", never a hang.
        assert!(!any_output_line(Command::new("/nonexistent/avahi-browse"), Duration::from_secs(10), ours));
        assert!(BROWSE_TIMEOUT <= Duration::from_secs(3));
    }

    /// One request on a connection served in-process: (status, body).
    fn serve_one(connection: Connection, client: &mut impl Read, request: &str, writer: &mut impl Write) -> (u16, serde_json::Value) {
        let server = std::thread::spawn(move || serve_connection(connection, None));
        writer.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        (head.split_whitespace().nth(1).unwrap().parse().unwrap(), serde_json::from_str(body).unwrap())
    }

    #[test]
    fn test_daemon_health_probe_skips_address_discovery_only_on_the_control_socket() {
        let probe = format!("GET /api/v1/ping?{HEALTH_QUERY} HTTP/1.1\r\nConnection: close\r\n\r\n");
        // The daemon's probe over the owner-only socket: just what
        // `bridge_running` checks, with no addresses (no `ip` run).
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut writer = client.try_clone().unwrap();
        let (status, body) = serve_one(Connection::local(server), &mut client, &probe, &mut writer);
        assert_eq!(status, 200);
        assert_eq!((body["status"].as_str(), body["service"].as_str()), (Some("ok"), Some(SERVICE_NAME)));
        for field in ["addresses", "lanIp", "tailscaleIp", "hostname", "time"] {
            assert!(body.get(field).is_none(), "{field} in {body}");
        }
        // Over the network the same query is the unchanged phone-facing ping.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let (server, _) = listener.accept().unwrap();
        let mut writer = client.try_clone().unwrap();
        let (status, body) = serve_one(Connection::plain(server), &mut client, &probe, &mut writer);
        assert_eq!(status, 200);
        for field in ["status", "service", "protocolVersion", "hostname", "lanIp", "bridgeId", "addresses", "tailscaleIp", "port", "time"] {
            assert!(body.get(field).is_some(), "{field} missing from {body}");
        }
    }

    #[test]
    fn test_mdns_state_sits_next_to_the_bridge_config() {
        assert_eq!(mdns_state_path().file_name().unwrap(), "mdns.json");
        assert_eq!(mdns_state_path().parent(), state_path().parent());
    }

    #[test]
    fn test_mdns_summary_reports_offline_bridge() {
        let text = mdns_summary(false);
        assert!(text.starts_with(MDNS_SERVICE_TYPE), "got: {text}");
        assert!(text.contains("bridge offline"), "got: {text}");
    }

    #[test]
    fn test_bridge_log_sits_next_to_the_bridge_config() {
        let log = bridge_log_path();
        assert_eq!(log.file_name().unwrap(), "bridge.log");
        assert_eq!(
            log.parent(),
            state_path().parent(),
            "the bridge log belongs in the harness-bridge state dir"
        );
    }
}
