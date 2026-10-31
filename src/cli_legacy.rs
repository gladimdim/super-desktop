//! The legacy daemon socket both entry points speak for `status`, `toggle`
//! and the other legacy commands: one command line out, then the reply until
//! the daemon closes the connection. A command is never replayed: a timed-out
//! toggle may already have been applied.
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// How long a client waits for the daemon. Longer than the daemon's own 2s
/// wait for the GTK thread, so a slow answer is not mistaken for no daemon.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// The outcome of connecting to the daemon socket.
pub enum Dial {
    Connected(UnixStream),
    /// No socket file: nothing is listening.
    Missing,
    /// A leftover socket file from a daemon that is gone.
    Refused,
    /// Anything else, such as a sandbox denial, says nothing about liveness.
    Failed(io::Error),
}

pub fn dial(connected: io::Result<UnixStream>) -> Dial {
    match connected {
        Ok(stream) => Dial::Connected(stream),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Dial::Missing,
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => Dial::Refused,
        Err(e) => Dial::Failed(e),
    }
}

/// Sends one command line.
pub fn send(stream: &mut UnixStream, command: &str) -> io::Result<()> {
    stream.write_all(format!("{command}\n").as_bytes())
}

/// Reads the reply until the daemon closes the connection, at most `limit` bytes.
pub fn receive(stream: &mut UnixStream, limit: u64) -> io::Result<String> {
    let mut reply = String::new();
    stream.take(limit).read_to_string(&mut reply)?;
    Ok(reply)
}

/// The legacy text for a `status` or `toggle` reply.
pub fn visibility_text(action: &str, reply: &serde_json::Value) -> Option<String> {
    let visible = reply["visible"].as_bool().unwrap_or(false);
    match action {
        "status" => Some(format!(
            "SUPER DESKTOP (Rust): {}\nNotes: {}, Terminals: {}",
            if visible { "Visible" } else { "Hidden" },
            reply["notes_count"].as_i64().unwrap_or(0),
            reply["terminals_count"].as_i64().unwrap_or(0)
        )),
        "toggle" => Some(format!(
            "SUPER DESKTOP (Rust): {}",
            if visible { "Shown" } else { "Hidden" }
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_text_keeps_the_legacy_lines() {
        let reply = serde_json::json!({"visible":true,"notes_count":2,"terminals_count":3});
        assert_eq!(
            visibility_text("status", &reply).unwrap(),
            "SUPER DESKTOP (Rust): Visible\nNotes: 2, Terminals: 3"
        );
        assert_eq!(visibility_text("toggle", &reply).unwrap(), "SUPER DESKTOP (Rust): Shown");
        let hidden = serde_json::json!({});
        assert_eq!(
            visibility_text("status", &hidden).unwrap(),
            "SUPER DESKTOP (Rust): Hidden\nNotes: 0, Terminals: 0"
        );
        assert_eq!(visibility_text("toggle", &hidden).unwrap(), "SUPER DESKTOP (Rust): Hidden");
        assert!(visibility_text("show", &reply).is_none());
    }
}
