//! Harness bridge hosted by super-desktop (Rust-only backend).
//!
//! Re-exports this machine's `sd_term_*` tmux sessions over LAN / Tailscale
//! as JSON for the OmarchyAILauncher Android client. The launcher only reads
//! this API and reacts to user actions — all session truth lives here,
//! reusing `crate::tmux` status/prompt helpers directly (no shell-out
//! re-implementation of the heuristics).
//!
//! Endpoints (see OmarchyAILauncher/PROTOCOL.md, wire v1):
//!   GET  /api/v1/ping
//!   GET  /api/v1/harnesses            (Bearer token or loopback)
//!   GET  /api/v1/theme                 (Bearer token or loopback)
//!   DELETE /api/v1/harnesses/<id>     (Bearer token or loopback)
//!   POST /api/v1/pair                 (open; requests desktop approval)
//!   POST /api/v1/pair/poll            (unguessable request capability)
//!   GET  /api/v1/pair/state           (loopback-only pending requests)
//!   POST /api/v1/pair/approve, /deny   (loopback-only decision)

use serde::{Deserialize, Serialize};
#[path = "bridge_pairing.rs"]
mod pairing;
pub use pairing::{pending_requests, decide_request, paired_devices, revoke_device, pairing_invitation};
#[path = "bridge_security.rs"]
mod security;
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
    capture_pane_text, extract_last_prompt, get_agent_config,
    get_composer_draft, get_opencode_user_text_by_id, inspect_status,
    inspect_status_with_screen, resolve_own_opencode_id, resolve_workspace_dir,
    strip_terminal_escapes, truncate_prompt_title, SessionStatus,
};

pub const BRIDGE_PORT: u16 = 8759;
const SERVICE_NAME: &str = "Omarchy Harness Bridge";
const PROTOCOL_VERSION: u32 = 3;
#[path = "desktop_bridge.rs"]
mod desktop;
static INPUT_ACTIVITY: (Mutex<u64>, Condvar) = (Mutex::new(0), Condvar::new());

