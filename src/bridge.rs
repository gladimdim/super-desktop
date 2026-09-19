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
//!   POST /api/v1/pair                 (open; PIN or pairing window)
//!   POST /api/v1/pair/open            (loopback only, opens 120s window)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::state::load_state;
use crate::tmux::{
    capture_pane_ansi, capture_pane_text, extract_last_prompt, get_agent_config,
    get_composer_draft, get_opencode_user_text_by_id, inspect_status,
    inspect_status_with_screen, resolve_own_opencode_id, resolve_workspace_dir,
    strip_terminal_escapes, truncate_prompt_title, SessionStatus,
};

pub const BRIDGE_PORT: u16 = 8759;
const SERVICE_NAME: &str = "Omarchy Harness Bridge";
const PROTOCOL_VERSION: u32 = 1;
const PAIR_WINDOW_SECS: f64 = 120.0;
type LauncherSessionMeta = (String, String, Option<String>, u8, Option<String>);

fn utc_now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// `(fetched_at, value)` for `tailscale_ip`; see the TTL there.
static TAILSCALE_CACHE: Mutex<Option<(f64, Option<String>)>> = Mutex::new(None);

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
    // Priority mirrors mini_terminal.rs refresh_status(): composer draft,
    // then exact opencode DB text, then pane-scrape heuristic. The DB id is
    // resolved to the session OWNED by this pane (own `--session` flag, else
    // a claims-aware match), so a closed console's prompt never leaks here.
    if let Some(draft) = get_composer_draft(session) {
        return Some(draft);
    }
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

// ---------- pairing state ----------

#[derive(Debug, Serialize, Deserialize)]
struct BridgeConfig {
    #[serde(default)]
    paired_tokens: Vec<String>,
    #[serde(default)]
    pin: String,
    #[serde(default)]
    pairing_open_until: f64,
}

fn state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = PathBuf::from(home).join(".local/state/omarchy/harness-bridge");
    let _ = fs::create_dir_all(&dir);
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
    if ok {
        buf
    } else {
        // Bare-metal fallback: time + pid mix (never blocks).
        let t = now_epoch().to_bits().to_le_bytes();
        let p = std::process::id().to_le_bytes();
        (0..n)
            .map(|i| t[i % 8] ^ p[i % 4] ^ (i as u8).wrapping_mul(31))
            .collect()
    }
}

