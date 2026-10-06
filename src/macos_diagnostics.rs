//! Read-only launch diagnostics; never initialize GTK or start a daemon.
use std::{fs::File, io::Read, path::PathBuf};

pub fn print() {
    println!("SUPER DESKTOP {} — macOS diagnostics", env!("CARGO_PKG_VERSION"));
    if let Ok(exe) = std::env::current_exe() {
        println!("Executable: {}", exe.display());
    }
    println!("Control socket: {}", crate::get_socket_path().display());
    match crate::ipc_request_with(&crate::get_socket_path(), "status", false, |path| {
        std::os::unix::net::UnixStream::connect(path)
    }) {
        crate::Ipc::Reply(_) => println!("App: running and reachable"),
        crate::Ipc::NoDaemon => println!("App: not running; open SUPER DESKTOP Dev.app first"),
        crate::Ipc::Stalled => println!("App: busy or inaccessible; no duplicate was started"),
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let config = home.join(".config/super-desktop");
    // Do not call load_state: it can migrate and save the user's state.
    let state = read_bounded(config.join("state.json"), 4 * 1024 * 1024)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let combo = crate::shortcut::current_combo(
        state.as_ref().and_then(|value| value["toggle_shortcut"].as_str()),
    );
    println!("Configured shortcut: {combo}");
    if let Err(error) = crate::platform::macos_key::parse(&combo) {
        println!("Shortcut configuration error: {error}");
    }
    println!("The shortcut only works while the app is running; the SD menu also offers Show / Hide.");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let tmux = std::env::split_paths(&path).map(|dir| dir.join("tmux")).find(|file| {
        use std::os::unix::fs::PermissionsExt;
        file.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    });
    println!("tmux: {}", tmux.map(|p| p.display().to_string()).unwrap_or_else(|| "missing".into()));
    if let Some(bytes) = read_bounded(config.join("panic.log"), 1024 * 1024) {
        // Only our launch markers, never panic payloads, note text or prompts.
        let text = String::from_utf8_lossy(&bytes);
        let records: Vec<_> = text.lines().filter(|line| {
            line.starts_with('[') && (line.contains("] shortcut registered:")
                || line.contains("] shortcut registration failed:")
                || line.contains("] cannot bind control socket")
                || line.contains("] super-desktop daemon"))
        }).collect();
        for record in records.iter().rev().take(8).rev() {
            println!("{record}");
        }
    }
}

fn read_bounded(path: PathBuf, limit: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path).ok()?.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}
