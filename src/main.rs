mod brand;
mod assets;
mod asset_history;
mod completion;
mod custom_harness;
mod prompt_image;
mod prompt_history;
mod shell_title;
mod harness_metadata;
mod harness_record;
mod preload;
mod terminal_text;
mod asset_pdf;
mod asset_view;
mod bridge;
mod card_resize;
mod card_status;
mod crashlog;
mod desktop_protocol;
mod peer_client;
mod peer_store;
mod peer_terminal;
mod peer_events;
mod peer_cli;
mod peer_pairing;
mod peer_pairing_ui;
mod pairing_invite;
mod pairing_request_ui;
mod card_source;
mod command_feedback;
mod harness_bar;
mod machine_selector;
mod remote_terminal;
mod remote_workspace;
mod harness_settings;
mod hotcorner;
mod jev;
mod launcher_settings;
mod mini_terminal;
mod terminal_picker;
mod terminal_clipboard;
mod overlap_ghost;
mod shortcut;
mod sleep_lock;
mod session_task;
mod state;
mod startup;
mod sticky_note;
mod styles;
mod tag;
mod theme;
mod tmux;
mod tmux_control;
mod terminal_transport;
mod usage;
mod window;
mod workspace_bar;
mod workspace_model;
mod ws;

/// Test-only helper for the GTK-dependent tests.
///
/// GTK may only be used from the first thread that ever initialized it, but
/// libtest runs every test on its own thread — so a second GTK test in the same
/// process fails with "Attempted to initialize GTK from two different threads"
/// / "GTK may only be used from the main thread". A GTK assertion therefore
/// re-runs this test binary in a child process filtered to just that one test.
#[cfg(test)]
pub mod gtk_test {
    pub const CHILD_ENV: &str = "SUPER_DESKTOP_GTK_TEST_CHILD";