fn random_hex(bytes: usize) -> String {
    urandom_bytes(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn new_pin() -> String {
    let data = urandom_bytes(2);
    let n = ((data[0] as u32) << 8 | (data[1] as u32)) % 9000 + 1000;
    format!("{n:04}")
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
        let mut cfg: BridgeConfig = fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_else(|| BridgeConfig {
                paired_tokens: vec![],
                pin: new_pin(),
                pairing_open_until: 0.0,
            });
        if cfg.pin.len() != 4 {
            cfg.pin = new_pin();
        }
        let state = Self {
            cfg,
            mtime: 0.0,
        };
        state.save();
        let mtime = config_mtime();
        Self {
            cfg: state.cfg,
            mtime,
        }
    }

    fn save(&self) {
        let path = state_path();
        if let Ok(json) = serde_json::to_string_pretty(&self.cfg) {
            let _ = fs::write(&path, json);
        }
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
            let mut tokens = disk.paired_tokens;
            for t in &self.cfg.paired_tokens {
                if !tokens.contains(t) {
                    tokens.push(t.clone());
                }
            }
            let pin = if disk.pin.len() == 4 {
                disk.pin
            } else {
                self.cfg.pin.clone()
            };
            self.cfg = BridgeConfig {
                paired_tokens: tokens,
                pin,
                pairing_open_until: disk.pairing_open_until,
            };
        }
        self.mtime = config_mtime();
    }

    fn valid(&mut self, token: &str) -> bool {
        self.refresh();
        !token.is_empty() && self.cfg.paired_tokens.iter().any(|t| t == token)
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
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    let mut buf = vec![0u8; 65536];
    let mut total = 0usize;
    let header_end = loop {
        let n = stream.read(&mut buf[total..]).ok()?;
        if n == 0 {
            if total == 0 {
                return None;
            }
            break total;
        }
        total += n;
        if let Some(pos) = find_subslice(&buf[..total], b"\r\n\r\n") {
            break pos + 4;
        }
        if total >= buf.len() {
            break total;
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
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    let content_len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..total].to_vec();
    while body.len() < content_len {
        let mut chunk = vec![0u8; 8192];
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_len.min(1 << 20));
    Some(Request {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

fn respond(stream: &mut TcpStream, code: u16, reason: &str, value: &serde_json::Value) {
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
    if h.len() > 7 && h[..7].eq_ignore_ascii_case("bearer ") {
        h[7..].trim().to_string()
    } else {
        String::new()
    }
}

/// Bearer-token or loopback authorization (same rule as `/api/v1/harnesses`).
fn authorize(req: &Request, local: bool) -> bool {
    local
        || pair_state()
            .lock()
            .map(|mut s| s.valid(&bearer(&req.headers)))
            .unwrap_or(false)
}

/// Complete a WebSocket upgrade; `false` when this is not a WS request (the
/// caller then answers with plain HTTP).
fn ws_upgrade(stream: &mut TcpStream, req: &Request) -> bool {
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
fn ws_client_alive(stream: &mut TcpStream) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1)));
    match crate::ws::read_frame(stream) {
        Ok(None) | Ok(Some(crate::ws::Frame::Close)) => false,
        Ok(Some(crate::ws::Frame::Ping(payload))) => crate::ws::write_pong(stream, &payload).is_ok(),
        Ok(Some(_)) => true,
        Err(e) => matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
    }
}

/// Push the full `/harnesses` document once a second.
///
/// Liveness comes from writes: a client that vanished makes the write (or the
/// 5s write timeout) fail, which ends the thread — no reader thread needed.
fn stream_harness_list(mut stream: TcpStream) {
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
        });
        if crate::ws::write_text(&mut stream, &document.to_string()).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
    let _ = crate::ws::write_close(&mut stream, 1000, "reconnect");
}

