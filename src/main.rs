mod brand;
mod bridge;
mod mini_terminal;
mod state;
mod sticky_note;
mod styles;
mod tag;
mod theme;
mod tmux;
mod usage;
mod window;

use gtk4::gio::prelude::{ApplicationExt, ApplicationExtManual};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::Application;
use serde_json::json;
use std::cell::RefCell;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use styles::apply_styles;
use window::SuperDesktopWindow;

fn get_socket_path() -> PathBuf {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    PathBuf::from(runtime_dir).join("super-desktop.sock")
}

fn send_ipc_command(cmd: &str) -> Option<String> {
    let sock_path = get_socket_path();
    if !sock_path.exists() {
        return None;
    }

    if let Ok(mut stream) = UnixStream::connect(&sock_path) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
        let _ = stream.write_all(format!("{}\n", cmd.trim()).as_bytes());

        let mut resp = String::new();
        if stream.read_to_string(&mut resp).is_ok() {
            return Some(resp.trim().to_string());
        }
    }

    None
}

struct AppContext {
    window: Option<Rc<SuperDesktopWindow>>,
    last_toggle: Instant,
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("toggle");

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
        if let Some(resp) = send_ipc_command("kill") {
            println!("SUPER DESKTOP: {}", resp);
        } else {
            println!("SUPER DESKTOP: Daemon not running.");
        }
        return;
    }

    let full_cmd = if args.len() > 1 { args[1..].join(" ") } else { "toggle".to_string() };
    if let Some(resp) = send_ipc_command(&full_cmd) {
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
        } else {
            println!("SUPER DESKTOP (Rust): {}", resp);
        }
        return;
    }

    // Daemon not running -> spawn it
    if action == "toggle" || action == "show" {
        let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("super-desktop"));
        let _ = std::process::Command::new(exe)
            .arg("daemon")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        for _ in 0..25 {
            thread::sleep(Duration::from_millis(60));
            if send_ipc_command("show").is_some() {
                println!("SUPER DESKTOP (Rust): Started and Shown");
                return;
            }
        }
        eprintln!("Error: Failed to launch SUPER DESKTOP daemon");
        std::process::exit(1);
    } else {
        println!("SUPER DESKTOP: Daemon not running.");
    }
}

fn run_daemon(start_visible: bool) {
    let _ = gtk4::init();

    let app = Application::builder()
        .application_id("org.omarchy.superdesktop")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let _ = app.register(gtk4::gio::Cancellable::NONE);
    std::mem::forget(app.hold());
    ensure_omarchy_theme_hook();
    apply_styles();

    let context = Rc::new(RefCell::new(AppContext {
        window: None,
        last_toggle: Instant::now() - Duration::from_secs(10),
    }));

    let (ipc_tx, ipc_rx) = channel::<IpcMessage>();

    // Wake pipe: lets the IPC thread wake the GTK main loop only when a
    // command arrives (see unix_fd_add_local below).
    let (wake_read, wake_write) = make_wake_pipe();

    let ctx_activate = Rc::clone(&context);
    let app_clone = app.clone();

    app.connect_activate(move |application| {
        if start_visible {
            show_window(&ctx_activate, application);
        }
    });

    start_ipc_thread(ipc_tx, wake_write);

    // NOTE: glib 0.22 removed `unix_fd_add_local`, so the event-driven
    // dispatch can't compile against this stack. Poll at 50ms (still 3x
    // fewer wakeups than the old 16ms loop) and drain the wake pipe each
    // tick so the IPC thread's best-effort writes never fill it up.
    // Whoever re-adds event-driven wake can reuse make_wake_pipe().
    let ctx_timer = Rc::clone(&context);
    let app_timer = app.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        if let Some(fd) = wake_read {
            drain_wake_pipe(fd);
        }
        while let Ok(msg) = ipc_rx.try_recv() {
            let resp = handle_ipc_command(&msg.cmd, &ctx_timer, &app_timer);
            let _ = msg.responder.send(resp);
        }
        glib::ControlFlow::Continue
    });

    app_clone.run_with_args::<&str>(&[]);
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

fn show_window(ctx: &Rc<RefCell<AppContext>>, app: &Application) {
    if theme::check_theme_changed() {
        styles::reload_styles();
    }

    if ctx.borrow().window.is_some() {
        return;
    }

    let ctx_close = Rc::clone(ctx);
    let win = SuperDesktopWindow::new(app, move || {
        hide_window(&ctx_close);
    });

    win.window.present();
    win.start_slide_in();
    ctx.borrow_mut().window = Some(win);
}