    /// Run `inner_test` (full path, e.g. `styles::tests::css_gtk_inner`) alone
    /// in a fresh process, and fail this test when the child fails.
    pub fn run_in_child_process(inner_test: &str) {
        assert!(
            !is_child(),
            "run_in_child_process must not be called from the child process"
        );
        let exe = std::env::current_exe().expect("current test executable");
        let out = std::process::Command::new(exe)
            .args(["--exact", inner_test, "--nocapture"])
            .env(CHILD_ENV, "1")
            .env("RUST_TEST_THREADS", "1")
            .output()
            .expect("spawn child test process");
        assert!(
            out.status.success(),
            "GTK test `{inner_test}` failed in its own process:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    /// True inside that child process, where the GTK assertions may run.
    pub fn is_child() -> bool {
        std::env::var(CHILD_ENV).is_ok()
    }
}

use gtk4::gio::prelude::{ApplicationExt, ApplicationExtManual};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::Application;
use futures_util::StreamExt;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;

use styles::apply_styles;
use window::SuperDesktopWindow;

fn runtime_dir() -> PathBuf {
    env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
}

fn get_socket_path() -> PathBuf {
    socket_path_in(&runtime_dir())
}

/// Pure path helper: tests point this at a private dir instead of mutating
/// `XDG_RUNTIME_DIR`, which is process-wide (and would leak into any child
/// process a test spawns, including GTK ones).
fn socket_path_in(runtime_dir: &std::path::Path) -> PathBuf {
    runtime_dir.join("super-desktop.sock")
}

/// Outcome of talking to the daemon over the Unix socket.
///
/// The distinction matters: a daemon that is merely busy must never be treated
/// as "no daemon", because the caller then spawns a SECOND daemon. Every new
/// daemon used to unlink and re-bind the socket path, so the duplicate stole the
/// socket from the daemon whose overlay was on screen — that window could no
/// longer be hidden (every `toggle` reached the other process) and Ctrl-C /
/// [SUPER+SHIFT+Q] looked like "hide, then show again".
enum Ipc {
    /// The daemon answered.
    Reply(String),
    /// Nothing is listening: it is safe to start one.
    NoDaemon,
    /// A listener exists but did not answer in time. Do NOT start a second one.
    Stalled,
}

fn ipc_request(cmd: &str) -> Ipc {
    ipc_request_at(&get_socket_path(), cmd)
}

fn ipc_request_at(sock_path: &std::path::Path, cmd: &str) -> Ipc {
    if !sock_path.exists() {
        return Ipc::NoDaemon;
    }

    let mut stream = match UnixStream::connect(sock_path) {
        Ok(s) => s,
        // Connection refused: the file is a leftover from a daemon that is
        // gone. Clear it so the daemon we start can bind cleanly.
        Err(_) => {
            let _ = fs::remove_file(sock_path);
            return Ipc::NoDaemon;
        }
    };

    // Longer than the daemon's own 2s wait for the GTK thread, so a slow
    // answer is never mistaken for "no daemon". The socket file is left alone
    // here: the listener behind it is alive.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.write_all(format!("{}\n", cmd.trim()).as_bytes());

    let mut resp = String::new();
    match stream.take(1024 * 1024 + 1).read_to_string(&mut resp) {
        Ok(_) if resp.len() <= 1024 * 1024 && !resp.trim().is_empty() => Ipc::Reply(resp.trim().to_string()),
        _ => Ipc::Stalled,
    }
}

/// Safety net for the duplicate-daemon bug: remember which socket file this
/// process bound and quit as soon as the path stops pointing at it.
///
/// Even with the liveness probe in `start_ipc_thread`, an older build (or a
/// future mistake) can replace the socket file We would then be a daemon whose
/// window is on screen but unreachable — exactly the "it hides and immediately
/// shows again" state. Dying closes our window, so at most one overlay survives.
fn watch_socket_ownership(sock_path: &PathBuf) {
    use std::os::unix::fs::MetadataExt;

    let Ok(own_ino) = fs::metadata(sock_path).map(|m| m.ino()) else {
        return;
    };
    let path = sock_path.clone();

    glib::timeout_add_local(Duration::from_secs(1), move || {
        let still_ours = fs::metadata(&path).map(|m| m.ino()).ok() == Some(own_ino);
        if !still_ours {
            // Leave the socket file alone: it belongs to whoever replaced us.
            eprintln!(
                "SUPER DESKTOP: another daemon took over {} — exiting so only one overlay is on screen",
                path.display()
            );
            std::process::exit(0);
        }
        glib::ControlFlow::Continue
    });
}

struct AppContext {
    local_workspace: Rc<workspace_model::LocalWorkspace>,
    /// Kept for the whole daemon lifetime once built: hiding only unmaps it
    /// (see `hide_window`), so showing again costs no rebuild.
    window: Option<Rc<SuperDesktopWindow>>,
    /// Whether the overlay is currently mapped. `window.is_some()` is not the
    /// same thing any more — a hidden window stays alive on purpose.
    shown: bool,
    /// "The pointer is parked in the top-left corner zone", written by the
    /// corner surface while the overlay is hidden and by the overlay window
    /// itself while it is visible (see `hotcorner`).
    hot_inside: Rc<Cell<bool>>,
}

fn main() {
    // Before anything else, and before any thread exists: layer-shell is
    // already mapped into this process, and must not leak into tmux, card
    // shells/agents, the bridge or any helper subprocess.
    preload::strip_from_process_env();
    startup::mark("process entry");
    // First thing: release builds abort on panic and the daemon's stderr goes
    // to /dev/null, so without this a crash leaves no readable trace.
    crashlog::install_panic_hook();

    let args: Vec<String> = env::args().collect();
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("toggle");

    if action == "harness-event" {
        harness_record::record(args.get(2).map(String::as_str).unwrap_or(""));
        return;
    }
    if action == "integrate-openclaw" {
        if let Err(error) = harness_metadata::install_openclaw() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if matches!(action, "peer-add" | "peer-list" | "peer-workspace" | "peer-attach" | "peer-events" | "peer-command" | "peer-forget") {
        if let Err(error) = peer_cli::run(action, &args[2..]) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if action == "daemon" || action == "start" {
        run_daemon(action == "start");
        return;
    }

    if action == "harness-bridge" || action == "bridge" {
        let mut port = bridge::BRIDGE_PORT;
        for a in args.iter().skip(2) {
            if let Some(v) = a.strip_prefix("--port=") {
                port = v.parse().unwrap_or(bridge::BRIDGE_PORT);
            } else if let Ok(v) = a.parse::<u16>() {
                port = v;
            }
        }
        bridge::serve(port);
        return;
    }

    if action == "harnesses" {
        bridge::print_once();
        return;
    }

    if action == "kill" {
        match ipc_request("kill") {
            Ipc::Reply(resp) => println!("SUPER DESKTOP: {resp}"),
            _ => println!("SUPER DESKTOP: Daemon not running."),
        }
        return;
    }

    let full_cmd = if args.len() > 1 { args[1..].join(" ") } else { "toggle".to_string() };
    let resp = match ipc_request(&full_cmd) {
        Ipc::Reply(resp) => Some(resp),
        Ipc::Stalled => {
            // A daemon is alive but busy: starting another one would orphan the
            // overlay it is showing (the second process rebinds the socket).
            eprintln!(
                "SUPER DESKTOP: daemon is up but did not answer `{full_cmd}` in time — try again"
            );
            std::process::exit(2);
        }
        Ipc::NoDaemon => None,
    };
    if let Some(resp) = resp {
        if action == "status" {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&resp) {
                let vis = val["visible"].as_bool().unwrap_or(false);
                let notes = val["notes_count"].as_i64().unwrap_or(0);
                let terms = val["terminals_count"].as_i64().unwrap_or(0);
                println!("SUPER DESKTOP (Rust): {}", if vis { "Visible" } else { "Hidden" });
                println!("Notes: {}, Terminals: {}", notes, terms);
            } else {
                println!("{}", resp);
            }
        } else if action == "toggle" {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&resp) {
                let vis = val["visible"].as_bool().unwrap_or(false);
                println!("SUPER DESKTOP (Rust): {}", if vis { "Shown" } else { "Hidden" });
            } else {
                println!("{}", resp);
            }
        } else if action == "reload-theme" || action == "refresh-theme" {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&resp) {
                let name = val["theme"].as_str().unwrap_or("Unknown");
                let mode = val["mode"].as_str().unwrap_or("dark");
                println!("SUPER DESKTOP: Theme reloaded -> {} ({})", name, mode);
            } else {
                println!("{}", resp);
            }
        } else if action == "theme" {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&resp) {
                let name = val["theme"].as_str().unwrap_or("Unknown");
                let mode = val["mode"].as_str().unwrap_or("dark");
                let accent = val["accent"].as_str().unwrap_or("");
                println!("SUPER DESKTOP Active Theme: {} ({}) [Accent: {}]", name, mode, accent);
            } else {
                println!("{}", resp);
            }
        } else if action == "desktop-workspace" {
            println!("{}", resp);
        } else {
            println!("SUPER DESKTOP (Rust): {}", resp);
        }
        return;
    }

