//! Blocking, certificate-pinned desktop bridge client. Use off the GTK thread.
//! No cookies, environment proxies, redirects, credential URLs or automatic
//! request retries. Error values never contain response bodies or credentials.
use base64::Engine;
use reqwest::blocking::Client;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MAX_INVITATION: usize = 8192;
const MAX_RESPONSE: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerError(pub &'static str);
impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for PeerError {}
pub type Result<T> = std::result::Result<T, PeerError>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    pub fn new(host: &str, port: u16) -> Result<Self> {
        if port == 0 || host.is_empty() || host.len() > 253 {
            return Err(PeerError("invalid_peer_address"));
        }
        let host = match host.parse::<std::net::IpAddr>() {
            Ok(ip) if !ip.is_unspecified() && !ip.is_multicast() => ip.to_string(),
            Ok(_) => return Err(PeerError("invalid_peer_address")),
            Err(_) => {
                if !host.is_ascii()
                    || !host.split('.').all(|label| {
                        !label.is_empty()
                            && label.len() <= 63
                            && !label.starts_with('-')
                            && !label.ends_with('-')
                            && label
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                {
                    return Err(PeerError("invalid_peer_address"));
                }
                host.to_ascii_lowercase()
            }
        };
        Ok(Self { host, port })
    }

    fn url(&self, path: &str) -> Result<reqwest::Url> {
        let checked = Self::new(&self.host, self.port)?;
        let authority = if checked.host.contains(':') {
            format!("[{}]", checked.host)
        } else {
            checked.host
        };
        reqwest::Url::parse(&format!("https://{authority}:{}{path}", checked.port))
            .map_err(|_| PeerError("invalid_peer_address"))
    }
}

// Deliberately no Debug/Serialize: invitation secrets must not reach logs.
pub struct Invitation {
    pub endpoint: Endpoint,
    pub fingerprint: String,
    secret: String,
}

impl Invitation {
    pub fn parse(input: &str) -> Result<Self> {
        if input.len() > MAX_INVITATION {
            return Err(PeerError("invalid_pairing_invitation"));
        }
        let input = input.trim();
        let bytes = if let Some(data) = input.strip_prefix("superdesktop://pair?data=") {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(data)
                .map_err(|_| PeerError("invalid_pairing_invitation"))?
        } else if input.starts_with('{') {
            input.as_bytes().to_vec()
        } else {
            return Err(PeerError("invalid_pairing_invitation"));
        };
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            v: u32,
            host: String,
            port: u16,
            fingerprint: String,
            secret: String,
            expires_in: u64,
        }
        let wire: Wire =
            serde_json::from_slice(&bytes).map_err(|_| PeerError("invalid_pairing_invitation"))?;
        if wire.v != 3 {
            return Err(PeerError("unsupported_pairing_protocol"));
        }
        if !(1..=300).contains(&wire.expires_in) || !hex(&wire.secret, 48) {
            return Err(PeerError("invalid_pairing_invitation"));
        }
        parse_pin(&wire.fingerprint)?;
        Ok(Self {
            endpoint: Endpoint::new(&wire.host, wire.port)?,
            fingerprint: wire.fingerprint.to_ascii_lowercase(),
            secret: wire.secret,
        })
    }
}

/// Only serialized by the private peer store. Use summary() for UI/CLI output.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub machine_id: String,
    pub label: String,
    pub endpoint: Endpoint,
    pub fingerprint: String,
    token: String,
    pub expires_at: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerSummary {
    pub machine_id: String,
    pub label: String,
    pub endpoint: Endpoint,
    pub expires_at: Option<f64>,
    pub expired: bool,
}

impl Peer {
    pub fn validate(&self) -> Result<()> {
        Endpoint::new(&self.endpoint.host, self.endpoint.port)?;
        parse_pin(&self.fingerprint)?;
        if !hex(&self.machine_id, 32)
            || !hex(&self.token, 48)
            || self.label != label(&self.label)
            || self.label.is_empty()
            || self.expires_at.is_some_and(|t| !t.is_finite() || t <= 0.0)
        {
            return Err(PeerError("invalid_peer_record"));
        }
        Ok(())
    }

    pub fn summary(&self) -> PeerSummary {
        PeerSummary {
            machine_id: self.machine_id.clone(),
            label: self.label.clone(),
            endpoint: self.endpoint.clone(),
            expires_at: self.expires_at,
            expired: self.expires_at.is_some_and(|t| t <= now()),
        }
    }
}

pub fn label(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect::<String>()
        .trim()
        .to_string()
}
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parse_pin(value: &str) -> Result<[u8; 32]> {
    if !hex(value, 64) {
        return Err(PeerError("invalid_certificate_pin"));
    }
    let mut bytes = [0; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[2 * i..2 * i + 2], 16).unwrap();
    }
    Ok(bytes)
}

