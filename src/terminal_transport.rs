//! Server-side PTY attachment for the desktop byte transport.
//!
//! An authenticated remote viewer gets one existing session attached on a
//! private Linux PTY. This object only attaches an existing session, never
//! creates a harness or resizes the host pane. Dropping it reaps exactly its
//! tmux client, not the tmux server/session.
#![allow(dead_code)] // The WSS adapter and its tests use this module.

use crate::desktop_protocol::TerminalSize;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Largest read one attachment forwards. The same bound the viewer's frame
/// limit uses, so a chunk is never split for the sake of a size limit.
pub const MAX_CHUNK: usize = crate::desktop_protocol::ATTACH_MAX_CHUNK;

#[derive(Debug)]
pub enum Output {
    Bytes(Vec<u8>),
    Pending,
    Closed,
}

pub struct PtyAttachment {
    master: File,
    child: Child,
    session: String,
    prompt_input: crate::prompt_history::InputTracker,
}

impl PtyAttachment {
    /// Attach one viewer at a grid read moments earlier.
    ///
    /// `session` must already have been resolved from an owned local card. The
    /// grid must still be the one the host owns or the attach is refused; the
    /// caller reads it before the upgrade so a viewer can be told why it has no
    /// stream yet, and the returned grid is the one its emulator must match.
    pub fn open_at(session: &str, grid: TerminalSize) -> io::Result<(Self, TerminalSize)> {
        Self::open_with(|| Command::new(crate::tmux::tmux_bin()), session, grid)
    }

    fn open_with(
        tmux_command: impl Fn() -> Command,
        session: &str,
        grid: TerminalSize,
    ) -> io::Result<(Self, TerminalSize)> {
        Ok((Self::spawn(tmux_command, session, grid)?, grid))
    }

    /// The owned session this attachment belongs to. Already validated as an
    /// `sd_term_*` name; used to arbitrate remote input against the same
    /// per-session guard as local and phone input.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The host's live client grid for this session. The host is the only
    /// authority: a viewer's reported size is applied only when it equals this.
    pub fn host_grid(&self) -> io::Result<TerminalSize> {
        client_grid(&|| Command::new(crate::tmux::tmux_bin()), &self.session)
    }