    // Daemon not running -> spawn it
    if action == "toggle" || action == "show" || action == "pairing-review" {
        // A clicked pairing notification must still end at the approval panel.
        let first = if action == "pairing-review" { "pairing-review" } else { "show" };
        let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("super-desktop"));
        let mut command = std::process::Command::new(exe);
        // This child is the overlay itself: it needs the preload that `main`
        // stripped from the inherited environment.
        preload::preload_layer_shell(&mut command);
        let _ = command
            .arg("daemon")
            // Outlive the launcher's terminal/process-group cleanup.
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        for _ in 0..25 {
            thread::sleep(Duration::from_millis(60));
            // `Stalled` counts as "answered by something": the daemon we
            // spawned exits when it finds a live one instead of stealing the
            // socket (see start_ipc_thread), so never spawn a second process
            // from this loop.
            match ipc_request(first) {
                Ipc::Reply(_) => {
                    println!("SUPER DESKTOP (Rust): Started and Shown");
                    return;
                }
                Ipc::Stalled => {
                    eprintln!(
                        "SUPER DESKTOP: a daemon is already running and busy — not starting another"
                    );
                    std::process::exit(2);
                }
                Ipc::NoDaemon => {}
            }
        }
        eprintln!("Error: Failed to launch SUPER DESKTOP daemon");
        std::process::exit(1);
    } else {
        println!("SUPER DESKTOP: Daemon not running.");
    }
}

fn run_daemon(start_visible: bool) {
    let initial_state = state::load_state();
    sleep_lock::set_enabled(initial_state.sleep_lock_on_ac);
    let _ = gtk4::init();
    startup::mark("GTK initialized");

    let app = Application::builder()
        .application_id("org.omarchy.superdesktop")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let _ = app.register(gtk4::gio::Cancellable::NONE);
    std::mem::forget(app.hold());
    ensure_omarchy_theme_hook();
    apply_styles();
    startup::mark("theme and styles ready");

    let context = Rc::new(RefCell::new(AppContext {
        local_workspace: Rc::new(workspace_model::LocalWorkspace::new(initial_state)),
        window: None,
        shown: false,
        hot_inside: Rc::new(Cell::new(false)),
    }));

    // Older installs bound the shortcut on press (which repeats while held).
    // Upgrade in place to a release-bind so a held key is one toggle, not a
    // strobe, and a second tap during the slide-in can reverse immediately.
    shortcut::ensure_release_toggle();

    let (ipc_tx, mut ipc_rx) = futures_channel::mpsc::unbounded::<IpcMessage>();

    let ctx_activate = Rc::clone(&context);
    let app_clone = app.clone();

    // The hot corner has to outlive every hide/show of the overlay, so its
    // surface is spawned once, here: a window cannot be presented before the
    // application is running, and keeping it in this handler is what keeps it
    // mapped.
    let hot_corner: Rc<RefCell<Option<hotcorner::HotCorner>>> = Rc::new(RefCell::new(None));
    app.connect_activate(move |application| {
        if hot_corner.borrow().is_none() {
            let hot_inside = Rc::clone(&ctx_activate.borrow().hot_inside);
            let ctx_toggle = Rc::clone(&ctx_activate);
            let app_toggle = application.clone();
            *hot_corner.borrow_mut() =
                hotcorner::HotCorner::spawn(application, hot_inside, move || {
                    toggle_window(&ctx_toggle, &app_toggle);
                });
        }
        if start_visible {
            show_window(&ctx_activate, application);
        }
    });

    // After the socket bind: a duplicate daemon exits inside `start_ipc_thread`
    // (see its liveness probe) and must not look like a run in the crash log.
    crashlog::note_start(if start_visible {
        "daemon (visible)"
    } else {
        "daemon"
    });
    start_ipc_thread(ipc_tx);
    startup::mark("IPC listening");
    // Panes created in an already running tmux server inherit its global
    // environment, which still carries layer-shell if that server was started
    // from a preloaded environment.
    let _ = thread::Builder::new()
        .name("super-desktop-tmux-env".to_string())
        .spawn(preload::clean_tmux_global_env);

    // The bridge waits for its child to become reachable, so keep that work
    // off GTK's main thread. Start it only after this process owns the daemon
    // socket: a duplicate daemon exits in `start_ipc_thread` and must not
    // restart or disturb the bridge.
    if let Err(error) = thread::Builder::new()
        .name("super-desktop-bridge-watch".to_string())
        .spawn(bridge::supervise_bridge)
    {
        eprintln!("SUPER DESKTOP: could not start bridge supervision: {error}");
    }

    // Warm the UI right after start, never on the startup path: startup stays
    // fast (the socket is up first) and by the time a human presses the
    // shortcut the overlay is already built.
    {
        let ctx_warm = Rc::clone(&context);
        let app_warm = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(60), move || {
            warm_window(&ctx_warm, &app_warm);
        });
    }

    // The channel's waker schedules this future on GTK as soon as a command
    // arrives. No polling timer, idle CPU use, or extra wake-pipe descriptors.
    let ctx_ipc = Rc::clone(&context);
    let app_ipc = app.clone();
    glib::MainContext::default().spawn_local(async move {
        let mut toggles = ToggleGate::default();
        while let Some(msg) = ipc_rx.next().await {
            let response = if msg.cmd.trim() == "toggle" {
                let visible =
                    toggles.dispatch(msg.received_at, || toggle_window(&ctx_ipc, &app_ipc));
                json!({ "ok": true, "visible": visible }).to_string()
            } else {
                toggles.last = None;
                handle_ipc_command(&msg.cmd, &ctx_ipc, &app_ipc)
            };
            let _ = msg.responder.send(response);
        }
    });

    app_clone.run_with_args::<&str>(&[]);
    state::flush_state_saves();
}

