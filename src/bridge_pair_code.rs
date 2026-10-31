//! Pairing a TV by a code it shows, typed here by the owner. A TV has no
//! camera for the QR invitation, so it asks for one while showing a code,
//! and gets the invitation sealed with a key only that code yields: a device
//! in the middle can neither read it nor forge one naming its own certificate.
//! Requests are refused unless the owner has "Pair a TV" open, never notify,
//! and are chosen by typing the code, so nearby devices cannot spam the owner.
use super::*;
use ring::{hmac, pbkdf2};
use std::num::NonZeroU32;

/// How long "Pair a TV" accepts requests after the owner opens or renews it.
const WINDOW_SECS: f64 = 120.0;
/// Key stretching for the 60-bit code; about a second on a slow TV.
const ROUNDS: u32 = 100_000;
const NONCE_LEN: usize = 16;
const CODE_LEN: usize = 12;
const ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

struct CodeRequest {
    id: String,
    nonce: Vec<u8>,
    commit: String,
    device: String,
    device_type: String,
    peer: String,
    expires: f64,
    /// The invitation sealed for this TV, once its code was typed: (box, mac).
    sealed: Option<(String, String)>,
}

#[derive(Default)]
struct CodeState {
    window_until: f64,
    entries: Vec<CodeRequest>,
}

fn state() -> &'static Mutex<CodeState> {
    static STATE: OnceLock<Mutex<CodeState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(CodeState::default()))
}

/// Routes of this module.
pub(super) fn handles(path: &str) -> bool {
    path == "/api/v1/pair/code" || path.starts_with("/api/v1/pair/code/")
}

/// The two a TV calls over the network; the rest are the owner's.
pub(super) fn remote(path: &str) -> bool {
    matches!(path, "/api/v1/pair/code" | "/api/v1/pair/code/poll")
}

/// The code as typed: case, spaces and dashes ignored, `O` read as `0` and
/// `I`/`L` as `1`. `None` unless it is a whole code.
pub(crate) fn normalize_code(typed: &str) -> Option<String> {
    let mut code = String::new();
    for c in typed.chars() {
        match c.to_ascii_uppercase() {
            ' ' | '-' => {}
            'O' => code.push('0'),
            'I' | 'L' => code.push('1'),
            c if ALPHABET.contains(c) => code.push(c),
            _ => return None,
        }
    }
    (code.len() == CODE_LEN).then_some(code)
}

fn derive_key(code: &str, nonce: &[u8]) -> [u8; 32] {
    let mut salt = b"sd-tv-pair-v1".to_vec();
    salt.extend_from_slice(nonce);
    let mut key = [0u8; 32];
    pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, NonZeroU32::new(ROUNDS).unwrap(), &salt, code.as_bytes(), &mut key);
    key
}

fn mac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut context = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, key));
    for part in parts {
        context.update(part);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(context.sign().as_ref());
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str, len: usize) -> Option<Vec<u8>> {
    crate::control::is_hex(text, len * 2).then(|| {
        (0..len).map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap()).collect()
    })
}

fn commitment(key: &[u8]) -> String {
    hex(&mac(key, &[b"commit"]))
}

/// Encrypt-then-MAC with HMAC-SHA256 as the keystream, bound to the request.
fn seal(key: &[u8], request_id: &str, plaintext: &[u8]) -> (String, String) {
    use base64::Engine;
    let enc = mac(key, &[b"enc"]);
    let mut sealed = plaintext.to_vec();
    for (i, chunk) in sealed.chunks_mut(32).enumerate() {
        let block = mac(&enc, &[request_id.as_bytes(), &(i as u32).to_be_bytes()]);
        chunk.iter_mut().zip(block).for_each(|(b, k)| *b ^= k);
    }
    let tag = mac(&mac(key, &[b"mac"]), &[request_id.as_bytes(), &sealed]);
    (base64::engine::general_purpose::STANDARD.encode(&sealed), hex(&tag))
}