#[derive(Debug)]
struct PinVerifier {
    pin: [u8; 32],
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _chain: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if Sha256::digest(cert.as_ref()).as_slice() != self.pin {
            return Err(rustls::Error::General(
                "desktop certificate pin mismatch".into(),
            ));
        }
        // The verified invitation pins the exact certificate instead of a CA
        // or DNS name. Handshake signatures are still verified below.
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, signature, &self.algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, signature, &self.algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Certificate-pinned TLS client configuration.
///
/// The verified invitation's pin replaces CA and DNS-name trust, so the same
/// config is used for HTTPS requests and for the WebSocket terminal stream.
/// Handshake signatures are still verified against the pinned certificate.
fn pinned_tls(fingerprint: &str) -> Result<Arc<rustls::ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinVerifier {
        pin: parse_pin(fingerprint)?,
        algorithms: provider.signature_verification_algorithms,
    });
    Ok(Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| PeerError("tls_configuration_failed"))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth(),
    ))
}

/// Blocking pinned connection to a peer's bridge for a streaming protocol.
///
/// No proxies, no redirects, no plaintext fallback and no certificate-name
/// trust: only the invitation's pin. The TLS handshake is completed here so a
/// pin mismatch fails before use, and the returned stream keeps a short read
/// timeout so a streaming reader can poll for cancellation or disconnect.
pub type PinnedStream = rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>;

/// Read timeout for streaming protocols: how long a reader may block before it
/// can notice that the viewer no longer wants this stream.
pub const STREAM_READ_TIMEOUT: Duration = Duration::from_millis(250);

pub fn pinned_stream(peer: &Peer) -> Result<PinnedStream> {
    peer.validate()?;
    if peer.summary().expired {
        return Err(PeerError("peer_revoked_or_expired"));
    }
    let config = pinned_tls(&peer.fingerprint)?;
    let name = rustls::pki_types::ServerName::try_from(peer.endpoint.host.clone())
        .map_err(|_| PeerError("invalid_peer_address"))?;
    let connection = rustls::ClientConnection::new(config, name)
        .map_err(|_| PeerError("tls_configuration_failed"))?;
    let addresses = (peer.endpoint.host.as_str(), peer.endpoint.port)
        .to_socket_addrs()
        .map_err(|_| PeerError("connection_failed_or_pin_mismatch"))?
        .collect::<Vec<_>>();
    let mut socket = None;
    for address in addresses {
        if let Ok(connected) = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(5))
        {
            socket = Some(connected);
            break;
        }
    }
    let socket = socket.ok_or(PeerError("connection_failed_or_pin_mismatch"))?;
    let _ = socket.set_nodelay(true);
    let _ = socket.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
    let mut stream = rustls::StreamOwned::new(connection, socket);
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|_| PeerError("connection_failed_or_pin_mismatch"))?;
    }
    // A streaming reader polls for cancellation, so it must never block on a
    // host that has nothing to say. The handshake above keeps the longer
    // timeout: a laggy link must not fail the connection setup.
    let _ = stream
        .sock
        .set_read_timeout(Some(STREAM_READ_TIMEOUT));
    Ok(stream)
}

