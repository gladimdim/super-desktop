//! Pending requests are ephemeral, bounded and never authorize access before
//! a loopback-only desktop approval. Existing bearer tokens remain valid.
use super::*;
use serde_json::{json, Value};

#[derive(Clone)]
struct Pending {
    id: String,
    code: String,
    device: String,
    peer: String,
    expires: f64,
    decision: Option<bool>,
    token: Option<String>,
}

#[derive(Default)]
struct Requests { entries: Vec<Pending> }

impl Requests {
    fn prune(&mut self, now: f64) { self.entries.retain(|p| p.expires > now); }
    fn decide(&mut self, id: &str, approve: bool, now: f64) -> Option<&mut Pending> {
        self.prune(now);
        let request = self.entries.iter_mut().find(|p| p.id == id && p.decision.is_none())?;
        request.decision = Some(approve);
        Some(request)
    }
}

fn requests() -> &'static Mutex<Requests> {
    static STATE: OnceLock<Mutex<Requests>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(Requests::default()))
}

#[derive(Default)]
struct Invitation { secret: String, expires: f64 }
static INVITATION: Mutex<Option<Invitation>> = Mutex::new(None);

pub(super) fn handle(stream: &mut Connection, req: &Request, local: bool, path: &str) {
    let local_only = !matches!(path, "/api/v1/pair" | "/api/v1/pair/poll");
    if local_only && !local {
        return respond(stream, 403, "Forbidden", &json!({"error":"desktop_approval_only"}));
    }
    let body: Value = serde_json::from_str(&req.body).unwrap_or(Value::Null);
    let now = now_epoch();
    let mut state = requests().lock().unwrap();
    state.prune(now);
    match (req.method.as_str(), path) {
        ("POST", "/api/v1/pair/invitation") => {
            let secret = random_hex(24);
            *INVITATION.lock().unwrap() = Some(Invitation { secret: secret.clone(), expires: now + 180.0 });
            respond(stream, 200, "OK", &json!({"v":3,"host":lan_ip(),"port":BRIDGE_PORT,
                "fingerprint":security::fingerprint(),"secret":secret,"expiresIn":180}));
        }
        ("GET", "/api/v1/pair/devices") => {
            let paired = pair_state().lock().unwrap();
            let devices: Vec<_> = paired.cfg.devices.iter().map(|d| json!({"id":d.id,"name":d.name,"expires":d.expires,
                "active":d.expires > now && security::device_active(&d.token_hash)})).collect();
            respond(stream, 200, "OK", &json!({"devices":devices}));
        }
        ("POST", "/api/v1/pair/revoke") => {
            let mut paired = pair_state().lock().unwrap();
            let id = body["deviceId"].as_str().unwrap_or("");
            let previous = paired.cfg.devices.clone();
            let hash = previous.iter().find(|d| d.id == id).map(|d| d.token_hash.clone());
            paired.cfg.devices.retain(|d| d.id != id);
            if !paired.save() {
                paired.cfg.devices = previous;
                return respond(stream, 500, "Internal Server Error", &json!({"error":"could_not_save_revocation"}));
            }
            if let Some(hash) = hash {
                security::disconnect(&hash);
                state.entries.retain(|r| !r.token.as_ref().is_some_and(|t| security::equal(&security::digest(t.as_bytes()), &hash)));
            }
            respond(stream, 200, "OK", &json!({"status":"revoked"}));
        }
        ("POST", "/api/v1/pair") => {
            let mut invitation = INVITATION.lock().unwrap();
            if !invitation.as_ref().is_some_and(|i| i.expires > now && security::equal(&i.secret, body["secret"].as_str().unwrap_or(""))) {
                return respond(stream, 403, "Forbidden", &json!({"error":"scan_fresh_desktop_pairing_qr"}));
            }
            let peer = stream.peer_addr().map(|p| p.ip().to_string()).unwrap_or_default();
            // Requests are rate-limited by source and globally bounded, including
            // decided requests until expiry, to limit notification spam.
            if state.entries.len() >= 8 || state.entries.iter().any(|p| p.peer == peer) {
                return respond(stream, 429, "Too Many Requests", &json!({"error":"pairing_request_already_pending_or_rate_limited"}));
            }
            let device: String = body["deviceName"].as_str().unwrap_or("Android phone")
                .chars().filter(|c| !c.is_control()).take(64).collect();
            let id = random_hex(24);
            let code = format!("{:06}", u32::from_str_radix(&random_hex(4), 16).unwrap() % 1_000_000);
            state.entries.push(Pending { id: id.clone(), code: code.clone(), device, peer,
                expires: now + 120.0, decision: None, token: None });
            *invitation = None;
            // Constant notification text: remote device names are untrusted.
            if !cfg!(test) { std::thread::spawn(|| {
                let _ = Command::new("notify-send").args(["SUPER DESKTOP: phone pairing request",
                    "Open SUPER DESKTOP settings → Android. Compare the code on your phone, then Approve or Deny."]).status();
            }); }
            respond(stream, 202, "Accepted", &json!({"status":"pending","requestId":id,"code":code,"expiresIn":120}));
        }
        ("GET", "/api/v1/pair/state") => {
            let pending: Vec<_> = state.entries.iter().filter(|p| p.decision.is_none()).map(|p|
                json!({"requestId":p.id,"code":p.code,"deviceName":p.device,"address":p.peer})).collect();
            respond(stream, 200, "OK", &json!({"requests":pending}));
        }
        ("POST", "/api/v1/pair/approve" | "/api/v1/pair/deny") => {
            let approve = path.ends_with("/approve");
            let Some(request) = state.decide(body["requestId"].as_str().unwrap_or(""), approve, now) else {
                return respond(stream, 404, "Not Found", &json!({"error":"request_expired_or_already_decided"}));
            };
            if approve {
                let token = random_hex(24);
                let mut paired = pair_state().lock().unwrap();
                paired.refresh();
                if paired.cfg.devices.len() >= 64 {
                    request.decision = None;
                    return respond(stream, 409, "Conflict", &json!({"error":"revoke_old_devices_first"}));
                }
                let device_id = random_hex(16);
                paired.cfg.devices.push(PairedDevice { id: device_id.clone(), name: request.device.clone(),
                    token_hash: security::digest(token.as_bytes()), expires: now + 90.0 * 86400.0 });
                if !paired.save() {
                    paired.cfg.devices.retain(|d| d.id != device_id);
                    request.decision = None;
                    return respond(stream, 500, "Internal Server Error", &json!({"error":"could_not_save_pairing"}));
                }
                request.token = Some(token);
            }
            respond(stream, 200, "OK", &json!({"status":if approve {"approved"} else {"denied"}}));
        }
        ("POST", "/api/v1/pair/poll") => {
            let Some(request) = state.entries.iter().find(|p| Some(p.id.as_str()) == body["requestId"].as_str()) else {
                return respond(stream, 404, "Not Found", &json!({"error":"request_expired"}));
            };
            let result = match request.decision {
                None => json!({"status":"pending"}),
                Some(false) => json!({"status":"denied"}),
                Some(true) => json!({"status":"paired","token":request.token}),
            };
            respond(stream, 200, "OK", &result);
        }
        _ => respond(stream, 409, "Conflict", &json!({"error":"explicit_desktop_approval_required"})),
    }
}

