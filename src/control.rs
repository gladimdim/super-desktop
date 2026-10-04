//! Versioned owner-only local control. This socket is independent of bridge IPC.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

pub const VERSION: u32 = 1;
pub const MAX_REQUEST: usize = 16 * 1024;
pub const MAX_REPLY: usize = 1024 * 1024;
pub const MAX_CONNECTIONS: usize = 8;
pub const DEADLINE: Duration = Duration::from_secs(3);
pub const METHODS: &[&str] = &[
    "app.status",
    "capabilities",
    "terminal.list",
    "terminal.inspect",
    "terminal.runtime",
    "terminal.capture",
    "terminal.geometry",
    "terminal.move",
    "terminal.resize",
    "terminal.close",
    "harness.list",
    "harness.inspect",
    "harness.launch",
    "request.inspect",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", deny_unknown_fields)]
pub enum Command {
    #[serde(rename = "app.status")]
    Status {},
    #[serde(rename = "capabilities")]
    Capabilities {},
    #[serde(rename = "terminal.list")]
    Terminals {},
    #[serde(rename = "terminal.inspect")]
    Terminal { id: String },
    #[serde(rename = "terminal.runtime")]
    Runtime { id: String },
    #[serde(rename = "terminal.capture")]
    Capture {
        id: String,
        #[serde(default)]
        history: bool,
        #[serde(default)]
        lines: Option<u32>,
    },
    #[serde(rename = "terminal.geometry")]
    Geometry { id: String },
    #[serde(rename = "terminal.move")]
    Move {
        id: String,
        x: i32,
        y: i32,
        #[serde(default)]
        clamp: bool,
        #[serde(rename = "expectEpoch")]
        expect_epoch: String,
        #[serde(rename = "expectRevision")]
        expect_revision: String,
    },
    #[serde(rename = "terminal.resize")]
    Resize {
        id: String,
        width: u32,
        height: u32,
        #[serde(default)]
        clamp: bool,
        #[serde(rename = "expectEpoch")]
        expect_epoch: String,
        #[serde(rename = "expectRevision")]
        expect_revision: String,
    },
    #[serde(rename = "terminal.close")]
    Close {
        id: String,
        #[serde(rename = "expectEpoch")]
        expect_epoch: String,
        #[serde(rename = "expectRevision")]
        expect_revision: String,
        #[serde(rename = "expectPaneIdentity")]
        expect_pane_identity: String,
    },
    #[serde(rename = "harness.list")]
    Harnesses { all: bool },
    #[serde(rename = "harness.inspect")]
    Harness { id: String },
    #[serde(rename = "harness.launch")]
    Launch {
        harness: String,
        cwd: String,
        #[serde(default, rename = "allowUnsafeHarness")]
        allow_unsafe_harness: bool,
        #[serde(default, rename = "allowDownload")]
        allow_download: bool,
    },
    #[serde(rename = "request.inspect")]
    InspectRequest { id: String },
}

impl Command {
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::Launch { .. } | Self::Move { .. } | Self::Resize { .. } | Self::Close { .. }
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub control_version: u32,
    pub request_id: String,
    pub command: Command,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub schema_version: u32,
    pub request_id: String,
    pub target: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Error {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub outcome: String,
}

impl Reply {
    pub fn success(id: &str, data: Value) -> Self {
        Self {
            schema_version: VERSION,
            request_id: id.into(),
            target: "local".into(),
            ok: true,
            data: Some(data),
            error: None,
        }
    }
    pub fn failure(id: &str, code: &str, message: &str) -> Self {
        Self {
            schema_version: VERSION,
            request_id: id.into(),
            target: "local".into(),
            ok: false,
            data: None,
            error: Some(Error {
                code: code.into(),
                message: message.into(),
                retryable: false,
                outcome: "not_applied".into(),
            }),
        }
    }
    pub fn unknown(id: &str) -> Self {
        let mut reply = Self::failure(id, "unknown_outcome", "Mutation outcome is unknown. Inspect this request ID and current terminal state; do not retry with a new ID.");
        reply.error.as_mut().unwrap().outcome = "unknown".into();
        reply
    }
    pub fn exit_code(&self) -> i32 {
        if self.ok {
            return 0;
        }
        match self.error.as_ref().map(|e| e.code.as_str()) {
            Some("invalid_arguments" | "invalid_request") => 2,
            Some("not_found" | "terminal_not_running") => 3,
            Some(
                "permission_denied"
                | "unsafe_socket"
                | "unsafe_harness"
                | "download_requires_opt_in",
            ) => 4,
            Some("conflict") => 5,
            Some("timeout" | "unknown_outcome") => 7,
            Some("output_failed" | "invalid_response") => 8,
            _ => 6,
        }
    }
}

pub fn capabilities() -> Value {
    json!({"controlVersion": VERSION, "serverVersion": env!("CARGO_PKG_VERSION"),
        "target": "local", "access": "owner", "readOnly": false, "methods": METHODS,
        "limits": {"requestBytes":MAX_REQUEST,"replyBytes":MAX_REPLY,"connections":MAX_CONNECTIONS,"timeoutMs":DEADLINE.as_millis()},
        "terminalInventory": "saved-cards", "terminalRuntimeObserved": false,
        "delegatedAccess": false, "remoteTargets": false,
        "terminalObservation":{"readOnly":true,"maxHistoryLines":2000,"defaultHistoryLines":200,"maxCaptureBytes":65536,"rawAnsi":false,"resize":false},
        "terminalClose":{"requiresRequestId":true,"requiresEpochAndRevision":true,"requiresPaneIdentity":true,"missingPaneRemoval":false,"singleUnlinkedPaneOnly":true},
        "terminalGeometry":{"units":"logical-pixels","requiresRequestId":true,"requiresEpochAndRevision":true,"clamp":"explicit","gridControl":false},
        "launch": {"requiresRequestId":true,"initialPrompt":false,"argumentOverrides":false,"focus":false,"journalEntries":4096}})
}

#[cfg(target_os = "linux")]
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })))
}

