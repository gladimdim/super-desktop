//! Pause the local cards' `tmux attach` clients while the overlay stays hidden.
//!
//! A hidden overlay keeps every card's VTE and its `tmux attach` client, so the
//! next show is instant. tmux still renders every pane change into those
//! clients, and the daemon's main thread reads and parses all of it: ~70–80 KB/s
//! and ~100 wakeups/s for one busy agent, with nothing on screen.
//!
//! After [`PAUSE_AFTER`] hidden, `suspend-client` stops tmux writing to each
//! client (0 bytes measured, down from ~70 KB/s). The client process keeps
//! running: VTE makes it the leader of its own session, so its process group
//! is orphaned and the kernel discards the SIGTSTP it sends itself. On show,
//! SIGCONT makes it send tmux MSG_WAKEUP, and tmux redraws the whole screen
//! (~7 KB for a 200×56 card; ten cards were redrawn within 3 ms).
//!
//! Sizing. A suspended client no longer counts when tmux sizes a window, under
//! every `window-size` policy. So a card is only paused while its client is the
//! only one that can size its session: no other non-control client (a terminal,
//! ssh, another PC's view) and no control client that may have set a size.
//! tmux does not report whether a control client ran `refresh-client -C`, so a
//! control client counts unless it was attached with `ignore-size`, as the
//! phone's are (they never set a size). A session that has such a client keeps
//! its card live, exactly as before this pause existed.
//!
//! A client that attaches while the card is paused sizes the window, as it
//! would under `latest` as soon as it is used, and the window keeps that size
//! after it leaves until the overlay is shown and the card's client counts
//! again. The same goes for an `ignore-size` control client that sets a size.
//!
//! A suspended client does not count as attached either: with
//! `destroy-unattached` on, tmux destroys the session. Such sessions are never
//! paused.
//!
//! If the daemon exits or crashes while paused, the kernel hangs up the VTE
//! ptys and the clients exit (~5 ms measured); nothing is left behind in tmux.

use std::process::Command;
use std::time::Duration;

/// How long the overlay stays hidden before its clients are paused. Quick
/// hide/show toggles pay nothing.
pub const PAUSE_AFTER: Duration = Duration::from_secs(30);

/// Every attached client of every session, in one call.
const LISTING: [&str; 3] = [
    "list-clients",
    "-F",
    "#{client_pid}\t#{client_tty}\t#{client_name}\t#{session_name}\t\
     #{client_control_mode}\t#{client_flags}\t#{destroy-unattached}",
];

/// One card's session and the pty of its VTE, which is the tty of the
/// `tmux attach` client the VTE spawned (`/dev/pts/N`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub session: String,
    pub tty: String,
}

/// The clients [`pause`] suspended, for [`resume_clients`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Paused {
    pub pids: Vec<i32>,
}

impl Paused {
    pub fn absorb(&mut self, other: Paused) {
        self.pids.extend(other.pids);
    }
}

/// Hide/show bookkeeping, kept free of GTK and tmux so it can be tested.
///
/// Every hide and show bumps a generation. A pause timer or a pause worker
/// only acts on the generation it was started for, so a show always wins over
/// a pause that was already under way.
#[derive(Debug, Default)]
pub struct PauseState {
    generation: u64,
    hidden: bool,
    phase: Phase,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    #[default]
    Live,
    Pausing,
    Paused,
}

impl PauseState {
    /// The overlay was hidden. Returns the token to arm the pause timer with,
    /// or `None` when it already was hidden (the timer is already armed).
    pub fn hidden(&mut self) -> Option<u64> {
        if self.hidden {
            return None;
        }
        self.hidden = true;
        self.generation = self.generation.wrapping_add(1);
        Some(self.generation)
    }

    /// The pause timer fired. `true` = start pausing now.
    pub fn timer_fired(&mut self, token: u64) -> bool {
        if token != self.generation || !self.hidden || self.phase != Phase::Live {
            return false;
        }
        self.phase = Phase::Pausing;
        true
    }

    /// Whether a pause worker for `token` should keep going.
    pub fn still_wanted(&self, token: u64) -> bool {
        token == self.generation && self.hidden && self.phase == Phase::Pausing
    }