fn ensure_omarchy_theme_hook() {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let hook_dir = PathBuf::from(&home).join(".config/omarchy/hooks/theme-set.d");
    let hook_file = hook_dir.join("super-desktop");
    if !hook_file.exists() {
        let _ = fs::create_dir_all(&hook_dir);
        let content = "#!/usr/bin/env bash\nsuper-desktop reload-theme >/dev/null 2>&1 || true\n";
        if fs::write(&hook_file, content).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&hook_file, fs::Permissions::from_mode(0o755));
            }
        }
    }
}

/// Returns whether the overlay is on screen afterwards. `false` only happens
/// when there is no display to draw on.
fn show_window(ctx: &Rc<RefCell<AppContext>>, app: &Application) -> bool {
    // No display (a daemon started from a bare shell, a test runner, a session
    // without Wayland): building widgets would dereference NULL deep inside
    // GTK. That is the one SIGSEGV this program has produced — see the
    // 2026-09-17 18:39 core dump — so refuse loudly instead of crashing.
    if gtk4::gdk::Display::default().is_none() {
        crashlog::note("no display available; skipping window build");
        eprintln!("SUPER DESKTOP: no display — cannot show the overlay");
        return false;
    }

    if theme::check_theme_changed() {
        styles::reload_styles();
    }

    // Already built: re-map it. Rebuilding the whole UI (state load, panels,
    // harness detection, one `tmux` exec per card plus a VTE and an attach
    // client each) took ~1.5s on a loaded machine — that was the delay between
    // the shortcut and the overlay appearing.
    if let Some(win) = live_window(ctx) {
        win.show_again();
        ctx.borrow_mut().shown = true;
        return true;
    }

    let ctx_close = Rc::clone(ctx);
    let hot_inside = Rc::clone(&ctx.borrow().hot_inside);
    let local_state = ctx.borrow().local_workspace.state();
    let win = SuperDesktopWindow::new(app, move || hide_window(&ctx_close), hot_inside, local_state);

    win.window.present();
    win.start_slide_in();
    ctx.borrow_mut().window = Some(win);
    ctx.borrow_mut().shown = true;
    true
}

/// Snapshot the live window, if any, **without holding the `RefCell` borrow**.
///
/// Load-bearing detail: `if let Some(win) = ctx.borrow().window.clone() { … }`
/// keeps the read borrow alive until the end of the whole `if let` block, so a
/// `ctx.borrow_mut()` inside it panics with `BorrowMutError` — and release
/// builds use `panic = "abort"`, so that panic kills the daemon.
fn live_window(ctx: &Rc<RefCell<AppContext>>) -> Option<Rc<SuperDesktopWindow>> {
    ctx.borrow().window.clone()
}

/// Build the overlay without mapping it, so the first `toggle` only has to
/// present it (`show_window` reuses any window it finds).
///
/// Trade-off, chosen deliberately: the widget tree, one VTE + `tmux attach`
/// client per card and any missing tmux session come up with the daemon, even
/// if the overlay is never opened — in exchange for an instant first shortcut.
fn warm_window(ctx: &Rc<RefCell<AppContext>>, app: &Application) {
    if live_window(ctx).is_some() {
        return;
    }
    if gtk4::gdk::Display::default().is_none() {
        // See `show_window`: without a display GTK crashes during widget
        // construction, and the warm-up runs unattended at daemon start.
        crashlog::note("no display available; skipping window warm-up");
        return;
    }
    if theme::check_theme_changed() {
        styles::reload_styles();
    }
    let ctx_close = Rc::clone(ctx);
    let hot_inside = Rc::clone(&ctx.borrow().hot_inside);
    let local_state = ctx.borrow().local_workspace.state();
    let win = SuperDesktopWindow::new(app, move || hide_window(&ctx_close), hot_inside, local_state);
    // Not presented: the window stays unmapped until the first show.
    ctx.borrow_mut().window = Some(win);
}