fn hide_window(ctx: &Rc<RefCell<AppContext>>) {
    let win_opt = ctx.borrow_mut().window.take();
    if let Some(win) = win_opt {
        let win_clone = Rc::clone(&win);
        win.start_slide_out(move || {
            win_clone.window.close();
        });
    }
}

fn toggle_window(ctx: &Rc<RefCell<AppContext>>, app: &Application) -> bool {
    let now = Instant::now();
    if now.duration_since(ctx.borrow().last_toggle) < Duration::from_millis(450) {
        return ctx.borrow().window.is_some();
    }
    ctx.borrow_mut().last_toggle = now;

    if ctx.borrow().window.is_some() {
        hide_window(ctx);
        false
    } else {
        show_window(ctx, app);
        true
    }
}

struct IpcMessage {
    cmd: String,
    responder: Sender<String>,
}

/// Creates a non-blocking pipe used to wake the GTK main loop from the IPC
/// thread. Returns (read_fd, write_fd) or (None, None) on failure, in which
/// case the caller falls back to timeout polling.
fn make_wake_pipe() -> (Option<RawFd>, Option<RawFd>) {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return (None, None);
    }
    for fd in fds {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    (Some(fds[0]), Some(fds[1]))
}

fn drain_wake_pipe(fd: RawFd) {
    let mut buf = [0u8; 64];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 || (n as usize) < buf.len() {
            break;
        }
    }
}

fn start_ipc_thread(ipc_tx: Sender<IpcMessage>, wake_fd: Option<RawFd>) {
    let sock_path = get_socket_path();
    let _ = fs::remove_file(&sock_path);

    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind IPC socket: {}", e);
            return;
        }
    };

    let running = Arc::new(AtomicBool::new(true));
    let r_clone = Arc::clone(&running);

    thread::spawn(move || {
        for stream in listener.incoming() {
            if !r_clone.load(Ordering::SeqCst) {
                break;
            }

            if let Ok(mut s) = stream {
                let mut buf = [0u8; 1024];
                if let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        continue;
                    }
                    let line = String::from_utf8_lossy(&buf[..n]);
                    let cmd = line.trim().to_string();

                    let (resp_tx, resp_rx) = channel();
                    if ipc_tx.send(IpcMessage { cmd, responder: resp_tx }).is_ok() {
                        // Wake the GTK main loop (best-effort; the fallback
                        // poll also drains the channel if this fails).
                        if let Some(fd) = wake_fd {
                            unsafe {
                                libc::write(fd, [1u8].as_ptr() as *const libc::c_void, 1);
                            }
                        }
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

fn handle_ipc_command(cmd: &str, ctx: &Rc<RefCell<AppContext>>, app: &Application) -> String {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let action = parts.get(0).unwrap_or(&"toggle");

    match *action {
        "toggle" => {
            let vis = toggle_window(ctx, app);
            json!({ "ok": true, "visible": vis }).to_string()
        }
        "show" => {
            show_window(ctx, app);
            json!({ "ok": true, "visible": true }).to_string()
        }
        "hide" => {
            hide_window(ctx);
            json!({ "ok": true, "visible": false }).to_string()
        }
        "status" => {
            let is_vis = ctx.borrow().window.is_some();
            let state = state::load_state();
            json!({
                "ok": true,
                "visible": is_vis,
                "notes_count": state.notes.len(),
                "terminals_count": state.terminals.len(),
            })
            .to_string()
        }
        "add-note" => {
            show_window(ctx, app);
            let text = if parts.len() > 1 { parts[1..].join(" ") } else { "New note...".to_string() };
            if let Some(win) = &ctx.borrow().window {
                win.create_new_note(None, None, &text);
            }
            json!({ "ok": true }).to_string()
        }
        "add-term" => {
            show_window(ctx, app);
            let agent = parts.get(1).unwrap_or(&"shell");
            if let Some(win) = &ctx.borrow().window {
                win.create_new_terminal(agent, None, None, None);
            }
            json!({ "ok": true }).to_string()
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
            app.quit();
            json!({ "ok": true, "action": "quitting" }).to_string()
        }
        _ => json!({ "ok": false, "error": format!("unknown_command: {}", action) }).to_string(),
    }
}
