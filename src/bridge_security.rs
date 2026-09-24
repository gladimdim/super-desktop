//! TLS transport, owner-only control socket, and bounded connection admission.
use super::*;
use std::io;
use std::os::unix::{fs::{PermissionsExt, OpenOptionsExt}, net::{UnixListener, UnixStream}};
use std::sync::Arc;
use sha2::{Digest, Sha256};

pub(super) fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |v, (a,b)| v | (a ^ b)) == 0
}
pub(super) fn control_path() -> PathBuf { state_path().with_file_name("control.sock") }

#[derive(Serialize, Deserialize)]
struct Identity { certificate: Vec<u8>, key: Vec<u8> }
pub(super) fn tls_config() -> Result<Arc<rustls::ServerConfig>, Box<dyn std::error::Error>> {
    let path = state_path().with_file_name("tls-identity.json");
    let identity: Identity = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let generated = rcgen::generate_simple_self_signed(vec!["super-desktop.local".into()])?;
            let identity = Identity { certificate: generated.cert.der().to_vec(), key: generated.signing_key.serialize_der() };
            let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
            file.write_all(&serde_json::to_vec(&identity)?)?;
            file.sync_all()?;
            identity
        }
        Err(e) => return Err(e.into()),
    };
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let _ = FINGERPRINT.set(digest(&identity.certificate));
    let config = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(
        vec![rustls::pki_types::CertificateDer::from(identity.certificate)],
        rustls::pki_types::PrivatePkcs8KeyDer::from(identity.key).into())?;
    Ok(Arc::new(config))
}
static FINGERPRINT: OnceLock<String> = OnceLock::new();
pub(super) fn fingerprint() -> &'static str { FINGERPRINT.get().map(String::as_str).unwrap_or("") }

pub(super) enum Transport {
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
    Local(UnixStream),
    #[cfg(test)] Plain(TcpStream),
}
pub(super) struct Connection {
    transport: Transport,
    /// Bytes already read from the transport but not yet consumed: a peeked
    /// byte, or the start of a pipelined request read past the previous body.
    pending: Vec<u8>,
    deadline: Option<std::time::Instant>,
    credential_hash: Option<String>,
    /// Last revocation check for `credential_hash`; see `AuthCache`.
    auth: AuthCache,
    /// Read timeout last applied to the socket, so hot loops do not issue a
    /// `setsockopt` for an unchanged value.
    read_timeout: std::cell::Cell<Option<Option<Duration>>>,
    /// The current response may leave the connection open for another request
    /// (HTTP/1.1 keep-alive). Cleared by any route that takes the stream over.
    pub(super) persist: bool,
}

/// How often an authenticated connection re-reads the paired-device list.
/// Explicit revocation also shuts the socket down at once (`disconnect`), and
/// the watchdog closes expired devices every 250 ms, so this bounds only the
/// window in which already-buffered TLS input can still be processed.
pub(super) const AUTH_RECHECK: Duration = Duration::from_secs(1);

