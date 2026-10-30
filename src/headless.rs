//! The daemon without a display: Settings → System launch starts it at boot,
//! before anyone logs in to a desktop. It draws nothing, but the phone still
//! reaches the PC: it takes the login shell's environment (the boot service
//! starts with a bare `PATH`, so no harness would be found), restores the
//! saved cards' tmux sessions, keeps the phone bridge up and answers the
//! daemon socket's card requests (create, close, workspace choices) itself.
//!
//! A desktop session's own `super-desktop daemon` asks it to step aside
//! (`take_over`), then owns the socket, the saved state and the bridge, and
//! attaches to the sessions already running. Once that daemon is gone this one
//! takes them back.

use crate::state::{self, AppState, TerminalData};
use crate::{crashlog, tmux, Ipc};
use serde_json::json;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

/// The command a desktop daemon sends to hand the socket over.
const YIELD: &str = "headless-yield";

/// Whether this process owns the socket, and with it the bridge.
static SERVING: AtomicBool = AtomicBool::new(false);

/// How often a standing-by daemon checks whether the desktop daemon is gone.
const STANDBY_CHECK: Duration = Duration::from_secs(5);

/// Longest wait for the login shell to print its environment.
const LOGIN_ENV_TIMEOUT: Duration = Duration::from_secs(5);

/// The default size of a new card, as the overlay makes it.
const CARD_WIDTH: i32 = 640;
const CARD_HEIGHT: i32 = 480;

pub fn run() {
    if matches!(crate::ipc_request("status"), Ipc::Reply(reply) if is_headless(&reply)) {
        eprintln!("SUPER DESKTOP: a headless daemon is already running — exiting");
        return;
    }
    eprintln!("SUPER DESKTOP: no display — running headless: cards and the phone bridge, no overlay");
    import_login_environment();
    crate::launch_args::install(&state::load_state().harness_args);
    crashlog::note_start("daemon (headless)");
    if let Err(error) = thread::Builder::new()
        .name("super-desktop-bridge-watch".into())
        .spawn(|| crate::bridge::supervise_bridge_while(|| SERVING.load(Ordering::SeqCst)))
    {
        eprintln!("SUPER DESKTOP: could not start bridge supervision: {error}");
    }
    // At start there is nothing to wait for. After a hand-over, give the
    // desktop daemon time to bind before deciding it is gone.
    let mut quiet_checks = 1;
    loop {
        wait_until_no_daemon(quiet_checks);
        quiet_checks = 2;
        let Some((listener, inode)) = bind() else {
            thread::sleep(STANDBY_CHECK);
            continue;
        };
        SERVING.store(true, Ordering::SeqCst);
        let _ = thread::Builder::new()
            .name("super-desktop-restore".into())
            .spawn(restore_cards);
        serve(listener, inode);
        SERVING.store(false, Ordering::SeqCst);
        eprintln!("SUPER DESKTOP: a desktop daemon took over — standing by");
    }
}

/// Called by a desktop daemon before it reads the saved state: when a
/// headless daemon owns the socket, ask it to step aside. True when one did,
/// so the caller re-reads the state it may have changed.
pub fn take_over() -> bool {
    for _ in 0..8 {
        match crate::ipc_request("status") {
            Ipc::Reply(reply) if is_headless(&reply) => {
                if let Ipc::Reply(reply) = crate::ipc_request(YIELD) {
                    if serde_json::from_str::<serde_json::Value>(&reply)
                        .is_ok_and(|reply| reply["yielded"] == true)
                    {
                        eprintln!("SUPER DESKTOP: took over from the headless daemon");
                        return true;
                    }
                }
            }
            // Busy: a headless daemon may be starting a session. The caller's
            // own check refuses a busy desktop daemon as before.
            Ipc::Stalled => {}
            _ => return false,
        }
        thread::sleep(Duration::from_millis(250));
    }
    false
}

/// Whether a daemon's reply came from a headless daemon.
pub fn is_headless(reply: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(reply).is_ok_and(|reply| reply["headless"] == true)
}