fn local_request(method: &str, path: &str, body: Value) -> Result<Value, String> {
    let mut stream = std::os::unix::net::UnixStream::connect(security::control_path())
        .map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
    let body = body.to_string();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|e| e.to_string())?;
    let result: Value = serde_json::from_str(response.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(""))
        .map_err(|e| e.to_string())?;
    if let Some(error) = result["error"].as_str() { return Err(error.to_string()); }
    Ok(result)
}

pub fn pending_requests() -> Vec<Value> {
    local_request("GET", "/api/v1/pair/state", json!({})).ok()
        .and_then(|v| v["requests"].as_array().cloned()).unwrap_or_default()
}

pub fn decide_request(id: &str, approve: bool) -> Result<(), String> {
    local_request("POST", if approve {"/api/v1/pair/approve"} else {"/api/v1/pair/deny"}, json!({"requestId":id})).map(|_| ())
}

pub fn paired_devices() -> Vec<Value> {
    local_request("GET", "/api/v1/pair/devices", json!({})).ok()
        .and_then(|v| v["devices"].as_array().cloned()).unwrap_or_else(|| {
            // Keep the registered count when the bridge is stopped; never initialize
            // or rewrite its credential store from the UI process.
            fs::read(state_path()).ok().and_then(|bytes| serde_json::from_slice::<BridgeConfig>(&bytes).ok())
                .map(|cfg| cfg.devices.iter().map(|d| json!({"id":d.id,"name":d.name,"expires":d.expires,"active":false})).collect())
                .unwrap_or_default()
        })
}
pub fn revoke_device(id: &str) -> Result<(), String> {
    local_request("POST", "/api/v1/pair/revoke", json!({"deviceId":id})).map(|_| ())
}
pub fn pairing_invitation() -> Result<String, String> {
    local_request("POST", "/api/v1/pair/invitation", json!({})).map(|v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending() -> Pending { Pending { id:"secret".into(), code:"123456".into(), device:"phone".into(), peer:"ip".into(), expires:120.0, decision:None, token:None } }
    #[test] fn requests_require_explicit_decisions() {
        let mut requests = Requests { entries: vec![pending()] };
        assert!(requests.entries[0].token.is_none());
        assert!(requests.decide("wrong", true, 1.0).is_none());
        assert_eq!(requests.decide("secret", false, 1.0).unwrap().decision, Some(false));
        assert!(requests.decide("secret", true, 2.0).is_none());
        assert!(requests.entries[0].token.is_none());
    }
    #[test] fn expired_requests_cannot_be_approved() {
        let mut requests = Requests { entries: vec![pending()] };
        assert!(requests.decide("secret", true, 120.0).is_none());
        assert!(requests.entries.is_empty());
    }

    #[test] fn http_pairing_never_grants_access_without_local_approval() {
        fn call(path: &str, method: &str, body: Value, local: bool) -> (u16, Value) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (server, _) = listener.accept().unwrap();
            let mut server = Connection::plain(server);
            let request = Request { method:method.into(), path:path.into(), body:body.to_string(), headers:HashMap::new() };
            handle(&mut server, &request, local, path);
            drop(server);
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            (response.split_whitespace().nth(1).unwrap().parse().unwrap(),
                serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap())
        }
        assert_eq!(call("/api/v1/pair", "POST", json!({"deviceName":"test","pin":"1234"}), false).0, 403);
        assert_eq!(call("/api/v1/pair/invitation", "POST", json!({}), false).0, 403);
        let invite = call("/api/v1/pair/invitation", "POST", json!({}), true).1;
        let (status, request) = call("/api/v1/pair", "POST", json!({"deviceName":"test","secret":invite["secret"]}), false);
        assert_eq!(status, 202);
        assert!(request.get("token").is_none());
        assert_eq!(call("/api/v1/pair", "POST", json!({"secret":invite["secret"]}), false).0, 403);
        let id = request["requestId"].clone();
        assert_eq!(call("/api/v1/pair/approve", "POST", json!({"requestId":id}), false).0, 403);
        assert_eq!(call("/api/v1/pair/state", "GET", json!({}), false).0, 403);
        assert_eq!(call("/api/v1/pair/poll", "POST", json!({"requestId":"unknown"}), false).0, 404);
        let pending = call("/api/v1/pair/poll", "POST", json!({"requestId":id}), false).1;
        assert_eq!(pending["status"], "pending");
        assert!(pending.get("token").is_none());
        assert_eq!(call("/api/v1/pair/deny", "POST", json!({"requestId":id}), true).0, 200);
        assert_eq!(call("/api/v1/pair/poll", "POST", json!({"requestId":id}), false).1["status"], "denied");
        assert_eq!(call("/api/v1/pair/approve", "POST", json!({"requestId":id}), true).0, 404);
        requests().lock().unwrap().entries.retain(|p| Some(p.id.as_str()) != id.as_str());
    }
}
