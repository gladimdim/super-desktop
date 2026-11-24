use super::*;

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
    let _session = TmuxSession::start(&id, &[], &["cat"]);
    let server_id = id.clone();
    let (mut client, worker) = loopback_server(move |mut stream| stream_keys(&mut stream, &server_id));
    // Pipeline messages without waiting for each acknowledgement.
    ws_send(&mut client, serde_json::json!({"sequence":1,"text":"alpha","enter":true}));
    ws_send(&mut client, serde_json::json!({"sequence":2,"text":"beta","enter":true}));
    ws_send(&mut client, serde_json::json!({"sequence":3,"text":"x".repeat(4097)}));
    for sequence in 1..=3 {
        let ack = ws_receive(&mut client);
        assert_eq!(ack["sequence"], sequence);
        assert_eq!(ack["ok"], sequence < 3);
    }
    assert_eq!(last_user_text(&id, "codex", None).as_deref(), Some("beta"));
    // Draft input and rejected submissions must not replace the last prompt.
    ws_send(&mut client, serde_json::json!({"sequence":4,"text":"draft","enter":false}));
    assert_eq!(ws_receive(&mut client)["ok"], true);
    ws_send(&mut client, serde_json::json!({"sequence":5,"text":"x".repeat(4097),"enter":true}));
    assert_eq!(ws_receive(&mut client)["ok"], false);
    assert_eq!(last_user_text(&id, "codex", None).as_deref(), Some("beta"));
    ws_send(&mut client, serde_json::json!({"sequence":6,"text":"\nUnicode привіт ✓","enter":true}));
    assert_eq!(ws_receive(&mut client)["ok"], true);
    assert_eq!(last_user_text(&id, "codex", None).as_deref(), Some("Unicode привіт ✓"));
    let screen = capture_pane_text(&id).unwrap();
    assert!(screen.find("alpha").unwrap() < screen.find("beta").unwrap());
    let mut control = crate::tmux_control::Control::open(&id).unwrap();
    assert_eq!(control.capture().unwrap(), crate::tmux::capture_pane_ansi(&id).unwrap());
    control.send("literal ' ; $() \\ Ukrainian: привіт", true).unwrap();
    let expected = "literal ' ; $() \\ Ukrainian: привіт";
    wait_until("tmux to render the acknowledged input", || control.capture().unwrap().contains(expected));
    drop(client);
    worker.join().unwrap();
}