struct PinnedClient {
    http: Client,
    endpoint: Endpoint,
}
impl PinnedClient {
    fn new(endpoint: Endpoint, fingerprint: &str) -> Result<Self> {
        let tls = pinned_tls(fingerprint)?;
        // reqwest 0.13 type-erases the backend (`impl Any`) and downcasts to
        // `Option<ClientConfig>` by value; an `Arc` lands in `Unknown` and
        // fails `build` with "unknown TLS backend". The streaming side keeps
        // the `Arc` for `ClientConnection::new`.
        let http = Client::builder()
            .tls_backend_preconfigured(Arc::unwrap_or_clone(tls))
            .http1_only()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(8))
            .http1_max_headers(64)
            .pool_max_idle_per_host(0)
            .referer(false)
            .build()
            .map_err(|_| PeerError("tls_configuration_failed"))?;
        Ok(Self { http, endpoint })
    }

    fn request<T: DeserializeOwned>(
        &self,
        path: &'static str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> Result<T> {
        let url = self.endpoint.url(path)?;
        let mut request = match body {
            Some(body) => self.http.post(url).json(&body),
            None => self.http.get(url),
        };
        if let Some(token) = token {
            let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| PeerError("invalid_peer_record"))?;
            auth.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, auth);
        }
        let response = request
            .send()
            .map_err(|_| PeerError("connection_failed_or_pin_mismatch"))?;
        match response.status().as_u16() {
            200..=299 => {}
            300..=399 => return Err(PeerError("redirect_rejected")),
            401 => return Err(PeerError("peer_revoked_or_expired")),
            403 => return Err(PeerError("invitation_rejected")),
            404 => return Err(PeerError("peer_endpoint_unavailable")),
            429 => return Err(PeerError("pairing_rate_limited")),
            503 => return Err(PeerError("remote_desktop_unavailable")),
            _ => return Err(PeerError("peer_request_rejected")),
        }
        if response.content_length().is_some_and(|n| n > MAX_RESPONSE) {
            return Err(PeerError("peer_response_too_large"));
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| PeerError("peer_response_incomplete"))?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err(PeerError("peer_response_too_large"));
        }
        serde_json::from_slice(&bytes).map_err(|_| PeerError("invalid_peer_response"))
    }

    fn identity(&self) -> Result<Identity> {
        let identity: Identity = self.request("/api/v1/ping", None, None)?;
        if identity.protocol_version != 3 {
            return Err(PeerError("unsupported_pairing_protocol"));
        }
        if !hex(&identity.bridge_id, 32) {
            return Err(PeerError("invalid_peer_response"));
        }
        Ok(identity)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    bridge_id: String,
    hostname: String,
    protocol_version: u32,
}

/// Pending request capability stays in memory; it is not a registered peer yet.
pub struct Pairing {
    client: PinnedClient,
    request_id: String,
    pub code: String,
    machine_id: String,
    label: String,
    fingerprint: String,
    deadline: Instant,
}

