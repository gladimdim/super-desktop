use super::*;

// ---------- minimal HTTP ----------

pub(in crate::bridge) struct Request {
    pub(in crate::bridge) method: String,
    pub(in crate::bridge) path: String,
    /// Raw query string (after `?`), empty when absent.
    pub(in crate::bridge) query: String,
    /// HTTP/1.1 without `Connection: close` (HTTP/1.0 never persists).
    pub(in crate::bridge) keep_alive: bool,
    pub(in crate::bridge) headers: HashMap<String, String>,
    pub(in crate::bridge) body: String,
    pub(in crate::bridge) _upload_slot: Option<crate::assets::Transfer>,
}

pub(in crate::bridge) fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

pub(in crate::bridge) fn read_request(stream: &mut Connection, admission: Option<&security::Admission>) -> Option<Request> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    let mut buf = vec![0u8; 16384];
    let mut total = 0usize;
    let header_end = loop {
        let n = stream.read(&mut buf[total..]).ok()?;
        if n == 0 {
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
        "POST" if crate::transcribe::route(&path) => Some((
            crate::transcribe::MAX_BODY,
            crate::transcribe::UPLOAD_DEADLINE,
            "audio_too_large",
        )),
        _ => None,
    };
    if headers.contains_key("transfer-encoding") { return None; }
    let mut upload_slot = None;
    if let Some((limit, deadline, too_large)) = upload {
        if headers.contains_key("origin") || headers.contains_key("sec-fetch-site") {
            reply_error(stream, 403, Envelope::Plain, "browser_access_disabled");
            return None;
        }
        if !require_pairing(stream, &headers, Envelope::Plain) {
            return None;
        }
        if content_len > limit {
            reply_error(stream, 413, Envelope::Plain, too_large);
            return None;
        }
        upload_slot = crate::assets::Transfer::acquire();
        if upload_slot.is_none() {
            reply_error(stream, 429, Envelope::Plain, "busy");
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

/// The reason phrase the bridge sends with each status code it answers with.
pub(in crate::bridge) fn reason_phrase(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

pub(in crate::bridge) fn reply_json(stream: &mut Connection, code: u16, value: &serde_json::Value) {
    let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    reply_body(stream, code, &body);
}

/// `reply_json` for callers that still name the reason phrase, which must be
/// the one `reason_phrase` sends.
/// `reply_json` for an already serialized JSON body. The connection stays open
/// for another request only when `stream.persist` allows it.
pub(in crate::bridge) fn reply_body(stream: &mut Connection, code: u16, body: &str) {
    let connection = if stream.persist { "keep-alive" } else { "close" };
    let head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n",
        reason_phrase(code),
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

/// The two error documents: `{"error"}`, and the `{"status":"error","error"}`
/// envelope the older Android routes have always answered with.
#[derive(Clone, Copy)]
pub(in crate::bridge) enum Envelope {
    Plain,
    Status,
}

impl Envelope {
    pub(in crate::bridge) fn error(self, error: impl Serialize) -> serde_json::Value {
        match self {
            Envelope::Plain => serde_json::json!({"error": error}),
            Envelope::Status => serde_json::json!({"status": "error", "error": error}),
        }
    }
}

pub(in crate::bridge) fn reply_error(stream: &mut Connection, code: u16, envelope: Envelope, error: impl Serialize) {
    reply_json(stream, code, &envelope.error(error));
}

/// A binary `200 OK` that ends the connection; `headers` follow
/// Content-Length in the given order.
pub(in crate::bridge) fn reply_binary(stream: &mut Connection, content_type: &str, headers: &[&str], bytes: &[u8]) {
    stream.persist = false;
    let mut head = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n", bytes.len());
    for header in headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    // Connection checks revocation on every write, including buffered TLS.
    for chunk in bytes.chunks(64 * 1024) {
        if stream.write_all(chunk).is_err() {
            break;
        }
    }
}

pub(in crate::bridge) fn bearer(headers: &HashMap<String, String>) -> &str {
    let h = headers.get("authorization").map(String::as_str).unwrap_or("");
    if h.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("bearer ")) {
        h.get(7..).unwrap_or("").trim()
    } else {
        ""
    }
}

/// Protected Android and desktop routes require a registered bearer token.
/// Public ping and pairing, and owner-only Unix routes, are dispatched separately.
pub(in crate::bridge) fn authorize(headers: &HashMap<String, String>) -> bool {
    pair_state()
            .lock()
            .map(|mut s| s.valid(bearer(headers)))
            .unwrap_or(false)
}

/// Keep the established `status` envelope on older Android routes while
/// sharing the token check. Other routes use the plain error document.
/// Every caller returns immediately on `false`.
pub(in crate::bridge) fn require_pairing(stream: &mut Connection, headers: &HashMap<String, String>, envelope: Envelope) -> bool {
    if authorize(headers) {
        return true;
    }
    reply_error(stream, 401, envelope, "not_paired");
    false
}

/// A paired device and a live session, as the per-session routes require.
/// On `false` the refusal was sent and the caller returns.
pub(in crate::bridge) fn require_live_session(stream: &mut Connection, req: &Request, id: &str, envelope: Envelope) -> bool {
    if !require_pairing(stream, &req.headers, envelope) {
        return false;
    }
    if !crate::tmux::session_exists(id) {
        reply_error(stream, 404, envelope, "no_such_session");
        return false;
    }
    true
}

/// Complete a WebSocket upgrade; `false` when this is not a WS request (the
/// caller then answers with plain HTTP).
pub(in crate::bridge) fn ws_upgrade(stream: &mut Connection, req: &Request) -> bool {
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
pub(in crate::bridge) const STREAM_MAX_SECS: u64 = 1800;

/// Drain any frames the client sent without blocking.
///
/// Returns false when the peer went away (EOF or a Close frame), true when it
/// is still there — including a Ping, which is answered so picky clients stay
/// happy. A read timeout just means "nothing to read".
pub(in crate::bridge) fn ws_client_alive(stream: &mut Connection) -> bool {
    ws_client_message(stream, |_| {})
}

/// The normal streams discard text; an opted-in terminal stream may handle
/// viewport requests. Existing ping, close and partial-frame handling is shared.
pub(in crate::bridge) fn ws_client_message(stream: &mut Connection, mut on_text: impl FnMut(&str)) -> bool {
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
        Ok(Some(crate::ws::Frame::Text(text))) => { on_text(&text); true },
        Ok(Some(_)) => true,
        Err(_) => false,
    }
}

