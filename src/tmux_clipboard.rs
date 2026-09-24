//! Mouse copy from a card's tmux pane to the desktop clipboard.
//!
//! Cards are VTE widgets running `tmux attach`, and the user's tmux config
//! (Omarchy ships `mouse on` and `set-clipboard on`) makes tmux own mouse
//! selection: releasing a drag runs `copy-pipe-and-cancel`, which clears the
//! highlight and forwards the text as OSC 52. VTE ignores OSC 52, so nothing
//! reached the Wayland clipboard. Apps that copy themselves (fullscreen Claude
//! Code's own selection, `/copy`-style commands) also use OSC 52.
//!
//! Both paths are routed to `wl-copy`, for SUPER DESKTOP sessions only:
//! * tmux's mouse copy bindings (drag end, double and triple click) are
//!   wrapped in `if-shell -F "#{m:sd_term_*,#{session_name}}"`. Card sessions
//!   run the same binding with `copy-pipe-and-cancel "wl-copy"` and a short
//!   "Copied to clipboard" message; every other session runs the user's
//!   original binding unchanged. (`copy-command` cannot do this: it is a server
//!   option, is not format-expanded, and its job does not know the pane.)
//!   A reloaded tmux config restores the originals; the next card session
//!   setup wraps them again.
//! * OSC 52 from a card's pane fires the pane-scoped `pane-set-clipboard` hook.
//!
//! End-to-end check with real mouse input: `python3 tests/card_mouse_copy_smoke.py`.

use std::process::Command;

const SESSION_PREFIX: &str = "sd_term_";
const CONDITION: &str = "#{m:sd_term_*,#{session_name}}";
const COPY: &str = "copy-pipe-and-cancel";

/// Mouse bindings that copy in tmux's default and vi copy tables.
const BINDINGS: &[(&str, &str)] = &[
    ("copy-mode", "MouseDragEnd1Pane"),
    ("copy-mode", "DoubleClick1Pane"),
    ("copy-mode", "TripleClick1Pane"),
    ("copy-mode-vi", "MouseDragEnd1Pane"),
    ("copy-mode-vi", "DoubleClick1Pane"),
    ("copy-mode-vi", "TripleClick1Pane"),
    ("root", "DoubleClick1Pane"),
    ("root", "TripleClick1Pane"),
];

/// The card-session variant of a binding: every bare `copy-pipe-and-cancel`
/// (no command of its own) pipes to `copier` and confirms the copy. `None`
/// when the binding does not copy that way, e.g. a user's own copy command.
fn card_variant(command: &str, copier: &str) -> Option<String> {
    let mut out = String::with_capacity(command.len() + 64);
    let mut rest = command;
    let mut changed = false;
    while let Some(at) = rest.find(COPY) {
        let after = &rest[at + COPY.len()..];
        let next = after.trim_start();
        let bare = next.is_empty()
            || next.starts_with(';')
            || next.starts_with('}')
            || next.starts_with("\\;");
        out.push_str(&rest[..at + COPY.len()]);
        if bare {
            out.push_str(&format!(
                " \"{copier}\" ; display-message -d 1500 \"Copied to clipboard\""
            ));
            changed = true;
        }
        rest = after;
    }
    out.push_str(rest);
    changed.then_some(out)
}

/// `pane-set-clipboard` hook command for one card's pane.
fn pane_hook_with(copier: &str) -> String {
    format!("run-shell -b \"tmux save-buffer - | {copier}\"")
}

fn tmux(base: &[&str]) -> Command {
    let mut command = Command::new(base[0]);
    command.args(&base[1..]);
    command
}

