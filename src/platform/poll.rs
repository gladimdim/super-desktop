//! Socket readiness flags; EOF remains readable on platforms without POLLRDHUP.
#[cfg(target_os = "linux")]
const READ_HALF_CLOSED: libc::c_short = libc::POLLRDHUP;
#[cfg(not(target_os = "linux"))]
const READ_HALF_CLOSED: libc::c_short = 0;

pub const READ_EVENTS: libc::c_short = libc::POLLIN | READ_HALF_CLOSED;
pub const CLOSED_EVENTS: libc::c_short =
    libc::POLLERR | libc::POLLHUP | libc::POLLNVAL | READ_HALF_CLOSED;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::{fd::AsRawFd, unix::net::UnixStream};

    #[test]
    fn socket_half_close_wakes_readers_without_losing_buffered_bytes() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"next request").unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        let mut fd = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: READ_EVENTS,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 2000) }, 1);
        assert_ne!(fd.revents & (libc::POLLIN | CLOSED_EVENTS), 0);
        let mut bytes = [0; 12];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"next request");
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 2000) }, 1);
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    }
}
