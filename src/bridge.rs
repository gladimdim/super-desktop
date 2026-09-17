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
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::state::load_state;
use crate::tmux::{
    capture_pane_text, extract_last_prompt, get_agent_config, get_composer_draft,
    get_opencode_session_id, get_opencode_user_text_by_id, inspect_status, truncate_prompt_title,
};

pub const BRIDGE_PORT: u16 = 8759;
const SERVICE_NAME: &str = "Omarchy Harness Bridge";
const PROTOCOL_VERSION: u32 = 1;
const PAIR_WINDOW_SECS: f64 = 120.0;

fn utc_now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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

fn last_user_text(session: &str, agent_type: &str) -> Option<String> {
    // Priority mirrors mini_terminal.rs refresh_status(): composer draft,
    // then exact opencode DB text, then pane-scrape heuristic.
    if let Some(draft) = get_composer_draft(session) {
        return Some(draft);
    }
    if agent_type == "opencode" {
        if let Some(id) = get_opencode_session_id(session) {
            if let Some(text) = get_opencode_user_text_by_id(&id) {
                return Some(text);
            }
        }
    }
    capture_pane_text(session)
        .as_deref()
        .and_then(extract_last_prompt)
        .map(|s| truncate_prompt_title(&s))
}

pub fn collect_harnesses() -> Vec<serde_json::Value> {
    let state = load_state();
    let meta: HashMap<String, (String, String)> = state
        .terminals
        .iter()
        .map(|t| {
            (
                t.session_name.clone(),
                (t.agent_type.clone(), t.command.clone()),
            )
        })
        .collect();
    let mut out = vec![];
    for session in live_sessions() {
        let (agent_type, cmd_fallback) = meta
            .get(&session)
            .cloned()
            .unwrap_or_else(|| ("shell".to_string(), String::new()));
        let cfg = get_agent_config(&agent_type);
        let status = inspect_status(&session, &agent_type);
        let screen = capture_pane_text(&session).unwrap_or_default();
        let lines: Vec<&str> = screen
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        let start = lines.len().saturating_sub(12);
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
            "lastPrompt": last_user_text(&session, &agent_type),
            "composerDraft": get_composer_draft(&session),
            "preview": lines[start..].join("\n"),
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

    match (req.method.as_str(), req.path.as_str()) {
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
                }),
            );
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
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                std::thread::spawn(move || handle_client(s));
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

/// One-shot JSON dump for `super-desktop harnesses`.
pub fn print_once() {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "harnesses": collect_harnesses(),
        }))
        .unwrap_or_default()
    );
}

// ---------- overlay-facing helpers (used by the ⚙ Launcher settings panel) ----------

/// First IPv4 from `tailscale ip -4`, if Tailscale is up.
pub fn tailscale_ip() -> Option<String> {
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

/// Start `super-desktop harness-bridge` detached. No-op when already running.
pub fn start_bridge() -> Result<(), String> {
    if bridge_running(BRIDGE_PORT) {
        return Err("already running".to_string());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Command::new(exe)
        .arg("harness-bridge")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    for _ in 0..25 {
        if bridge_running(BRIDGE_PORT) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("started but not responding".to_string())
}

/// Stop a locally running bridge.
pub fn stop_bridge() -> Result<(), String> {
    let st = Command::new("pkill")
        .args(["-f", "super-desktop harness-bridge"])
        .status()
        .map_err(|e| e.to_string())?;
    // pkill exits 1 when nothing matched — that means already stopped.
    if st.code() == Some(1) {
        return Ok(());
    }
    if !st.success() {
        return Err(format!("pkill exited {}", st.code().unwrap_or(-1)));
    }
    for _ in 0..20 {
        if !bridge_running(BRIDGE_PORT) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("still responding".to_string())
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