    /// A pause worker finished. `true` = keep its clients paused; `false` = the
    /// overlay was shown meanwhile, so the caller resumes them right away.
    pub fn pause_finished(&mut self, token: u64) -> bool {
        if self.still_wanted(token) {
            self.phase = Phase::Paused;
            return true;
        }
        false
    }

    /// The overlay is shown again. Returns whether clients were paused.
    pub fn shown(&mut self) -> bool {
        self.hidden = false;
        self.generation = self.generation.wrapping_add(1);
        std::mem::replace(&mut self.phase, Phase::Live) == Phase::Paused
    }
}

/// The overlay's side: arms a pause when it hides and undoes it when it shows.
pub struct Controller {
    state: std::cell::RefCell<PauseState>,
    paused: std::cell::RefCell<Paused>,
    /// The token a pause worker may still act for (0 = none), read off the GTK
    /// thread so a show stops the worker before its next client.
    wanted: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Controller {
    pub fn new() -> std::rc::Rc<Self> {
        std::rc::Rc::new(Self {
            state: Default::default(),
            paused: Default::default(),
            wanted: Default::default(),
        })
    }

    /// The overlay is off screen: pause after [`PAUSE_AFTER`] unless it is
    /// shown first. `targets` is asked when the timer fires, so a card created
    /// or closed in between is seen as it is then.
    pub fn on_hidden(self: &std::rc::Rc<Self>, targets: impl Fn() -> Vec<Target> + 'static) {
        let Some(token) = self.state.borrow_mut().hidden() else {
            return;
        };
        let weak = std::rc::Rc::downgrade(self);
        gtk4::glib::timeout_add_local_once(PAUSE_AFTER, move || {
            if let Some(controller) = weak.upgrade() {
                controller.start_pause(token, targets());
            }
        });
    }

    fn start_pause(self: &std::rc::Rc<Self>, token: u64, targets: Vec<Target>) {
        use std::sync::atomic::Ordering;
        if !self.state.borrow_mut().timer_fired(token) {
            return;
        }
        self.wanted.store(token, Ordering::SeqCst);
        let wanted = std::sync::Arc::clone(&self.wanted);
        let weak = std::rc::Rc::downgrade(self);
        gtk4::glib::MainContext::default().spawn_local(async move {
            let result = gtk4::gio::spawn_blocking(move || {
                pause(&tmux_command, &targets, &|| wanted.load(Ordering::SeqCst) == token)
            })
            .await
            .unwrap_or_default();
            match weak.upgrade() {
                Some(controller) if controller.state.borrow_mut().pause_finished(token) => {
                    controller.paused.borrow_mut().absorb(result);
                }
                // Shown (or gone) while the worker ran: wake what it paused.
                _ => resume_clients(&result.pids),
            }
        });
    }

    /// The overlay is about to be shown: wake every paused client first, so
    /// its redraw (a few ms) lands before the slide-in's first frames.
    pub fn on_shown(&self) {
        self.wanted.store(0, std::sync::atomic::Ordering::SeqCst);
        if self.state.borrow_mut().shown() {
            let paused = std::mem::take(&mut *self.paused.borrow_mut());
            resume_clients(&paused.pids);
        }
    }

    /// The daemon is about to exit. Its clients would exit with their ptys
    /// anyway; waking them first lets each leave as a normal client.
    pub fn resume_before_exit(&self) {
        self.wanted.store(0, std::sync::atomic::Ordering::SeqCst);
        let paused = std::mem::take(&mut *self.paused.borrow_mut());
        resume_clients(&paused.pids);
    }
}

/// `tmux` for the user's server, as every other card command runs it.
pub fn tmux_command() -> Command {
    let mut command = Command::new(crate::tmux::tmux_bin());
    command.env_remove("TMUX").env_remove("TMUX_PANE");
    command
}

#[derive(Debug, PartialEq, Eq)]
struct Client<'a> {
    pid: i32,
    tty: &'a str,
    name: &'a str,
    session: &'a str,
    control: bool,
    ignore_size: bool,
    keeps_unattached: bool,
}