fn wake_terminal_streams() {
    if let Ok(mut generation) = INPUT_ACTIVITY.0.lock() {
        *generation = generation.wrapping_add(1);
        INPUT_ACTIVITY.1.notify_all();
    }
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

fn live_sessions() -> Vec<String> {
    let out = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output();
    let mut sessions: Vec<(String, i64)> = vec![];
    if let Ok(o) = out {
        if o.status.success() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let name = line.trim().to_string();
                if name.starts_with("sd_term_") {
                    sessions.push((name, 0));
                }
            }
        }
    }
    // Order oldest-first when creation times are known via state.json.
    let state = load_state();
    let order: HashMap<String, f64> = state
        .terminals
        .iter()
        .map(|t| (t.session_name.clone(), t.created_at))
        .collect();
    sessions.sort_by(|a, b| {
        order
            .get(&a.0)
            .unwrap_or(&f64::MAX)
            .partial_cmp(order.get(&b.0).unwrap_or(&f64::MAX))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sessions.into_iter().map(|(n, _)| n).collect()
}

fn last_user_text(
    session: &str,
    agent_type: &str,
    persisted: Option<&str>,
    screen: &str,
) -> Option<String> {
    // Codex sets the pane title to its submitted task and workspace. The
    // terminal screen may contain only the response, so parse this first.
    if agent_type == "codex" {
        if let Some(prompt) = codex_prompt_from_pane_title(session) {
            return Some(prompt);
        }
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
    extract_last_prompt(screen)
        .map(|s| truncate_prompt_title(&s))
}

fn codex_prompt_from_pane_title(session: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "-t", session, "#{pane_title}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    codex_prompt_from_title(&String::from_utf8_lossy(&output.stdout))
}

/// Codex titles end in the workspace. When action is required, the title is
/// `status | prompt | workspace`, so the prompt is always the penultimate part.
fn codex_prompt_from_title(title: &str) -> Option<String> {
    let parts: Vec<_> = title.split('|').map(str::trim).filter(|part| !part.is_empty()).collect();
    let candidate = parts.get(parts.len().checked_sub(2)?).copied()?;
    let candidate = candidate.trim_start_matches(|c: char| {
        c.is_whitespace() || ('\u{2801}'..='\u{28FF}').contains(&c)
    });
    let cleaned = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
    (cleaned.chars().count() >= 2).then(|| truncate_prompt_title(&cleaned))
}

fn is_regular_terminal(agent_type: &str) -> bool {
    matches!(agent_type, "shell" | "bash" | "terminal")
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
    // agent type, fallback command, persisted agent session id and the user's
    // group colour (super-desktop's 8-swatch tag) for every session we know
    // about.
    let meta: HashMap<String, LauncherSessionMeta> = state
        .terminals
        .iter()
        .map(|t| {
            (
                t.session_name.clone(),
                (
                    t.agent_type.clone(),
                    t.command.clone(),
                    t.agent_session_id.clone(),
                    t.tag,
                    t.workspace_dir.clone(),
                ),
            )
        })
        .collect();
    let mut out = vec![];
    for session in live_sessions() {
        let (agent_type, cmd_fallback, persisted_sid, tag, workspace_dir) = meta
            .get(&session)
            .cloned()
            .unwrap_or_else(|| ("shell".to_string(), String::new(), None, 0, None));
        let cfg = get_agent_config(&agent_type);
        let screen = capture_pane_text(&session).unwrap_or_default();
        let (model, effort) = harness_model_effort(&agent_type, &screen);
        let status = inspect_status_with_screen(&session, &agent_type, &screen);
        let harness_home = resolve_workspace_dir(workspace_dir.as_deref());
        let (directory, directory_kind) =
            launcher_directory(&agent_type, &harness_home, &status.cwd);
        let directory_display = crate::state::display_dir(&directory);
        let cmd = if status.cmd.is_empty() {
            if cmd_fallback.is_empty() {
                agent_type.clone()
            } else {
                cmd_fallback.clone()
            }
        } else {
            status.cmd.clone()
        };
        out.push(serde_json::json!({
            "id": session,
            "agentType": agent_type,
            "agentName": cfg.name,
            "model": model,
            "effort": effort,
            "icon": cfg.icon,
            "status": status.status,
            "label": status.label,
            "pid": status.pid,
            "cmd": cmd,
            "lastPrompt": last_user_text(
                &session,
                &agent_type,
                persisted_sid.as_deref(),
                &screen,
            ),
            "composerDraft": get_composer_draft(&session),
            "preview": preview_with_directory(&screen, directory_kind, &directory_display),
            "directory": &directory,
            "directoryDisplay": &directory_display,
            "directoryKind": directory_kind,
            "homeDirectory": (directory_kind == "home").then_some(directory.as_str()),
            "cwd": (directory_kind == "cwd").then_some(directory.as_str()),
            // Group colour: the same 8-swatch tag the desktop card shows, so a
            // phone row can be coloured identically (`tagColor` is null when
            // the card has no tag).
            "tag": tag,
            "tagColor": tag_color(tag),
            "updatedAt": utc_now_iso(),
        }));
    }
    out
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairedDevice { id: String, name: String, token_hash: String, expires: f64 }

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
    let dir = std::env::var_os("SUPER_DESKTOP_BRIDGE_STATE_DIR").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".local/state/omarchy/harness-bridge"));
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
            };
        }
        self.mtime = config_mtime();
    }

    fn valid(&mut self, token: &str) -> bool {
        self.refresh();
        !token.is_empty() && self.cfg.devices.iter().any(|d| d.expires > now_epoch() && security::equal(&d.token_hash, &security::digest(token.as_bytes())))
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
    let path = raw_path.split('?').next().unwrap_or("/").to_string();
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
    let image_upload = method == "POST" && crate::prompt_image::route(&path).is_some();
    if headers.contains_key("transfer-encoding") { return None; }
    let mut upload_slot = None;
    if image_upload {
        let head = Request { method: method.clone(), path: path.clone(), headers: headers.clone(), body: String::new(), _upload_slot: None };
        if headers.contains_key("origin") || headers.contains_key("sec-fetch-site") {
            respond(stream, 403, "Forbidden", &serde_json::json!({"error":"browser_access_disabled"}));
            return None;
        }
        if !authorize(&head, false) {
            respond(stream, 401, "Unauthorized", &serde_json::json!({"error":"not_paired"}));
            return None;
        }
        if content_len > crate::prompt_image::MAX_BODY {
            respond(stream, 413, "Payload Too Large", &serde_json::json!({"error":"image_too_large"}));
            return None;
        }
        upload_slot = crate::assets::Transfer::acquire();
        if upload_slot.is_none() {
            respond(stream, 429, "Too Many Requests", &serde_json::json!({"error":"busy"}));
            return None;
        }
        let token = bearer(&headers);
        stream.credential(&token);
        if let Some(guard) = admission { guard.identify(&token); }
        stream.upload_deadline();
    } else if content_len > 16384 { return None; }
    let mut body = buf[header_end..total].to_vec();
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
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
        _upload_slot: upload_slot,
    })
}