/// Throttled result of the paired-device check. A failed check is final for
/// the connection: a revoked credential never becomes valid again.
pub(super) struct AuthCache(std::cell::Cell<(Option<std::time::Instant>, bool)>);
impl AuthCache {
    pub(super) fn new() -> Self { Self(std::cell::Cell::new((None, true))) }
    /// Mark the credential as verified now (the request was just authorized).
    pub(super) fn verified(&self, now: std::time::Instant) { self.0.set((Some(now), true)); }
    /// `check` runs at most once per `AUTH_RECHECK`.
    pub(super) fn check(&self, now: std::time::Instant, check: impl FnOnce() -> bool) -> bool {
        let (last, ok) = self.0.get();
        if !ok { return false }
        if last.is_some_and(|at| now.saturating_duration_since(at) < AUTH_RECHECK) { return true }
        let ok = check();
        self.0.set((Some(now), ok));
        ok
    }
}
impl Connection {
    pub(super) fn tls(socket: TcpStream, config: Arc<rustls::ServerConfig>) -> io::Result<Self> {
        socket.set_nodelay(true)?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self::with(Transport::Tls(Box::new(rustls::StreamOwned::new(
            rustls::ServerConnection::new(config).map_err(io::Error::other)?, socket))),
            Some(std::time::Instant::now() + Duration::from_secs(5))))
    }
    pub(super) fn local(socket: UnixStream) -> Self {
        Self::with(Transport::Local(socket), Some(std::time::Instant::now()+Duration::from_secs(5)))
    }
    #[cfg(test)] pub(super) fn plain(socket: TcpStream) -> Self {
        Self::with(Transport::Plain(socket), None)
    }
    fn with(transport: Transport, deadline: Option<std::time::Instant>) -> Self {
        Self { transport, pending: Vec::new(), deadline, credential_hash: None, auth: AuthCache::new(),
            read_timeout: std::cell::Cell::new(None), persist: false }
    }
    pub(super) fn is_local(&self) -> bool { matches!(self.transport, Transport::Local(_)) }
    pub(super) fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        match &self.transport { Transport::Tls(s) => s.sock.peer_addr(), Transport::Local(_) => Err(io::Error::other("local")),
            #[cfg(test)] Transport::Plain(s) => s.peer_addr() }
    }
    pub(super) fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        if self.read_timeout.get() == Some(timeout) { return Ok(()) }
        match &self.transport { Transport::Tls(s) => s.sock.set_read_timeout(timeout), Transport::Local(s) => s.set_read_timeout(timeout),
            #[cfg(test)] Transport::Plain(s) => s.set_read_timeout(timeout) }?;
        self.read_timeout.set(Some(timeout));
        Ok(())
    }
    pub(super) fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match &self.transport { Transport::Tls(s) => s.sock.set_write_timeout(timeout), Transport::Local(s) => s.set_write_timeout(timeout),
            #[cfg(test)] Transport::Plain(s) => s.set_write_timeout(timeout) }
    }
    pub(super) fn set_nodelay(&self, value: bool) -> io::Result<()> {
        match &self.transport { Transport::Tls(s) => s.sock.set_nodelay(value), Transport::Local(_) => Ok(()),
            #[cfg(test)] Transport::Plain(s) => s.set_nodelay(value) }
    }
    pub(super) fn peek(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() { return Ok(0) }
        if self.pending.is_empty() {
            let mut byte = [0];
            if self.read(&mut byte)? == 0 { return Ok(0) }
            self.pending.push(byte[0]);
        }
        buf[0] = self.pending[0]; Ok(1)
    }
    /// Return bytes read past the end of one request (HTTP pipelining) so the
    /// next `read` sees them first.
    pub(super) fn unread(&mut self, bytes: &[u8]) {
        if bytes.is_empty() { return }
        let mut pending = bytes.to_vec();
        pending.append(&mut self.pending);
        self.pending = pending;
    }
    /// Wait up to `idle` for the first byte of another request on a kept-alive
    /// connection, then give that request the usual header/body deadline.
    pub(super) fn await_next_request(&mut self, idle: Duration) -> bool {
        self.deadline = None;
        if self.pending.is_empty() && self.set_read_timeout(Some(idle)).is_err() { return false }
        if !matches!(self.peek(&mut [0u8; 1]), Ok(1)) { return false }
        self.deadline = Some(std::time::Instant::now() + Duration::from_secs(5));
        true
    }
    pub(super) fn streaming(&mut self) { self.deadline = None; }
    pub(super) fn upload_deadline(&mut self) { self.deadline = Some(std::time::Instant::now() + Duration::from_secs(30)); }
    pub(super) fn credential(&mut self, token: &str) {
        self.credential_hash = Some(digest(token.as_bytes()));
        // The caller has just validated this token.
        self.auth = AuthCache::new();
        self.auth.verified(std::time::Instant::now());
    }
    /// A kept-alive connection authenticates every request on its own.
    pub(super) fn forget_credential(&mut self) {
        self.credential_hash = None;
        self.auth = AuthCache::new();
    }
    /// Token hash this connection authenticated with, for per-device resource
    /// budgets. Never logged, never returned to a peer, never persisted here.
    pub(super) fn credential_id(&self) -> Option<&str> { self.credential_hash.as_deref() }
    /// Raw socket, so a streaming handler can poll both directions instead of
    /// blocking on one. TLS may still hold buffered plaintext; see `peek`.
    pub(super) fn raw_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        match &self.transport {
            Transport::Tls(s) => Some(s.sock.as_raw_fd()),
            Transport::Local(s) => Some(s.as_raw_fd()),
            #[cfg(test)] Transport::Plain(s) => Some(s.as_raw_fd()),
        }
    }
    /// Re-reads the paired-device list at most once per `AUTH_RECHECK`, not on
    /// every socket read/write (streams poll their peer up to 60 times a
    /// second, and the device list sits behind the global pairing mutex).
    pub(super) fn still_authorized(&self) -> bool {
        let Some(hash) = self.credential_hash.as_ref() else { return true };
        self.auth.check(std::time::Instant::now(), || pair_state().lock().map(|p|
            p.cfg.devices.iter().any(|d| d.expires > now_epoch() && equal(hash, &d.token_hash))).unwrap_or(false))
    }
}
impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.still_authorized() { return Err(io::Error::new(io::ErrorKind::PermissionDenied,"device revoked or expired")) }
        if buf.is_empty() { return Ok(0) }
        if !self.pending.is_empty() {
            let n = self.pending.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            return Ok(n)
        }
        if let Some(deadline) = self.deadline {
            let left = deadline.checked_duration_since(std::time::Instant::now()).ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut,"request deadline"))?;
            self.set_read_timeout(Some(left))?;
        }
        match &mut self.transport { Transport::Tls(s) => s.read(buf), Transport::Local(s) => s.read(buf),
            #[cfg(test)] Transport::Plain(s) => s.read(buf) }
    }
}
impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.still_authorized() { return Err(io::Error::new(io::ErrorKind::PermissionDenied,"device revoked or expired")) }
        match &mut self.transport { Transport::Tls(s) => s.write(buf), Transport::Local(s) => s.write(buf),
            #[cfg(test)] Transport::Plain(s) => s.write(buf) }
    }
    fn flush(&mut self) -> io::Result<()> {
        match &mut self.transport { Transport::Tls(s) => s.flush(), Transport::Local(s) => s.flush(),
            #[cfg(test)] Transport::Plain(s) => s.flush() }
    }
}

