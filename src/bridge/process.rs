use super::*;

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

pub(in crate::bridge) fn tailscale_ip_uncached() -> Option<String> {
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
pub(in crate::bridge) const HEALTH_QUERY: &str = "health=1";

/// Answer to the daemon's health probe: what `bridge_running` checks, without
/// the phone-facing ping's addresses, LAN IP or hostname (no subprocess).
pub(in crate::bridge) fn health_body() -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "service": SERVICE_NAME,
        "protocolVersion": PROTOCOL_VERSION,
        "port": BRIDGE_PORT,
    })
}

/// True when this machine's bridge answers on its owner-only control socket.
/// The socket is per user, not per port.
pub fn bridge_running() -> bool {
    pairing::control_exchange(&format!("GET /api/v1/ping?{HEALTH_QUERY} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"))
        .ok()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .is_some_and(|body| body["status"] == "ok" && body["service"] == SERVICE_NAME)
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

pub(in crate::bridge) fn start_bridge_inner() -> Result<(), String> {
    if bridge_running() {
        return Ok(());
    }
    if port_taken(BRIDGE_PORT) {
        // Something is on our port: if it is a wedged bridge of ours, drop it
        // and take the port over; otherwise say who holds it.
        let _ = stop_bridge_inner();
        if bridge_running() {
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
    if let Some(parent) = log_path.parent() {
        let _ = crate::control::ensure_private_dir(parent);
    }
    // Keep the last run's diagnostics when recovering an unexpected exit.
    let prev_path = log_path.with_file_name("bridge.previous.log");
    let _ = fs::remove_file(&prev_path);
    if fs::symlink_metadata(&log_path).is_ok_and(|m| m.file_type().is_symlink()) {
        let _ = fs::remove_file(&log_path);
    } else {
        let _ = fs::rename(&log_path, &prev_path);
    }
    let log = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&log_path)
            .ok()
    };
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
        if bridge_running() {
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
pub(in crate::bridge) fn bridge_exe() -> Result<PathBuf, String> {
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
pub(in crate::bridge) fn port_taken(port: u16) -> bool {
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
pub(in crate::bridge) fn bridge_log_path() -> PathBuf {
    let mut path = state_path();
    path.set_file_name("bridge.log");
    path
}

/// Last non-empty line of the bridge log: its banner, or the bind error that
/// killed the process.
pub(in crate::bridge) fn bridge_log_tail() -> Option<String> {
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

/// The bridge process exactly (`pkill -f`, extended regex): its own binary as
/// the first word, then the subcommand. A looser pattern also matched any
/// process whose command text merely mentioned "super-desktop harness-bridge",
/// such as an agent's shell command in a card, and killed it.
pub(in crate::bridge) const BRIDGE_PROCESS: &str = "^([^ ]*/)?super-desktop harness-bridge( |$)";

pub(in crate::bridge) fn stop_bridge_inner() -> Result<(), String> {
    // pkill exits 1 when nothing matched — that means already stopped.
    if pkill_bridge(&["-f", BRIDGE_PROCESS])? == 1 {
        return Ok(());
    }
    if wait_port_free() {
        return Ok(());
    }
    let _ = pkill_bridge(&["-9", "-f", BRIDGE_PROCESS]);
    if wait_port_free() {
        return Ok(());
    }
    Err(format!(
        "port {BRIDGE_PORT} is still held after SIGKILL — check `ss -ltnp | grep {BRIDGE_PORT}`"
    ))
}

pub(in crate::bridge) fn pkill_bridge(args: &[&str]) -> Result<i32, String> {
    let uid = unsafe { libc::geteuid() }.to_string();
    let st = Command::new("pkill")
        .args(["-u", &uid])
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
pub(in crate::bridge) fn wait_port_free() -> bool {
    for _ in 0..20 {
        if !port_taken(BRIDGE_PORT) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

// ---------- firewall (UFW) ----------

pub(in crate::bridge) fn firewall_marker_path() -> PathBuf {
    crate::state::home_dir().join(".local/state/omarchy/harness-bridge/firewall_open")
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

