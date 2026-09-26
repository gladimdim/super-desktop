//! "Jump to newest" for local cards.
//!
//! A card's terminal runs `tmux attach`, and with tmux's `mouse` option on
//! (Omarchy's default) scrolling a card enters tmux copy mode: the pane shows
//! its history and stops following new output until copy mode ends. While it
//! is in copy mode the card shows **↓ Jump to newest**, which ends it.
//!
//! The card learns the mode from the overlay's once-a-second pane listing
//! (`tmux::PaneRow::in_mode`) and, so the button appears without that wait,
//! from one `display-message` shortly after the user scrolls in the card or
//! types while the button shows (typing `q` or Enter also ends copy mode).
use gtk4::{glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long after the last scroll or key the card asks tmux: a fling or a
/// burst of typing costs one question.
const CHECK_DELAY: Duration = Duration::from_millis(150);
/// A listing taken before a jump may still report copy mode; it is not
/// believed for this long after the jump.
const JUMP_SETTLE: Duration = Duration::from_millis(1500);

/// A card's questions to tmux about its pane. Tests point it at their own
/// server.
#[derive(Clone)]
pub struct Scrollback {
    tmux: Arc<dyn Fn() -> Command + Send + Sync>,
}

impl Scrollback {
    /// The user's tmux server, as every other card command uses it.
    pub fn user() -> Self {
        Self::with(crate::hidden_pause::tmux_command)
    }

    pub fn with(tmux: impl Fn() -> Command + Send + Sync + 'static) -> Self {
        Self { tmux: Arc::new(tmux) }
    }

    /// Whether `session`'s pane is in copy mode; `None` when tmux did not
    /// answer (no server, or no such session).
    pub fn in_mode(&self, session: &str) -> Option<bool> {
        let out = (self.tmux)()
            .args(["display-message", "-p", "-t", &target(session), "#{pane_in_mode}"])
            .output()
            .ok()?;
        // tmux answers a missing session with success and no output.
        match (out.status.success(), String::from_utf8_lossy(&out.stdout).trim()) {
            (true, "1") => Some(true),
            (true, "0") => Some(false),
            _ => None,
        }
    }

    /// End copy mode (and any other mode) in `session`'s pane, so it follows
    /// the newest output again.
    pub fn jump_to_newest(&self, session: &str) -> bool {
        (self.tmux)()
            .args(["copy-mode", "-q", "-t", &target(session)])
            .output()
            .is_ok_and(|out| out.status.success())
    }
}

/// The active pane of `session`'s current window, the session matched exactly.
fn target(session: &str) -> String {
    format!("={session}:")
}

/// A card's **↓ Jump to newest** button and what decides whether it shows.
#[derive(Clone)]
pub struct JumpButton {
    pub button: gtk4::Button,
    session: Rc<str>,
    scrollback: Rc<RefCell<Scrollback>>,
    /// Bumped by every check and jump: only the latest may change the button.
    generation: Rc<Cell<u64>>,
    jumped_at: Rc<Cell<Option<Instant>>>,
    /// Whether the card shows a live terminal (not iconified, not a preview).
    live: Rc<dyn Fn() -> bool>,
}

impl JumpButton {
    pub fn new(session: &str, live: impl Fn() -> bool + 'static) -> Self {
        let button = gtk4::Button::with_label("↓ Jump to newest");
        button.add_css_class("term-jump-newest");
        button.set_tooltip_text(Some("Leave the history and follow new output"));
        button.set_halign(gtk4::Align::Center);
        button.set_valign(gtk4::Align::End);
        button.set_margin_bottom(10);
        // The keyboard stays with the terminal underneath.
        button.set_focusable(false);
        button.set_focus_on_click(false);
        button.set_visible(false);
        let jump = Self {
            button,
            session: Rc::from(session),
            scrollback: Rc::new(RefCell::new(Scrollback::user())),
            generation: Rc::new(Cell::new(0)),
            jumped_at: Rc::new(Cell::new(None)),
            live: Rc::new(live),
        };
        // The handler gets the button as an argument; holding it here too
        // would keep it alive through its own signal.
        let (scrollback, generation, jumped_at, session) = (
            Rc::clone(&jump.scrollback),
            Rc::clone(&jump.generation),
            Rc::clone(&jump.jumped_at),
            Rc::clone(&jump.session),
        );
        jump.button.connect_clicked(move |button| {
            button.set_visible(false);
            generation.set(generation.get().wrapping_add(1));
            jumped_at.set(Some(Instant::now()));
            let scrollback = scrollback.borrow().clone();
            let session = session.to_string();
            gtk4::gio::spawn_blocking(move || scrollback.jump_to_newest(&session));
        });
        jump
    }

    /// The pane's mode from a fresh question to tmux.
    fn show_for(&self, in_mode: bool) {
        let visible = in_mode && (self.live)();
        if self.button.is_visible() != visible {
            self.button.set_visible(visible);
        }
    }

    /// The pane's mode from the overlay's periodic listing, which may predate
    /// a jump the user just made.
    pub fn listed(&self, in_mode: bool) {
        let settling = self.jumped_at.get().is_some_and(|at| at.elapsed() < JUMP_SETTLE);
        if in_mode && settling {
            return;
        }
        self.show_for(in_mode);
    }

    pub fn hide(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.button.set_visible(false);
    }

    /// The user scrolled, or typed while the button shows: ask tmux shortly.
    pub fn check_soon(&self) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let this = self.clone();
        glib::timeout_add_local_once(CHECK_DELAY, move || {
            if this.generation.get() != generation {
                return;
            }
            let scrollback = this.scrollback.borrow().clone();
            let session = this.session.to_string();
            glib::MainContext::default().spawn_local(async move {
                let answer = gtk4::gio::spawn_blocking(move || scrollback.in_mode(&session)).await;
                if this.generation.get() != generation {
                    return;
                }
                if let Ok(Some(in_mode)) = answer {
                    this.show_for(in_mode);
                }
            });
        });
    }

    /// Watch the card's terminal area: scrolling may enter or leave copy mode,
    /// and a key typed in copy mode may end it. Both only observe; the
    /// terminal still gets every event.
    pub fn watch(&self, area: &impl IsA<gtk4::Widget>) {
        let scroll = gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let jump = self.clone();
        scroll.connect_scroll(move |_, _, _| {
            jump.check_soon();
            glib::Propagation::Proceed
        });
        area.add_controller(scroll);
        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let jump = self.clone();
        keys.connect_key_pressed(move |_, _, _, _| {
            if jump.button.is_visible() {
                jump.check_soon();
            }
            glib::Propagation::Proceed
        });
        area.add_controller(keys);
    }

    #[cfg(test)]
    pub fn use_scrollback(&self, scrollback: Scrollback) {
        *self.scrollback.borrow_mut() = scrollback;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Own server, socket and config per test. Never touches the user's tmux.
    pub(crate) struct Server {
        directory: PathBuf,
    }

    impl Server {
        /// A server with `session` holding far more output than its screen.
        pub(crate) fn with_history(session: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let directory =
                std::env::temp_dir().join(format!("sd-scrollback-{}-{serial}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            let server = Self { directory };
            server.run(&["new-session", "-d", "-s", session, "-x", "80", "-y", "20",
                "sh -c 'seq 1 400; exec sleep 60'"]);
            let deadline = Instant::now() + Duration::from_secs(5);
            while server.run(&["display-message", "-p", "-t", &target(session), "#{history_size}"])
                .parse::<u32>().unwrap_or(0) < 300
            {
                assert!(Instant::now() < deadline, "the output never reached the history");
                std::thread::sleep(Duration::from_millis(20));
            }
            server
        }

        fn socket(&self) -> PathBuf {
            self.directory.join("socket")
        }

        pub(crate) fn command(&self) -> Command {
            command_for(&self.socket())
        }

        pub(crate) fn run(&self, args: &[&str]) -> String {
            let out = self.command().args(args).output().expect("tmux must be installed");
            assert!(out.status.success(), "tmux {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        }

        pub(crate) fn scrollback(&self) -> Scrollback {
            let socket = self.socket();
            Scrollback::with(move || command_for(&socket))
        }

        /// Scroll `session`'s pane back, as the mouse wheel does over a card.
        pub(crate) fn scroll_back(&self, session: &str) {
            self.run(&["copy-mode", "-e", "-t", &target(session)]);
            self.run(&["send-keys", "-t", &target(session), "-X", "-N", "5", "scroll-up"]);
        }

        pub(crate) fn listed_in_mode(&self, session: &str) -> bool {
            let listing = self.run(&["list-panes", "-a", "-F", &crate::tmux::pane_snapshot_format()]);
            match crate::tmux::parse_pane_snapshot(&format!("{listing}\n")).lookup(session) {
                crate::tmux::PaneLookup::Row(row) => row.in_mode,
                _ => panic!("{session} is not listed"),
            }
        }
    }

    fn command_for(socket: &std::path::Path) -> Command {
        let mut command = Command::new(crate::tmux::tmux_bin());
        command
            .args(["-S", socket.to_str().unwrap(), "-f", "/dev/null"])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        command
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.command().arg("kill-server").output();
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn scrollback_follows_copy_mode_and_jumps_back_to_the_newest_output() {
        let session = "sd_term_scrollback";
        let server = Server::with_history(session);
        let scrollback = server.scrollback();
        assert_eq!(scrollback.in_mode(session), Some(false));
        assert!(!server.listed_in_mode(session));

        server.scroll_back(session);
        assert_eq!(scrollback.in_mode(session), Some(true));
        // The overlay's once-a-second listing reports it too.
        assert!(server.listed_in_mode(session));

        assert!(scrollback.jump_to_newest(session));
        assert_eq!(scrollback.in_mode(session), Some(false));
        assert!(!server.listed_in_mode(session));
        // Jumping again, already following, changes nothing.
        assert!(scrollback.jump_to_newest(session));
        assert_eq!(scrollback.in_mode(session), Some(false));

        // A name is matched exactly, and a missing session is unknown.
        assert_eq!(scrollback.in_mode("sd_term_scroll"), None);
        assert!(!scrollback.jump_to_newest("sd_term_missing"));
    }
}
