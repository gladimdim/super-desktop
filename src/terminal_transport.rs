//! Server-side PTY attachment for the upcoming desktop byte transport.
//!
//! Not exposed to network clients yet. Authentication, card ownership and input
//! arbitration belong to the bridge adapter. This object only attaches an
//! existing session, never creates a harness or resizes the host pane. Dropping
//! it reaps exactly its tmux client, not the tmux server/session.
#![allow(dead_code)] // Wired into WSS after the isolated feasibility milestone.

use crate::desktop_protocol::TerminalSize;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub const MAX_CHUNK: usize = 16 * 1024;

#[derive(Debug)]
pub enum Output {
    Bytes(Vec<u8>),
    Pending,
    Closed,
}

pub struct PtyAttachment {
    master: File,
    child: Child,
}

impl PtyAttachment {
    /// `session` must already have been resolved from an owned local card.
    pub fn open(session: &str, size: TerminalSize) -> io::Result<Self> {
        Self::spawn(|| Command::new(crate::tmux::tmux_bin()), session, size)
    }

    fn spawn(
        tmux_command: impl Fn() -> Command,
        session: &str,
        size: TerminalSize,
    ) -> io::Result<Self> {
        size.validate().map_err(invalid)?;
        if !session.starts_with("sd_term_")
            || session.len() > 128
            || !session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(invalid("invalid_owned_session_name"));
        }
        // ignore-size is insufficient when *all* clients have that flag.
        // Require the host model to own sizing before admitting a viewer.
        // This query does not change any session options.
        let policy = tmux_command()
            .args([
                "show-options",
                "-w",
                "-v",
                "-t",
                &format!("={session}:"),
                "window-size",
            ])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()?;
        if !policy.status.success() || policy.stdout != b"manual\n" {
            return Err(invalid("host_grid_not_managed"));
        }
        let mut command = tmux_command();
        let (master, slave) = open_pty(size)?;
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(io::Error::last_os_error());
        }
        command
            .args([
                "-2",
                "attach-session",
                "-f",
                "ignore-size",
                "-t",
                &format!("={session}"),
            ])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // Only async-signal-safe syscalls after fork. std::process has already
        // mapped the slave onto fd 0 before executing this hook.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        Ok(Self { master, child })
    }

    /// Bounded read suitable for a worker loop. Preserve arbitrary bytes:
    /// UTF-8 characters and escape sequences can span multiple frames.
    pub fn read_output(&mut self, timeout: Duration) -> io::Result<Output> {
        let mut fd = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready =
            unsafe { libc::poll(&mut fd, 1, timeout.as_millis().min(i32::MAX as u128) as i32) };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        if ready == 0 {
            return Ok(Output::Pending);
        }
        let mut bytes = vec![0; MAX_CHUNK];
        match self.master.read(&mut bytes) {
            Ok(0) => Ok(Output::Closed),
            Ok(n) => {
                bytes.truncate(n);
                Ok(Output::Bytes(bytes))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(Output::Pending),
            // Linux PTY masters report EIO once the last slave closes.
            Err(e) if e.raw_os_error() == Some(libc::EIO) => Ok(Output::Closed),
            Err(e) => Err(e),
        }
    }

    /// Nonblocking partial write: the caller must respect the returned count
    /// and keep only a bounded pending buffer. Never replay it after reconnect.
    pub fn write_input(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_CHUNK {
            return Err(invalid("input_chunk_too_large"));
        }
        self.master.write(bytes)
    }

    /// Change this client's viewport. The required manual host sizing policy
    /// keeps the owning pane's grid unchanged; ignore-size is additional defense.
    pub fn set_view_size(&mut self, size: TerminalSize) -> io::Result<()> {
        size.validate().map_err(invalid)?;
        let size = winsize(size);
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for PtyAttachment {
    fn drop(&mut self) {
        // Child::kill targets only the client PID. No shell, process group or
        // tmux kill-session command is involved, even on an error path.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn winsize(size: TerminalSize) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows,
        ws_col: size.columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn open_pty(size: TerminalSize) -> io::Result<(File, File)> {
    // Linux TIOCGPTPEER opens the slave directly from the master. CLOEXEC is
    // atomic on both opens so another worker's concurrent spawn cannot inherit
    // a descriptor between openpty() and a later fcntl().
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
    let slave = unsafe { File::from_raw_fd(slave) };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &winsize(size)) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((master, slave))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::Instant;

    const SESSION: &str = "sd_term_transport_probe";

    /// Every test gets its own server/socket/config. Never touches user tmux.
    struct Server {
        directory: PathBuf,
    }

    impl Server {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let directory =
                std::env::temp_dir().join(format!("sd-pty-{}-{serial}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            let server = Self { directory };
            server.run(&[
                "new-session",
                "-d",
                "-s",
                SESSION,
                "-x",
                "120",
                "-y",
                "40",
                "bash",
                "--noprofile",
                "--norc",
            ]);
            server.run(&["set-option", "-t", SESSION, "detach-on-destroy", "on"]);
            server.run(&["set-option", "-t", SESSION, "status", "off"]);
            // Simulate the future host model's explicit grid authority.
            server.run(&["resize-window", "-t", SESSION, "-x", "120", "-y", "40"]);
            server
        }

        fn command(&self) -> Command {
            let mut command = Command::new(crate::tmux::tmux_bin());
            command
                .args([
                    "-S",
                    self.directory.join("socket").to_str().unwrap(),
                    "-f",
                    "/dev/null",
                ])
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .env("PS1", "SD_PROMPT> ");
            command
        }

        fn run(&self, args: &[&str]) -> String {
            let out = self
                .command()
                .args(args)
                .output()
                .expect("tmux must be installed for transport tests");
            assert!(
                out.status.success(),
                "tmux {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        }

        fn attach(&self, columns: u16, rows: u16) -> PtyAttachment {
            PtyAttachment::spawn(|| self.command(), SESSION, TerminalSize { columns, rows })
                .unwrap()
        }

        fn pane(&self) -> String {
            self.run(&[
                "display-message",
                "-p",
                "-t",
                SESSION,
                "#{pane_pid}:#{pane_width}:#{pane_height}",
            ])
        }

        fn clients(&self) -> usize {
            self.run(&["list-clients", "-t", SESSION, "-F", "#{client_pid}"])
                .lines()
                .count()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self
                .command()
                .args(["kill-session", "-t", SESSION])
                .output();
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn until_output(pty: &mut PtyAttachment, marker: &str) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut bytes = Vec::new();
        while Instant::now() < deadline {
            match pty.read_output(Duration::from_millis(50)).unwrap() {
                Output::Bytes(part) => {
                    bytes.extend(part);
                    assert!(bytes.len() < 1024 * 1024, "unbounded test output");
                    if bytes.windows(marker.len()).any(|w| w == marker.as_bytes()) {
                        return bytes;
                    }
                }
                Output::Pending => {}
                Output::Closed => panic!("PTY closed: {}", String::from_utf8_lossy(&bytes)),
            }
        }
        panic!("no {marker:?} in {}", String::from_utf8_lossy(&bytes));
    }

    fn send(pty: &mut PtyAttachment, bytes: &[u8]) {
        let mut rest = bytes;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !rest.is_empty() {
            assert!(Instant::now() < deadline, "blocked input");
            match pty.write_input(rest) {
                Ok(0) => panic!("zero-length PTY write"),
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("PTY input: {e}"),
            }
        }
    }

    fn wait_clients(server: &Server, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while server.clients() != expected {
            assert!(
                Instant::now() < deadline,
                "tmux clients did not reach {expected}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn two_viewers_preserve_host_grid_and_session_across_resize_and_detach() {
        let server = Server::new();
        let original = server.pane();
        assert!(original.ends_with(":120:40"), "{original}");
        let mut first = server.attach(120, 40);
        until_output(&mut first, "SD_PROMPT>");
        let mut second = server.attach(80, 24);
        until_output(&mut second, "SD_PROMPT>");
        wait_clients(&server, 2);
        assert_eq!(server.pane(), original);
        second
            .set_view_size(TerminalSize {
                columns: 60,
                rows: 18,
            })
            .unwrap();
        send(&mut second, b"printf 'SD_%s\\n' RESIZED\r");
        until_output(&mut second, "SD_RESIZED");
        assert_eq!(server.pane(), original);
        drop(second);
        wait_clients(&server, 1);
        send(&mut first, b"printf 'SD_%s\\n' ALIVE\r");
        until_output(&mut first, "SD_ALIVE");
        drop(first);
        wait_clients(&server, 0);
        assert_eq!(server.pane(), original);
        let mut reconnected = server.attach(120, 40);
        until_output(&mut reconnected, "SD_ALIVE");
        assert_eq!(server.pane(), original);
    }

    #[test]
    fn raw_stream_supports_unicode_control_input_and_alternate_screen() {
        let server = Server::new();
        let mut pty = server.attach(120, 40);
        until_output(&mut pty, "SD_PROMPT>");
        send(
            &mut pty,
            "printf '\\033[?1049h\\033[H\\033[31mSD_%s\\033[0m' '🦀'\r".as_bytes(),
        );
        let bytes = until_output(&mut pty, "SD_🦀");
        assert!(bytes.contains(&0x1b), "missing terminal rendering escapes");
        // tmux renders the application's alternate screen into its own outer
        // terminal; it need not forward the nested 1049 sequence verbatim.
        assert_eq!(
            server.run(&["display-message", "-p", "-t", SESSION, "#{alternate_on}"]),
            "1"
        );
        send(
            &mut pty,
            b"printf '\\033[?1049lSD_%s\\n' SLEEPING; sleep 30\r",
        );
        until_output(&mut pty, "SD_SLEEPING");
        assert_eq!(
            server.run(&["display-message", "-p", "-t", SESSION, "#{alternate_on}"]),
            "0"
        );
        send(&mut pty, b"\x03");
        until_output(&mut pty, "SD_PROMPT>");
        send(&mut pty, b"printf 'SD_%s\\n' INTERRUPTED\r");
        until_output(&mut pty, "SD_INTERRUPTED");
    }

    #[test]
    fn unmanaged_grid_is_rejected_without_attaching_or_resizing() {
        let server = Server::new();
        server.run(&["set-option", "-w", "-t", SESSION, "window-size", "latest"]);
        let before = server.pane();
        let result = PtyAttachment::spawn(
            || server.command(),
            SESSION,
            TerminalSize {
                columns: 80,
                rows: 24,
            },
        );
        assert!(matches!(result, Err(ref e) if e.to_string() == "host_grid_not_managed"));
        assert_eq!(server.clients(), 0);
        assert_eq!(server.pane(), before);
    }

    #[test]
    fn raw_output_renders_in_vte() {
        crate::gtk_test::run_in_child_process("terminal_transport::tests::vte_render_inner");
    }

    #[test]
    fn vte_render_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        use vte4::prelude::*;
        gtk4::init().expect("a graphical session is required for the VTE integration check");
        let server = Server::new();
        let mut pty = server.attach(120, 40);
        let term = vte4::Terminal::new();
        term.set_size(120, 40);
        term.feed(&until_output(&mut pty, "SD_PROMPT>"));
        send(&mut pty, "printf '\\033[2J\\033[HSD_%s' '🦀'\r".as_bytes());
        let bytes = until_output(&mut pty, "SD_🦀");
        // Exercise fragmentation inside escape sequences and UTF-8, not just
        // complete strings. WSS boundaries have no character semantics.
        for byte in bytes {
            term.feed(&[byte]);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            while gtk4::glib::MainContext::default().iteration(false) {}
            if term
                .text_format(vte4::Format::Text)
                .is_some_and(|t| t.contains("SD_🦀"))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "VTE did not render raw PTY output"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn rejects_invalid_targets_and_sizes_before_starting_a_process() {
        for session in [
            "foreign",
            "sd_term_*",
            "sd_term_a;touch /tmp/no",
            "sd_term_a:b",
        ] {
            assert!(PtyAttachment::spawn(
                || Command::new("nonexistent-tmux-test"),
                session,
                TerminalSize {
                    columns: 80,
                    rows: 24
                }
            )
            .is_err());
        }
        assert!(PtyAttachment::spawn(
            || Command::new("nonexistent-tmux-test"),
            SESSION,
            TerminalSize {
                columns: 0,
                rows: 24
            }
        )
        .is_err());
    }
}