/// The command bound to `key` in `table`, as `list-keys` prints it
/// (`bind-key [-r] [-N note] -T table key command`).
fn bound_command(base: &[&str], table: &str, key: &str) -> Option<String> {
    let out = tmux(base).args(["list-keys", "-T", table]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let marker = format!(" -T {table} ");
    text.lines().find_map(|line| {
        // list-keys pads columns with extra spaces; normalise the prefix only.
        let (head, _) = line.split_at(line.find(key)?);
        let head = head.split_whitespace().collect::<Vec<_>>().join(" ") + " ";
        if !head.starts_with("bind-key") || !head.contains(&marker) || !head.ends_with(&marker[1..]) {
            return None;
        }
        let command = line[line.find(key)? + key.len()..].trim();
        // Guard against a key name that is a prefix of another (e.g. ...Pane vs ...PaneX).
        line[line.find(key)? + key.len()..].starts_with(' ').then(|| command.to_string())
    })
}

fn install_bindings_on(base: &[&str], copier: &str) {
    for (table, key) in BINDINGS {
        let Some(original) = bound_command(base, table, key) else { continue };
        if original.contains(CONDITION) {
            continue; // Already wrapped.
        }
        let Some(card) = card_variant(&original, copier) else { continue };
        let _ = tmux(base)
            .args(["bind-key", "-T", table, key, "if-shell", "-F", CONDITION, &card, &original])
            .output();
    }
}

fn install_pane_hook_on(base: &[&str], session: &str, hook: &str) {
    let _ = tmux(base)
        .args(["set-hook", "-p", "-t", &format!("={session}:"), "pane-set-clipboard", hook])
        .output();
}

/// Route mouse and OSC 52 copies of `session` to the desktop clipboard.
/// Idempotent; called whenever a card's session is created or attached.
pub fn install_session(session: &str) {
    if !session.starts_with(SESSION_PREFIX) {
        return;
    }
    let bin = crate::tmux::tmux_bin();
    let base = [bin.as_str()];
    install_bindings_on(&base, "wl-copy");
    install_pane_hook_on(&base, session, &pane_hook_with("wl-copy"));
}

/// Daemon start: cover cards whose sessions outlived the previous daemon.
pub fn install_existing_sessions() {
    let Ok(out) = Command::new(crate::tmux::tmux_bin())
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    for session in String::from_utf8_lossy(&out.stdout).lines() {
        install_session(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

    struct Server {
        socket: String,
    }

    impl Server {
        fn start(tag: &str) -> Option<Self> {
            let server = Server { socket: format!("sd-clip-{tag}-{}", std::process::id()) };
            // Keep an idle session so the server survives between commands.
            let ok = server
                .cmd()
                .args(["-f", "/dev/null", "new-session", "-d", "-s", "keepalive", "sleep 60"])
                .status()
                .ok()?;
            ok.success().then_some(server)
        }
        fn base(&self) -> [&str; 3] {
            ["tmux", "-L", &self.socket]
        }
        fn cmd(&self) -> Command {
            let mut command = Command::new("tmux");
            // An inherited TMUX/TMUX_PANE would leak into the test server's jobs.
            command.env_remove("TMUX").env_remove("TMUX_PANE").args(["-L", &self.socket]);
            command
        }
        fn session(&self, name: &str, script: &str) {
            let ok = self
                .cmd()
                .args(["new-session", "-d", "-s", name, "-x", "80", "-y", "10", script])
                .status()
                .unwrap();
            assert!(ok.success());
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.cmd().arg("kill-server").output();
        }
    }

    fn wait_for(path: &Path) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(path) {
                if !text.is_empty() {
                    return Some(text);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sd-clip-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn card_variant_pipes_only_bare_copies() {
        assert_eq!(
            card_variant("send-keys -X copy-pipe-and-cancel", "wl-copy").unwrap(),
            r#"send-keys -X copy-pipe-and-cancel "wl-copy" ; display-message -d 1500 "Copied to clipboard""#
        );
        let root = r##"select-pane -t = \; if-shell -F "#{||:#{pane_in_mode},#{mouse_any_flag}}" { send-keys -M } { copy-mode -H ; send-keys -X select-word ; run-shell -d 0.3 ; send-keys -X copy-pipe-and-cancel }"##;
        let card = card_variant(root, "wl-copy").unwrap();
        assert!(
            card.ends_with(r#"copy-pipe-and-cancel "wl-copy" ; display-message -d 1500 "Copied to clipboard" }"#),
            "{card}"
        );
        assert!(card.contains("send-keys -M"), "apps that own the mouse must still get it");
        // A user's own copy command, or a non-copying binding, is left alone.
        assert!(card_variant(r#"send-keys -X copy-pipe-and-cancel "xclip -in""#, "wl-copy").is_none());
        assert!(card_variant("select-pane", "wl-copy").is_none());
    }

    #[test]
    fn bindings_are_wrapped_once_and_keep_the_original_for_other_sessions() {
        let Some(server) = Server::start("bind") else { return };
        let base = server.base();
        let before = bound_command(&base, "copy-mode", "MouseDragEnd1Pane").expect("default binding");
        assert_eq!(before, "send-keys -X copy-pipe-and-cancel");
        install_bindings_on(&base, "wl-copy");
        install_bindings_on(&base, "wl-copy");
        let after = bound_command(&base, "copy-mode", "MouseDragEnd1Pane").unwrap();
        assert!(after.starts_with(&format!("if-shell -F \"{CONDITION}\"")), "{after}");
        assert_eq!(after.matches("if-shell -F").count(), 1, "wrapped twice: {after}");
        assert!(after.ends_with(&format!("\"{before}\"")), "other sessions must keep {before:?}: {after}");
        for (table, key) in BINDINGS {
            let bound = bound_command(&base, table, key).unwrap_or_default();
            assert!(bound.contains("wl-copy"), "{table} {key} not routed: {bound}");
        }
    }

    /// A real mouse drag through an attached client (via `script`, which gives
    /// tmux a terminal), the way a card's VTE sends it.
    fn drag_first_line(server: &Server, session: &str) {
        use std::io::Write;
        let attach = format!("tmux -L {} attach -t ={session}", server.socket);
        let mut client = Command::new("script")
            .args(["-qfec", &attach, "/dev/null"])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("TERM", "xterm-256color")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("script(1) is needed for the mouse test");
        let stdin = client.stdin.as_mut().unwrap();
        std::thread::sleep(Duration::from_millis(800));
        for event in ["\x1b[<0;1;1M", "\x1b[<32;6;1M", "\x1b[<32;12;1M", "\x1b[<0;12;1m"] {
            stdin.write_all(event.as_bytes()).unwrap();
            stdin.flush().unwrap();
            std::thread::sleep(Duration::from_millis(120));
        }
        std::thread::sleep(Duration::from_millis(800));
        let _ = server.cmd().args(["detach-client", "-s", &format!("={session}")]).output();
        let _ = client.kill();
        let _ = client.wait();
    }

    #[test]
    fn a_mouse_drag_copies_card_text_and_leaves_other_sessions_alone() {
        if Command::new("script").arg("--version").output().is_err() {
            return;
        }
        let Some(server) = Server::start("drag") else { return };
        let dir = scratch("drag");
        let out = dir.join("copied");
        server.cmd().args(["set-option", "-g", "mouse", "on"]).status().unwrap();
        server.session("sd_term_card", "printf 'card text here\\n'; sleep 30");
        server.session("personal", "printf 'personal text\\n'; sleep 30");
        install_bindings_on(&server.base(), &format!("cat >> {}", out.display()));

        drag_first_line(&server, "personal");
        drag_first_line(&server, "sd_term_card");
        let copied = wait_for(&out).expect("the card drag did not reach the copier");
        assert_eq!(copied.trim(), "card text h", "only the card's selection may be piped");
        // The personal session still copied into tmux as before.
        let buffers = server.cmd().arg("list-buffers").output().unwrap();
        assert!(String::from_utf8_lossy(&buffers.stdout).contains("personal te"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn app_osc52_copies_reach_the_clipboard_only_for_card_panes() {
        let Some(server) = Server::start("osc") else { return };
        let dir = scratch("osc");
        let out = dir.join("copied");
        // The hook saves tmux's newest buffer, so copies within milliseconds
        // of each other in different panes could swap; stagger them here.
        let emit = |text: &str, delay: &str| {
            format!("sleep {delay}; printf '\\033]52;c;%s\\a' $(printf {text} | base64); sleep 30")
        };
        server.cmd().args(["set-option", "-g", "set-clipboard", "on"]).status().unwrap();
        server.session("sd_term_card", &emit("CARDCOPY", "0.8"));
        server.session("personal", &emit("PERSONALCOPY", "2"));
        // Hooks are installed per card pane only; the personal session has none.
        install_pane_hook_on(
            &server.base(),
            "sd_term_card",
            &pane_hook_with(&format!("cat >> {}", out.display())),
        );
        wait_for(&out).expect("OSC 52 from the card pane was not copied");
        std::thread::sleep(Duration::from_millis(2000));
        let copied = std::fs::read_to_string(&out).unwrap();
        assert!(copied.contains("CARDCOPY"), "{copied:?}");
        assert!(!copied.contains("PERSONALCOPY"), "a non-card pane reached the clipboard: {copied:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