#[cfg(target_os = "macos")]
pub fn runtime_dir() -> PathBuf {
    crate::platform::runtime::directory()
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "unsafe local control path or peer",
    )
}

#[doc(hidden)]
pub fn private_dir(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(denied());
    }
    // Reject symlink components, including an environment-selected runtime path.
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(denied());
        }
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(denied());
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
        return Err(denied());
    }
    Ok(())
}

#[doc(hidden)]
pub fn private_file(path: &Path, socket: bool) -> io::Result<fs::Metadata> {
    let m = fs::symlink_metadata(path)?;
    if m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o777 != 0o600
        || if socket {
            !m.file_type().is_socket()
        } else {
            !m.is_file() || m.nlink() != 1
        }
    {
        return Err(denied());
    }
    Ok(m)
}

// SO_SNDTIMEO bounds a blocking AF_UNIX connect when a listener's backlog is
// full. Set it before connect, not just before writing the request.
#[cfg(target_os = "linux")]
fn connect_bounded(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "control path is too long",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    stream.set_write_timeout(Some(remaining(deadline)?))?;
    let rc = unsafe {
        libc::connect(
            stream.as_raw_fd(),
            &address as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stream)
}

#[cfg(target_os = "macos")]
fn connect_bounded(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "control path is too long",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    address.sun_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as u8;
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // Darwin has no SOCK_CLOEXEC and a send timeout does not reliably bound
    // connect. Keep the socket nonblocking until the deadline-controlled poll.
    stream.set_nonblocking(true)?;
    remaining(deadline)?;
    let rc = unsafe {
        libc::connect(
            fd,
            &address as *const _ as *const libc::sockaddr,
            address.sun_len.into(),
        )
    };
    if rc != 0 {
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EINPROGRESS) | Some(libc::EWOULDBLOCK)
        ) {
            return Err(error);
        }
        loop {
            let timeout = remaining(deadline)?.as_millis().clamp(1, i32::MAX as u128) as i32;
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if ready == 0 {
                continue;
            }
            if let Some(error) = stream.take_error()? {
                return Err(error);
            }
            if poll.revents & libc::POLLOUT != 0 {
                break;
            }
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "control connect failed",
            ));
        }
    }
    stream.set_nonblocking(false)?;
    Ok(stream)
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut size,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if size as usize != std::mem::size_of::<libc::ucred>() {
        return Err(denied());
    }
    Ok(cred.uid)
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

