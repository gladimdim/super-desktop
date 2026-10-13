//! The `sd-qml-host` process for one surface: spawning it with its end of a
//! SOCK_SEQPACKET socketpair as fd 3, packets with an optional fd, and the
//! shared-memory frame buffer it renders into.
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const MAX_PACKET: usize = 1 << 20;

pub struct SpawnOptions<'a> {
    pub host: &'a Path,
    pub qml: &'a Path,
    pub plugin_dir: &'a Path,
    pub plugin_id: &'a str,
    pub surface_id: &'a str,
    pub width: i32,
    pub height: i32,
    /// Device scale of the output (1, 1.25, 2…).
    pub scale: f64,
    pub theme: &'a Value,
    /// Host stderr goes here (the plugin log).
    pub log: Option<PathBuf>,
}

pub struct HostProcess {
    child: Option<Child>,
    socket: OwnedFd,
}

fn cvt(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

impl HostProcess {
    pub fn spawn(options: &SpawnOptions) -> io::Result<Self> {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `fds` has room for the two descriptors socketpair writes.
        cvt(unsafe {
            libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr())
        })?;
        // SAFETY: socketpair succeeded, so both are fresh descriptors we own.
        let (ours, theirs) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };

        let mut command = Command::new(options.host);
        command
            .arg("--serve")
            .args(["--fd", "3"])
            .arg("--qml")
            .arg(options.qml)
            .arg("--plugin-dir")
            .arg(options.plugin_dir)
            .args(["--plugin-id", options.plugin_id, "--surface-id", options.surface_id])
            .args(["--width", &options.width.max(1).to_string()])
            .args(["--height", &options.height.max(1).to_string()])
            .args(["--theme", &options.theme.to_string()])
            .current_dir(options.plugin_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        // Qt picks the device pixel ratio up from here; the host never sees
        // the real output.
        if options.scale > 0.0 && (options.scale - 1.0).abs() > f64::EPSILON {
            command.env("QT_SCALE_FACTOR", format!("{}", options.scale));
        } else {
            command.env_remove("QT_SCALE_FACTOR");
        }
        for key in ["WAYLAND_DISPLAY", "DISPLAY", "QT_QPA_PLATFORM"] {
            command.env_remove(key);
        }
        match options.log.as_deref().and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok()) {
            Some(file) => command.stderr(Stdio::from(file)),
            None => command.stderr(Stdio::null()),
        };
        let child_fd = theirs.as_raw_fd();
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(move || {
                // dup2 clears close-on-exec on the copy.
                if libc::dup2(child_fd, 3) < 0 {
                    return Err(io::Error::last_os_error());
                }
                // The host ends with the overlay, whatever way it ends.
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(theirs);
        Ok(Self { child: Some(child), socket: ours })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    /// A second handle on the connection for a reader thread: `next()`
    /// blocks until a packet arrives and fails with `UnexpectedEof` once the
    /// host is gone.
    pub fn reader(&self) -> io::Result<Reader> {
        Ok(Reader { socket: self.socket.try_clone()? })
    }

    pub fn send(&self, message: &Value) -> io::Result<()> {
        send_packet(self.socket.as_raw_fd(), message.to_string().as_bytes())
    }

    /// The next packet, or `Ok(None)` when none is waiting. `Err` with
    /// `UnexpectedEof` when the host closed its end.
    pub fn receive(&self) -> io::Result<Option<(Vec<u8>, Option<OwnedFd>)>> {
        receive_packet(self.socket.as_raw_fd(), false)
    }

    /// Whether the process has ended (and reaps it).
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.as_mut().and_then(|child| child.try_wait().ok().flatten())
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.send(&serde_json::json!({"t": "quit"}));
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // Reap off the GTK thread: a host stuck in uninterruptible sleep
            // must not hold the overlay.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

pub struct Reader {
    socket: OwnedFd,
}

impl Reader {
    pub fn next(&self) -> io::Result<(Vec<u8>, Option<OwnedFd>)> {
        loop {
            if let Some(packet) = receive_packet(self.socket.as_raw_fd(), true)? {
                return Ok(packet);
            }
        }
    }
}

pub fn send_packet(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_PACKET {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "message larger than 1 MiB"));
    }
    loop {
        // SAFETY: the buffer is valid for its length.
        let n = unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
        if n >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// One packet. `block` waits for it; otherwise `Ok(None)` when none is there.
pub fn receive_packet(fd: RawFd, block: bool) -> io::Result<Option<(Vec<u8>, Option<OwnedFd>)>> {
    let flags = libc::MSG_CMSG_CLOEXEC | if block { 0 } else { libc::MSG_DONTWAIT };
    let mut buffer = vec![0u8; MAX_PACKET];
    let mut control = [0u8; 64];
    loop {
        let mut iov = libc::iovec { iov_base: buffer.as_mut_ptr().cast(), iov_len: buffer.len() };
        // SAFETY: zeroed msghdr is a valid empty header; fields set below.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = control.len() as _;
        // SAFETY: msg points at live buffers for the duration of the call.
        let n = unsafe { libc::recvmsg(fd, &mut msg, flags) };
        if n < 0 {
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(None),
                _ => return Err(error),
            }
        }
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "host closed the connection"));
        }
        let mut passed = None;
        // SAFETY: walking the control messages the kernel filled in.
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                    let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                    for i in 0..count {
                        let raw = std::ptr::read_unaligned(data.add(i));
                        let owned = OwnedFd::from_raw_fd(raw);
                        if passed.is_none() {
                            passed = Some(owned);
                        }
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
        buffer.truncate(n as usize);
        return Ok(Some((buffer, passed)));
    }
}