fn respond(stream: &mut Connection, code: u16, reason: &str, value: &serde_json::Value) {
    let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn bearer(headers: &HashMap<String, String>) -> String {
    let h = headers.get("authorization").cloned().unwrap_or_default();
    if h.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("bearer ")) {
        h.get(7..).unwrap_or("").trim().to_string()
    } else {
        String::new()
    }
}

/// Bearer-token or loopback authorization (same rule as `/api/v1/harnesses`).
fn authorize(req: &Request, _local: bool) -> bool {
    pair_state()
            .lock()
            .map(|mut s| s.valid(&bearer(&req.headers)))
            .unwrap_or(false)
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

/// Push the full `/harnesses` document once a second.
///
/// Liveness comes from writes: a client that vanished makes the write (or the
/// 5s write timeout) fail, which ends the thread — no reader thread needed.
fn stream_harness_list(mut stream: Connection) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let deadline = std::time::Instant::now() + Duration::from_secs(STREAM_MAX_SECS);
    while std::time::Instant::now() < deadline {
        if !ws_client_alive(&mut stream) {
            return;
        }
        let document = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "timestamp": utc_now_iso(),
            "harnesses": collect_harnesses(),
            "usage": crate::usage::launcher_usage(),
            "theme": theme_document(),
        });
        if crate::ws::write_text(&mut stream, &document.to_string()).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
    let _ = crate::ws::write_close(&mut stream, 1000, "reconnect");
}

/// Push changed terminal snapshots, waking immediately after phone input.
///
/// The first frame doubles as the "attached" signal; the phone renders `tail`
/// in its terminal screen and stops when `status` turns `EXITED`.
fn stream_one_harness(mut stream: Connection, id: &str) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let deadline = std::time::Instant::now() + Duration::from_secs(STREAM_MAX_SECS);
    let (agent_type, tag) = session_meta(id);
    let Ok(mut control) = crate::tmux_control::Control::open(id) else {
        let _ = crate::ws::write_close(&mut stream, 1011, "terminal unavailable");
        return;
    };
    let mut cached_status = inspect_status(id, &agent_type);
    let mut status_updated = std::time::Instant::now();
    let mut previous: Option<String> = None;
    let mut active_until = std::time::Instant::now();
    let mut heartbeat = std::time::Instant::now();
    while std::time::Instant::now() < deadline {
        let input_generation = INPUT_ACTIVITY.0.lock().map(|g| *g).unwrap_or(0);
        if !ws_client_alive(&mut stream) {
            return;
        }
        // One styled tmux capture drives both representations. This is cheaper
        // than capturing once for status/plain text and again for colour.
        let captured = control.capture();
        let alive = captured.is_ok();
        let ansi_tail = captured.ok();
        let plain_tail = ansi_tail.as_deref().map(strip_terminal_escapes);
        if !alive {
            cached_status = SessionStatus {
                status: "EXITED",
                label: "○ EXITED",
                pid: String::new(),
                cmd: String::new(),
                cwd: String::new(),
            };
        } else if status_updated.elapsed() >= Duration::from_millis(500) {
            cached_status = inspect_status_with_screen(id, &agent_type, plain_tail.as_deref().unwrap_or(""));
            status_updated = std::time::Instant::now();
        }
        let status = &cached_status;
        let mut document = serde_json::json!({
            "id": id,
            "agentType": agent_type,
            "status": status.status,
            "label": status.label,
            "tag": tag,
            "tagColor": tag_color(tag),
            // `tail` remains plain for existing launchers. `tailAnsi` is the
            // exact tmux styling for clients that render ANSI SGR attributes.
            "tail": plain_tail,
            "tailAnsi": ansi_tail,
            "tailFormat": alive.then_some("ansi-sgr"),
        });
        let frame = document.to_string();
        let changed = previous.as_deref() != Some(frame.as_str());
        if changed || heartbeat.elapsed() >= Duration::from_secs(5) {
            if changed { active_until = std::time::Instant::now() + Duration::from_secs(2); }
            document["updatedAt"] = serde_json::json!(utc_now_iso());
            if crate::ws::write_text(&mut stream, &document.to_string()).is_err() {
                return;
            }
            previous = Some(frame);
            heartbeat = std::time::Instant::now();
        }
        if !alive {
            let _ = crate::ws::write_close(&mut stream, 1000, "session ended");
            return;
        }
        let delay = Duration::from_millis(if std::time::Instant::now() < active_until { 16 } else { 100 });
        if let Ok(generation) = INPUT_ACTIVITY.0.lock() {
            if let Ok((generation, _)) = INPUT_ACTIVITY.1.wait_timeout_while(
                generation, delay, |g| *g == input_generation,
            ) {
                if *generation != input_generation {
                    active_until = std::time::Instant::now() + Duration::from_secs(2);
                }
            }
        }
    }
    let _ = crate::ws::write_close(&mut stream, 1000, "reconnect");
}