/// Whether this process could draw an overlay if it were started now.
pub fn display_available() -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY", "BROADWAY_DISPLAY"]
        .iter()
        .any(|name| env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// Return once `checks` probes in a row, `STANDBY_CHECK` apart, found no
/// daemon on the socket.
fn wait_until_no_daemon(checks: u32) {
    let mut quiet = 0;
    loop {
        if matches!(crate::ipc_request("status"), Ipc::NoDaemon) {
            quiet += 1;
            if quiet >= checks {
                return;
            }
        } else {
            quiet = 0;
        }
        thread::sleep(STANDBY_CHECK);
    }
}

fn bind() -> Option<(UnixListener, u64)> {
    let path = crate::get_socket_path();
    let _ = fs::remove_file(&path);
    match UnixListener::bind(&path) {
        Ok(listener) => {
            let inode = fs::metadata(&path).map(|meta| meta.ino()).unwrap_or(0);
            Some((listener, inode))
        }
        Err(error) => {
            eprintln!("SUPER DESKTOP: headless daemon cannot bind {}: {error}", path.display());
            None
        }
    }
}

/// What one request does besides its reply.
#[derive(Debug, PartialEq)]
enum After {
    Continue,
    /// Hand the socket to a desktop daemon.
    Yield,
    /// `kill`/`quit`: stop this process.
    Quit,
}

/// Answer requests one at a time, like the desktop daemon's listener, until a
/// desktop daemon takes the socket over.
fn serve(listener: UnixListener, inode: u64) {
    let path = crate::get_socket_path();
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let Some(command) = crate::read_ipc_command(&mut stream) else { continue };
        // The bridge's change feed needs the overlay's workspace model. A
        // closed connection is the refusal it already handles.
        if command == "desktop-watch" {
            continue;
        }
        let (reply, after) = answer(&command);
        if after != After::Continue {
            state::flush_state_saves();
            // Unlink before replying, so the new owner binds a fresh socket.
            if fs::metadata(&path).map(|meta| meta.ino()).ok() == Some(inode) {
                let _ = fs::remove_file(&path);
            }
        }
        let _ = stream.write_all(reply.as_bytes());
        let _ = stream.shutdown(std::net::Shutdown::Both);
        match after {
            After::Continue => {}
            After::Yield => return,
            After::Quit => std::process::exit(0),
        }
    }
}

fn answer(command: &str) -> (String, After) {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let action = parts.first().copied().unwrap_or("toggle");
    let reply = match action {
        YIELD => return (json!({"ok": true, "yielded": true}).to_string(), After::Yield),
        "kill" | "quit" => return (json!({"ok": true, "headless": true}).to_string(), After::Quit),
        "status" => {
            let state = state::load_state();
            json!({"ok": true, "visible": false, "headless": true,
                "notes_count": state.notes.len(), "terminals_count": state.terminals.len()})
        }
        "toggle" | "show" => json!({"ok": false, "visible": false, "headless": true}),
        "hide" => json!({"ok": true, "visible": false, "headless": true}),
        "workspace-choices" => {
            let state = state::load_state();
            json!({"workspace": state::effective_workspace_dir(&state),
                "recentDirectories": state::shared_recent_dirs(&state),
                "usedDirectories": state.used_dirs})
        }
        "add-term" => add_card(parts.get(1).copied().unwrap_or("shell"), None),
        "add-term-in" => {
            let payload = command.strip_prefix("add-term-in ").unwrap_or("");
            let request: serde_json::Value = serde_json::from_str(payload).unwrap_or_default();
            match request["workspace"].as_str().and_then(state::clean_dir) {
                Some(directory) => add_card(request["agentType"].as_str().unwrap_or(""), Some(directory)),
                None => json!({"ok": false, "error": "invalid_workspace"}),
            }
        }
        "close-term" => match parts.get(1) {
            None => json!({"ok": false, "error": "missing_session"}),
            Some(session) if !session.starts_with("sd_term_") => {
                json!({"ok": false, "error": "foreign_session"})
            }
            Some(session) => close_card(session),
        },
        // Everything else needs the overlay. The bridge reads this the way it
        // reads a desktop daemon whose window is not built yet.
        _ => json!({"ok": false, "error": "desktop_not_ready", "headless": true}),
    };
    (reply.to_string(), After::Continue)
}

