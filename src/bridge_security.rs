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
    peeked: Option<u8>,
    deadline: Option<std::time::Instant>,
    credential_hash: Option<String>,
}
impl Connection {
    pub(super) fn tls(socket: TcpStream, config: Arc<rustls::ServerConfig>) -> io::Result<Self> {
        socket.set_nodelay(true)?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self { transport: Transport::Tls(Box::new(rustls::StreamOwned::new(
            rustls::ServerConnection::new(config).map_err(io::Error::other)?, socket))), peeked: None,
            deadline: Some(std::time::Instant::now() + Duration::from_secs(5)), credential_hash: None })
    }
    pub(super) fn local(socket: UnixStream) -> Self {
        Self { transport: Transport::Local(socket), peeked: None, deadline: Some(std::time::Instant::now()+Duration::from_secs(5)), credential_hash: None }
    }
    #[cfg(test)] pub(super) fn plain(socket: TcpStream) -> Self {
        Self { transport: Transport::Plain(socket), peeked: None, deadline: None, credential_hash: None }
    }
    pub(super) fn is_local(&self) -> bool { matches!(self.transport, Transport::Local(_)) }
    pub(super) fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        match &self.transport { Transport::Tls(s) => s.sock.peer_addr(), Transport::Local(_) => Err(io::Error::other("local")),
            #[cfg(test)] Transport::Plain(s) => s.peer_addr() }
    }
    pub(super) fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match &self.transport { Transport::Tls(s) => s.sock.set_read_timeout(timeout), Transport::Local(s) => s.set_read_timeout(timeout),
            #[cfg(test)] Transport::Plain(s) => s.set_read_timeout(timeout) }
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
        if self.peeked.is_none() {
            let mut byte = [0];
            if self.read(&mut byte)? == 0 { return Ok(0) }
            self.peeked = Some(byte[0]);
        }
        buf[0] = self.peeked.unwrap(); Ok(1)
    }
    pub(super) fn streaming(&mut self) { self.deadline = None; }
    pub(super) fn credential(&mut self, token: &str) { self.credential_hash = Some(digest(token.as_bytes())); }
    pub(super) fn still_authorized(&self) -> bool {
        self.credential_hash.as_ref().is_none_or(|hash| pair_state().lock().map(|p|
            p.cfg.devices.iter().any(|d| d.expires > now_epoch() && equal(hash, &d.token_hash))).unwrap_or(false))
    }
}
impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.still_authorized() { return Err(io::Error::new(io::ErrorKind::PermissionDenied,"device revoked or expired")) }
        if buf.is_empty() { return Ok(0) }
        if let Some(b) = self.peeked.take() { buf[0] = b; return Ok(1) }
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
    std::thread::spawn(move || { for socket in listener.incoming().flatten() { handle_client(Connection::local(socket), None); } });
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