fn check_peer(stream: &UnixStream) -> io::Result<()> {
    if peer_uid(stream)? != unsafe { libc::geteuid() } {
        return Err(denied());
    }
    Ok(())
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "control deadline exceeded"))
}

#[cfg(target_os = "linux")]
fn read_chunk(stream: &mut UnixStream, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
    stream.set_read_timeout(Some(remaining(deadline)?))?;
    stream.read(bytes)
}

#[cfg(target_os = "macos")]
fn read_chunk(stream: &mut UnixStream, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
    // Darwin rejects SO_RCVTIMEO after the peer closes, even when a complete
    // reply is buffered. Poll against the same absolute deadline and receive
    // without blocking so a closed peer cannot discard an already sent reply.
    loop {
        let timeout = remaining(deadline)?.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut poll = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        remaining(deadline)?;
        let count = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if count >= 0 {
            return Ok(count as usize);
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            continue;
        }
        return Err(error);
    }
}

fn read_exact(stream: &mut UnixStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        match read_chunk(stream, bytes, deadline) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated control frame",
                ))
            }
            Ok(n) => {
                bytes = &mut bytes[n..];
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn read_frame(stream: &mut UnixStream, limit: usize, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    read_exact(stream, &mut header, deadline)?;
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid control frame length",
        ));
    }
    let mut body = vec![0; len];
    read_exact(stream, &mut body, deadline)?;
    Ok(body)
}

pub fn write_frame(
    stream: &mut UnixStream,
    body: &[u8],
    limit: usize,
    deadline: Instant,
) -> io::Result<()> {
    if body.is_empty() || body.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid control frame length",
        ));
    }
    for mut bytes in [&(body.len() as u32).to_be_bytes()[..], body] {
        while !bytes.is_empty() {
            stream.set_write_timeout(Some(remaining(deadline)?))?;
            match stream.write(bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "control write failed",
                    ))
                }
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

pub struct Server {
    listener: UnixListener,
    _lock: File,
    path: PathBuf,
    inode: u64,
}

impl Server {
    pub fn bind(runtime: &Path) -> io::Result<Self> {
        private_dir(runtime)?;
        let directory = runtime.join("super-desktop");
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        private_dir(&directory)?;
        let lock_path = directory.join("control-v1.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&lock_path)?;
        let metadata = private_file(&lock_path, false)?;
        if lock.metadata()?.ino() != metadata.ino() {
            return Err(denied());
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "local control already owned",
            ));
        }
        let path = directory.join("control-v1.sock");
        match private_file(&path, true) {
            Ok(metadata) => {
                match connect_bounded(&path, Instant::now() + DEADLINE) {
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            "local control already listening",
                        ))
                    }
                    Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {}
                    Err(e) => return Err(e),
                }
                // Only a refused, owned socket whose inode is unchanged may be removed.
                if private_file(&path, true)?.ino() != metadata.ino() {
                    return Err(denied());
                }
                fs::remove_file(&path)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let inode = private_file(&path, true)?.ino();
        Ok(Self {
            listener,
            _lock: lock,
            path,
            inode,
        })
    }

    pub fn run(self, handler: impl Fn(Request, Instant) -> Reply + Send + Sync + 'static) {
        let handler = Arc::new(handler);
        let active = Arc::new(AtomicUsize::new(0));
        for stream in self.listener.incoming() {
            let Ok(stream) = stream else { continue };
            if check_peer(&stream).is_err() {
                continue;
            }
            if active.load(Ordering::Acquire) >= MAX_CONNECTIONS {
                continue;
            }
            active.fetch_add(1, Ordering::AcqRel);
            let permit = Permit(active.clone());
            let handler = handler.clone();
            // The permit drops even when thread creation or the handler fails.
            let _ = std::thread::Builder::new()
                .name("sd-control-client".into())
                .spawn(move || {
                    let _permit = permit;
                    serve_connection(stream, |request, deadline| handler(request, deadline));
                });
        }
    }
}

struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.ino() == self.inode && m.file_type().is_socket())
        {
            let _ = fs::remove_file(&self.path);
        }
        // A concurrent fork may briefly retain this open-file description
        // until exec closes it. Explicit unlock prevents a false stale-owner
        // refusal after our listener has already shut down.
        unsafe {
            libc::flock(self._lock.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub fn serve_connection(mut stream: UnixStream, handler: impl FnOnce(Request, Instant) -> Reply) {
    if check_peer(&stream).is_err() {
        return;
    }
    let deadline = Instant::now() + DEADLINE;
    let result = read_frame(&mut stream, MAX_REQUEST, deadline);
    let reply = match result {
        Ok(body) => match serde_json::from_slice::<Request>(&body) {
            Ok(request)
                if request.request_id.is_empty()
                    || request.request_id.len() > 64
                    || !request
                        .request_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
            {
                Reply::failure("", "invalid_request", "Invalid request ID.")
            }
            Ok(request) if request.control_version != VERSION => Reply::failure(
                &request.request_id,
                "unsupported_version",
                "This daemon requires local control version 1.",
            ),
            Ok(request) => handler(request, deadline),
            Err(_) => Reply::failure(
                "",
                "invalid_request",
                "Unknown method or malformed request.",
            ),
        },
        Err(_) => Reply::failure("", "invalid_request", "Incomplete or oversized request."),
    };
    if let Ok(body) = serde_json::to_vec(&reply) {
        let body = if body.len() <= MAX_REPLY {
            body
        } else {
            serde_json::to_vec(&Reply::failure(
                &reply.request_id,
                "response_too_large",
                "The inventory exceeds the response limit.",
            ))
            .unwrap()
        };
        let _ = write_frame(&mut stream, &body, MAX_REPLY, deadline);
    }
}

pub fn request_at(runtime: &Path, request: &Request) -> Reply {
    let mut attempted = false;
    match exchange(runtime, request, &mut attempted) {
        Ok(reply) => reply,
        Err(_) if attempted && request.command.is_mutation() => Reply::unknown(&request.request_id),
        Err(e) => {
            let (code, message) = match e.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => ("unavailable", "Local control is unavailable. Start a compatible SUPER DESKTOP daemon; this command never starts one."),
                io::ErrorKind::PermissionDenied => ("unsafe_socket", "Local control requires private owner-only runtime directories, socket and peer."),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ("timeout", "The read-only control request timed out; the daemon was not restarted."),
                _ => ("invalid_response", "Local control returned an invalid or incomplete response."),
            };
            Reply::failure(&request.request_id, code, message)
        }
    }
}

fn exchange(runtime: &Path, request: &Request, attempted: &mut bool) -> io::Result<Reply> {
    private_dir(runtime)?;
    let directory = runtime.join("super-desktop");
    private_dir(&directory)?;
    let path = directory.join("control-v1.sock");
    private_file(&path, true)?;
    let deadline = Instant::now() + DEADLINE;
    let mut stream = connect_bounded(&path, deadline)?;
    check_peer(&stream)?;
    let body = serde_json::to_vec(request)?;
    *attempted = true;
    write_frame(&mut stream, &body, MAX_REQUEST, deadline)?;
    let body = read_frame(&mut stream, MAX_REPLY, deadline)?;
    let reply: Reply = serde_json::from_slice(&body)?;
    if reply.schema_version != VERSION
        || reply.request_id != request.request_id
        || reply.target != "local"
        || (reply.ok && (reply.data.is_none() || reply.error.is_some()))
        || (!reply.ok && (reply.error.is_none() || reply.data.is_some()))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mismatched control response",
        ));
    }
    Ok(reply)
}