static CLIENTS: Mutex<Vec<(u64, String, String, TcpStream, Option<std::time::Instant>)>> = Mutex::new(Vec::new());
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static LAST_SEEN: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
pub(super) fn note_activity(token: &str) {
    if let Ok(mut seen) = LAST_SEEN.get_or_init(Default::default).lock() {
        seen.retain(|_, at| at.elapsed() < Duration::from_secs(60));
        seen.insert(digest(token.as_bytes()), std::time::Instant::now());
    }
}
pub(super) fn device_active(hash: &str) -> bool {
    let live = CLIENTS.lock().map(|clients| clients.iter().any(|c| equal(&c.2, hash))).unwrap_or(false);
    live || LAST_SEEN.get_or_init(Default::default).lock().map(|seen|
        seen.get(hash).is_some_and(|at| at.elapsed() < Duration::from_secs(60))).unwrap_or(false)
}
pub(super) struct Admission(u64);
impl Admission {
    pub(super) fn acquire(socket: &TcpStream) -> Option<Self> {
        let peer = socket.peer_addr().ok()?.ip().to_string();
        let mut clients = CLIENTS.lock().ok()?;
        if clients.len() >= 64 || clients.iter().filter(|(_,p,_,_,_)| p == &peer).count() >= 12 { return None }
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        clients.push((id,peer,String::new(),socket.try_clone().ok()?,Some(std::time::Instant::now()+Duration::from_secs(5)))); Some(Self(id))
    }
    pub(super) fn identify(&self, token: &str) {
        if let Ok(mut clients) = CLIENTS.lock() { if let Some(c) = clients.iter_mut().find(|c| c.0 == self.0) { c.2 = digest(token.as_bytes()); c.4 = None; } }
    }
}
impl Drop for Admission { fn drop(&mut self) { if let Ok(mut c) = CLIENTS.lock() { c.retain(|c| c.0 != self.0); } } }
pub(super) fn disconnect(hash: &str) {
    if let Ok(mut seen) = LAST_SEEN.get_or_init(Default::default).lock() { seen.remove(hash); }
    if let Ok(clients) = CLIENTS.lock() {
        for (_,_,token,socket,_) in clients.iter() { if equal(token, hash) { let _ = socket.shutdown(std::net::Shutdown::Both); } }
    }
}
pub(super) fn serve_control() -> io::Result<()> {
    let path = control_path();
    if UnixStream::connect(&path).is_ok() { return Err(io::Error::new(io::ErrorKind::AddrInUse,"control socket already active")) }
    if path.exists() { fs::remove_file(&path)?; }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    // Sequential owner-only requests cannot create unbounded threads.
    std::thread::spawn(move || { for socket in listener.incoming().flatten() { serve_connection(Connection::local(socket), None); } });
    // Enforce a total handshake/header/body deadline even if a TLS peer trickles
    // bytes fast enough to avoid individual socket read timeouts.
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_millis(250));
        let expired: Vec<String> = pair_state().lock().map(|s| s.cfg.devices.iter()
            .filter(|d| d.expires <= now_epoch()).map(|d| d.token_hash.clone()).collect()).unwrap_or_default();
        if let Ok(clients) = CLIENTS.lock() {
            for (_,_,hash,socket,deadline) in clients.iter() {
                if deadline.is_some_and(|d| std::time::Instant::now() >= d) || expired.iter().any(|h| equal(h,hash)) {
                    let _ = socket.shutdown(std::net::Shutdown::Both);
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn authorization_is_rechecked_at_most_once_per_interval() {
        let cache = AuthCache::new();
        let checks = std::cell::Cell::new(0);
        let start = Instant::now();
        let check = |result: bool| { checks.set(checks.get() + 1); result };
        // First use checks; hot-loop uses inside the interval do not.
        assert!(cache.check(start, || check(true)));
        for ms in [1, 16, 500, 999] {
            assert!(cache.check(start + Duration::from_millis(ms), || check(true)));
        }
        assert_eq!(checks.get(), 1);
        // A revocation is seen on the first check after the interval ...
        assert!(!cache.check(start + AUTH_RECHECK, || check(false)));
        assert_eq!(checks.get(), 2);
        // ... and is final: no later check can restore access.
        assert!(!cache.check(start + AUTH_RECHECK * 5, || check(true)));
        assert_eq!(checks.get(), 2);
        // A just-authorized request does not need another lookup.
        let fresh = AuthCache::new();
        fresh.verified(start);
        assert!(fresh.check(start + Duration::from_millis(10), || check(false)));
        assert_eq!(checks.get(), 2);
    }

    #[test]
    fn unauthenticated_connections_skip_the_device_lookup() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let connection = Connection::plain(server);
        assert!(connection.still_authorized());
    }

    #[test]
    fn pushed_back_bytes_are_read_first() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let mut connection = Connection::plain(server);
        client.write_all(b"cd").unwrap();
        connection.unread(b"ab");
        let mut peeked = [0u8; 1];
        assert_eq!(connection.peek(&mut peeked).unwrap(), 1);
        assert_eq!(&peeked, b"a");
        let mut all = [0u8; 4];
        connection.read_exact(&mut all).unwrap();
        assert_eq!(&all, b"abcd");
    }
}