#[test]
fn persistent_input_submits_after_a_harness_reads_the_text_as_a_paste() {
    // Like Claude Code, this harness treats keys that arrive within 100 ms
    // of each other as one paste; a Return inside it is a newline in the
    // draft. Return sent straight after the text used to land there, so
    // the prompt sat in the composer unsent.
    let id = format!("sd_paste_test_{}", std::process::id());
    let script = std::env::temp_dir().join(format!("{id}.py"));
    std::fs::write(&script, r#"
import os, select, tty
tty.setraw(0)
draft = b""
while True:
    chunk = os.read(0, 4096)
    while select.select([0], [], [], 0.1)[0]:
        chunk += os.read(0, 4096)
    if chunk == b"\r":
        os.write(1, b"\r\nSUBMIT:" + draft + b"\r\n")
        draft = b""
    else:
        draft += chunk.replace(b"\r", b"|")
        os.write(1, chunk.replace(b"\r", b"|"))
"#).unwrap();
    let _session = TmuxSession::start(&id, &[], &["python3", &script.to_string_lossy()]).removing(script);
    // Let the harness put its terminal in raw mode before typing.
    std::thread::sleep(Duration::from_millis(300));
    let server_id = id.clone();
    let (mut client, worker) = loopback_server(move |mut stream| stream_keys_as(&mut stream, &server_id, "claude"));
    for (sequence, prompt) in [(1, "first prompt"), (2, "second prompt")] {
        ws_send(&mut client, serde_json::json!({"sequence":sequence,"text":prompt,"enter":true}));
        assert_eq!(ws_receive(&mut client)["ok"], true);
        let expected = format!("SUBMIT:{prompt}");
        wait_until(&format!("{expected} on the screen"), || capture_pane_text(&id).is_some_and(|screen| screen.contains(&expected)));
        let screen = capture_pane_text(&id).unwrap_or_default();
        assert!(!screen.contains('|'), "Return joined the paste: {screen:?}");
    }
    drop(client);
    worker.join().unwrap();
}

#[test]
fn persistent_input_reopens_a_failed_tmux_connection() {
    // One failed tmux command used to poison the socket's control client,
    // so every later phone input was refused with "tmux connection must
    // be reopened" until the phone dropped the socket.
    let id = format!("sd_reopen_test_{}", std::process::id());
    let _session = TmuxSession::start(&id, &[], &["cat"]);
    let server_id = id.clone();
    let (mut client, worker) = loopback_server(move |mut stream| stream_keys(&mut stream, &server_id));
    ws_send(&mut client, serde_json::json!({"sequence":1,"text":"alpha","enter":true}));
    assert_eq!(ws_receive(&mut client)["ok"], true);

    // Drop the bridge's control client from under it, as a tmux hiccup would.
    crate::test_isolation::tmux(&["detach-client", "-s", &format!("={id}")]);
    ws_send(&mut client, serde_json::json!({"sequence":2,"text":"beta","enter":true}));
    let failed = ws_receive(&mut client);
    assert_eq!(failed["sequence"], 2);

    ws_send(&mut client, serde_json::json!({"sequence":3,"text":"gamma","enter":true}));
    let ack = ws_receive(&mut client);
    assert_eq!(ack["sequence"], 3);
    assert_eq!(ack["ok"], true, "input after a failed command must reopen tmux: {ack}");
    wait_until("the reopened client to deliver the input", || {
        capture_pane_text(&id).is_some_and(|screen| screen.contains("gamma"))
    });
    if failed["ok"] == false {
        // A failed input is reported, never replayed on the new client.
        assert!(!capture_pane_text(&id).unwrap().contains("beta"));
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

use crate::test_isolation::{wait_until, TmuxSession};

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
    let (mut client, server) = loopback_server(|connection| serve_connection(connection, None));
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
    let (mut client, server) = loopback_server(|connection| serve_connection(connection, None));
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
    #[cfg(target_os = "linux")]
    let flags = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(target_os = "macos")]
    let flags = libc::SOCK_STREAM;
    let fd = unsafe { libc::socket(libc::AF_INET, flags, 0) };
    assert!(fd >= 0);
    #[cfg(target_os = "macos")]
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }, 0);
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    address.sin_family = libc::AF_INET as libc::sa_family_t;
    #[cfg(target_os = "macos")]
    { address.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8; }
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
fn agent_response_markers_are_not_prompt_fallbacks() {
    // The pane's screen is not an input at all, so response text can
    // never stand in for a session without a recorded prompt.
    assert_eq!(last_user_text("sd_term_missing_prompt_test", "shell", None), None);
    assert_eq!(last_user_text("sd_term_missing_prompt_test", "claude", None), None);
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

/// Everything the bridge writes back for `request` on a fresh connection.
fn raw_exchange(connection: Connection, mut client: impl Read + Write, request: &str) -> String {
    let server = std::thread::spawn(move || serve_connection(connection, None));
    client.write_all(request.as_bytes()).unwrap();
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    server.join().unwrap();
    reply
}

/// One request on a connection served in-process: (status, body).
fn serve_one(connection: Connection, client: impl Read + Write, request: &str) -> (u16, serde_json::Value) {
    let response = raw_exchange(connection, client, request);
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (head.split_whitespace().nth(1).unwrap().parse().unwrap(), serde_json::from_str(body).unwrap())
}

/// A client on the owner-only control socket, and the bridge's end of it.
fn control_pair() -> (std::os::unix::net::UnixStream, Connection) {
    let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    (client, Connection::local(server))
}

#[test]
fn test_daemon_health_probe_skips_address_discovery_only_on_the_control_socket() {
    let probe = format!("GET /api/v1/ping?{HEALTH_QUERY} HTTP/1.1\r\nConnection: close\r\n\r\n");
    // The daemon's probe over the owner-only socket: just what
    // `bridge_running` checks, with no addresses (no `ip` run).
    let (client, server) = control_pair();
    let (status, body) = serve_one(server, client, &probe);
    assert_eq!(status, 200);
    assert_eq!((body["status"].as_str(), body["service"].as_str()), (Some("ok"), Some(SERVICE_NAME)));
    for field in ["addresses", "lanIp", "tailscaleIp", "hostname", "time"] {
        assert!(body.get(field).is_none(), "{field} in {body}");
    }
    // Over the network the same query is the unchanged phone-facing ping.
    let (client, server) = loopback_pair();
    let (status, body) = serve_one(Connection::plain(server), client, &probe);
    assert_eq!(status, 200);
    for field in ["status", "service", "protocolVersion", "hostname", "lanIp", "bridgeId", "addresses", "tailscaleIp", "port", "time"] {
        assert!(body.get(field).is_some(), "{field} missing from {body}");
    }
    // Hardware addresses go only to paired phones, in the harness list.
    assert!(body.get("wake").is_none(), "wake in the open ping: {body}");
}

fn preview_request(method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, serde_json::Value) {
    let (client, server) = loopback_pair();
    serve_one(Connection::plain(server), client, &http(method, path, token, body))
}

#[test]
fn preview_sessions_require_pairing_and_remain_owned_and_revocable() {
    let body = r#"{"url":"http://localhost:3000/a?q=1#art","futureField":true}"#;
    assert_eq!(preview_request("POST", "/api/v1/previews", None, body).0, 401);
    let token = Some(paired_token());
    let (status, created) = preview_request("POST", "/api/v1/previews", token, body);
    assert_eq!(status, 201);
    assert_eq!(created["path"], "/a?q=1#art");
    let path = format!("/api/v1/previews/{}", created["id"].as_str().unwrap());
    let (client, server) = loopback_pair();
    let browser = http("POST", "/api/v1/previews", token, body).replacen("Host:", "Origin: http://localhost\r\nHost:", 1);
    assert!(raw_exchange(Connection::plain(server), client, &browser).contains("browser_access_disabled"));
    // A different paired device cannot open or delete the session.
    let stranger = random_hex(24);
    {
        let mut state = pair_state().lock().unwrap();
        state.cfg.devices.push(PairedDevice { id: random_hex(16), name: "preview stranger".into(),
            device_type: "android".into(), token_hash: security::digest(stranger.as_bytes()), expires: now_epoch()+3600.0 });
        assert!(state.save());
    }
    assert_eq!(preview_request("DELETE", &path, Some(&stranger), "").0, 404);
    assert_eq!(preview_request("GET", &format!("{path}/tunnel"), Some(&stranger), "").0, 404);
    assert_eq!(preview_request("DELETE", &path, token, "").0, 200);
    assert_eq!(preview_request("GET", &format!("{path}/tunnel"), token, "").0, 404);
    for url in ["http://192.168.0.1:3000", "http://localhost:8759", "https://localhost:3000"] {
        assert_eq!(preview_request("POST", "/api/v1/previews", token, &serde_json::json!({"url":url}).to_string()).0, 400);
    }
}

#[test]
fn preview_tunnel_streams_http_bytes_and_closes_on_session_delete() {
    use tungstenite::client::IntoClientRequest;
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = upstream.local_addr().unwrap().port();
    let token = Some(paired_token());
    let (status, created) = preview_request("POST", "/api/v1/previews", token,
        &serde_json::json!({"url":format!("http://localhost:{port}/")}).to_string());
    assert_eq!(status, 201);
    let path = format!("/api/v1/previews/{}", created["id"].as_str().unwrap());
    let (client, server) = loopback_pair();
    let bridge = std::thread::spawn(move || serve_connection(Connection::plain(server), None));
    let mut request = format!("ws://127.0.0.1{path}/tunnel").into_client_request().unwrap();
    request.headers_mut().insert("Authorization", format!("Bearer {}", token.unwrap()).parse().unwrap());
    let (mut ws, _) = tungstenite::client(request, client).unwrap();
    let (mut service, _) = upstream.accept().unwrap();
    service.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let request = b"POST /art HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\nhello";
    // Separate messages model a request head/body arriving on different reads.
    ws.send(tungstenite::Message::Binary(request[..11].to_vec().into())).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    ws.send(tungstenite::Message::Binary(request[11..].to_vec().into())).unwrap();
    let mut bytes = vec![0; request.len()];
    service.read_exact(&mut bytes).unwrap();
    assert_eq!(bytes, request);
    let response = vec![0x7f; 70000];
    service.write_all(&response).unwrap();
    let mut received = Vec::new();
    while received.len() < response.len() {
        match ws.read().unwrap() {
            tungstenite::Message::Binary(bytes) => received.extend_from_slice(&bytes),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(received, response);
    assert_eq!(preview_request("DELETE", &path, token, "").0, 200);
    assert!(matches!(ws.read(), Ok(tungstenite::Message::Close(_)) | Err(_)));
    bridge.join().unwrap();
    assert_eq!(service.read(&mut [0]).unwrap(), 0);
}

#[test]
fn preview_tunnel_stops_when_its_device_is_revoked() {
    use tungstenite::client::IntoClientRequest;
    let token = random_hex(24);
    let hash = security::digest(token.as_bytes());
    {
        let mut state = pair_state().lock().unwrap();
        state.cfg.devices.push(PairedDevice { id: random_hex(16), name: "preview revocation".into(),
            device_type: "android".into(), token_hash: hash.clone(), expires: now_epoch()+3600.0 });
        assert!(state.save());
    }
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let (status, created) = preview_request("POST", "/api/v1/previews", Some(&token),
        &serde_json::json!({"url":format!("http://localhost:{}/", upstream.local_addr().unwrap().port())}).to_string());
    assert_eq!(status, 201);
    let path = format!("/api/v1/previews/{}/tunnel", created["id"].as_str().unwrap());
    let (mut client, server) = loopback_pair();
    client.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let bridge = std::thread::spawn(move || serve_connection(Connection::plain(server), None));
    let mut request = format!("ws://127.0.0.1{path}").into_client_request().unwrap();
    request.headers_mut().insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tungstenite::client(request, &mut client).unwrap();
    let (mut service, _) = upstream.accept().unwrap();
    service.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    // Leave a partial client frame buffered when authorization disappears.
    ws.get_mut().write_all(&[0x82, 0x85, 1, 2]).unwrap();
    {
        let mut state = pair_state().lock().unwrap();
        state.cfg.devices.retain(|d| d.token_hash != hash);
        assert!(state.save());
    }
    assert!(ws.read().is_err() || !ws.can_read());
    bridge.join().unwrap();
    assert_eq!(service.read(&mut [0]).unwrap(), 0);
    assert_eq!(preview_request("GET", &path, Some(&token), "").0, 401);
}

/// A device token the bridge accepts, registered once per test process
/// in the test-only credential store (`state_path`).
pub(super) fn paired_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let token = random_hex(24);
        let mut state = pair_state().lock().unwrap();
        state.refresh();
        state.cfg.devices.push(PairedDevice {
            id: random_hex(16),
            name: "reply bytes test".into(),
            device_type: "android".into(),
            token_hash: security::digest(token.as_bytes()),
            expires: now_epoch() + 3600.0,
        });
        assert!(state.save());
        token
    })
}