    /// Poll the PTY master, for a loop that also watches the network socket.
    pub fn raw_fd(&self) -> std::os::fd::RawFd {
        self.master.as_raw_fd()
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
        // A tmux client whose terminal is exactly the grid the host already
        // renders cannot change the host pane under ANY `window-size` policy:
        // `latest`/`largest`/`smallest` all arbitrate to the same size, and
        // `manual` ignores clients. Attaching at any other size could shrink or
        // grow the host's grid, so it is refused instead. `ignore-size` is
        // additional defence for the resize case (see `set_view_size`).
        if client_grid(&tmux_command, session)? != size {
            return Err(invalid("host_grid_mismatch"));
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
        Ok(Self {
            master,
            child,
            session: session.to_string(),
            prompt_input: crate::prompt_history::InputTracker::default(),
        })
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
        let count = self.master.write(bytes)?;
        for prompt in self.prompt_input.feed(&bytes[..count]) {
            crate::prompt_history::record(&self.session, &prompt);
        }
        Ok(count)
    }

    /// Change this client's own viewport. The attachment carries `ignore-size`,
    /// so while the host has any other client this cannot change the host grid.
    /// Callers only ever pass the grid the host itself reported.
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

/// The grid tmux currently renders into this session's clients.
///
/// Used before an upgrade so the host can report "no grid yet" as a status
/// instead of opening a stream it cannot size.
pub fn grid(session: &str) -> io::Result<TerminalSize> {
    client_grid(&|| Command::new(crate::tmux::tmux_bin()), session)
}

/// The terminal grid tmux renders into one of this session's clients.
///
/// A client's terminal is the window plus the status line(s) tmux draws on it,
/// so this is exactly the grid a viewer's emulator has to match. It is derived
/// from the live window size, which is what makes attaching at it a no-op for
/// the host's own grid. Reads no state and changes no option.
fn client_grid(tmux_command: &impl Fn() -> Command, session: &str) -> io::Result<TerminalSize> {
    if !session.starts_with("sd_term_")
        || session.len() > 128
        || !session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(invalid("invalid_owned_session_name"));
    }
    let output = tmux_command()
        .args([
            "display-message",
            "-p",
            "-t",
            &format!("={session}:"),
            "#{window_width} #{window_height} #{status}",
        ])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()?;
    if !output.status.success() {
        return Err(invalid("host_grid_unavailable"));
    }
    parse_client_grid(&String::from_utf8_lossy(&output.stdout))
}

/// `"<columns> <rows> <status>"` as reported by `client_grid`'s tmux query.
fn parse_client_grid(text: &str) -> io::Result<TerminalSize> {
    let mut fields = text.split_whitespace();
    let unavailable = || invalid("host_grid_unavailable");
    let columns: u16 = fields.next().and_then(|v| v.parse().ok()).ok_or_else(unavailable)?;
    let rows: u16 = fields.next().and_then(|v| v.parse().ok()).ok_or_else(unavailable)?;
    let status_rows: u16 = match fields.next().ok_or_else(unavailable)? {
        "off" | "0" => 0,
        "on" => 1,
        value => value.parse().map_err(|_| unavailable())?,
    };
    TerminalSize {
        columns,
        rows: rows.checked_add(status_rows).ok_or_else(unavailable)?,
    }
    .validate()
    .map_err(invalid)
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

        /// The grid the host owns, as `PtyAttachment::open` derives it.
        fn grid(&self) -> TerminalSize {
            let command = || self.command();
            client_grid(&command, SESSION).unwrap()
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
        // Both viewers attach at the grid the host already owns. That is the
        // only size this transport admits, and it is what makes a second viewer
        // unable to resize the host's pane.
        assert_eq!(server.grid(), TerminalSize { columns: 120, rows: 40 });
        let mut first = server.attach(120, 40);
        until_output(&mut first, "SD_PROMPT>");
        let mut second = server.attach(120, 40);
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
    fn typed_input_reaches_the_shell_and_detach_keeps_the_session() {
        // The same write path remote keystrokes take: bytes into the PTY
        // master, shell echo and output back out, detach reaping only its
        // own tmux client.
        let server = Server::new();
        let before = server.pane();
        let mut pty = server.attach(120, 40);
        until_output(&mut pty, "SD_PROMPT>");
        pty.write_input(b"echo TRANSPORT_DUPLEX_MARK\n").unwrap();
        let out = until_output(&mut pty, "TRANSPORT_DUPLEX_MARK");
        assert!(String::from_utf8_lossy(&out).contains("TRANSPORT_DUPLEX_MARK"));
        // An interrupt is just another byte on the same path.
        pty.write_input(b"sleep 30\n").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        pty.write_input(b"\x03").unwrap();
        until_output(&mut pty, "SD_PROMPT>");
        drop(pty);
        wait_clients(&server, 0);
        assert_eq!(server.pane(), before);
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
    fn a_viewer_that_would_resize_the_host_is_rejected_instead_of_attached() {
        let server = Server::new();
        // The production default: tmux sizes the window from its latest client.
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
        assert!(matches!(result, Err(ref e) if e.to_string() == "host_grid_mismatch"));
        assert_eq!(server.clients(), 0);
        assert_eq!(server.pane(), before);
        // The host's own grid is admitted, and attaching at it is a no-op.
        let (attachment, grid) = PtyAttachment::open_with(
            || server.command(),
            SESSION,
            TerminalSize {
                columns: 120,
                rows: 40,
            },
        )
        .unwrap();
        assert_eq!(grid, TerminalSize { columns: 120, rows: 40 });
        assert_eq!(server.pane(), before);
        drop(attachment);
        assert_eq!(server.pane(), before);
    }

    #[test]
    fn the_client_grid_adds_the_status_lines_tmux_draws() {
        assert_eq!(
            parse_client_grid("120 40 off\n").unwrap(),
            TerminalSize {
                columns: 120,
                rows: 40
            }
        );
        assert_eq!(
            parse_client_grid("80 23 on\n").unwrap(),
            TerminalSize {
                columns: 80,
                rows: 24
            }
        );
        assert_eq!(
            parse_client_grid("80 22 2\n").unwrap(),
            TerminalSize {
                columns: 80,
                rows: 24
            }
        );
        for bad in ["", "120", "120 40", "120 40 sideways", "0 40 off", "120 0 off"] {
            assert!(parse_client_grid(bad).is_err(), "{bad:?}");
        }
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