fn hide_window(ctx: &Rc<RefCell<AppContext>>) {
    let win = match live_window(ctx) {
        Some(win) => win,
        None => return,
    };
    ctx.borrow_mut().shown = false;
    // Slide out, then unmap: the widget tree, the VTE terminals and their tmux
    // attach clients stay alive, so the next show is instant. The token makes a
    // show that lands during the animation win over this unmap.
    //
    // The slide animates live terminals, so on a saturated GPU Hyprland may
    // never deliver the frames the spring needs. The fallback unmaps anyway.
    let token = win.current_show_token();
    let win_hide = Rc::clone(&win);
    win.start_slide_out(move || win_hide.hide_if_unchanged(token));
    let win_fb = Rc::clone(&win);
    glib::timeout_add_local_once(crate::window::HIDE_FALLBACK, move || {
        win_fb.hide_if_unchanged(token);
    });
}

fn toggle_window(ctx: &Rc<RefCell<AppContext>>, app: &Application) -> bool {
    // A tap during the slide-in reverses immediately. Duplicate IPC events
    // from the keysym and physical-key bindings are handled by ToggleGate.
    if ctx.borrow().shown {
        hide_window(ctx);
        false
    } else {
        show_window(ctx, app);
        true
    }
}

/// Hyprland can fire both the keysym and physical-key binding on one release.
/// Apply the first immediately; coalesce duplicates within the old 10ms poll
/// interval without delaying real taps that reverse an in-flight animation.
#[derive(Default)]
struct ToggleGate {
    last: Option<(std::time::Instant, bool)>,
}

impl ToggleGate {
    fn dispatch(&mut self, at: std::time::Instant, toggle: impl FnOnce() -> bool) -> bool {
        if let Some((last, visible)) = self.last {
            if at.saturating_duration_since(last) < Duration::from_millis(10) {
                return visible;
            }
        }
        let visible = toggle();
        self.last = Some((at, visible));
        visible
    }
}

struct IpcMessage {
    cmd: String,
    responder: Sender<String>,
    received_at: std::time::Instant,
}

/// Bind the daemon's control socket, refusing to take over a live daemon's.
///
/// Two daemons on one machine is the failure that made [SUPER + SHIFT + Q] look
/// broken: this function used to `remove_file` the socket path unconditionally
/// and bind a fresh one, so the newer process silently owned the socket while
/// the older process kept its overlay window on screen — every later `toggle`
/// reached the new process, so the visible window could not be hidden any more.
/// Now a live daemon is probed first; if it answers, this process exits instead.
fn start_ipc_thread(ipc_tx: futures_channel::mpsc::UnboundedSender<IpcMessage>) {
    let sock_path = get_socket_path();

    match ipc_request("status") {
        Ipc::Reply(resp) => {
            eprintln!(
                "SUPER DESKTOP: another daemon is already running (status: {resp}) — exiting instead of taking over its socket"
            );
            std::process::exit(0);
        }
        Ipc::Stalled => {
            // A listener is behind the path, it just did not answer in time.
            // Taking the socket now would orphan the overlay it is showing.
            eprintln!(
                "SUPER DESKTOP: another daemon is already running but busy — exiting instead of taking over its socket"
            );
            std::process::exit(0);
        }
        Ipc::NoDaemon => {}
    }

    // No live listener: any file left behind is stale.
    let _ = fs::remove_file(&sock_path);

    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => {
            // Without the socket this process would still open an overlay
            // window that nothing can control — leave instead.
            eprintln!("Failed to bind IPC socket: {e}");
            std::process::exit(1);
        }
    };

    watch_socket_ownership(&sock_path);

    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut s) = stream {
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                if let Some(cmd) = read_ipc_command(&mut s) {
                    // A long-lived change feed for the bridge's desktop event
                    // route. It never reaches GTK and never holds this
                    // sequential listener: it gets a bounded thread of its own.
                    if cmd == "desktop-watch" {
                        crate::workspace_model::serve_watch(s);
                        continue;
                    }

                    let (resp_tx, resp_rx) = channel();
                    if ipc_tx
                        .unbounded_send(IpcMessage {
                            cmd,
                            responder: resp_tx,
                            received_at: std::time::Instant::now(),
                        })
                        .is_ok()
                    {
                        if let Ok(resp) = resp_rx.recv_timeout(Duration::from_millis(2000)) {
                            let _ = s.write_all(resp.as_bytes());
                            let _ = s.shutdown(std::net::Shutdown::Both);
                        }
                    }
                }
            }
        }
    });
}