pub(super) fn handle(stream: &mut Connection, method: &str, path: &str, body: &Value, now: f64) {
    let mut codes = state().lock().unwrap();
    codes.entries.retain(|r| r.expires > now);
    match (method, path) {
        ("POST", "/api/v1/pair/code/open") => {
            codes.window_until = now + WINDOW_SECS;
            reply_json(stream, 200, &json!({"expiresIn": WINDOW_SECS as u64}));
        }
        ("POST", "/api/v1/pair/code/close") => {
            codes.window_until = 0.0;
            reply_json(stream, 200, &json!({"status":"closed"}));
        }
        ("GET", "/api/v1/pair/code/state") => {
            let requests: Vec<_> = codes.entries.iter().map(|r| json!({"requestId":r.id,"deviceName":r.device,
                "deviceType":r.device_type,"address":r.peer,"ready":r.sealed.is_some(),
                "expiresIn":(r.expires - now).max(0.0).ceil() as u64})).collect();
            reply_json(stream, 200, &json!({"open":codes.window_until > now,
                "expiresIn":(codes.window_until - now).max(0.0).ceil() as u64,"requests":requests}));
        }
        ("POST", "/api/v1/pair/code/confirm") => {
            let Some(code) = body["code"].as_str().and_then(normalize_code) else {
                return reply_error(stream, 400, Envelope::Plain, "invalid_code");
            };
            let Some((request, key)) = codes.entries.iter_mut().filter(|r| r.sealed.is_none()).find_map(|r| {
                let key = derive_key(&code, &r.nonce);
                security::equal(&commitment(&key), &r.commit).then_some((r, key))
            }) else {
                return reply_error(stream, 404, Envelope::Plain, "no_matching_device");
            };
            let secret = random_hex(24);
            let invitation = json!({"v":3,"host":lan_ip(),"port":BRIDGE_PORT,
                "fingerprint":security::fingerprint(),"secret":secret,"expiresIn":300});
            *INVITATION.lock().unwrap() = Some(Invitation { secret, expires: now + 300.0, preapproved: true });
            request.sealed = Some(seal(&key, &request.id, invitation.to_string().as_bytes()));
            reply_json(stream, 200, &json!({"status":"ready","deviceName":request.device}));
        }
        ("POST", "/api/v1/pair/code") => {
            if codes.window_until <= now {
                return reply_error(stream, 403, Envelope::Plain, "pairing_code_mode_off");
            }
            let nonce = body["nonce"].as_str().and_then(|n| from_hex(n, NONCE_LEN));
            let commit = body["commit"].as_str().filter(|c| crate::control::is_hex(c, 64)).map(str::to_ascii_lowercase);
            let (Some(nonce), Some(commit)) = (nonce, commit) else {
                return reply_error(stream, 400, Envelope::Plain, "invalid_code_request");
            };
            let peer = stream.peer_addr().map(|p| p.ip().to_string()).unwrap_or_default();
            let device: String = body["deviceName"].as_str().unwrap_or("Device")
                .chars().filter(|c| !c.is_control()).take(64).collect();
            let device_id = device_id(body);
            let blocked = {
                let mut paired = pair_state().lock().unwrap();
                paired.refresh();
                paired.cfg.rejected.iter().any(|r| r.matches(&device_id, &device, &peer))
            };
            if blocked {
                return reply_error(stream, 403, Envelope::Plain, "pairing_blocked");
            }
            if codes.entries.len() >= 8 || codes.entries.iter().any(|r| r.peer == peer) {
                return reply_error(stream, 429, Envelope::Plain, "pairing_request_already_pending_or_rate_limited");
            }
            let id = random_hex(24);
            codes.entries.push(CodeRequest { id: id.clone(), nonce, commit, device, device_type: device_type(body).into(),
                peer, expires: now + REQUEST_SECS, sealed: None });
            reply_json(stream, 202, &json!({"status":"pending","requestId":id,"expiresIn":REQUEST_SECS as u64}));
        }
        ("POST", "/api/v1/pair/code/poll") => {
            let Some(request) = codes.entries.iter().find(|r| Some(r.id.as_str()) == body["requestId"].as_str()) else {
                return reply_error(stream, 404, Envelope::Plain, "request_expired");
            };
            let result = match &request.sealed {
                None => json!({"status":"pending"}),
                Some((sealed, tag)) => json!({"status":"ready","box":sealed,"mac":tag}),
            };
            reply_json(stream, 200, &result);
        }
        _ => reply_error(stream, 409, Envelope::Plain, "explicit_desktop_approval_required"),
    }
}

