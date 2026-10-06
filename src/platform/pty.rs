//! Private PTYs with close-on-exec set during allocation of both endpoints.
use std::fs::File;
use std::io;
#[cfg(any(target_os = "macos", test))]
use std::os::fd::AsFd;
use std::os::fd::{AsRawFd, BorrowedFd};

/// Configure a child whose stdin is the slave to own a new terminal session.
pub fn configure_child_session(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // std has mapped stdin before this hook. Only async-signal-safe operations
    // are allowed here, because another thread may have held a lock at fork.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY.into(), 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

pub fn open(columns: u16, rows: u16) -> io::Result<(File, File)> {
    let (master, slave) = open_pair()?;
    let size = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((master, slave))
}

#[cfg(target_os = "linux")]
fn open_pair() -> io::Result<(File, File)> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { File::from_raw_fd(fd) };
    if unsafe { libc::grantpt(fd) } < 0 || unsafe { libc::unlockpt(fd) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let slave = unsafe {
        libc::ioctl(
            fd,
            libc::TIOCGPTPEER,
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((master, unsafe { File::from_raw_fd(slave) }))
}

#[cfg(target_os = "macos")]
fn open_pair() -> io::Result<(File, File)> {
    use std::os::unix::fs::OpenOptionsExt;
    let device = |path: &str| {
        // Rust opens files with O_CLOEXEC. openpty + fcntl would leave a window
        // for another thread's spawned process to inherit an endpoint.
        File::options()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(path)
    };
    let master = device("/dev/ptmx")?;
    if unsafe { libc::grantpt(master.as_raw_fd()) } < 0
        || unsafe { libc::unlockpt(master.as_raw_fd()) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let slave = device(&name(master.as_fd())?)?;
    Ok((master, slave))
}

/// The slave device belonging to this master, without a shared static buffer.
pub fn name(master: BorrowedFd<'_>) -> io::Result<String> {
    let mut bytes = [0u8; 128];
    #[cfg(target_os = "linux")]
    {
        let error =
            unsafe { libc::ptsname_r(master.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
        if error != 0 {
            return Err(if error > 0 {
                io::Error::from_raw_os_error(error)
            } else {
                io::Error::last_os_error()
            });
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Darwin sys/ttycom.h: _IOC(IOC_OUT, 't', 83, 128).
        // Not exposed by libc; the kernel fills this caller-owned 128-byte buffer.
        const TIOCPTYGNAME: libc::c_ulong = 0x4000_0000 | (128 << 16) | ((b't' as u64) << 8) | 83;
        if unsafe { libc::ioctl(master.as_raw_fd(), TIOCPTYGNAME, bytes.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unterminated PTY name"))?;
    let name = std::str::from_utf8(&bytes[..end])
        .ok()
        .filter(|name| name.starts_with("/dev/"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid PTY device name"))?;
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn raw(slave: &File) {
        let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), attributes.as_mut_ptr()) },
            0
        );
        let mut attributes = unsafe { attributes.assume_init() };
        unsafe { libc::cfmakeraw(&mut attributes) };
        assert_eq!(
            unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &attributes) },
            0
        );
    }

    fn read_exact_ready(file: &mut File, expected: &[u8]) {
        let mut received = vec![0; expected.len()];
        let mut offset = 0;
        while offset < received.len() {
            let mut poll = libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(
                unsafe { libc::poll(&mut poll, 1, 2000) },
                1,
                "PTY data timed out"
            );
            assert_ne!(poll.revents & libc::POLLIN, 0);
            let n = file.read(&mut received[offset..]).unwrap();
            assert!(n > 0);
            offset += n;
        }
        assert_eq!(received, expected);
    }

    #[test]
    fn pty_preserves_grid_transfers_bytes_and_closes_on_exec() {
        let (mut master, mut slave) = open(120, 40).unwrap();
        raw(&slave);
        assert!(name(master.as_fd()).unwrap().starts_with("/dev/"));
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
            let mut size = libc::winsize {
                ws_row: 0,
                ws_col: 0,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            assert_eq!(unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) }, 0);
            assert_eq!((size.ws_col, size.ws_row), (120, 40));
        }
        master.write_all(b"prompt\n").unwrap();
        read_exact_ready(&mut slave, b"prompt\n");
        slave.write_all(b"\x1b[31mreply\x1b[0m\n").unwrap();
        read_exact_ready(&mut master, b"\x1b[31mreply\x1b[0m\n");
    }

    #[test]
    fn pty_name_rejects_an_unrelated_descriptor() {
        let file = File::open("/dev/null").unwrap();
        assert!(name(file.as_fd()).is_err());
    }

    #[test]
    fn pty_child_owns_a_terminal_and_exchanges_input_output() {
        use std::process::{Command, Stdio};
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let (mut master, slave) = open(120, 40).unwrap();
        raw(&slave);
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "test -t 0 && test -t 1 && test -t 2 || exit 2; read line; stty size; printf 'got:%s\\n' \"$line\""])
            .env("LC_ALL", "C")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        configure_child_session(&mut command);
        let mut child = Child(command.spawn().unwrap());
        master.write_all(b"hello\n").unwrap();
        read_exact_ready(&mut master, b"40 120\ngot:hello\n");
        assert!(child.0.wait().unwrap().success());
    }

    #[test]
    fn simultaneous_ptys_keep_distinct_names() {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    let pair = open(80, 24).unwrap();
                    let name = name(pair.0.as_fd()).unwrap();
                    (pair, name)
                })
            })
            .collect();
        let pairs: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let names: std::collections::HashSet<_> = pairs.iter().map(|(_, name)| name).collect();
        assert_eq!(names.len(), pairs.len());
        for ((master, _), original) in &pairs {
            assert_eq!(&name(master.as_fd()).unwrap(), original);
        }
    }
}