/// A frame buffer the host shares with us (read only on this side).
pub struct SharedBuffer {
    ptr: *mut libc::c_void,
    len: usize,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub scale: f64,
    _file: File,
}

impl SharedBuffer {
    pub fn map(fd: OwnedFd, width: i32, height: i32, stride: i32, scale: f64) -> io::Result<Self> {
        if width <= 0 || height <= 0 || stride < width.saturating_mul(4) || width > 16384 || height > 16384 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame buffer size"));
        }
        let len = stride as usize * height as usize;
        let file = File::from(fd);
        if file.metadata()?.len() < len as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame buffer smaller than announced"));
        }
        // SAFETY: mapping a file we hold, read only, for its announced size.
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, file.as_raw_fd(), 0) };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { ptr, len, width, height, stride, scale, _file: file })
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping is `len` bytes and lives as long as `self`.
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }
}

impl Drop for SharedBuffer {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what `map` mapped.
        unsafe {
            libc::munmap(self.ptr, self.len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::protocol::{self, FromHost};
    use std::time::{Duration, Instant};

    fn wait_for(host: &HostProcess, deadline: Duration, mut want: impl FnMut(FromHost, Option<OwnedFd>) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            match host.receive() {
                Ok(Some((bytes, fd))) => {
                    if want(protocol::parse(&bytes), fd) {
                        return true;
                    }
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => {
                    eprintln!("host connection ended: {e}");
                    return false;
                }
            }
        }
        false
    }

    /// Runs the real `sd-qml-host` when it is built
    /// (`scripts/build-qml-host.sh debug`), else says why it is skipped.
    #[test]
    fn plugins_host_round_trip_renders_and_forwards_clicks() {
        let Some(binary) = crate::plugins::paths::qml_host_binary() else {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        };
        let dir = std::env::temp_dir().join(format!("sd-plugins-host-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let qml = dir.join("Dock.qml");
        std::fs::write(&qml, r#"import QtQuick
Rectangle {
    color: sd.theme.accent
    MouseArea { anchors.fill: parent; onClicked: sd.call("overlay.hide", {"from": "test"}) }
}
"#).unwrap();
        let theme = serde_json::json!({"accent": "#ff0000"});
        let host = HostProcess::spawn(&SpawnOptions {
            host: &binary, qml: &qml, plugin_dir: &dir, plugin_id: "test", surface_id: "main",
            width: 40, height: 30, scale: 1.0, theme: &theme, log: Some(dir.join("host.log")),
        }).unwrap();
        assert!(wait_for(&host, Duration::from_secs(10), |m, _| m == FromHost::Ready),
            "no ready: {}", std::fs::read_to_string(dir.join("host.log")).unwrap_or_default());
        host.send(&protocol::resize(40, 30)).unwrap();
        let mut buffer = None;
        assert!(wait_for(&host, Duration::from_secs(10), |m, fd| {
            if let FromHost::Buffer { width, height, stride, scale } = m {
                buffer = Some(SharedBuffer::map(fd.expect("buffer fd"), width, height, stride, scale).unwrap());
            }
            matches!(m, FromHost::Frame { .. }) && buffer.as_ref().is_some_and(|b| b.width == 40)
        }), "no frame");
        let pixels = buffer.unwrap();
        // BGRA premultiplied: pure red, opaque.
        assert_eq!(&pixels.bytes()[..4], &[0, 0, 255, 255]);
        host.send(&protocol::ack(1)).unwrap();
        host.send(&protocol::pointer("move", 10.0, 10.0, 0, 0)).unwrap();
        host.send(&protocol::pointer("press", 10.0, 10.0, 1, 0)).unwrap();
        host.send(&protocol::pointer("release", 10.0, 10.0, 1, 0)).unwrap();
        assert!(wait_for(&host, Duration::from_secs(10), |m, _| matches!(m,
            FromHost::Call { ref method, ref params, .. } if method == "overlay.hide" && params["from"] == "test")),
            "the click did not reach QML");
        drop(host);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