fn parse_client(line: &str) -> Option<Client<'_>> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [pid, tty, name, session, control, flags, destroy] = fields.as_slice() else {
        return None;
    };
    let pid = pid.parse::<i32>().ok().filter(|pid| *pid > 0)?;
    Some(Client {
        pid,
        tty: *tty,
        name: *name,
        session: *session,
        control: *control == "1",
        ignore_size: flags.split(',').any(|flag| flag == "ignore-size"),
        keeps_unattached: matches!(*destroy, "off" | "0"),
    })
}

/// The clients to suspend, as `(pid, client name)`, from [`LISTING`]'s output.
///
/// A target is taken only when tmux lists a client on its tty attached to its
/// own session, the session survives having no attached client, and no other
/// client of that session can size its windows.
fn plan(targets: &[Target], listing: &str) -> Vec<(i32, String)> {
    let clients: Vec<Client> = listing.lines().filter_map(parse_client).collect();
    let mut plans = Vec::new();
    for target in targets {
        let Some(own) = clients
            .iter()
            .find(|client| client.tty == target.tty && client.session == target.session)
        else {
            continue;
        };
        if own.name.is_empty() || !own.keeps_unattached {
            continue;
        }
        let shared = clients.iter().any(|other| {
            other.pid != own.pid
                && other.session == target.session
                && (!other.control || !other.ignore_size)
        });
        if !shared {
            plans.push((own.pid, own.name.to_string()));
        }
    }
    plans
}

/// The terminal name (`/dev/pts/N`) of a pty master: the tty tmux reports for
/// the client running on it.
pub fn pty_name(master: std::os::fd::BorrowedFd<'_>) -> Option<String> {
    use std::os::fd::AsRawFd;
    let mut buffer = [0 as libc::c_char; 128];
    if unsafe { libc::ptsname_r(master.as_raw_fd(), buffer.as_mut_ptr(), buffer.len()) } != 0 {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
    name.to_str().ok().filter(|name| name.starts_with("/dev/")).map(str::to_string)
}

fn run(tmux: &dyn Fn() -> Command, args: &[&str]) -> Option<String> {
    let output = tmux().args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Suspend each target's client that [`plan`] allows. Blocking: run it off the
/// GTK thread. `keep_going` is asked before each client, so a show stops the
/// work early. Every client asked to suspend is returned, even if tmux refused:
/// waking a client that was never suspended does nothing.
pub fn pause(tmux: &dyn Fn() -> Command, targets: &[Target], keep_going: &dyn Fn() -> bool) -> Paused {
    let mut paused = Paused::default();
    if targets.is_empty() {
        return paused;
    }
    let Some(listing) = run(tmux, &LISTING) else {
        return paused;
    };
    for (pid, client) in plan(targets, &listing) {
        if !keep_going() {
            break;
        }
        paused.pids.push(pid);
        let _ = run(tmux, &["suspend-client", "-t", &client]);
    }
    paused
}

/// Wake paused clients: SIGCONT makes a tmux client send MSG_WAKEUP, and tmux
/// redraws its whole screen. Cheap enough for the GTK thread (one `/proc` read
/// and one `kill` per client). A pid that is no longer a tmux client of this
/// process (card closed, client exited, pid reused) is left alone; waking a
/// client that was not suspended does nothing.
pub fn resume_clients(pids: &[i32]) {
    let parent = std::process::id() as i32;
    for &pid in pids {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        if is_live_tmux_child(&stat, parent) {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGCONT);
            }
        }
    }
}