fn add_card(agent: &str, directory: Option<String>) -> serde_json::Value {
    let mut state = state::load_state();
    match new_card(&mut state, agent, directory, tmux::create_session) {
        Ok(id) => {
            let ok = tmux::session_exists(&id);
            if ok {
                state::save_state(&state);
            }
            json!({"ok": ok, "id": id})
        }
        Err(error) => json!({"ok": false, "error": error}),
    }
}

/// Add a card like the overlay's launch does: same folder history, folder
/// colour and custom harness command. `start` makes the session and returns
/// its name and the command it runs.
fn new_card(
    state: &mut AppState,
    agent: &str,
    directory: Option<String>,
    start: impl FnOnce(&str, Option<&str>, Option<&str>) -> (String, String),
) -> Result<String, &'static str> {
    let custom = state
        .custom_harnesses
        .iter()
        .find(|item| item.id == agent && item.validate().is_ok())
        .map(|item| item.command());
    if !tmux::HARNESS_KEYS.contains(&agent) && custom.is_none() {
        return Err("unsupported_harness");
    }
    if state.terminals.len() >= crate::workspace_model::MAX_DESKTOP_CARDS {
        return Err("too_many_desktop_cards");
    }
    let workspace_dir = directory.unwrap_or_else(|| state::effective_workspace_dir(state));
    state::remember_workspace_dir(state, &workspace_dir);
    let tag = crate::folder_colors::tag_for_new_card(state, &workspace_dir);
    crate::folder_colors::remember(state, &workspace_dir, tag);
    let (session, command) = start(agent, custom.as_deref(), Some(&workspace_dir));
    // Cascade like the overlay, on a nominal screen: the overlay keeps a
    // card's spot inside its own screen when it shows it.
    let offset = (state.terminals.len() as i32 % 5) * 32;
    state.terminals.push(TerminalData {
        id: session.clone(),
        session_name: session.clone(),
        agent_type: agent.to_string(),
        command,
        x: 100 + offset,
        y: 100 + offset,
        width: CARD_WIDTH,
        height: CARD_HEIGHT,
        restored_width: CARD_WIDTH,
        restored_height: CARD_HEIGHT,
        iconified: false,
        icon_x: None,
        icon_y: None,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
        tag,
        agent_session_id: None,
        workspace_dir: Some(workspace_dir),
    });
    state::normalize_terminal_order(state);
    Ok(session)
}

/// Close a card the way the desktop daemon does without its window.
fn close_card(session: &str) -> serde_json::Value {
    let mut state = state::load_state();
    let removed = remove_card(&mut state, session);
    if tmux::session_exists(session) {
        tmux::kill_session(session);
    }
    if removed {
        state::save_state(&state);
        json!({"ok": true, "id": session})
    } else {
        json!({"ok": false, "error": "no_such_session", "id": session})
    }
}

fn remove_card(state: &mut AppState, session: &str) -> bool {
    let before = state.terminals.len();
    state.terminals.retain(|terminal| terminal.session_name != session);
    let removed = state.terminals.len() != before;
    if removed {
        state::normalize_terminal_order(state);
    }
    removed
}

/// Bring back every saved card's session after a reboot, resuming its
/// harness conversation, exactly as the overlay does when it opens a card.
/// Sessions that are still alive are left alone.
fn restore_cards() {
    for card in state::load_state().terminals {
        if !SERVING.load(Ordering::SeqCst) {
            return;
        }
        let cwd = tmux::resolve_workspace_dir(card.workspace_dir.as_deref());
        tmux::ensure_session_with_inventory(
            &card.session_name,
            &card.agent_type,
            Some(&card.command),
            card.agent_session_id.as_deref(),
            Some(&cwd),
            None,
        );
    }
}

