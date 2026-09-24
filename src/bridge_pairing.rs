//! Pending requests are ephemeral, bounded and never authorize access before
//! a loopback-only desktop approval. Existing bearer tokens remain valid.
//! A rejected device is remembered and refused until the owner removes it.
use super::*;
use serde_json::{json, Value};

/// Length of the installation ID a SUPER DESKTOP PC sends with its pairing
/// request: its own bridge ID.
const DEVICE_ID_LEN: usize = 32;
/// Oldest rejections are dropped past this, so the store stays bounded.
const MAX_REJECTED: usize = 256;
/// How long a request waits for the desktop decision.
const REQUEST_SECS: f64 = 120.0;

// Descriptive metadata only; never used to grant permissions. Unknown clients stay unclassified.
fn device_type(body: &Value) -> &str {
    match body["deviceType"].as_str() {
        Some("pc") => "pc",
        Some("android") => "android",
        Some("mobile") => "mobile",
        _ => "unknown",
    }
}

/// The requester's installation ID, when it sent a well-formed one. Like the
/// name, it is reported by the device: it recognises a device, never trusts it.
fn device_id(body: &Value) -> String {
    body["deviceId"]
        .as_str()
        .filter(|id| id.len() == DEVICE_ID_LEN && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

impl RejectedDevice {
    /// The same installation ID, or, for clients without one, the same name
    /// from the same address.
    fn matches(&self, device_id: &str, name: &str, address: &str) -> bool {
        (!self.device_id.is_empty() && self.device_id == device_id)
            || (!address.is_empty() && self.address == address && self.name == name)
    }

    fn to_json(&self) -> Value {
        json!({"id":self.id,"name":self.name,"deviceType":self.device_type,"address":self.address,
            "rejectedAt":self.rejected_at,"identified":!self.device_id.is_empty()})
    }
}

#[derive(Clone)]
struct Pending {
    id: String,
    code: String,
    device: String,
    device_type: String,
    device_id: String,
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

/// Remember a rejected request's device. The entry stays in memory even when
/// the disk write fails, so this bridge still refuses the device until it
/// restarts; the return value says whether it was saved.
fn remember_rejection(paired: &mut PairState, request: &Pending, now: f64) -> bool {
    let entry = RejectedDevice {
        id: random_hex(8),
        name: request.device.clone(),
        device_type: request.device_type.clone(),
        device_id: request.device_id.clone(),
        address: request.peer.clone(),
        rejected_at: now,
    };
    add_rejection(&mut paired.cfg.rejected, entry);
    paired.save()
}

/// One entry per device: rejecting it again only refreshes its entry.
fn add_rejection(list: &mut Vec<RejectedDevice>, entry: RejectedDevice) {
    list.retain(|r| !r.matches(&entry.device_id, &entry.name, &entry.address));
    list.push(entry);
    let overflow = list.len().saturating_sub(MAX_REJECTED);
    list.drain(..overflow);
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
            *INVITATION.lock().unwrap() = Some(Invitation { secret: secret.clone(), expires: now + 300.0 });
            respond(stream, 200, "OK", &json!({"v":3,"host":lan_ip(),"port":BRIDGE_PORT,
                "fingerprint":security::fingerprint(),"secret":secret,"expiresIn":300}));
        }
        ("GET", "/api/v1/pair/devices") => {
            let paired = pair_state().lock().unwrap();
            let devices: Vec<_> = paired.cfg.devices.iter().map(|d| json!({"id":d.id,"name":d.name,"deviceType":d.device_type,"expires":d.expires,
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
            let device: String = body["deviceName"].as_str().unwrap_or("Device")
                .chars().filter(|c| !c.is_control()).take(64).collect();
            let device_id = device_id(&body);
            // A rejected device never reaches the desktop again, and does not
            // use up an invitation that was meant for another device.
            let blocked = {
                let mut paired = pair_state().lock().unwrap();
                paired.refresh();
                paired.cfg.rejected.iter().any(|r| r.matches(&device_id, &device, &peer))
            };
            if blocked {
                return respond(stream, 403, "Forbidden", &json!({"error":"pairing_blocked"}));
            }
            // Requests are rate-limited by source and globally bounded, including
            // decided requests until expiry, to limit notification spam.
            if state.entries.len() >= 8 || state.entries.iter().any(|p| p.peer == peer) {
                return respond(stream, 429, "Too Many Requests", &json!({"error":"pairing_request_already_pending_or_rate_limited"}));
            }
            let id = random_hex(24);
            let code = format!("{:06}", u32::from_str_radix(&random_hex(4), 16).unwrap() % 1_000_000);
            state.entries.push(Pending { id: id.clone(), code: code.clone(), device, device_type: device_type(&body).into(),
                device_id, peer, expires: now + REQUEST_SECS, decision: None, token: None });
            *invitation = None;
            if !cfg!(test) {
                std::thread::spawn(announce_request);
            }
            respond(stream, 202, "Accepted", &json!({"status":"pending","requestId":id,"code":code,"expiresIn":REQUEST_SECS as u64}));
        }
        ("GET", "/api/v1/pair/state") => {
            let pending: Vec<_> = state.entries.iter().filter(|p| p.decision.is_none()).map(|p|
                json!({"requestId":p.id,"code":p.code,"deviceName":p.device,"deviceType":p.device_type,
                    "address":p.peer,"identified":!p.device_id.is_empty(),
                    "expiresIn":(p.expires - now).max(0.0).ceil() as u64})).collect();
            respond(stream, 200, "OK", &json!({"requests":pending}));
        }
        ("GET", "/api/v1/pair/rejected") => {
            let mut paired = pair_state().lock().unwrap();
            paired.refresh();
            let rejected: Vec<_> = paired.cfg.rejected.iter().map(RejectedDevice::to_json).collect();
            respond(stream, 200, "OK", &json!({"rejected":rejected}));
        }
        ("POST", "/api/v1/pair/rejected/remove") => {
            let mut paired = pair_state().lock().unwrap();
            paired.refresh();
            let id = body["id"].as_str().unwrap_or("");
            let Some(removed) = paired.cfg.rejected.iter().find(|r| r.id == id).cloned() else {
                return respond(stream, 404, "Not Found", &json!({"error":"not_rejected"}));
            };
            let previous = paired.cfg.rejected.clone();
            paired.cfg.rejected.retain(|r| r.id != id);
            if !paired.save() {
                paired.cfg.rejected = previous;
                return respond(stream, 500, "Internal Server Error", &json!({"error":"could_not_save_rejected_list"}));
            }
            // Its denied request would otherwise rate-limit an immediate retry.
            state.entries.retain(|r| !(r.decision == Some(false) && r.peer == removed.address));
            respond(stream, 200, "OK", &json!({"status":"removed"}));
        }
        ("POST", "/api/v1/pair/approve" | "/api/v1/pair/deny") => {
            let approve = path.ends_with("/approve");
            let Some(request) = state.decide(body["requestId"].as_str().unwrap_or(""), approve, now) else {
                return respond(stream, 404, "Not Found", &json!({"error":"request_expired_or_already_decided"}));
            };
            if !approve {
                let mut paired = pair_state().lock().unwrap();
                paired.refresh();
                let remembered = remember_rejection(&mut paired, request, now);
                return respond(stream, 200, "OK", &json!({"status":"denied","remembered":remembered}));
            }
            {
                let token = random_hex(24);
                let mut paired = pair_state().lock().unwrap();
                paired.refresh();
                if paired.cfg.devices.len() >= 64 {
                    request.decision = None;
                    return respond(stream, 409, "Conflict", &json!({"error":"revoke_old_devices_first"}));
                }
                let device_id = random_hex(16);
                paired.cfg.devices.push(PairedDevice { id: device_id.clone(), name: request.device.clone(), device_type: request.device_type.clone(),
                    token_hash: security::digest(token.as_bytes()), expires: now + 90.0 * 86400.0 });
                if !paired.save() {
                    paired.cfg.devices.retain(|d| d.id != device_id);
                    request.decision = None;
                    return respond(stream, 500, "Internal Server Error", &json!({"error":"could_not_save_pairing"}));
                }
                request.token = Some(token);
            }
            respond(stream, 200, "OK", &json!({"status":"approved"}));
        }
        ("POST", "/api/v1/pair/poll") => {
            let Some(request) = state.entries.iter().find(|p| Some(p.id.as_str()) == body["requestId"].as_str()) else {
                return respond(stream, 404, "Not Found", &json!({"error":"request_expired"}));
            };
            let result = match request.decision {
                None => json!({"status":"pending"}),
                Some(false) => json!({"status":"denied"}),
                Some(true) => {
                    let paired = pair_state().lock().unwrap();
                    let expires = request.token.as_ref().and_then(|token| paired.cfg.devices.iter()
                        .find(|d| d.token_hash == security::digest(token.as_bytes())).map(|d| d.expires));
                    json!({"status":"paired","token":request.token,"expires":expires})
                },
            };
            respond(stream, 200, "OK", &result);
        }
        _ => respond(stream, 409, "Conflict", &json!({"error":"explicit_desktop_approval_required"})),
    }
}

/// Owner-only request over the bridge's control socket. Errors are stable
/// codes: the bridge's own, or `bridge_offline` / `bridge_not_responding` /
/// `invalid_bridge_response` when it could not answer.
fn local_request(method: &str, path: &str, body: Value) -> Result<Value, String> {
    let mut stream = std::os::unix::net::UnixStream::connect(security::control_path())
        .map_err(|_| "bridge_offline".to_string())?;
    let not_responding = |_| "bridge_not_responding".to_string();
    stream.set_read_timeout(Some(Duration::from_secs(3))).map_err(not_responding)?;
    stream.set_write_timeout(Some(Duration::from_secs(3))).map_err(not_responding)?;
    let body = body.to_string();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).map_err(not_responding)?;
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(not_responding)?;
    let result: Value = serde_json::from_str(response.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(""))
        .map_err(|_| "invalid_bridge_response".to_string())?;
    if let Some(error) = result["error"].as_str() { return Err(error.to_string()); }
    Ok(result)
}

/// The bridge's config as last written, for lists shown while it is stopped.
/// Read-only: the UI process never initializes or rewrites the credential store.
fn saved_config() -> Option<BridgeConfig> {
    fs::read(state_path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// Requests waiting for a decision, oldest first.
pub fn pairing_requests() -> Result<Vec<Value>, String> {
    local_request("GET", "/api/v1/pair/state", json!({}))
        .map(|v| v["requests"].as_array().cloned().unwrap_or_default())
}

pub fn pending_requests() -> Vec<Value> {
    pairing_requests().unwrap_or_default()
}

/// `Ok(false)` only for a rejection this bridge enforces but could not save:
/// it is forgotten when the bridge restarts.
pub fn decide_request(id: &str, approve: bool) -> Result<bool, String> {
    local_request("POST", if approve {"/api/v1/pair/approve"} else {"/api/v1/pair/deny"}, json!({"requestId":id}))
        .map(|v| v["remembered"] != false)
}

pub fn paired_devices() -> Vec<Value> {
    local_request("GET", "/api/v1/pair/devices", json!({})).ok()
        .and_then(|v| v["devices"].as_array().cloned()).unwrap_or_else(|| {
            // Keep the registered count when the bridge is stopped.
            saved_config().map(|cfg| cfg.devices.iter().map(|d| json!({"id":d.id,"name":d.name,"deviceType":d.device_type,"expires":d.expires,"active":false})).collect())
                .unwrap_or_default()
        })
}
pub fn revoke_device(id: &str) -> Result<(), String> {
    local_request("POST", "/api/v1/pair/revoke", json!({"deviceId":id})).map(|_| ())
}
pub fn pairing_invitation() -> Result<String, String> {
    local_request("POST", "/api/v1/pair/invitation", json!({})).map(|v| v.to_string())
}

/// Rejected devices, oldest first. Still listed while the bridge is stopped.
pub fn rejected_devices() -> Vec<Value> {
    local_request("GET", "/api/v1/pair/rejected", json!({})).ok()
        .and_then(|v| v["rejected"].as_array().cloned()).unwrap_or_else(|| {
            saved_config().map(|cfg| cfg.rejected.iter().map(RejectedDevice::to_json).collect())
                .unwrap_or_default()
        })
}

/// Let a rejected device request pairing again.
pub fn forget_rejected(id: &str) -> Result<(), String> {
    local_request("POST", "/api/v1/pair/rejected/remove", json!({"id":id})).map(|_| ())
}

/// One command to the SUPER DESKTOP daemon, which owns the approval panel.
fn owner_command(command: &str) -> Option<Value> {
    let mut socket = std::os::unix::net::UnixStream::connect(crate::get_socket_path()).ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    socket.set_write_timeout(Some(Duration::from_secs(3))).ok()?;
    socket.write_all(format!("{command}\n").as_bytes()).ok()?;
    let mut reply = String::new();
    socket.take(64 * 1024).read_to_string(&mut reply).ok()?;
    serde_json::from_str(reply.trim()).ok()
}

/// Put a new request in front of the owner. A visible overlay opens its
/// approval panel straight away; otherwise a notification does, when clicked.
/// The text is constant: device names are untrusted.
fn announce_request() {
    if owner_command("pairing-request").is_some_and(|reply| reply["presented"] == true) {
        return;
    }
    let exe = bridge_exe().ok();
    let mut notify = Command::new("notify-send");
    notify.args([
        "--app-name=SUPER DESKTOP",
        "--icon=network-workgroup",
        "--expire-time=120000",
        "--action=default=Review request",
    ]);
    // Omarchy's shell runs this argv on click, even from notification history
    // after the sender is gone. Other servers use the libnotify action above.
    if let Some(click) = exe.as_ref().and_then(|exe| {
        serde_json::to_string(&[exe.to_string_lossy().as_ref(), "pairing-review"]).ok()
    }) {
        notify.arg(format!("--hint=string:omarchy-exec-argv:{click}"));
    }
    let output = notify
        .args([
            "SUPER DESKTOP: connection request",
            "A device wants to connect to this PC. Click to review it, then approve or reject.",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let clicked = output.is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "default");
    if let (true, Some(exe)) = (clicked, exe) {
        // Through the CLI, which also starts the daemon when it is not running.
        let _ = Command::new(exe).arg("pairing-review")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending() -> Pending { Pending { id:"secret".into(), code:"123456".into(), device:"phone".into(), device_type:"android".into(), device_id:String::new(), peer:"ip".into(), expires:120.0, decision:None, token:None } }
    fn rejected(name: &str, device_id: &str, address: &str) -> RejectedDevice {
        RejectedDevice { id: random_hex(8), name: name.into(), device_type: "pc".into(), device_id: device_id.into(),
            address: address.into(), rejected_at: 1.0 }
    }
    #[test] fn rejected_devices_are_recognised_by_installation_id_or_name_and_address() {
        let pc = "ab".repeat(16);
        assert_eq!(device_id(&json!({"deviceId": "AB".repeat(16)})), pc);
        for bad in [json!({}), json!({"deviceId": "ab"}), json!({"deviceId": "zz".repeat(16)}), json!({"deviceId": 7})] {
            assert_eq!(device_id(&bad), "");
        }
        let entry = rejected("laptop", &pc, "10.0.0.5");
        assert!(entry.matches(&pc, "renamed", "10.0.0.9"), "a renamed PC keeps its installation ID");
        assert!(entry.matches("", "laptop", "10.0.0.5"), "an older client is recognised by name and address");
        assert!(!entry.matches("", "laptop", "10.0.0.9"));
        assert!(!entry.matches(&"cd".repeat(16), "phone", "10.0.0.5"));
        let phone = rejected("phone", "", "10.0.0.7");
        assert!(!phone.matches("", "phone", ""), "an unknown address never matches");
        assert!(!phone.matches("", "", "10.0.0.7"));
        // An old config without the list, or entries from an older build, still load.
        let old: BridgeConfig = serde_json::from_value(json!({"bridge_id":"x","devices":[]})).unwrap();
        assert!(old.rejected.is_empty());
        let minimal: RejectedDevice = serde_json::from_value(json!({"id":"1","name":"n","rejected_at":2.0})).unwrap();
        assert!(minimal.device_id.is_empty() && minimal.address.is_empty());
    }
    #[test] fn rejecting_a_device_again_refreshes_its_entry_and_the_list_stays_bounded() {
        let mut list = vec![rejected("laptop", "", "10.0.0.5")];
        let mut again = rejected("laptop", "", "10.0.0.5");
        again.rejected_at = 9.0;
        add_rejection(&mut list, again);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].rejected_at, 9.0);
        for n in 0..MAX_REJECTED + 5 {
            add_rejection(&mut list, rejected(&format!("device {n}"), "", "10.0.0.6"));
        }
        assert_eq!(list.len(), MAX_REJECTED);
        assert_eq!(list.last().unwrap().name, format!("device {}", MAX_REJECTED + 4));
        assert!(list.iter().all(|r| r.name != "laptop"), "the oldest entries go first");
    }
    #[test]
    fn old_pairings_keep_access_without_inventing_a_device_type() {
        let old = json!({"id":"id", "name":"Samsung PC", "token_hash":"hash", "expires":123.0});
        let device: PairedDevice = serde_json::from_value(old).unwrap();
        assert!(device.device_type.is_empty());
        assert_eq!(device.token_hash, "hash");
        let mut device = device;
        device.device_type = device_type(&json!({"deviceType":"android"})).into();
        let restored: PairedDevice = serde_json::from_value(serde_json::to_value(device).unwrap()).unwrap();
        assert_eq!(restored.device_type, "android");
        assert_eq!(device_type(&json!({"deviceType":"unexpected"})), "unknown");
        assert_eq!(device_type(&json!({"deviceName":"Android phone"})), "unknown");
    }
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
            let request = Request { method:method.into(), path:path.into(), body:body.to_string(), headers:HashMap::new(), query:String::new(), keep_alive:false, _upload_slot:None };
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
        let denied = call("/api/v1/pair/deny", "POST", json!({"requestId":id}), true);
        assert_eq!((denied.0, denied.1["remembered"].clone()), (200, json!(true)));
        assert_eq!(call("/api/v1/pair/poll", "POST", json!({"requestId":id}), false).1["status"], "denied");
        assert_eq!(call("/api/v1/pair/approve", "POST", json!({"requestId":id}), true).0, 404);

        // The rejected device is remembered, owner-only, and refused from now on
        // without a prompt, while the invitation stays usable.
        assert_eq!(call("/api/v1/pair/rejected", "GET", json!({}), false).0, 403);
        let listed = call("/api/v1/pair/rejected", "GET", json!({}), true).1["rejected"].clone();
        let entry = listed.as_array().unwrap().iter().find(|r| r["name"] == "test").unwrap().clone();
        assert_eq!((entry["address"].as_str(), entry["identified"].as_bool()), (Some("127.0.0.1"), Some(false)));
        let invite = call("/api/v1/pair/invitation", "POST", json!({}), true).1;
        let blocked = call("/api/v1/pair", "POST", json!({"deviceName":"test","secret":invite["secret"]}), false);
        assert_eq!((blocked.0, blocked.1["error"].as_str()), (403, Some("pairing_blocked")));
        assert!(call("/api/v1/pair/state", "GET", json!({}), true).1["requests"].as_array().unwrap().is_empty());

        // Removing it lets the same device ask again at once, with that invitation.
        assert_eq!(call("/api/v1/pair/rejected/remove", "POST", json!({"id":entry["id"]}), false).0, 403);
        assert_eq!(call("/api/v1/pair/rejected/remove", "POST", json!({"id":"missing"}), true).0, 404);
        assert_eq!(call("/api/v1/pair/rejected/remove", "POST", json!({"id":entry["id"]}), true).0, 200);
        let pc = "ab".repeat(16);
        let (status, retry) = call("/api/v1/pair", "POST", json!({"deviceName":"test","deviceType":"pc","deviceId":pc,"secret":invite["secret"]}), false);
        assert_eq!(status, 202, "{retry}");
        let waiting = call("/api/v1/pair/state", "GET", json!({}), true).1["requests"][0].clone();
        assert_eq!((waiting["deviceType"].as_str(), waiting["identified"].as_bool()), (Some("pc"), Some(true)));
        assert!((1..=120).contains(&waiting["expiresIn"].as_u64().unwrap()));
        assert_eq!(call("/api/v1/pair/deny", "POST", json!({"requestId":retry["requestId"]}), true).0, 200);

        // A PC that sends its installation ID stays rejected under a new name.
        let invite = call("/api/v1/pair/invitation", "POST", json!({}), true).1;
        let renamed = call("/api/v1/pair", "POST", json!({"deviceName":"renamed","deviceId":"AB".repeat(16),"secret":invite["secret"]}), false);
        assert_eq!((renamed.0, renamed.1["error"].as_str()), (403, Some("pairing_blocked")));

        let listed = call("/api/v1/pair/rejected", "GET", json!({}), true).1["rejected"].clone();
        for entry in listed.as_array().unwrap() {
            assert_eq!(call("/api/v1/pair/rejected/remove", "POST", json!({"id":entry["id"]}), true).0, 200);
        }
        *INVITATION.lock().unwrap() = None;
        requests().lock().unwrap().entries.retain(|p| Some(p.id.as_str()) != id.as_str() && Some(p.id.as_str()) != retry["requestId"].as_str());
    }
}
