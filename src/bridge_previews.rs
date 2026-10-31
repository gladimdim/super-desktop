//! Paired-device preview sessions, restricted to one loopback service.
use super::*;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tungstenite::{
    protocol::{Role, WebSocketConfig},
    Message, WebSocket,
};

const LIFETIME: Duration = Duration::from_secs(1800);
const MAX_SESSIONS: usize = 16;
const DEVICE_SESSIONS: usize = 4;
const MAX_TUNNELS: usize = 24;
const DEVICE_TUNNELS: usize = 6;
const CHUNK: usize = 16384;

struct Session {
    owner: String,
    address: SocketAddr,
    until: Instant,
    active: Arc<AtomicBool>,
}
#[derive(Default)]
struct Registry {
    sessions: HashMap<String, Session>,
    tunnels: HashMap<String, usize>,
}
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(Default::default)
}

/// Never resolve a caller-supplied hostname or connect outside loopback.
fn target(input: &str) -> Result<(SocketAddr, String), &'static str> {
    if input.len() > 8192 {
        return Err("invalid_preview_url");
    }
    let url = reqwest::Url::parse(input).map_err(|_| "invalid_preview_url")?;
    if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
        return Err("preview_requires_local_http");
    }
    let ip = match url.host_str() {
        Some("localhost" | "127.0.0.1" | "0.0.0.0") => IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some("[::1]" | "[::]") => IpAddr::V6(Ipv6Addr::LOCALHOST),
        _ => return Err("preview_requires_loopback"),
    };
    let port = url.port_or_known_default().ok_or("invalid_preview_port")?;
    if port < 1024 || port == BRIDGE_PORT {
        return Err("preview_port_not_allowed");
    }
    let mut path = url.path().to_string();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    if let Some(fragment) = url.fragment() {
        path.push('#');
        path.push_str(fragment);
    }
    Ok((SocketAddr::new(ip, port), path))
}

impl Registry {
    fn prune(&mut self) {
        self.sessions.retain(|_, s| {
            let keep = s.until > Instant::now() && s.active.load(Ordering::Relaxed);
            if !keep {
                s.active.store(false, Ordering::Relaxed);
            }
            keep
        });
    }
}
struct Permit(String);
impl Drop for Permit {
    fn drop(&mut self) {
        let mut r = registry().lock().unwrap();
        if let Some(count) = r.tunnels.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                r.tunnels.remove(&self.0);
            }
        }
    }
}

pub(super) fn handle(stream: &mut Connection, req: &Request) {
    let owner = security::digest(bearer(&req.headers).as_bytes());
    if req.path == "/api/v1/previews" && req.method == "POST" {
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
        let (address, path) = match target(body["url"].as_str().unwrap_or("")) {
            Ok(v) => v,
            Err(e) => return reply_error(stream, 400, Envelope::Plain, e),
        };
        let mut r = registry().lock().unwrap();
        r.prune();
        if r.sessions.len() >= MAX_SESSIONS
            || r.sessions.values().filter(|s| s.owner == owner).count() >= DEVICE_SESSIONS
        {
            return reply_error(stream, 429, Envelope::Plain, "preview_sessions_busy");
        }
        let id = random_hex(24);
        r.sessions.insert(
            id.clone(),
            Session {
                owner,
                address,
                until: Instant::now() + LIFETIME,
                active: Arc::new(AtomicBool::new(true)),
            },
        );
        return reply_json(
            stream,
            201,
            &serde_json::json!({"id":id,"path":path,"expiresInSeconds":LIFETIME.as_secs()}),
        );
    }
    let rest = req.path.strip_prefix("/api/v1/previews/").unwrap_or("");
    let (id, tunnel) = rest
        .strip_suffix("/tunnel")
        .map(|id| (id, true))
        .unwrap_or((rest, false));
    if id.is_empty() || id.contains('/') {
        return reply_error(stream, 404, Envelope::Plain, "preview_not_found");
    }
    let (address, active, until, _permit) = {
        let mut r = registry().lock().unwrap();
        r.prune();
        let Some(s) = r.sessions.get(id).filter(|s| s.owner == owner) else {
            return reply_error(stream, 404, Envelope::Plain, "preview_not_found");
        };
        if !tunnel && req.method == "DELETE" {
            s.active.store(false, Ordering::Relaxed);
            r.sessions.remove(id);
            return reply_json(stream, 200, &serde_json::json!({"status":"closed"}));
        }
        if !tunnel || req.method != "GET" {
            return reply_error(stream, 404, Envelope::Plain, "unknown_preview_route");
        }
        if !req
            .headers
            .get("upgrade")
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
            || !req.headers.contains_key("sec-websocket-key")
        {
            return reply_error(stream, 400, Envelope::Plain, "expected_websocket");
        }
        if r.tunnels.values().sum::<usize>() >= MAX_TUNNELS
            || r.tunnels.get(&owner).copied().unwrap_or(0) >= DEVICE_TUNNELS
        {
            return reply_error(stream, 429, Envelope::Plain, "preview_tunnels_busy");
        }
        let values = (s.address, s.active.clone(), s.until);
        *r.tunnels.entry(owner.clone()).or_default() += 1;
        (values.0, values.1, values.2, Permit(owner))
    };
    let upstream = match TcpStream::connect_timeout(&address, Duration::from_secs(2)) {
        Ok(s) => s,
        Err(_) => return reply_error(stream, 502, Envelope::Plain, "preview_server_unavailable"),
    };
    if !ws_upgrade(stream, req) {
        return;
    }
    relay(stream, upstream, active, until);
}

fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

fn relay(
    stream: &mut Connection,
    mut upstream: TcpStream,
    active: Arc<AtomicBool>,
    until: Instant,
) {
    // Keep partially received WebSocket frames across timeouts. The incremental
    // decoder also handles client fragmentation without changing older streams.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = upstream.set_read_timeout(Some(Duration::from_millis(1)));
    let _ = upstream.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = upstream.set_nodelay(true);
    let config = WebSocketConfig::default()
        .read_buffer_size(CHUNK)
        .write_buffer_size(0)
        .max_write_buffer_size(CHUNK * 2)
        .max_message_size(Some(CHUNK))
        .max_frame_size(Some(CHUNK));
    let mut ws = WebSocket::from_raw_socket(stream, Role::Server, Some(config));
    let mut buf = [0u8; CHUNK];
    while active.load(Ordering::Relaxed)
        && Instant::now() < until
        && ws.get_ref().still_authorized()
    {
        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                if upstream.write_all(&bytes).is_err() {
                    break;
                }
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {
                if ws.flush().is_err() {
                    break;
                }
            }
            Err(tungstenite::Error::Io(e)) if retry(&e) => {}
            _ => break,
        }
        // Drain bounded chunks in both directions to avoid starving input on a
        // streaming response. Socket timeouts bound memory and backpressure.
        for _ in 0..16 {
            match upstream.read(&mut buf) {
                Ok(0) => {
                    let _ = ws.close(None);
                    return;
                }
                Ok(n) => {
                    if ws.send(Message::Binary(buf[..n].to_vec().into())).is_err() {
                        return;
                    }
                }
                Err(e) if retry(&e) => break,
                Err(_) => return,
            }
        }
    }
    let _ = ws.close(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_targets_stay_on_loopback_and_retain_paths() {
        for host in ["localhost", "127.0.0.1", "0.0.0.0", "[::1]"] {
            let (address, path) = target(&format!("http://{host}:3000/a%20b?q=x#art")).unwrap();
            assert!(address.ip().is_loopback());
            assert_eq!(address.port(), 3000);
            assert_eq!(path, "/a%20b?q=x#art");
        }
        for input in [
            "http://192.168.1.1:3000",
            "http://example.com:3000",
            "http://localhost.evil:3000",
            "http://user:pass@localhost:3000",
            "https://localhost:3000",
            "file:///etc/passwd",
            "http://localhost:8759",
            "http://localhost:22",
            "http://localhost:0",
            "http://localhost",
        ] {
            assert!(target(input).is_err(), "{input}");
        }
    }

    #[test]
    fn preview_expiry_invalidates_active_tunnels_before_registry_removal() {
        let active = Arc::new(AtomicBool::new(true));
        let mut registry = Registry::default();
        registry.sessions.insert(
            "expired".into(),
            Session {
                owner: "owner".into(),
                address: "127.0.0.1:3000".parse().unwrap(),
                until: Instant::now() - Duration::from_secs(1),
                active: active.clone(),
            },
        );
        registry.prune();
        assert!(!active.load(Ordering::Relaxed));
        assert!(registry.sessions.is_empty());
    }
}

#[cfg(test)]
#[path = "bridge_preview_fixture.rs"]
mod android_fixture;
