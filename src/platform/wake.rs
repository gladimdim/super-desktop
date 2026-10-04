//! Coalescing, pollable worker notification. Notifications carry no payload;
//! the caller queues its work before notifying and checks the queue after waking.
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;

pub struct Wake {
    #[cfg(target_os = "linux")]
    event: std::os::fd::OwnedFd,
    #[cfg(not(target_os = "linux"))]
    reader: std::os::unix::net::UnixStream,
    #[cfg(not(target_os = "linux"))]
    writer: std::os::unix::net::UnixStream,
}

impl Wake {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::try_new().expect("worker wake descriptor"))
    }

    fn try_new() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::FromRawFd;
            let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                event: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) },
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            // std sets close-on-exec on both ends. Keeping both ends owned by
            // this object also prevents a notifier from writing to a closed peer.
            let (reader, writer) = std::os::unix::net::UnixStream::pair()?;
            reader.set_nonblocking(true)?;
            writer.set_nonblocking(true)?;
            Ok(Self { reader, writer })
        }
    }

    pub fn raw(&self) -> RawFd {
        #[cfg(target_os = "linux")]
        {
            self.event.as_raw_fd()
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.reader.as_raw_fd()
        }
    }

    fn writer_fd(&self) -> RawFd {
        #[cfg(target_os = "linux")]
        {
            self.event.as_raw_fd()
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.writer.as_raw_fd()
        }
    }

    pub fn notify(&self) {
        let one = 1u64.to_ne_bytes();
        loop {
            let written = unsafe { libc::write(self.writer_fd(), one.as_ptr().cast(), one.len()) };
            if written >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                // A full nonblocking descriptor already has a pending wake.
                break;
            }
        }
    }

    pub fn drain(&self) {
        #[cfg(target_os = "linux")]
        let mut bytes = [0u8; 8];
        #[cfg(not(target_os = "linux"))]
        let mut bytes = [0u8; 1024];
        loop {
            let read = unsafe { libc::read(self.raw(), bytes.as_mut_ptr().cast(), bytes.len()) };
            if read == 0 {
                break;
            }
            if read < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
    }

    pub fn pending(&self) -> bool {
        let mut fd = libc::pollfd {
            fd: self.raw(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut fd, 1, 0) > 0 && fd.revents & libc::POLLIN != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn repeated_bursts_coalesce_without_blocking_and_rearm() {
        let wake = Wake::new();
        assert!(!wake.pending());
        for _ in 0..3 {
            for _ in 0..100_000 {
                wake.notify();
            }
            assert!(wake.pending());
            wake.drain();
            assert!(!wake.pending());
            wake.drain(); // An empty drain must not block.
        }
    }

    #[test]
    fn notification_wakes_a_polling_thread() {
        let wake = Wake::new();
        let worker_wake = Arc::clone(&wake);
        let worker = std::thread::spawn(move || {
            let mut fd = libc::pollfd {
                fd: worker_wake.raw(),
                events: libc::POLLIN,
                revents: 0,
            };
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let ready = unsafe { libc::poll(&mut fd, 1, 100) };
                if ready > 0 {
                    assert_ne!(fd.revents & libc::POLLIN, 0);
                    break;
                }
                assert!(Instant::now() < deadline, "notification did not wake poll");
            }
            worker_wake.drain();
        });
        wake.notify();
        worker.join().unwrap();
        assert!(!wake.pending());
    }

    #[test]
    fn descriptors_are_nonblocking_and_not_inherited_by_exec() {
        let wake = Wake::new();
        for fd in [wake.raw(), wake.writer_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            let descriptor_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags >= 0 && descriptor_flags >= 0);
            assert_ne!(flags & libc::O_NONBLOCK, 0);
            assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
        }
    }
}