/// One request, with or without a device token, that closes its connection.
fn http(method: &str, path: &str, token: Option<&str>, body: &str) -> String {
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn json_reply(status: &str, body: &str) -> String {
    format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

/// The bytes on the wire are the protocol: released phones and PCs parse
/// these status lines, headers and bodies.
#[test]
fn replies_keep_their_exact_status_lines_headers_and_bodies() {
    let token = Some(paired_token());
    let pid = std::process::id();
    let live = format!("sd_term_reply_bytes_{pid}");
    let missing = format!("sd_term_reply_missing_{pid}");
    let _session = TmuxSession::start(&live, &[], &["cat"]);
    let machine = pair_state().lock().unwrap().cfg.bridge_id.clone();
    // The epoch matches desktop_bridge's replay test: the dedup cache keeps
    // one epoch at a time, so a second one would evict that test's answer.
    let command = |machine: &str| serde_json::json!({"requestId":format!("reply-bytes-{pid}"),"machineId":machine,
        "expectedEpoch":"reply-test-epoch","command":{"type":"setExpanded","cardId":"card-1","expectedRevision":1,"expanded":true}}).to_string();
    let browser = "GET /api/v1/ping HTTP/1.1\r\nOrigin: https://x\r\nConnection: close\r\n\r\n".to_string();
    let browser_upload = format!("POST /api/v1/harnesses/{live}/image-prompt HTTP/1.1\r\nOrigin: https://x\r\nConnection: close\r\n\r\n");
    let oversized = |route: &str| format!("POST /api/v1/harnesses/{live}/{route} HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: 999999999\r\nConnection: close\r\n\r\n", paired_token());
    let oversized_transcribe = format!("POST /api/v1/transcribe HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: 999999999\r\nConnection: close\r\n\r\n", paired_token());
    let cases = [
        (browser, "403 Forbidden", r#"{"error":"browser_access_disabled"}"#.to_string()),
        (browser_upload, "403 Forbidden", r#"{"error":"browser_access_disabled"}"#.into()),
        (http("GET", "/api/v1/desktop/capabilities", None, ""), "401 Unauthorized", r#"{"error":"not_paired"}"#.into()),
        (http("GET", "/api/v1/harnesses", None, ""), "401 Unauthorized", r#"{"error":"not_paired","status":"error"}"#.into()),
        (http("GET", "/api/v1/transcribe", None, ""), "401 Unauthorized", r#"{"error":"not_paired"}"#.into()),
        (http("POST", &format!("/api/v1/harnesses/{live}/image-prompt"), None, "{}"), "401 Unauthorized", r#"{"error":"not_paired"}"#.into()),
        (oversized("image-prompt"), "413 Payload Too Large", r#"{"error":"image_too_large"}"#.into()),
        (oversized("attachment-prompt"), "413 Payload Too Large", r#"{"error":"attachments_too_large"}"#.into()),
        (oversized_transcribe, "413 Payload Too Large", r#"{"error":"audio_too_large"}"#.into()),
        (http("GET", "/api/v1/transcribe", token, ""), "200 OK", crate::transcribe::status_json().to_string()),
        (http("POST", "/api/v1/transcribe", token, "nope"), "400 Bad Request", r#"{"error":"bad_json"}"#.into()),
        (http("POST", "/api/v1/transcribe", token, "{}"), "400 Bad Request", r#"{"error":"empty_audio"}"#.into()),
        (http("GET", "/api/v1/nope", None, ""), "404 Not Found", r#"{"error":"not found"}"#.into()),
        (http("GET", "/api/v1/theme", token, ""), "200 OK", theme_document().to_string()),
        (http("POST", "/api/v1/harnesses/other/relaunch", token, "{}"), "404 Not Found", r#"{"error":"not found"}"#.into()),
        (http("POST", &format!("/api/v1/harnesses/{live}/relaunch"), token, "nope"), "400 Bad Request", r#"{"error":"bad_json"}"#.into()),
        (http("POST", &format!("/api/v1/harnesses/{live}/relaunch"), token, r#"{"mode":"bogus"}"#), "400 Bad Request", r#"{"error":"invalid_mode"}"#.into()),
        (http("POST", &format!("/api/v1/harnesses/{missing}/relaunch"), token, r#"{"mode":"fresh"}"#), "503 Service Unavailable", r#"{"error":"desktop_not_running"}"#.into()),
        (http("GET", "/api/v1/desktop/commands", token, ""), "405 Method Not Allowed", r#"{"error":"method_not_allowed"}"#.into()),
        (http("POST", "/api/v1/desktop/commands", token, "nope"), "400 Bad Request", r#"{"error":"invalid_command"}"#.into()),
        (http("POST", "/api/v1/desktop/commands", token, &command("another-machine")), "409 Conflict", r#"{"error":"wrong_machine"}"#.into()),
        (http("POST", "/api/v1/desktop/commands", token, &command(&machine)), "503 Service Unavailable", r#"{"error":"desktop_unavailable"}"#.into()),
        (http("GET", "/api/v1/desktop/terminals/bad.id/attach", token, ""), "404 Not Found", r#"{"error":"unknown_card"}"#.into()),
        (http("GET", "/api/v1/desktop/terminals/a/b/attach", token, ""), "404 Not Found", r#"{"error":"not_found"}"#.into()),
        (http("GET", "/api/v1/desktop/workspace", token, ""), "503 Service Unavailable", r#"{"error":"desktop_unavailable"}"#.into()),
        (http("GET", "/api/v1/workspaces", token, ""), "503 Service Unavailable", r#"{"error":"desktop_not_running"}"#.into()),
        (http("POST", "/api/v1/harnesses", token, r#"{"agentType":"nope"}"#), "400 Bad Request", r#"{"error":"unsupported_harness"}"#.into()),
        (http("POST", "/api/v1/completions", token, "{}"), "400 Bad Request", r#"{"error":"invalid_sessions"}"#.into()),
        (http("GET", "/api/v1/harnesses/stream", token, ""), "400 Bad Request", r#"{"error":"expected_websocket","status":"error"}"#.into()),
        (http("DELETE", "/api/v1/harnesses/other", token, ""), "404 Not Found", r#"{"error":"no_such_session","status":"error"}"#.into()),
        (http("DELETE", &format!("/api/v1/harnesses/{missing}"), token, ""), "503 Service Unavailable", r#"{"error":"desktop_not_running","status":"error"}"#.into()),
        (http("GET", &format!("/api/v1/harnesses/{missing}/prompts"), token, ""), "404 Not Found", r#"{"error":"no_such_session","status":"error"}"#.into()),
        (http("POST", &format!("/api/v1/harnesses/{missing}/keys"), token, r#"{"text":"x"}"#), "404 Not Found", r#"{"error":"no_such_session","status":"error"}"#.into()),
        (http("POST", &format!("/api/v1/harnesses/{missing}/editor-action"), token, "{}"), "404 Not Found", r#"{"error":"no_such_session","status":"error"}"#.into()),
        (http("GET", &format!("/api/v1/harnesses/{missing}/input"), token, ""), "404 Not Found", r#"{"error":"no_such_session"}"#.into()),
        (http("POST", &format!("/api/v1/harnesses/{live}/keys"), token, "nope"), "400 Bad Request", r#"{"error":"bad_json","status":"error"}"#.into()),
        (http("POST", &format!("/api/v1/harnesses/{live}/keys"), token, &serde_json::json!({"text":"x".repeat(4097)}).to_string()),
            "413 Payload Too Large", r#"{"error":"text_too_long","status":"error"}"#.into()),
        (http("POST", &format!("/api/v1/harnesses/{live}/editor-action"), token, "nope"), "400 Bad Request", r#"{"error":"bad_json","status":"error"}"#.into()),
        (http("POST", &format!("/api/v1/harnesses/{live}/editor-action"), token, "{}"), "400 Bad Request", r#"{"error":"invalid_request","status":"error"}"#.into()),
        (http("GET", &format!("/api/v1/harnesses/{live}/input"), token, ""), "400 Bad Request", r#"{"error":"expected_websocket"}"#.into()),
        (http("GET", &format!("/api/v1/harnesses/{live}/stream"), token, ""), "400 Bad Request", r#"{"error":"expected_websocket","status":"error"}"#.into()),
        (http("GET", "/api/v1/pair/state", None, ""), "403 Forbidden", r#"{"error":"desktop_approval_only"}"#.into()),
        (http("POST", "/api/v1/pair/poll", None, r#"{"requestId":"unknown"}"#), "404 Not Found", r#"{"error":"request_expired"}"#.into()),
    ];
    for (request, status, body) in cases {
        let (client, server) = loopback_pair();
        let reply = raw_exchange(Connection::plain(server), client, &request);
        assert_eq!(reply, json_reply(status, &body), "{request}");
    }
    // The owner-only control socket.
    let local = [
        (format!("GET /api/v1/ping?{HEALTH_QUERY} HTTP/1.1\r\n\r\n"), "200 OK",
            format!(r#"{{"port":{BRIDGE_PORT},"protocolVersion":{PROTOCOL_VERSION},"service":"{SERVICE_NAME}","status":"ok"}}"#)),
        (http("POST", "/api/v1/pair/unknown", None, "{}"), "409 Conflict", r#"{"error":"explicit_desktop_approval_required"}"#.into()),
        (http("POST", "/api/v1/pair/rejected/remove", None, r#"{"id":"missing"}"#), "404 Not Found", r#"{"error":"not_rejected"}"#.into()),
    ];
    for (request, status, body) in local {
        let (client, server) = control_pair();
        let reply = raw_exchange(server, client, &request);
        assert_eq!(reply, json_reply(status, &body), "{request}");
    }
}

#[test]
fn every_status_the_bridge_sends_has_its_reason_phrase() {
    let mut codes: Vec<u16> = crate::desktop_protocol::COMMAND_ERRORS.iter()
        .map(|error| crate::desktop_protocol::command_status(error)).collect();
    codes.extend([200, 201, 202, 400, 401, 403, 404, 405, 409, 413, 429, 500, 501, 502, 503, 504]);
    for code in codes {
        assert_ne!(reason_phrase(code), "Unknown", "{code}");
    }
}

#[test]
fn binary_replies_keep_their_header_order() {
    let send = |content_type: &str, headers: &[&str], bytes: &[u8]| {
        let (mut client, server) = loopback_pair();
        let mut connection = Connection::plain(server);
        connection.persist = true;
        reply_binary(&mut connection, content_type, headers, bytes);
        assert!(!connection.persist, "a binary reply ends the connection");
        drop(connection);
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        reply
    };
    let file = vec![7u8; 70 * 1024];
    let mut expected = b"HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: 71680\r\nContent-Disposition: attachment\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n".to_vec();
    expected.extend_from_slice(&file);
    assert_eq!(send("text/markdown", &["Content-Disposition: attachment", "X-Content-Type-Options: nosniff", "Cache-Control: no-store"], &file), expected);
    assert_eq!(send("image/png", &["Cache-Control: no-store", "X-Content-Type-Options: nosniff"], b"png"),
        b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 3\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\npng".to_vec());
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

#[test]
fn completion_json_omits_absent_attention_and_includes_present_attention() {
    let base = crate::completion::Completion {
        id: "sd_term_a".into(),
        supported: true,
        state: "working".into(),
        completion_id: None,
        attention: None,
        attention_id: None,
    };
    let plain = serde_json::to_value(&base).unwrap();
    assert!(plain.get("attention").is_none());
    assert!(plain.get("attentionId").is_none());

    let mut waiting = base;
    waiting.attention = Some("waiting".into());
    waiting.attention_id = Some("f".repeat(64));
    let alerted = serde_json::to_value(&waiting).unwrap();
    assert_eq!(alerted["attention"], "waiting");
    assert_eq!(alerted["attentionId"], "f".repeat(64));
}