impl Pairing {
    pub fn begin(
        invitation: Invitation,
        local_id: Option<&str>,
        local_name: &str,
        alias: Option<&str>,
    ) -> Result<Self> {
        let client = PinnedClient::new(invitation.endpoint, &invitation.fingerprint)?;
        let identity = client.identity()?;
        if local_id == Some(identity.bridge_id.as_str()) {
            return Err(PeerError("cannot_pair_this_pc_with_itself"));
        }
        let remote_label = label(alias.unwrap_or(&identity.hostname));
        if remote_label.is_empty() {
            return Err(PeerError("invalid_peer_label"));
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Pending {
            request_id: String,
            code: String,
            expires_in: u64,
        }
        let pending: Pending = client
            .request(
                "/api/v1/pair",
                Some(json!({"secret":invitation.secret,
            "deviceName": label(local_name)})),
                None,
            )
            .map_err(|error| {
                if error.0 == "connection_failed_or_pin_mismatch"
                    || error.0 == "peer_response_incomplete"
                {
                    PeerError("pairing_outcome_uncertain_check_host")
                } else {
                    error
                }
            })?;
        if !hex(&pending.request_id, 48)
            || pending.code.len() != 6
            || !pending.code.bytes().all(|b| b.is_ascii_digit())
            || !(1..=120).contains(&pending.expires_in)
        {
            return Err(PeerError("invalid_peer_response"));
        }
        Ok(Self {
            client,
            request_id: pending.request_id,
            code: pending.code,
            machine_id: identity.bridge_id,
            label: remote_label,
            fingerprint: invitation.fingerprint,
            deadline: Instant::now() + Duration::from_secs(pending.expires_in),
        })
    }

    pub fn poll(&self) -> Result<Option<Peer>> {
        if Instant::now() >= self.deadline {
            return Err(PeerError("pairing_request_expired"));
        }
        #[derive(Deserialize)]
        struct Poll {
            status: String,
            token: Option<String>,
            expires: Option<f64>,
        }
        let response: Poll = self.client.request(
            "/api/v1/pair/poll",
            Some(json!({"requestId": self.request_id})),
            None,
        )?;
        match response.status.as_str() {
            "pending" => Ok(None),
            "denied" => Err(PeerError("pairing_denied")),
            "paired" => {
                let identity = self.client.identity()?;
                if identity.bridge_id != self.machine_id {
                    return Err(PeerError("peer_identity_changed"));
                }
                let peer = Peer {
                    machine_id: self.machine_id.clone(),
                    label: self.label.clone(),
                    endpoint: self.client.endpoint.clone(),
                    fingerprint: self.fingerprint.clone(),
                    token: response.token.ok_or(PeerError("invalid_peer_response"))?,
                    expires_at: response.expires,
                };
                peer.validate()?;
                Ok(Some(peer))
            }
            _ => Err(PeerError("invalid_peer_response")),
        }
    }
}

/// Verified identity and capability document for a peer.
///
/// Checks the pinned certificate, the persistent bridge identity and a
/// supported desktop API version before anything else is attempted.
pub fn capabilities(peer: &Peer) -> Result<crate::desktop_protocol::Capabilities> {
    peer.validate()?;
    if peer.summary().expired {
        return Err(PeerError("peer_revoked_or_expired"));
    }
    let client = PinnedClient::new(peer.endpoint.clone(), &peer.fingerprint)?;
    if client.identity()?.bridge_id != peer.machine_id {
        return Err(PeerError("peer_identity_changed"));
    }
    let capabilities: crate::desktop_protocol::Capabilities =
        client.request("/api/v1/desktop/capabilities", None, Some(&peer.token))?;
    if capabilities.machine_id != peer.machine_id {
        return Err(PeerError("peer_identity_changed"));
    }
    if capabilities.desktop_api_version != crate::desktop_protocol::DESKTOP_API_VERSION {
        return Err(PeerError("update_remote_super_desktop"));
    }
    Ok(capabilities)
}

/// One verified round of peer discovery: identity, capabilities and workspace.
///
/// The capability document is returned so a caller can decide what to enable
/// without a second negotiation, and so a host that only supports layout
/// snapshots is never mistaken for one that streams terminals.
pub fn verified_workspace(
    peer: &Peer,
) -> Result<(
    crate::desktop_protocol::Capabilities,
    crate::desktop_protocol::WorkspaceSnapshot,
)> {
    let capabilities = capabilities(peer)?;
    if !capabilities
        .capabilities
        .iter()
        .any(|c| c == crate::desktop_protocol::WORKSPACE_SNAPSHOT)
    {
        return Err(PeerError("update_remote_super_desktop"));
    }
    let client = PinnedClient::new(peer.endpoint.clone(), &peer.fingerprint)?;
    let workspace: crate::desktop_protocol::WorkspaceSnapshot =
        client.request("/api/v1/desktop/workspace", None, Some(&peer.token))?;
    if workspace.machine_id != peer.machine_id {
        return Err(PeerError("peer_identity_changed"));
    }
    Ok((capabilities, workspace))
}

pub fn workspace(peer: &Peer) -> Result<crate::desktop_protocol::WorkspaceSnapshot> {
    verified_workspace(peer).map(|(_, workspace)| workspace)
}

/// Upgrade a pinned, authenticated WebSocket to one of the host's desktop
/// endpoints. Identity and the required capability are checked first, and the
/// credential is only ever placed in the request header.
pub fn desktop_socket(
    peer: &Peer,
    path: &str,
    required: &str,
) -> Result<tungstenite::WebSocket<PinnedStream>> {
    if !path.starts_with("/api/v1/desktop/")
        || path.len() > 256
        || !path.bytes().all(|b| b.is_ascii_graphic() && b != b'\\')
    {
        return Err(PeerError("invalid_peer_response"));
    }
    if !capabilities(peer)?
        .capabilities
        .iter()
        .any(|c| c == required)
    {
        return Err(PeerError("update_remote_super_desktop"));
    }
    let stream = pinned_stream(peer)?;
    let authority = if peer.endpoint.host.contains(':') {
        format!("[{}]:{}", peer.endpoint.host, peer.endpoint.port)
    } else {
        format!("{}:{}", peer.endpoint.host, peer.endpoint.port)
    };
    let uri: tungstenite::http::Uri = format!("wss://{authority}{path}")
        .parse()
        .map_err(|_| PeerError("invalid_peer_address"))?;
    let mut request = tungstenite::client::IntoClientRequest::into_client_request(uri)
        .map_err(|_| PeerError("invalid_peer_address"))?;
    let mut authorization = tungstenite::http::HeaderValue::from_str(&format!("Bearer {}", peer.token))
        .map_err(|_| PeerError("invalid_peer_record"))?;
    authorization.set_sensitive(true);
    request
        .headers_mut()
        .insert(tungstenite::http::header::AUTHORIZATION, authorization);

    let mut config = tungstenite::protocol::WebSocketConfig::default()
        .read_buffer_size(crate::desktop_protocol::ATTACH_MAX_CHUNK * 2)
        // Write every frame immediately: terminal output is latency-sensitive
        // and each chunk is already bounded.
        .write_buffer_size(0);
    config.max_frame_size = Some(crate::desktop_protocol::ATTACH_MAX_CHUNK);
    config.max_message_size = Some(crate::desktop_protocol::ATTACH_MAX_CHUNK * 2);
    match tungstenite::client::client_with_config(request, stream, Some(config)) {
        Ok((socket, _response)) => Ok(socket),
        Err(tungstenite::HandshakeError::Failure(error)) => Err(socket_failure(error)),
        Err(tungstenite::HandshakeError::Interrupted(_)) => {
            Err(PeerError("connection_failed_or_pin_mismatch"))
        }
    }
}

/// Map a socket/protocol failure to a stable code. Response bodies and
/// credentials are deliberately never included.
fn socket_failure(error: tungstenite::Error) -> PeerError {
    match error {
        tungstenite::Error::Http(response) => {
            // The host answers a refused attach with a stable code. Only codes
            // this client knows are accepted; anything else stays unmapped.
            if let Some(reason) = response
                .body()
                .as_deref()
                .and_then(|body| serde_json::from_slice::<Value>(body).ok())
                .and_then(|document| {
                    document["error"]
                        .as_str()
                        .and_then(crate::desktop_protocol::known_reason)
                })
            {
                return PeerError(reason);
            }
            match response.status().as_u16() {
            401 => PeerError("peer_revoked_or_expired"),
            403 => PeerError("invitation_rejected"),
            404 => PeerError("peer_endpoint_unavailable"),
            429 => PeerError("attachment_limit"),
            503 => PeerError("remote_desktop_unavailable"),
            _ => PeerError("peer_request_rejected"),
        }
        }
        _ => PeerError("connection_failed_or_pin_mismatch"),
    }
}

#[cfg(test)]
pub fn test_peer(id: char) -> Peer {
    Peer {
        machine_id: id.to_string().repeat(32),
        label: "Laptop".into(),
        endpoint: Endpoint::new("127.0.0.1", 8759).unwrap(),
        fingerprint: "b".repeat(64),
        token: "c".repeat(48),
        expires_at: Some(now() + 3600.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitations_accept_the_existing_qr_link_but_not_urls_or_extra_queries() {
        let payload = json!({"v":3,"host":"Laptop.local","port":8759,"fingerprint":"a".repeat(64),"secret":"b".repeat(48),"expiresIn":300}).to_string();
        let link = format!(
            "superdesktop://pair?data={}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload)
        );
        assert_eq!(
            Invitation::parse(&link).unwrap().endpoint.host,
            "laptop.local"
        );
        assert!(Invitation::parse(&payload).is_ok());
        for bad in [
            "https://example.com/".to_string(),
            format!("{link}&redirect=example.com"),
            payload.replace("\"v\":3", "\"v\":2"),
        ] {
            assert!(Invitation::parse(&bad).is_err());
        }
    }

    #[test]
    fn endpoints_cannot_inject_schemes_credentials_paths_or_headers() {
        for host in [
            "https://example.com",
            "user@host",
            "host/path",
            "host?x=1",
            "host\r\nX:a",
            "0.0.0.0",
            "a..b",
        ] {
            assert!(Endpoint::new(host, 8759).is_err(), "{host:?}");
        }
        assert_eq!(
            Endpoint::new("::1", 1234)
                .unwrap()
                .url("/api/v1/ping")
                .unwrap()
                .as_str(),
            "https://[::1]:1234/api/v1/ping"
        );
        assert!(Endpoint::new("host", 0).is_err());
    }

    #[test]
    fn pinned_https_client_builds_with_preconfigured_backend() {
        // reqwest type-erases the backend and rejects anything it cannot
        // downcast; a regression here breaks all pairing, snapshot and attach
        // calls before a single byte is sent.
        let pin = "ab".repeat(32);
        assert!(pinned_tls(&pin).is_ok());
        let endpoint = Endpoint::new("127.0.0.1", 8759).unwrap();
        assert!(PinnedClient::new(endpoint, &pin).is_ok());
    }

    #[test]
    fn public_summaries_exclude_credentials_and_pin() {
        let peer = test_peer('a');
        let public = serde_json::to_string(&peer.summary()).unwrap();
        assert!(!public.contains(&peer.token) && !public.contains(&peer.fingerprint));
        assert_eq!(label("\x1b\n Laptop \t"), "Laptop");
        let mut expired = peer;
        expired.expires_at = Some(1.0);
        assert_eq!(
            workspace(&expired).err(),
            Some(PeerError("peer_revoked_or_expired"))
        );
    }
}