/// Longest command line the daemon reads from its IPC socket. A remote
/// `desktop-command` carries a folder path of up to `MAX_WORKSPACE` bytes
/// inside its JSON envelope, so one 1 KiB read is not enough.
const MAX_IPC_COMMAND: usize = 16 * 1024;

/// Read one command from an IPC client: until what it sent ends with a
/// newline (every client writes `command\n` in one go and then waits), its EOF
/// or `MAX_IPC_COMMAND` bytes, whichever comes first. A client that stalls
/// mid-command is given two seconds, not the sequential listener forever.
/// `None` for an empty or failed read.
fn read_ipc_command(stream: &mut impl Read) -> Option<String> {
    let mut command = Vec::new();
    let mut chunk = [0u8; 4096];
    while command.last() != Some(&b'\n') && command.len() < MAX_IPC_COMMAND {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => command.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    command.truncate(MAX_IPC_COMMAND);
    let command = String::from_utf8_lossy(&command).trim().to_string();
    (!command.is_empty()).then_some(command)
}

/// A typed command refusal for something that never reached the model.
fn refusal(snapshot: &crate::desktop_protocol::LocalWorkspaceSnapshot, error: &str) -> String {
    serde_json::to_string(&crate::desktop_protocol::CommandOutcome::rejected(
        snapshot, error,
    ))
    .unwrap_or_default()
}

fn handle_ipc_command(cmd: &str, ctx: &Rc<RefCell<AppContext>>, app: &Application) -> String {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let action = parts.get(0).unwrap_or(&"toggle");

    match *action {
        "toggle" => {
            let vis = toggle_window(ctx, app);
            json!({ "ok": true, "visible": vis }).to_string()
        }
        "show" => {
            let shown = show_window(ctx, app);
            json!({ "ok": shown, "visible": shown }).to_string()
        }
        "hide" => {
            hide_window(ctx);
            json!({ "ok": true, "visible": false }).to_string()
        }
        // From the bridge when a device asks to pair: a visible overlay puts
        // the request in front of the user; a hidden one is left alone and
        // the bridge notifies instead.
        "pairing-request" => {
            let shown = ctx.borrow().shown;
            let presented = shown && live_window(ctx).is_some_and(|win| win.review_pairing_requests());
            json!({ "ok": true, "presented": presented }).to_string()
        }
        // The pairing notification was clicked.
        "pairing-review" => {
            let presented =
                show_window(ctx, app) && live_window(ctx).is_some_and(|win| win.review_pairing_requests());
            json!({ "ok": presented, "presented": presented }).to_string()
        }
        "status" => {
            let context = ctx.borrow();
            let (notes, terminals) = context
                .window
                .as_ref()
                .map(|w| w.item_counts())
                .unwrap_or_else(|| {
                    let state = context.local_workspace.state();
                    let state = state.borrow();
                    (state.notes.len(), state.terminals.len())
                });
            json!({
                "ok": true,
                "visible": context.shown,
                "notes_count": notes,
                "terminals_count": terminals,
            })
            .to_string()
        }
        "add-note" => {
            show_window(ctx, app);
            let text = if parts.len() > 1 {
                parts[1..].join(" ")
            } else {
                "New note...".to_string()
            };
            if let Some(win) = &ctx.borrow().window {
                win.create_new_note(None, None, &text);
            }
            json!({ "ok": true }).to_string()
        }
        "desktop-workspace" => {
            let Some(win) = live_window(ctx) else {
                return json!({"ok": false, "error": "desktop_not_ready"}).to_string();
            };
            let model = ctx.borrow().local_workspace.clone();
            match win.desktop_snapshot(&model) {
                Ok(workspace) => json!({"ok": true, "workspace": workspace}).to_string(),
                Err(error) => json!({"ok": false, "error": error}).to_string(),
            }
        }
        "desktop-command" => {
            let Some(win) = live_window(ctx) else {
                return json!({"ok": false, "error": "desktop_not_ready"}).to_string();
            };
            let model = ctx.borrow().local_workspace.clone();
            // Publish current state before checking anything: an expected
            // revision describes the published workspace, not a pending local
            // edit, and a command must never compare against a moving target.
            let current = match win.desktop_snapshot(&model) {
                Ok(workspace) => workspace,
                Err(error) => return json!({"ok": false, "error": error}).to_string(),
            };
            let payload = cmd.strip_prefix("desktop-command ").unwrap_or("");
            let request = match serde_json::from_str::<crate::desktop_protocol::CommandRequest>(
                payload,
            ) {
                Ok(request) => request,
                Err(_) => {
                    return refusal(&current, "invalid_command");
                }
            };
            if let Err(rejected) = crate::desktop_protocol::check_command(&current, &request) {
                return serde_json::to_string(&rejected).unwrap_or_default();
            }
            // A create reports the card it made; every other command already
            // names the card it addressed.
            let created = match win.apply_workspace_command(&request.command) {
                Ok(created) => created,
                Err(error) => return refusal(&current, error),
            };
            // Report the state that was really published, so a viewer adopting
            // these revisions and this geometry cannot be told a stale layout.
            match win.desktop_snapshot(&model) {
                Ok(workspace) => {
                    let card_id = created.or_else(|| request.command.card_id().map(str::to_string));
                    let applied = crate::desktop_protocol::CommandOutcome::applied(
                        &workspace,
                        card_id,
                    );
                    serde_json::to_string(&applied).unwrap_or_default()
                }
                Err(error) => json!({"ok": false, "error": error}).to_string(),
            }
        }
        "workspace-choices" => {
            if let Some(win) = &ctx.borrow().window {
                return win.workspace_choices().to_string();
            }
            let shared = ctx.borrow().local_workspace.state();
            let state = shared.borrow();
            json!({"workspace": state::effective_workspace_dir(&state),
                "recentDirectories": state.recent_dirs,
                "usedDirectories": state.used_dirs}).to_string()
        }
        "add-term-in" => {
            // JSON preserves spaces and escapes newlines in paths over IPC.
            let payload = cmd.strip_prefix("add-term-in ").unwrap_or("");
            let request: serde_json::Value = serde_json::from_str(payload).unwrap_or_default();
            let agent = request["agentType"].as_str().unwrap_or("");
            if !tmux::HARNESS_KEYS.contains(&agent)
                && !state::load_state().custom_harnesses.iter().any(|item| item.id == agent && item.validate().is_ok()) {
                return json!({"ok":false,"error":"unsupported_harness"}).to_string();
            }
            let Some(directory) = request["workspace"].as_str().and_then(state::clean_dir) else {
                return json!({"ok":false,"error":"invalid_workspace"}).to_string();
            };
            show_window(ctx, app);
            if let Some(win) = &ctx.borrow().window {
                let id = win.create_new_terminal_in(agent, None, None, None, Some(&directory));
                return json!({"ok":tmux::session_alive(&id),"id":id}).to_string();
            }
            json!({"ok":false,"error":"desktop_unavailable"}).to_string()
        }
        "add-term" => {
            show_window(ctx, app);
            let agent = parts.get(1).unwrap_or(&"shell");
            if let Some(win) = &ctx.borrow().window {
                let id = win.create_new_terminal(agent, None, None, None);
                return json!({ "ok": tmux::session_alive(&id), "id": id }).to_string();
            }
            json!({ "ok": false, "error": "desktop_unavailable" }).to_string()
        }
        "close-term" => {
            let Some(sess) = parts.get(1) else {
                return json!({ "ok": false, "error": "missing_session" }).to_string();
            };
            if !sess.starts_with("sd_term_") {
                return json!({ "ok": false, "error": "foreign_session" }).to_string();
            }
            if let Some(win) = &ctx.borrow().window {
                return if win.close_terminal(sess) {
                    json!({ "ok": true, "id": sess }).to_string()
                } else {
                    json!({ "ok": false, "error": "no_such_session", "id": sess }).to_string()
                };
            }

            let shared = ctx.borrow().local_workspace.state();
            let mut state = shared.borrow_mut();
            let before = state.terminals.len();
            state.terminals.retain(|t| t.session_name != *sess);
            let removed = state.terminals.len() != before;
            if tmux::session_alive(sess) {
                tmux::kill_session(sess);
            }
            if removed {
                state::normalize_terminal_order(&mut state);
                let snapshot = state.clone();
                drop(state);
                state::save_state_async(snapshot);
                json!({ "ok": true, "id": sess }).to_string()
            } else {
                json!({ "ok": false, "error": "no_such_session", "id": sess }).to_string()
            }
        }
        "reload-theme" | "refresh-theme" | "theme-reload" => {
            let t = styles::reload_styles();
            if let Some(win) = ctx.borrow().window.as_ref() {
                win.reload_theme();
            }
            json!({ "ok": true, "theme": t.name, "mode": t.mode }).to_string()
        }
        "theme" => {
            let t = theme::current_theme();
            json!({ "ok": true, "theme": t.name, "mode": t.mode, "accent": t.accent, "background": t.background }).to_string()
        }
        "kill" | "quit" => {
            // Drop the socket first so the next `toggle` spawns a fresh daemon
            // instead of connecting to a corpse, then really leave: the app is
            // `hold()`-ed (see run_daemon) so `app.quit()` alone can keep the
            // process alive with no window — which is how a "killed" daemon
            // used to stay around holding the socket.
            let _ = fs::remove_file(get_socket_path());
            glib::timeout_add_local_once(Duration::from_millis(150), || {
                state::flush_state_saves();
                std::process::exit(0);
            });
            json!({ "ok": true, "action": "quitting" }).to_string()
        }
        _ => json!({ "ok": false, "error": format!("unknown_command: {}", action) }).to_string(),
    }
}

#[cfg(test)]
mod ipc_tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn duplicate_bindings_do_not_cancel_but_a_second_tap_reverses() {
        let mut gate = ToggleGate::default();
        let at = std::time::Instant::now();
        assert!(gate.dispatch(at, || true));
        assert!(gate.dispatch(at + Duration::from_millis(3), || panic!("duplicate toggled")));
        assert!(!gate.dispatch(at + Duration::from_millis(40), || false));
        assert!(!gate.dispatch(at + Duration::from_millis(41), || panic!("duplicate toggled")));
        gate.last = None; // An intervening command resets the gate.
        assert!(gate.dispatch(at + Duration::from_millis(42), || true));
    }

    #[test]
    fn ipc_channel_wakes_a_sleeping_main_context() {
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let (tx, mut rx) = futures_channel::mpsc::unbounded::<u32>();
            let seen = Rc::new(Cell::new(0));
            let result = Rc::clone(&seen);
            context.spawn_local(async move {
                result.set(rx.next().await.unwrap());
            });
            // Arm the receiver before sending from another thread.
            context.iteration(false);
            assert!(!context.pending(), "idle receiver must not spin");
            thread::spawn(move || tx.unbounded_send(42).unwrap()).join().unwrap();
            while context.pending() {
                context.iteration(false);
            }
            assert_eq!(seen.get(), 42);
        }).unwrap();
    }

    /// A remote command carries a folder path of up to 4 KiB in its JSON
    /// envelope; one 1 KiB read used to cut it off and refuse it as invalid.
    #[test]
    fn a_long_desktop_command_is_read_whole() {
        let (mut client, mut daemon) = std::os::unix::net::UnixStream::pair().unwrap();
        let folder = format!("/home/user/{}", "deep/".repeat(700));
        let command = format!(
            "desktop-command {}",
            json!({"requestId": "r1", "machineId": "m", "expectedEpoch": "e",
                   "command": {"type": "setWorkspace", "workspace": folder,
                               "expectedRevision": 3}})
        );
        assert!(command.len() > 3000);
        // The client writes in pieces, as a busy socket may deliver it.
        let sent = format!("{command}\n");
        let writer = thread::spawn(move || {
            for piece in sent.as_bytes().chunks(700) {
                client.write_all(piece).unwrap();
                thread::sleep(Duration::from_millis(5));
            }
            client
        });
        daemon.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert_eq!(read_ipc_command(&mut daemon).as_deref(), Some(command.as_str()));
        let _client = writer.join().unwrap();

        // Short commands, a client that closes without a newline and one that
        // sends nothing at all keep their old meaning.
        let (mut client, mut daemon) = std::os::unix::net::UnixStream::pair().unwrap();
        client.write_all(b"status\n").unwrap();
        assert_eq!(read_ipc_command(&mut daemon).as_deref(), Some("status"));
        let (mut client, mut daemon) = std::os::unix::net::UnixStream::pair().unwrap();
        client.write_all(b"toggle").unwrap();
        drop(client);
        assert_eq!(read_ipc_command(&mut daemon).as_deref(), Some("toggle"));
        let (client, mut daemon) = std::os::unix::net::UnixStream::pair().unwrap();
        daemon.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        assert_eq!(read_ipc_command(&mut daemon), None);
        drop(client);
        // Bounded: an endless client cannot grow the daemon's buffer.
        let mut endless = std::io::repeat(b'x');
        assert_eq!(read_ipc_command(&mut endless).map(|c| c.len()), Some(MAX_IPC_COMMAND));
    }

    /// Private dir so this test can never touch the user's real daemon.
    fn private_runtime_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("sd-ipc-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp runtime dir");
        dir
    }

    /// The three outcomes decide whether the caller starts a daemon, and only
    /// `NoDaemon` may. Getting this wrong is what left a second daemon holding
    /// the socket while the first one's overlay stayed on screen.
    #[test]
    fn test_ipc_outcomes_never_report_a_live_daemon_as_absent() {
        let dir = private_runtime_dir("outcomes");
        let sock_path = socket_path_in(&dir);

        // 1. Nothing at the path at all.
        assert!(matches!(ipc_request_at(&sock_path, "status"), Ipc::NoDaemon));

        // 2. Leftover file from a daemon that is gone: the connect fails, the
        //    file is cleared (so a freshly spawned daemon can bind) and the
        //    caller is told to start one.
        fs::write(&sock_path, b"").expect("write stale socket file");
        assert!(matches!(ipc_request_at(&sock_path, "status"), Ipc::NoDaemon));
        assert!(!sock_path.exists(), "a stale socket file must be removed");

        // 3. Live daemon: answers -> Reply; alive but silent -> Stalled.
        let listener = UnixListener::bind(&sock_path).expect("bind fake daemon");
        let server = thread::spawn(move || {
            let mut first = true;
            for stream in listener.incoming().take(2) {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 64];
                let n = s.read(&mut buf).unwrap_or(0);
                let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                if first && cmd == "status" {
                    first = false;
                    let _ = s.write_all(br#"{"ok":true}"#);
                }
                // Every later connection is read, then closed silently.
            }
        });

        assert!(
            matches!(ipc_request_at(&sock_path, "status"), Ipc::Reply(r) if r == r#"{"ok":true}"#),
            "a live daemon's answer must come back as Reply"
        );
        assert!(
            matches!(ipc_request_at(&sock_path, "status"), Ipc::Stalled),
            "a live daemon that stays silent must not look like NoDaemon"
        );
        server.join().expect("fake daemon thread");

        let _ = fs::remove_dir_all(&dir);
    }
}
