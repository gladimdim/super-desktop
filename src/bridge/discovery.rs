use super::*;

/// `(fetched_at, value)` for `tailscale_ip`; see the TTL there.
pub(in crate::bridge) static TAILSCALE_CACHE: Mutex<Option<(f64, Option<String>)>> = Mutex::new(None);
pub(in crate::bridge) static ADDRESSES_CACHE: Mutex<Option<(f64, Vec<serde_json::Value>)>> = Mutex::new(None);

pub(in crate::bridge) fn interface_addresses(document: &serde_json::Value) -> Vec<serde_json::Value> {
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

pub(in crate::bridge) fn bridge_addresses() -> Vec<serde_json::Value> {
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

pub(in crate::bridge) fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
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


/// mDNS service type the launcher browses for.
pub const MDNS_SERVICE_TYPE: &str = "_omarchy-harness._tcp";

/// Protocol version advertised in the TXT record (`ver=<PROTOCOL_VERSION>`).
pub(in crate::bridge) const MDNS_PROTOCOL_TXT_VERSION: u32 = PROTOCOL_VERSION;

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
pub(in crate::bridge) fn mdns_advertised(port: u16) -> bool {
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
pub(in crate::bridge) const BROWSE_TIMEOUT: Duration = Duration::from_secs(3);

/// Single `avahi-browse -rtp` sweep for our own record. Stops at the first
/// line that shows it, so a sweep stuck on another record still counts ours.
pub(in crate::bridge) fn browse_once(port: u16) -> bool {
    let mut command = Command::new("avahi-browse");
    command.args(["-rtp", MDNS_SERVICE_TYPE]);
    let own_host = hostname();
    any_output_line(command, BROWSE_TIMEOUT, |line| {
        browse_line_matches_host(line, port, Some(&own_host))
    })
}

/// Run `command` until a line of its stdout satisfies `found` (true), or it
/// exits or `timeout` passes (false). The child is killed and reaped on every
/// path; lines printed before a hang are still examined.
pub(in crate::bridge) fn any_output_line(mut command: Command, timeout: Duration, mut found: impl FnMut(&str) -> bool) -> bool {
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
#[cfg(test)]
pub(in crate::bridge) fn browse_line_matches(line: &str, port: u16) -> bool {
    browse_line_matches_host(line, port, None)
}

fn browse_line_matches_host(line: &str, port: u16, expected_host: Option<&str>) -> bool {
    if !line.starts_with('=') {
        return false;
    }
    let fields: Vec<&str> = line.split(';').collect();
    // 4 = service type, 8 = port (6 = host, 7 = address, 9 = TXT).
    if fields.get(4) != Some(&MDNS_SERVICE_TYPE)
        || fields.get(8).and_then(|p| p.parse::<u16>().ok()) != Some(port)
    {
        return false;
    }
    if let Some(own) = expected_host.map(str::trim).filter(|s| !s.is_empty()) {
        let own_short = own.split('.').next().unwrap_or(own);
        let Some(host) = fields.get(6).map(|s| s.trim().trim_end_matches('.')) else {
            return false;
        };
        let host_short = host.split('.').next().unwrap_or(host);
        if !host.eq_ignore_ascii_case(own)
            && !host_short.eq_ignore_ascii_case(own_short)
        {
            return false;
        }
    }
    true
}

/// State file the 📱 panel reads to show whether discovery is live.
pub(in crate::bridge) fn mdns_state_path() -> PathBuf {
    state_path().with_file_name("mdns.json")
}

/// `host:port` from the mDNS state file, when the bridge advertised itself.
pub(in crate::bridge) fn mdns_advertised_port() -> Option<u16> {
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
pub(in crate::bridge) fn log_file_append() -> Option<fs::File> {
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