pub fn new_request(command: Command) -> io::Result<Request> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(Request {
        control_version: VERSION,
        request_id: bytes.iter().map(|b| format!("{b:02x}")).collect(),
        command,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Runtime(PathBuf);
    impl Runtime {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "sd-control-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
            Self(p.canonicalize().unwrap())
        }
    }
    impl Drop for Runtime {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn socket_pair_peer_identity_is_the_current_owner() {
        let (first, second) = UnixStream::pair().unwrap();
        for stream in [&first, &second] {
            assert_eq!(peer_uid(stream).unwrap(), unsafe { libc::geteuid() });
            check_peer(stream).unwrap();
        }
    }

    #[test]
    fn private_socket_lock_stale_recovery_and_live_round_trip() {
        let runtime = Runtime::new();
        let server = Server::bind(&runtime.0).unwrap();
        assert_eq!(fs::metadata(&server.path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            Server::bind(&runtime.0).err().unwrap().kind(),
            io::ErrorKind::AddrInUse
        );
        assert!(server.path.exists());
        let expected = new_request(Command::Status {}).unwrap();
        let worker = std::thread::spawn(move || {
            let (socket, _) = server.listener.accept().unwrap();
            assert_eq!(peer_uid(&socket).unwrap(), unsafe { libc::geteuid() });
            assert_ne!(
                unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
            serve_connection(socket, |request, _| {
                Reply::success(&request.request_id, json!({"ready":true}))
            });
        });
        let reply = request_at(&runtime.0, &expected);
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.data.unwrap()["ready"], true);
        worker.join().unwrap();
        let path = runtime.0.join("super-desktop/control-v1.sock");
        assert!(!path.exists());
        let stale = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(stale);
        let _recovered = Server::bind(&runtime.0).unwrap();
    }

    #[test]
    fn unsafe_paths_are_refused_without_replacing_them() {
        let runtime = Runtime::new();
        let server = Server::bind(&runtime.0).unwrap();
        let path = server.path.clone();
        drop(server);
        fs::write(&path, "do not unlink").unwrap();
        assert!(Server::bind(&runtime.0).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "do not unlink");
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(runtime.0.join("missing"), &path).unwrap();
        assert!(Server::bind(&runtime.0).is_err());
        assert!(fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(&path).unwrap();
        let alias_root = Runtime::new();
        let alias = alias_root.0.join("runtime-alias");
        std::os::unix::fs::symlink(&runtime.0, &alias).unwrap();
        assert_eq!(
            Server::bind(&alias).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        fs::set_permissions(&runtime.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            Server::bind(&runtime.0).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn missing_or_unsafe_socket_never_starts_or_cleans_up_a_daemon() {
        let runtime = Runtime::new();
        let request = new_request(Command::Status {}).unwrap();
        assert_eq!(request_at(&runtime.0, &request).exit_code(), 6);
        let server = Server::bind(&runtime.0).unwrap();
        fs::set_permissions(&server.path, fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(request_at(&runtime.0, &request).exit_code(), 4);
        assert!(server.path.exists());
    }

    #[test]
    fn buffered_reply_survives_peer_close_and_truncation_still_fails() {
        for (bytes, complete) in [
            (vec![0, 0, 0, 2, b'{', b'}'], true),
            (vec![0, 0, 0, 2, b'{'], false),
        ] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            server.write_all(&bytes).unwrap();
            drop(server);
            let reply = read_frame(&mut client, MAX_REPLY, Instant::now() + DEADLINE);
            if complete {
                assert_eq!(reply.unwrap(), b"{}");
            } else {
                assert_eq!(reply.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
            }
        }
    }

    #[test]
    fn framing_rejects_truncation_and_oversized_lengths_before_dispatch() {
        for bytes in [
            vec![0, 0, 0, 0],
            ((MAX_REQUEST + 1) as u32).to_be_bytes().to_vec(),
            vec![0, 0, 0, 5, b'{'],
        ] {
            let (mut client, server) = UnixStream::pair().unwrap();
            let worker = std::thread::spawn(move || {
                serve_connection(server, |_, _| panic!("must not dispatch invalid frame"))
            });
            client.write_all(&bytes).unwrap();
            client.shutdown(std::net::Shutdown::Write).unwrap();
            let response = read_frame(&mut client, MAX_REPLY, Instant::now() + DEADLINE).unwrap();
            let reply: Reply = serde_json::from_slice(&response).unwrap();
            assert_eq!(reply.exit_code(), 2);
            worker.join().unwrap();
        }
    }

    #[test]
    fn malformed_request_and_version_skew_do_not_reach_handler() {
        for body in [
            json!({"controlVersion":2,"requestId":"test","command":{"method":"app.status"}}),
            json!({"controlVersion":1,"requestId":"test","command":{"method":"terminal.kill"}}),
            json!({"controlVersion":1,"requestId":"test","command":{"method":"app.status","extra":true}}),
            json!({"controlVersion":1,"requestId":"bad\nID","command":{"method":"app.status"}}),
        ] {
            let (mut client, server) = UnixStream::pair().unwrap();
            let worker = std::thread::spawn(move || {
                serve_connection(server, |_, _| panic!("must not dispatch invalid request"))
            });
            write_frame(
                &mut client,
                &serde_json::to_vec(&body).unwrap(),
                MAX_REQUEST,
                Instant::now() + DEADLINE,
            )
            .unwrap();
            let response = read_frame(&mut client, MAX_REPLY, Instant::now() + DEADLINE).unwrap();
            let reply: Reply = serde_json::from_slice(&response).unwrap();
            assert!(!reply.ok);
            worker.join().unwrap();
        }
    }

    #[test]
    fn mismatched_response_is_not_accepted_and_request_is_not_replayed() {
        let runtime = Runtime::new();
        let server = Server::bind(&runtime.0).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = server.listener.accept().unwrap();
            read_frame(&mut stream, MAX_REQUEST, Instant::now() + DEADLINE).unwrap();
            write_frame(
                &mut stream,
                &serde_json::to_vec(&Reply::success("wrong-id", json!({}))).unwrap(),
                MAX_REPLY,
                Instant::now() + DEADLINE,
            )
            .unwrap();
        });
        assert_eq!(
            request_at(&runtime.0, &new_request(Command::Status {}).unwrap()).exit_code(),
            8
        );
        worker.join().unwrap();
    }

    #[test]
    fn cli_launch_lost_reply_is_unknown_and_never_replayed() {
        let runtime = Runtime::new();
        let server = Server::bind(&runtime.0).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = server.listener.accept().unwrap();
            read_frame(&mut stream, MAX_REQUEST, Instant::now() + DEADLINE).unwrap();
            // The daemon may have applied the request. Drop without a reply.
        });
        let request = new_request(Command::Launch {
            harness: "shell".into(),
            cwd: "/tmp".into(),
            allow_unsafe_harness: false,
            allow_download: false,
        })
        .unwrap();
        let reply = request_at(&runtime.0, &request);
        assert_eq!(reply.exit_code(), 7);
        assert_eq!(reply.error.unwrap().outcome, "unknown");
        worker.join().unwrap();
        let absent = request_at(&runtime.0, &request);
        assert_eq!(absent.error.unwrap().outcome, "not_applied");
    }

    #[test]
    fn cli_geometry_lost_reply_is_unknown_and_never_replayed() {
        let runtime = Runtime::new();
        let server = Server::bind(&runtime.0).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = server.listener.accept().unwrap();
            read_frame(&mut stream, MAX_REQUEST, Instant::now() + DEADLINE).unwrap();
            // The daemon may have applied the request. Drop without a reply.
        });
        let request = new_request(Command::Move {
            id: "sd_term_test".into(),
            x: 80,
            y: 100,
            clamp: false,
            expect_epoch: "epoch".into(),
            expect_revision: "revision".into(),
        })
        .unwrap();
        let reply = request_at(&runtime.0, &request);
        assert_eq!(reply.exit_code(), 7);
        assert_eq!(reply.error.unwrap().outcome, "unknown");
        worker.join().unwrap();
        let absent = request_at(&runtime.0, &request);
        assert_eq!(absent.error.unwrap().outcome, "not_applied");
    }

    #[test]
    fn cli_close_lost_reply_is_unknown_and_never_replayed() {
        let runtime = Runtime::new();
        let server = Server::bind(&runtime.0).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = server.listener.accept().unwrap();
            read_frame(&mut stream, MAX_REQUEST, Instant::now() + DEADLINE).unwrap();
            // The daemon may have applied the request. Drop without a reply.
        });
        let request = new_request(Command::Close {
            id: "sd_term_test".into(),
            expect_epoch: "epoch".into(),
            expect_revision: "a".repeat(64),
            expect_pane_identity: "b".repeat(64),
        })
        .unwrap();
        let reply = request_at(&runtime.0, &request);
        assert_eq!(reply.exit_code(), 7);
        assert_eq!(reply.error.unwrap().outcome, "unknown");
        worker.join().unwrap();
        let absent = request_at(&runtime.0, &request);
        assert_eq!(absent.error.unwrap().outcome, "not_applied");
    }

    #[test]
    fn stalled_frame_has_an_absolute_deadline() {
        let (mut client, _server) = UnixStream::pair().unwrap();
        let start = Instant::now();
        let error =
            read_frame(&mut client, MAX_REQUEST, start + Duration::from_millis(30)).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