/// The TV's side, for tests: the commitment for a code, and opening a box.
#[cfg(test)]
pub(super) mod tv {
    use super::*;

    pub fn request(code: &str, nonce: &[u8]) -> ([u8; 32], String) {
        let key = derive_key(code, nonce);
        let commit = commitment(&key);
        (key, commit)
    }

    pub fn open(key: &[u8], request_id: &str, sealed: &str, tag: &str) -> Option<Value> {
        use base64::Engine;
        let mut data = base64::engine::general_purpose::STANDARD.decode(sealed).ok()?;
        let expected = hex(&mac(&mac(key, &[b"mac"]), &[request_id.as_bytes(), &data]));
        if !security::equal(&expected, tag) {
            return None;
        }
        let enc = mac(key, &[b"enc"]);
        for (i, chunk) in data.chunks_mut(32).enumerate() {
            let block = mac(&enc, &[request_id.as_bytes(), &(i as u32).to_be_bytes()]);
            chunk.iter_mut().zip(block).for_each(|(b, k)| *b ^= k);
        }
        serde_json::from_slice(&data).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_codes_forgive_case_dashes_and_lookalikes() {
        assert_eq!(normalize_code("k7qm-4xpa-9c0z").as_deref(), Some("K7QM4XPA9C0Z"));
        assert_eq!(normalize_code(" K7QM 4XPA 9CoZ ").as_deref(), Some("K7QM4XPA9C0Z"));
        assert_eq!(normalize_code("ilil-0000-0000").as_deref(), Some("111100000000"));
        assert_eq!(normalize_code("K7QM-4XPA-9C0"), None, "too short");
        assert_eq!(normalize_code("K7QM-4XPA-9C0ZZ"), None, "too long");
        assert_eq!(normalize_code("U7QM-4XPA-9C0Z"), None, "U is not in the alphabet");
    }

    /// Known-answer values the Android app checks too, so both sides derive
    /// the same key, commitment and box.
    #[test]
    fn key_commitment_and_box_match_the_android_vectors() {
        let nonce = [7u8; NONCE_LEN];
        let (key, commit) = tv::request("K7QM4XPA9C0Z", &nonce);
        assert_eq!(hex(&key), "10b0a0427cc1e22dce5ee6b5c56f461d9903f6b2e25a260240a010eddf45a277");
        assert_eq!(commit, "aae04a0ad5f39c92dea2eb3633957c7c29ae30fd3c4ff3159fd85bcd5aadac11");
        let (sealed, tag) = seal(&key, "req", br#"{"v":3}"#);
        assert_eq!(sealed, "Wqmu9eitMQ==");
        assert_eq!(tag, "e314fcdc9d8557ffe051e83cce54163a441c94cba0d17f96fdfea160b104c0e2");
        assert_eq!(tv::open(&key, "req", &sealed, &tag), Some(json!({"v":3})));
    }

    #[test]
    fn a_box_opens_only_with_the_right_code_and_request() {
        let nonce = [1u8; NONCE_LEN];
        let (key, _) = tv::request("K7QM4XPA9C0Z", &nonce);
        let (other, _) = tv::request("K7QM4XPA9C01", &nonce);
        let (sealed, tag) = seal(&key, "req", br#"{"secret":"s"}"#);
        assert!(tv::open(&key, "req", &sealed, &tag).is_some());
        assert!(tv::open(&other, "req", &sealed, &tag).is_none(), "another code");
        assert!(tv::open(&key, "other", &sealed, &tag).is_none(), "another request");
        let mut flipped = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &sealed).unwrap();
        flipped[0] ^= 1;
        let flipped = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, flipped);
        assert!(tv::open(&key, "req", &flipped, &tag).is_none(), "a changed box");
    }
}