/// The boot service starts with systemd's bare environment (`PATH` is
/// `/usr/local/bin:/usr/bin`), so harnesses installed under the home
/// directory would be missing. Take `PATH` and anything still unset from the
/// user's login shell, which every harness session then inherits.
fn import_login_environment() {
    let shell = env::var("SHELL")
        .ok()
        .filter(|shell| std::path::Path::new(shell).is_file())
        .unwrap_or_else(|| "/bin/sh".into());
    let Ok(mut child) = Command::new(&shell)
        .args(["-l", "-c", "env -0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let mut stdout = child.stdout.take();
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        if let Some(stdout) = stdout.as_mut() {
            let _ = stdout.read_to_end(&mut output);
        }
        output
    });
    let deadline = Instant::now() + LOGIN_ENV_TIMEOUT;
    while matches!(child.try_wait(), Ok(None)) {
        if Instant::now() >= deadline {
            eprintln!("SUPER DESKTOP: the login shell did not print its environment in time");
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let Ok(output) = reader.join() else { return };
    for (name, value) in login_variables(&output, |name| env::var_os(name).is_some()) {
        env::set_var(name, value);
    }
}

/// The variables to take from `env -0` output: `PATH` always, the rest only
/// when unset here, and never the shell's own bookkeeping.
fn login_variables(output: &[u8], is_set: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    String::from_utf8_lossy(output)
        .split('\0')
        .filter_map(|entry| entry.split_once('='))
        .filter(|(name, _)| !name.is_empty() && !matches!(*name, "_" | "PWD" | "OLDPWD" | "SHLVL"))
        .filter(|(name, _)| *name == "PATH" || !is_set(name))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(command: &str) -> serde_json::Value {
        serde_json::from_str(&answer(command).0).unwrap()
    }

    #[test]
    fn headless_replies_are_recognized_and_desktop_replies_are_not() {
        assert!(is_headless(&reply("toggle").to_string()));
        assert!(!is_headless(r#"{"ok":true,"visible":false,"notes_count":0,"terminals_count":1}"#));
        assert!(!is_headless("not json"));
    }

    #[test]
    fn headless_hands_over_and_quits_on_request() {
        assert_eq!(answer(YIELD).1, After::Yield);
        assert_eq!(reply(YIELD)["yielded"], true);
        assert_eq!(answer("kill").1, After::Quit);
        assert_eq!(answer("quit").1, After::Quit);
        assert_eq!(answer("toggle").1, After::Continue);
    }

    #[test]
    fn headless_refuses_overlay_requests_as_not_ready() {
        for command in ["desktop-workspace", "desktop-command {}", "add-note hi"] {
            let value = reply(command);
            assert_eq!(value["ok"], false, "{command}");
            assert_eq!(value["error"], "desktop_not_ready", "{command}");
        }
        assert_eq!(reply("toggle")["visible"], false);
        assert_eq!(reply("close-term other")["error"], "foreign_session");
        assert_eq!(reply("close-term")["error"], "missing_session");
        assert_eq!(reply("add-term-in {\"agentType\":\"shell\",\"workspace\":\"relative\"}")["error"], "invalid_workspace");
    }

    #[test]
    fn headless_new_card_is_saved_like_the_overlay_makes_it() {
        let mut state = AppState::default();
        let folder = std::env::temp_dir().to_string_lossy().to_string();
        let id = new_card(&mut state, "shell", Some(folder.clone()), |agent, custom, dir| {
            assert_eq!((agent, custom, dir), ("shell", None, Some(folder.as_str())));
            ("sd_term_headless_test".into(), "/bin/sh".into())
        })
        .unwrap();
        assert_eq!(id, "sd_term_headless_test");
        let card = &state.terminals[0];
        assert_eq!((card.width, card.height, card.command.as_str()), (CARD_WIDTH, CARD_HEIGHT, "/bin/sh"));
        assert_eq!(card.workspace_dir.as_deref(), Some(folder.as_str()));
        assert_eq!(state.used_dirs[0], folder);
        assert!(state.terminal_order.contains(&id));
        assert_eq!(
            new_card(&mut state, "no-such-harness", None, |_, _, _| unreachable!()),
            Err("unsupported_harness")
        );
        assert!(remove_card(&mut state, &id));
        assert!(state.terminals.is_empty() && !state.terminal_order.contains(&id));
        assert!(!remove_card(&mut state, &id));
    }

    #[test]
    fn login_environment_supplies_path_and_only_missing_variables() {
        let output = b"PATH=/home/u/.local/bin:/usr/bin\0HOME=/elsewhere\0EDITOR=nvim\0_=/usr/bin/env\0SHLVL=2\0PWD=/\0\0";
        let taken = login_variables(output, |name| matches!(name, "PATH" | "HOME"));
        assert_eq!(
            taken,
            vec![
                ("PATH".to_string(), "/home/u/.local/bin:/usr/bin".to_string()),
                ("EDITOR".to_string(), "nvim".to_string()),
            ]
        );
    }
}