/// Push one harness's live pane output (~2 frames/s) until the session dies.
///
/// The first frame doubles as the "attached" signal; the phone renders `tail`
/// in its terminal screen and stops when `status` turns `EXITED`.
fn stream_one_harness(mut stream: TcpStream, id: &str) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let deadline = std::time::Instant::now() + Duration::from_secs(STREAM_MAX_SECS);
    let (agent_type, tag) = session_meta(id);
    let mut previous: Option<String> = None;
    while std::time::Instant::now() < deadline {
        if !ws_client_alive(&mut stream) {
            return;
        }
        let alive = crate::tmux::session_alive(id);
        // One styled tmux capture drives both representations. This is cheaper
        // than capturing once for status/plain text and again for colour.
        let ansi_tail = alive.then(|| capture_pane_ansi(id)).flatten();
        let plain_tail = ansi_tail.as_deref().map(strip_terminal_escapes);
        let status = if let Some(screen) = plain_tail.as_deref() {
            inspect_status_with_screen(id, &agent_type, screen)
        } else if alive {
            inspect_status(id, &agent_type)
        } else {
            SessionStatus {
                status: "EXITED",
                label: "○ EXITED",
                pid: String::new(),
                cmd: String::new(),
                cwd: String::new(),
            }
        };
        let frame = serde_json::json!({
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
            "updatedAt": utc_now_iso(),
        })
        .to_string();
        if previous.as_deref() != Some(frame.as_str()) {
            if crate::ws::write_text(&mut stream, &frame).is_err() {
                return;
            }
            previous = Some(frame);
        }
        if !alive {
            let _ = crate::ws::write_close(&mut stream, 1000, "session ended");
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = crate::ws::write_close(&mut stream, 1000, "reconnect");
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
fn handle_keys(stream: &mut TcpStream, req: &Request, id: &str, local: bool) {
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
    match crate::tmux::send_keys(id, text, enter) {
        Ok(()) => respond(stream, 200, "OK", &serde_json::json!({"status": "ok"})),
        Err(e) => respond(
            stream,
            500,
            "Internal Server Error",
            &serde_json::json!({"status": "error", "error": e}),
        ),
    }
}

fn is_loopback(peer: &str) -> bool {
    peer.starts_with("127.0.0.1") || peer.starts_with("[::1]") || peer.starts_with("::1")
}

fn handle_client(mut stream: TcpStream) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let req = match read_request(&mut stream) {
        Some(r) => r,
        None => return,
    };
    let local = is_loopback(&peer);
    let path = req.path.split('?').next().unwrap_or("").to_string();

    // Dynamic harness routes: /api/v1/harnesses/<id>/stream (WebSocket, live
    // pane output) and /api/v1/harnesses/<id>/keys (phone → harness input).
    // Only our own `sd_term_*` sessions are addressable, so a paired phone
    // cannot type into unrelated tmux sessions.
    if let Some(rest) = path.strip_prefix("/api/v1/harnesses/") {
        if let Some((id, action)) = rest.rsplit_once('/') {
            if id.starts_with("sd_term_") {
                match (req.method.as_str(), action) {
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
        ("GET", "/api/v1/ping") => respond(
            &mut stream,
            200,
            "OK",
            &serde_json::json!({
                "status": "ok",
                "service": SERVICE_NAME,
                "protocolVersion": PROTOCOL_VERSION,
                "hostname": hostname(),
                "lanIp": lan_ip(),
                "port": BRIDGE_PORT,
                "time": utc_now_iso(),
            }),
        ),
        ("GET", "/api/v1/harnesses") => {
            let ok = local
                || pair_state()
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
                }),
            );
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
        ("POST", "/api/v1/pair/open") => {
            if !local {
                return respond(
                    &mut stream,
                    401,
                    "Unauthorized",
                    &serde_json::json!({"status": "error", "error": "not_paired"}),
                );
            }
            if let Ok(mut s) = pair_state().lock() {
                s.refresh();
                s.cfg.pairing_open_until = now_epoch() + PAIR_WINDOW_SECS;
                s.save();
            }
            respond(
                &mut stream,
                200,
                "OK",
                &serde_json::json!({"status": "open", "secondsRemaining": 120}),
            );
        }
        ("POST", "/api/v1/pair") => {
            let body: serde_json::Value =
                serde_json::from_str(&req.body).unwrap_or(serde_json::json!({}));
            let pin = body
                .get("pin")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let mut out: Option<(String, String)> = None;
            if let Ok(mut s) = pair_state().lock() {
                s.refresh();
                if !pin.is_empty() && pin == s.cfg.pin {
                    let tok = random_hex(24);
                    s.cfg.paired_tokens.push(tok.clone());
                    s.save();
                    out = Some((tok, "pin".into()));
                } else if now_epoch() < s.cfg.pairing_open_until {
                    let tok = random_hex(24);
                    s.cfg.paired_tokens.push(tok.clone());
                    s.cfg.pairing_open_until = 0.0;
                    s.save();
                    out = Some((tok, "window".into()));
                }
            }
            match out {
                Some((tok, method)) => respond(
                    &mut stream,
                    200,
                    "OK",
                    &serde_json::json!({
                        "status": "paired",
                        "token": tok,
                        "hostname": hostname(),
                        "serverIp": lan_ip(),
                        "method": method,
                    }),
                ),
                None => respond(
                    &mut stream,
                    401,
                    "Unauthorized",
                    &serde_json::json!({"status": "error", "error": "invalid_pin"}),
                ),
            }
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
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{SERVICE_NAME}: failed to bind 0.0.0.0:{port}: {e}");
            std::process::exit(1);
        }
    };
    println!("{SERVICE_NAME} on 0.0.0.0:{port} (lan {})", lan_ip());
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
                std::thread::spawn(move || handle_client(s));
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

/// mDNS service type the launcher browses for (OmarchyAILauncher/PROTOCOL.md).
pub const MDNS_SERVICE_TYPE: &str = "_omarchy-harness._tcp";

/// Protocol version advertised in the TXT record (`ver=1`).
const MDNS_PROTOCOL_TXT_VERSION: u32 = 1;

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
    // Names are const/hostname, but they can contain spaces, so quote them.
    let script = format!(
        "avahi-publish-service -s '{name}' {MDNS_SERVICE_TYPE} {port} \
         ver={MDNS_PROTOCOL_TXT_VERSION} '{txt_host}' & publisher=$!; \
         cat >/dev/null; kill $publisher 2>/dev/null; wait $publisher 2>/dev/null"
    );

    let mut command = Command::new("sh");
    command.arg("-c").arg(&script).stdin(Stdio::piped());
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

fn bridge_ping_body(port: u16) -> Option<String> {
    let mut s = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().ok()?,
        Duration::from_secs(2),
    )
    .ok()?;
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

/// Current pairing PIN (fresh from disk, so external edits are visible).
pub fn read_pin() -> String {
    let cfg: Option<BridgeConfig> = fs::read_to_string(state_path())
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok());
    match cfg.map(|c| c.pin) {
        Some(p) if p.len() == 4 => p,
        _ => pair_state()
            .lock()
            .map(|mut s| {
                s.refresh();
                s.cfg.pin.clone()
            })
            .unwrap_or_else(|_| "----".to_string()),
    }
}

/// Generate and persist a fresh PIN (takes effect immediately, even if the
/// bridge keeps running, via PairState::refresh).
pub fn rotate_pin() -> String {
    let pin = new_pin();
    if let Ok(mut s) = pair_state().lock() {
        s.refresh();
        s.cfg.pin = pin.clone();
        s.save();
        s.mtime = config_mtime();
    } else {
        // Bridge never ran in this process: write through the file directly.
        let mut cfg: BridgeConfig = fs::read_to_string(state_path())
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_else(|| BridgeConfig {
                paired_tokens: vec![],
                pin: pin.clone(),
                pairing_open_until: 0.0,
            });
        cfg.pin = pin.clone();
        let _ = fs::write(
            state_path(),
            serde_json::to_string_pretty(&cfg).unwrap_or_default(),
        );
    }
    pin
}

/// Seconds left on the laptop pairing window (0 = closed).
pub fn pairing_seconds_left() -> u64 {
    let cfg: Option<BridgeConfig> = fs::read_to_string(state_path())
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok());
    let until = cfg.map(|c| c.pairing_open_until).unwrap_or(0.0);
    let left = until - now_epoch();
    if left > 0.0 {
        left as u64
    } else {
        0
    }
}

/// Open the 120s pairing window via loopback. Returns seconds remaining.
pub fn open_pairing_window() -> Result<u64, String> {
    let mut s = TcpStream::connect(("127.0.0.1", BRIDGE_PORT))
        .map_err(|_| "bridge not running — start it first".to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let body = "{}";
    let req = format!(
        "POST /api/v1/pair/open HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    s.write_all(req.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut resp = String::new();
    s.read_to_string(&mut resp)
        .map_err(|e| e.to_string())?;
    let body = resp.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "bad bridge response".to_string())?;
    if v.get("status").and_then(|x| x.as_str()) == Some("open") {
        Ok(v
            .get("secondsRemaining")
            .and_then(|x| x.as_u64())
            .unwrap_or(120))
    } else {
        Err("bridge refused".to_string())
    }
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