/// `/proc/<pid>/stat` names a running `tmux` whose parent is `parent`.
fn is_live_tmux_child(stat: &str, parent: i32) -> bool {
    let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
        return false;
    };
    if close < open {
        return false;
    }
    let comm = &stat[open + 1..close];
    let mut rest = stat[close + 1..].split_whitespace();
    let state = rest.next().unwrap_or("");
    let ppid = rest.next().and_then(|v| v.parse::<i32>().ok());
    comm.starts_with("tmux") && !matches!(state, "Z" | "X" | "x") && ppid == Some(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, AsRawFd, FromRawFd};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Child, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn hidden_pause_waits_for_the_timer_of_the_current_hide() {
        let mut state = PauseState::default();
        let first = state.hidden().expect("first hide arms the timer");
        assert_eq!(state.hidden(), None, "a second hide of the same hidden period re-arms nothing");
        assert!(!state.shown(), "nothing was paused yet");
        assert!(!state.timer_fired(first), "a show cancels the timer of the hide before it");

        let second = state.hidden().unwrap();
        assert_ne!(first, second);
        assert!(!state.timer_fired(first), "a stale timer never pauses");
        assert!(state.timer_fired(second));
        assert!(!state.timer_fired(second), "one pause per hidden period");
        assert!(state.still_wanted(second));
        assert!(state.pause_finished(second));
        assert!(state.shown(), "the show resumes what was paused");
        assert!(!state.shown(), "and only once");
    }

    #[test]
    fn hidden_pause_show_during_a_pause_resumes_its_result() {
        let mut state = PauseState::default();
        let token = state.hidden().unwrap();
        assert!(state.timer_fired(token));
        assert!(!state.shown(), "the pause is still under way: nothing to resume yet");
        assert!(!state.still_wanted(token), "the worker stops at the next client");
        assert!(!state.pause_finished(token), "the caller resumes the worker's result itself");

        // Shown, then hidden again before the old worker returned.
        let mut state = PauseState::default();
        let old = state.hidden().unwrap();
        assert!(state.timer_fired(old));
        state.shown();
        let new = state.hidden().unwrap();
        assert!(!state.pause_finished(old), "a worker of an earlier hide never counts as paused");
        assert!(state.timer_fired(new), "the new hidden period still pauses on its own timer");
        assert!(state.pause_finished(new));
        assert!(state.shown());
    }

    #[test]
    fn hidden_pause_plan_only_takes_clients_that_alone_size_their_session() {
        let target = |session: &str, tty: &str| Target { session: session.into(), tty: tty.into() };
        let targets = [
            target("sd_term_a", "/dev/pts/1"),
            target("sd_term_b", "/dev/pts/2"),
            target("sd_term_c", "/dev/pts/3"),
            target("sd_term_d", "/dev/pts/4"),
            target("sd_term_e", "/dev/pts/5"),
            target("sd_term_f", "/dev/pts/6"),
            target("sd_term_g", "/dev/pts/7"),
        ];
        let line = |pid: i32, tty: &str, session: &str, control: bool, flags: &str, destroy: &str| {
            format!(
                "{pid}\t{tty}\tclient-{pid}\t{session}\t{}\t{flags}\t{destroy}\n",
                if control { 1 } else { 0 }
            )
        };
        let listing = [
            // a: alone, with the phone's unsized control clients.
            line(1, "/dev/pts/1", "sd_term_a", false, "attached,focused,UTF-8", "off"),
            line(11, "", "sd_term_a", true, "attached,control-mode,ignore-size,no-output,UTF-8", "off"),
            line(12, "", "sd_term_a", true, "attached,control-mode,ignore-size,UTF-8", "off"),
            // b: the user's own terminal is attached too.
            line(2, "/dev/pts/2", "sd_term_b", false, "attached,UTF-8", "off"),
            line(21, "/dev/pts/21", "sd_term_b", false, "attached,focused,UTF-8", "off"),
            // c: another PC's view (an ignore-size pty client) is attached.
            line(3, "/dev/pts/3", "sd_term_c", false, "attached,UTF-8", "off"),
            line(31, "/dev/pts/31", "sd_term_c", false, "attached,ignore-size,UTF-8", "off"),
            // d: a control client that may have run `refresh-client -C`.
            line(4, "/dev/pts/4", "sd_term_d", false, "attached,UTF-8", "off"),
            line(41, "", "sd_term_d", true, "attached,control-mode,UTF-8", "off"),
            // e: destroyed once no client counts as attached.
            line(5, "/dev/pts/5", "sd_term_e", false, "attached,UTF-8", "on"),
            // f: its tty belongs to another session's client.
            line(6, "/dev/pts/6", "sd_term_other", false, "attached,UTF-8", "off"),
            // g: no client on its tty at all (not spawned yet, or exited).
            "garbage line\n".to_string(),
        ]
        .concat();
        assert_eq!(plan(&targets, &listing), vec![(1, "client-1".to_string())]);
    }

    #[test]
    fn hidden_pause_resume_only_signals_live_tmux_children() {
        let me = 4242;
        assert!(is_live_tmux_child("77 (tmux: client) S 4242 77 77 0", me));
        assert!(!is_live_tmux_child("77 (tmux: client) Z 4242 77 77 0", me), "zombie");
        assert!(!is_live_tmux_child("77 (tmux: client) S 1 77 77 0", me), "not our child");
        assert!(!is_live_tmux_child("77 (bash) S 4242 77 77 0", me), "pid reused");
        assert!(!is_live_tmux_child("77 (a) b) S 4242", me));
        assert!(!is_live_tmux_child("", me));
    }

    /// Own server, socket and config per test. Never touches the user's tmux.
    struct Server {
        directory: PathBuf,
    }

    impl Server {
        fn new(session: &str, columns: u16, rows: u16, program: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let directory =
                std::env::temp_dir().join(format!("sd-hidden-{}-{serial}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            let server = Self { directory };
            let (columns, rows) = (columns.to_string(), rows.to_string());
            server.run(&["new-session", "-d", "-s", session, "-x", &columns, "-y", &rows, program]);
            server.run(&["set-option", "-g", "window-size", "latest"]);
            server
        }

        fn command(&self) -> Command {
            let mut command = Command::new(crate::tmux::tmux_bin());
            command
                .args(["-S", self.directory.join("socket").to_str().unwrap(), "-f", "/dev/null"])
                .env_remove("TMUX")
                .env_remove("TMUX_PANE");
            command
        }

        fn run(&self, args: &[&str]) -> String {
            let out = self.command().args(args).output().expect("tmux must be installed");
            assert!(out.status.success(), "tmux {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        }

        fn window_size(&self, session: &str) -> String {
            self.run(&["display-message", "-p", "-t", &format!("={session}:"), "#{window_width}x#{window_height}"])
        }

        /// A client like a card's: `tmux attach` in its own pty, leading its
        /// own session (so its process group is orphaned, as under VTE).
        fn attach(&self, session: &str, columns: u16, rows: u16) -> Attached {
            let mut master_fd = -1;
            let mut slave_fd = -1;
            let size = libc::winsize { ws_row: rows, ws_col: columns, ws_xpixel: 0, ws_ypixel: 0 };
            let opened = unsafe {
                libc::openpty(&mut master_fd, &mut slave_fd, std::ptr::null_mut(), std::ptr::null(), &size)
            };
            assert_eq!(opened, 0, "openpty");
            let master = unsafe { std::fs::File::from_raw_fd(master_fd) };
            let slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
            unsafe {
                libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
                libc::fcntl(slave.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
            }
            let tty = pty_name(master.as_fd()).expect("pty name");
            let mut command = self.command();
            command
                .args(["-2", "attach-session", "-t", &format!("={session}")])
                .env("TERM", "xterm-256color")
                .stdin(Stdio::from(slave.try_clone().unwrap()))
                .stdout(Stdio::from(slave.try_clone().unwrap()))
                .stderr(Stdio::from(slave));
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().unwrap();
            let bytes = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&bytes);
            std::thread::spawn(move || {
                let mut master = master;
                let mut buffer = [0u8; 65536];
                while let Ok(n) = master.read(&mut buffer) {
                    if n == 0 {
                        break;
                    }
                    counter.fetch_add(n, Ordering::Relaxed);
                }
            });
            Attached { child, bytes, tty }
        }

        /// A control client like the phone bridge's.
        fn control(&self, session: &str, flags: &str) -> Child {
            self.command()
                .args(["-C", "attach-session", "-f", flags, "-t", &format!("={session}")])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.command().arg("kill-server").output();
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    struct Attached {
        child: Child,
        bytes: Arc<AtomicUsize>,
        tty: String,
    }

    impl Attached {
        fn target(&self, session: &str) -> Target {
            Target { session: session.into(), tty: self.tty.clone() }
        }

        fn bytes_during(&self, period: Duration) -> usize {
            let before = self.bytes.load(Ordering::Relaxed);
            std::thread::sleep(period);
            self.bytes.load(Ordering::Relaxed) - before
        }
    }

    impl Drop for Attached {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    const BUSY: &str = "while :; do seq -f 'row %g of a busy agent xxxxxxxxxxxxxxxx' 1 30; sleep 0.02; done";

    #[test]
    fn hidden_pause_stops_output_and_redraws_on_resume() {
        const SESSION: &str = "sd_term_hidden_pause";
        let server = Server::new(SESSION, 100, 30, BUSY);
        let mut card = server.attach(SESSION, 120, 41);
        wait_until("the card's client to attach", || server.window_size(SESSION) == "120x40");
        assert!(card.bytes_during(Duration::from_millis(300)) > 0, "a live client receives output");
        // The phone's control clients do not keep a card live.
        let mut phone = server.control(SESSION, "ignore-size,no-output");
        std::thread::sleep(Duration::from_millis(200));

        let tmux = || server.command();
        let paused = pause(&tmux, &[card.target(SESSION)], &|| true);
        assert_eq!(paused.pids, vec![card.child.id() as i32]);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(card.bytes_during(Duration::from_millis(500)), 0, "a paused client receives nothing");
        assert!(card.child.try_wait().unwrap().is_none(), "the paused client keeps running");
        assert_eq!(server.window_size(SESSION), "120x40", "pausing alone never resizes");

        let before = card.bytes.load(Ordering::Relaxed);
        let woken = Instant::now();
        resume_clients(&paused.pids);
        wait_until("the redraw after resume", || card.bytes.load(Ordering::Relaxed) > before);
        assert!(woken.elapsed() < Duration::from_millis(500));
        assert_eq!(server.window_size(SESSION), "120x40");
        assert!(card.bytes_during(Duration::from_millis(300)) > 0, "output flows again");
        drop(phone.stdin.take());
        let _ = phone.wait();
    }

    #[test]
    fn hidden_pause_leaves_a_session_live_while_another_client_can_size_it() {
        const SESSION: &str = "sd_term_hidden_shared";
        let server = Server::new(SESSION, 100, 30, BUSY);
        let card = server.attach(SESSION, 120, 41);
        wait_until("the card's client to attach", || server.window_size(SESSION) == "120x40");
        let tmux = || server.command();

        // The user attaches from another terminal: under `latest` it sizes the
        // window, and the card must stay live so the size keeps following.
        let other = server.attach(SESSION, 90, 25);
        wait_until("the window to follow the other client", || server.window_size(SESSION) == "90x24");
        assert!(pause(&tmux, &[card.target(SESSION)], &|| true).pids.is_empty());
        assert!(card.bytes_during(Duration::from_millis(300)) > 0, "the card stays live");
        drop(other);
        wait_until("the window to follow the card again", || server.window_size(SESSION) == "120x40");

        // A control client that may have set a size keeps the card live too.
        let mut sized = server.control(SESSION, "no-output");
        sized.stdin.as_mut().unwrap().write_all(b"refresh-client -C 80x24\n").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(pause(&tmux, &[card.target(SESSION)], &|| true).pids.is_empty());
        assert!(card.bytes_during(Duration::from_millis(300)) > 0, "the card stays live");
        drop(sized.stdin.take());
        let _ = sized.wait();
    }

    #[test]
    fn hidden_pause_skips_sessions_that_die_unattached() {
        const SESSION: &str = "sd_term_hidden_fragile";
        let server = Server::new(SESSION, 100, 30, "exec sleep 100000");
        let card = server.attach(SESSION, 120, 41);
        wait_until("the card's client to attach", || server.window_size(SESSION) == "120x40");
        // Set once attached: an unattached session with it is destroyed at once.
        server.run(&["set-option", "-t", SESSION, "destroy-unattached", "on"]);
        let tmux = || server.command();
        let paused = pause(&tmux, &[card.target(SESSION)], &|| true);
        assert!(paused.pids.is_empty(), "suspending the only client would destroy this session");
        std::thread::sleep(Duration::from_millis(200));
        assert!(server.command().args(["has-session", "-t", SESSION]).status().unwrap().success());
    }
}