/// Ordered input on a persistent socket. Never replay an input after a lost
/// acknowledgement: the client must treat that outcome as uncertain.
fn stream_keys(mut stream: Connection, id: &str) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let (agent, _) = session_meta(id);
    let Ok(mut control) = crate::tmux_control::Control::open(id) else {
        let _ = crate::ws::write_close(&mut stream, 1011, "terminal unavailable");
        return;
    };
    loop {
        match crate::ws::read_frame(&mut stream) {
            Ok(Some(crate::ws::Frame::Text(text))) => {
                let Ok(body) = serde_json::from_str::<serde_json::Value>(&text) else { break };
                let value = body["text"].as_str().unwrap_or("");
                let enter = body["enter"].as_bool().unwrap_or(false);
                let result = (|| -> Result<(), String> {
                    let _input = crate::prompt_image::input_guard(id)?;
                    if value.len() > 4096 { return Err("text_too_long".into()); }
                    if body["checkIdle"].as_bool().unwrap_or(false) {
                        if inspect_status(id, &agent).status != "IDLE" {
                            return Err("Wait for the harness to become idle.".into());
                        }
                        if get_composer_draft(id).is_some_and(|s| !s.trim().is_empty()) {
                            return Err("Finish or clear the remote draft first.".into());
                        }
                    }
                    if !stream.still_authorized() { return Err("device_revoked".into()); }
                    control.send(value, false)?;
                    wake_terminal_streams();
                    // Codex paste detection needs settling, but it does not
                    // need another phone-to-bridge round trip.
                    if enter && !value.is_empty() && agent == "codex" {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    if !stream.still_authorized() { return Err("device_revoked".into()); }
                    if enter { control.send("", true)?; }
                    wake_terminal_streams();
                    Ok(())
                })();
                let ack = serde_json::json!({"sequence": body["sequence"],
                    "ok": result.is_ok(), "error": result.err()});
                if crate::ws::write_text(&mut stream, &ack.to_string()).is_err() { break; }
            }
            Ok(Some(crate::ws::Frame::Ping(payload))) => {
                if crate::ws::write_pong(&mut stream, &payload).is_err() { break; }
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
fn handle_keys(stream: &mut Connection, req: &Request, id: &str, local: bool) {
    if !authorize(req, local) {
        return respond(
            stream,
            401,
            "Unauthorized",
            &serde_json::json!({"status": "error", "error": "not_paired"}),
        );
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
fn handle_close_harness(stream: &mut Connection, req: &Request, id: &str, local: bool) {
    if !authorize(req, local) {
        return respond(
            stream,
            401,
            "Unauthorized",
            &serde_json::json!({"status": "error", "error": "not_paired"}),
        );
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

fn handle_client(mut stream: Connection, admission: Option<security::Admission>) {
    let req = match read_request(&mut stream, admission.as_ref()) {
        Some(r) => r,
        None => return,
    };
    let local = stream.is_local();
    if req.headers.contains_key("origin") || req.headers.contains_key("sec-fetch-site") {
        return respond(&mut stream, 403, "Forbidden", &serde_json::json!({"error":"browser_access_disabled"}));
    }
    if let Some(guard) = &admission { guard.identify(&bearer(&req.headers)); }
    // Recheck on each stream I/O as well as closing the socket: TLS may already
    // have buffered input when a device is revoked.
    if authorize(&req, false) {
        let token = bearer(&req.headers);
        security::note_activity(&token);
        stream.credential(&token);
    }
    let path = req.path.split('?').next().unwrap_or("").to_string();
    if req.method == "POST" {
        if let Some(session) = crate::prompt_image::route(&path) {
            if !authorize(&req, local) {
                return respond(&mut stream, 401, "Unauthorized", &serde_json::json!({"error":"not_paired"}));
            }
            stream.streaming();
            let body = serde_json::from_str(&req.body).unwrap_or_default();
            let result = crate::prompt_image::submit(session, &security::digest(bearer(&req.headers).as_bytes()), &body, || stream.still_authorized());
            wake_terminal_streams();
            return match result {
                Ok(()) => respond(&mut stream, 200, "OK", &serde_json::json!({"status":"submitted"})),
                Err(error) => respond(&mut stream, 409, "Conflict", &serde_json::json!({"error":error})),
            };
        }
    }

    // All pairing routes go through explicit desktop approval. In particular,
    // neither a legacy PIN nor an open window can mint a token any longer.
    if path == "/api/v1/pair" || path.starts_with("/api/v1/pair/") {
        return pairing::handle(&mut stream, &req, local, &path);
    }

    if let Some(rest) = path.strip_prefix("/api/v1/harnesses/") {
        let parts: Vec<_> = rest.split('/').collect();
        if parts.get(1) == Some(&"assets") {
            if !authorize(&req, local) {
                return respond(&mut stream, 401, "Unauthorized", &serde_json::json!({"error":"not_paired"}));
            }
            // Authenticated asset work has its own bounded renderer timeout;
            // the initial TLS/header/body deadline no longer applies.
            stream.streaming();
            let Some(_permit) = crate::assets::Transfer::acquire() else {
                return respond(&mut stream, 429, "Too Many Requests", &serde_json::json!({"error":"asset_transfer_busy"}));
            };
            if parts.len() == 2 && (req.method == "GET" || req.method == "POST") {
                let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
                let explicit = body["path"].as_str();
                if req.method == "POST" && explicit.is_none() {
                    return respond(&mut stream, 400, "Bad Request", &serde_json::json!({"error":"missing_path"}));
                }
                return match crate::assets::list(parts[0], if req.method == "POST" { explicit } else { None }) {
                    Ok(items) => respond(&mut stream, 200, "OK", &serde_json::json!({"assets":items,"maxFileBytes":crate::assets::MAX_FILE})),
                    Err(error) => respond(&mut stream, 400, "Bad Request", &serde_json::json!({"error":error})),
                };
            }
            if parts.len() == 4 && parts[3] == "content" && req.method == "GET" {
                return match crate::assets::read(parts[0], parts[2]) {
                    Ok((asset, bytes)) => {
                        let header = format!("HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nContent-Disposition: attachment\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", asset.mime_type, bytes.len());
                        if stream.write_all(header.as_bytes()).is_err() { return; }
                        // Connection checks revocation on every write, including buffered TLS.
                        for chunk in bytes.chunks(64 * 1024) {
                            if stream.write_all(chunk).is_err() { break; }
                        }
                    }
                    Err(error) => respond(&mut stream, 404, "Not Found", &serde_json::json!({"error":error})),
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
                        let header = format!("HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n", bytes.len());
                        if stream.write_all(header.as_bytes()).is_err() { return; }
                        for chunk in bytes.chunks(64 * 1024) { if stream.write_all(chunk).is_err() { break; } }
                    }
                    Err(error) => respond(&mut stream, 400, "Bad Request", &serde_json::json!({"error":error})),
                };
            }
            return respond(&mut stream, 404, "Not Found", &serde_json::json!({"error":"unknown_asset_route"}));
        }
    }

    if path == "/api/v1/workspaces" || path == "/api/v1/harness-types" || (path == "/api/v1/harnesses" && req.method == "POST") {
        if !authorize(&req, local) {
            return respond(&mut stream, 401, "Unauthorized", &serde_json::json!({"error":"not_paired"}));
        }
        if req.method == "GET" && path == "/api/v1/workspaces" {
            return match crate::ipc_request("workspace-choices") {
                crate::Ipc::Reply(reply) => match serde_json::from_str::<serde_json::Value>(&reply) {
                    Ok(value) => respond(&mut stream, 200, "OK", &value),
                    Err(_) => respond(&mut stream, 502, "Bad Gateway", &serde_json::json!({"error":"invalid_desktop_response"})),
                },
                _ => respond(&mut stream, 503, "Service Unavailable", &serde_json::json!({"error":"desktop_not_running"})),
            };
        }
        if req.method == "GET" && path == "/api/v1/harness-types" {
            let types: Vec<_> = crate::tmux::HARNESS_KEYS.iter().map(|key| {
                let config = get_agent_config(key);
                serde_json::json!({"id":key,"name":config.name,"icon":config.icon,
                    "available":crate::tmux::detect_harness_command(key).is_some()})
            }).collect();
            return respond(&mut stream, 200, "OK", &serde_json::json!({"types":types,
                "workspace":crate::state::effective_workspace_dir(&load_state())}));
        }
        if req.method == "POST" && path == "/api/v1/harnesses" {
            let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or(serde_json::Value::Null);
            let agent = body.get("agentType").and_then(|v| v.as_str()).unwrap_or("");
            if !crate::tmux::HARNESS_KEYS.contains(&agent) {
                return respond(&mut stream, 400, "Bad Request", &serde_json::json!({"error":"unsupported_harness"}));
            }
            if crate::tmux::detect_harness_command(agent).is_none() {
                return respond(&mut stream, 409, "Conflict", &serde_json::json!({"error":"harness_not_installed"}));
            }
            let Some(command) = creation_command(agent, &body) else {
                return respond(&mut stream, 400, "Bad Request", &serde_json::json!({"error":"invalid_workspace"}));
            };
            return match crate::ipc_request(&command) {
                crate::Ipc::Reply(reply) => {
                    let result: serde_json::Value = serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
                    if result["ok"] == true {
                        respond(&mut stream, 201, "Created", &result)
                    } else if result["error"] == "invalid_workspace" {
                        respond(&mut stream, 400, "Bad Request", &serde_json::json!({"error":"invalid_workspace"}))
                    } else {
                        respond(&mut stream, 500, "Internal Server Error", &serde_json::json!({"error":"creation_failed"}))
                    }
                }
                crate::Ipc::NoDaemon => respond(&mut stream, 503, "Service Unavailable", &serde_json::json!({"error":"desktop_not_running"})),
                crate::Ipc::Stalled => respond(&mut stream, 504, "Gateway Timeout", &serde_json::json!({"error":"creation_timeout_check_machine_before_retry"})),
            };
        }
    }

    // Dynamic harness routes: /api/v1/harnesses/<id>/stream (WebSocket, live
    // pane output), /keys (phone → harness input), and DELETE (close harness).
    // Only our own `sd_term_*` sessions are addressable, so a paired phone
    // cannot type into unrelated tmux sessions.
    if let Some(rest) = path.strip_prefix("/api/v1/harnesses/") {
        if req.method == "DELETE" && !rest.contains('/') {
            return handle_close_harness(&mut stream, &req, rest, local);
        }
        if let Some((id, action)) = rest.rsplit_once('/') {
            if id.starts_with("sd_term_") {
                match (req.method.as_str(), action) {
                    ("GET", "input") => {
                        if !authorize(&req, local) {
                            return respond(&mut stream, 401, "Unauthorized", &serde_json::json!({"error":"not_paired"}));
                        }
                        if !crate::tmux::session_alive(id) {
                            return respond(&mut stream, 404, "Not Found", &serde_json::json!({"error":"no_such_session"}));
                        }
                        if ws_upgrade(&mut stream, &req) { return stream_keys(stream, id); }
                        return respond(&mut stream, 400, "Bad Request", &serde_json::json!({"error":"expected_websocket"}));
                    }
                    ("GET", "stream") => {
                        if !authorize(&req, local) {
                            return respond(
                                &mut stream,
                                401,
                                "Unauthorized",
                                &serde_json::json!({"status": "error", "error": "not_paired"}),
                            );
                        }
                        return match ws_upgrade(&mut stream, &req) {
                            true => stream_one_harness(stream, id),
                            false => respond(
                                &mut stream,
                                400,
                                "Bad Request",
                                &serde_json::json!({"status": "error", "error": "expected_websocket"}),
                            ),
                        };
                    }
                    ("POST", "keys") => return handle_keys(&mut stream, &req, id, local),
                    _ => {}
                }
            }
        }
    }

    match (req.method.as_str(), path.as_str()) {
        ("GET", "/api/v1/desktop/capabilities") => {
            if !authorize(&req, local) {
                return respond(&mut stream, 401, "Unauthorized", &serde_json::json!({"error":"not_paired"}));
            }
            let machine_id = pair_state().lock().unwrap().cfg.bridge_id.clone();
            let capabilities = crate::desktop_protocol::Capabilities::current(machine_id);
            respond(&mut stream, 200, "OK", &serde_json::to_value(capabilities).unwrap());
        }
        ("GET", "/api/v1/desktop/workspace" | "/api/v1/desktop/events") => {
            if !authorize(&req, local) {
                return respond(&mut stream, 401, "Unauthorized", &serde_json::json!({"error":"not_paired"}));
            }
            if path.ends_with("/events") {
                if !ws_upgrade(&mut stream, &req) {
                    return respond(&mut stream, 400, "Bad Request", &serde_json::json!({"error":"expected_websocket"}));
                }
                desktop::stream_workspace(stream);
            } else {
                desktop::get_workspace(&mut stream);
            }
        }
        ("GET", "/api/v1/ping") => {
            let bridge_id = pair_state().lock().unwrap().cfg.bridge_id.clone();
            respond(
            &mut stream,
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
            let ok = pair_state()
                    .lock()
                    .map(|mut s| s.valid(&bearer(&req.headers)))
                    .unwrap_or(false);
            if !ok {
                return respond(
                    &mut stream,
                    401,
                    "Unauthorized",
                    &serde_json::json!({"status": "error", "error": "not_paired"}),
                );
            }
            respond(
                &mut stream,
                200,
                "OK",
                &serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "timestamp": utc_now_iso(),
                    "harnesses": collect_harnesses(),
                    "usage": crate::usage::launcher_usage(),
                    "theme": theme_document(),
                }),
            );
        }
        ("POST", "/api/v1/completions") => {
            if !authorize(&req, local) {
                return respond(&mut stream, 401, "Unauthorized", &serde_json::json!({"error":"not_paired"}));
            }
            let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
            let ids: Option<Vec<String>> = body["sessions"].as_array().filter(|v| v.len() <= 32).and_then(|v| {
                v.iter().map(|id| id.as_str().filter(|s| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')).map(str::to_string)).collect()
            });
            let Some(ids) = ids else {
                return respond(&mut stream, 400, "Bad Request", &serde_json::json!({"error":"invalid_sessions"}));
            };
            let Some(_job) = crate::assets::Transfer::acquire() else {
                return respond(&mut stream, 429, "Too Many Requests", &serde_json::json!({"error":"busy"}));
            };
            respond(&mut stream, 200, "OK", &serde_json::json!({"terminals": crate::completion::collect(&ids)}));
        }
        ("GET", "/api/v1/theme") => {
            if !authorize(&req, local) {
                return respond(
                    &mut stream,
                    401,
                    "Unauthorized",
                    &serde_json::json!({"status": "error", "error": "not_paired"}),
                );
            }
            respond(&mut stream, 200, "OK", &theme_document());
        }
        // PROTOCOL.md: full document on connect, then on every change (1s poll).
        ("GET", "/api/v1/harnesses/stream") => {
            if !authorize(&req, local) {
                return respond(
                    &mut stream,
                    401,
                    "Unauthorized",
                    &serde_json::json!({"status": "error", "error": "not_paired"}),
                );
            }
            if !ws_upgrade(&mut stream, &req) {
                return respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    &serde_json::json!({"status": "error", "error": "expected_websocket"}),
                );
            }
            stream_harness_list(stream);
        }
        _ => respond(
            &mut stream,
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
                        if let Ok(stream) = Connection::tls(s, tls) { handle_client(stream, Some(admission)); }
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

/// Single `avahi-browse -rtp` sweep for our own record.
fn browse_once(port: u16) -> bool {
    let out = Command::new("avahi-browse")
        .args(["-rtp", MDNS_SERVICE_TYPE])
        .output();
    let Ok(out) = out else { return false };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| browse_line_matches(line, port))
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

/// True when a bridge answers on loopback (this laptop).
pub fn bridge_running(port: u16) -> bool {
    bridge_ping_body(port).is_some()
}

fn bridge_ping_body(_port: u16) -> Option<String> {
    let mut s = std::os::unix::net::UnixStream::connect(security::control_path()).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    s.write_all(b"GET /api/v1/ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
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
    if bridge_running(BRIDGE_PORT) {
        return Ok(());
    }
    if port_taken(BRIDGE_PORT) {
        // Something is on our port: if it is a wedged bridge of ours, drop it
        // and take the port over; otherwise say who holds it.
        let _ = stop_bridge();
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
    let log = fs::File::create(bridge_log_path()).ok();
    let log_err = log.as_ref().and_then(|f| f.try_clone().ok());

    let mut command = Command::new(&exe);
    command
        .arg("harness-bridge")
        // Stable argv[0]: `stop_bridge()` and a user's `pkill` match on
        // "super-desktop harness-bridge" whichever path we re-executed.
        .arg0("super-desktop")
        .stdin(Stdio::null());
    match (log, log_err) {
        (Some(out), Some(err)) => {
            command.stdout(Stdio::from(out)).stderr(Stdio::from(err));
        }
        _ => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    command
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", exe.display()))?;

    // The first start of a desktop session can be slow (cold binary, busy box).
    for _ in 0..50 {
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
    TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(300),
    )
    .is_ok()
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
            stream_keys(Connection::plain(stream), &id);
        });
        fn send(client: &mut TcpStream, body: serde_json::Value) {
            let bytes = body.to_string().into_bytes();
            let mut frame = vec![0x81];
            if bytes.len() < 126 { frame.push(0x80 | bytes.len() as u8); }
            else { frame.push(0xfe); frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes()); }
            frame.extend_from_slice(&[0, 0, 0, 0]);
            frame.extend_from_slice(&bytes);
            client.write_all(&frame).unwrap();
        }
        fn receive(client: &mut TcpStream) -> serde_json::Value {
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
        // Pipeline messages without waiting for each acknowledgement.
        send(&mut client, serde_json::json!({"sequence":1,"text":"alpha","enter":true}));
        send(&mut client, serde_json::json!({"sequence":2,"text":"beta","enter":true}));
        send(&mut client, serde_json::json!({"sequence":3,"text":"x".repeat(4097)}));
        for sequence in 1..=3 {
            let ack = receive(&mut client);
            assert_eq!(ack["sequence"], sequence);
            assert_eq!(ack["ok"], sequence < 3);
        }
        let screen = capture_pane_text(&_session.0).unwrap();
        assert!(screen.find("alpha").unwrap() < screen.find("beta").unwrap());
        let mut control = crate::tmux_control::Control::open(&_session.0).unwrap();
        assert_eq!(control.capture().unwrap(), crate::tmux::capture_pane_ansi(&_session.0).unwrap());
        control.send("literal ' ; $() \\ Ukrainian: привіт", true).unwrap();
        assert!(control.capture().unwrap().contains("literal ' ; $() \\ Ukrainian: привіт"));
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

    #[test]
    fn test_port_taken_follows_the_listener() {
        // Occupy an ephemeral port, then release it: `start_bridge` relies on
        // this to tell "free" from "someone (maybe a wedged bridge) is there".
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
        let port = listener.local_addr().unwrap().port();
        assert!(port_taken(port), "listening port must read as taken");
        drop(listener);
        assert!(!port_taken(port), "released port must read as free");
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
        assert_eq!(tag_color(crate::tag::DEFAULT_TERMINAL_TAG), Some("#22d3ee"));
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
    fn codex_pane_titles_provide_the_submitted_prompt() {
        assert_eq!(
            codex_prompt_from_title("⠧ Remove the bottom-right resize icon | super-desktop"),
            Some("Remove the bottom-right resize icon".into()),
        );
        assert_eq!(
            codex_prompt_from_title("[ ! ] Action Required | Add directory selector | super-desktop"),
            Some("Add directory selector".into()),
        );
        assert_eq!(codex_prompt_from_title("OpenAI Codex"), None);
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
