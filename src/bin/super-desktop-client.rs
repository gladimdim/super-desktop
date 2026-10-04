//! Small, GTK-free IPC entry point. Never replay a command after connecting:
//! a timed-out toggle may already have been applied by the daemon.
use std::io::{Read, Write};
use std::os::unix::{net::UnixStream, process::CommandExt};
use std::path::Path;
use std::time::Duration;
use super_desktop::{harness_record, platform};

fn request(path: &Path, command: &str) -> std::io::Result<Option<String>> {
    let stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None)
        }
        Err(e) => return Err(e),
    };
    exchange(stream, command).map(Some)
}

fn exchange(mut stream: UnixStream, command: &str) -> std::io::Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(format!("{command}\n").as_bytes())?;
    let mut reply = String::new();
    stream.take(64 * 1024).read_to_string(&mut reply)?;
    if reply.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "empty daemon response",
        ));
    }
    Ok(reply)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = args.first().map(String::as_str).unwrap_or("toggle");
    if action == "harness-event" {
        harness_record::record(args.get(1).map(String::as_str).unwrap_or(""));
        return;
    }
    if args.len() <= 1 && matches!(action, "toggle" | "show" | "hide" | "status" | "kill") {
        match request(&platform::runtime::socket_path(), action) {
            Ok(Some(reply)) => {
                if action == "kill" {
                    println!("SUPER DESKTOP: {}", reply.trim());
                } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(&reply) {
                    let visible = value["visible"].as_bool().unwrap_or(false);
                    match action {
                        "status" => println!(
                            "SUPER DESKTOP (Rust): {}\nNotes: {}, Terminals: {}",
                            if visible { "Visible" } else { "Hidden" },
                            value["notes_count"].as_i64().unwrap_or(0),
                            value["terminals_count"].as_i64().unwrap_or(0)
                        ),
                        "toggle" => println!(
                            "SUPER DESKTOP (Rust): {}",
                            if visible { "Shown" } else { "Hidden" }
                        ),
                        _ => println!("SUPER DESKTOP (Rust): {}", reply.trim()),
                    }
                } else {
                    println!("SUPER DESKTOP (Rust): {}", reply.trim());
                }
                return;
            }
            Ok(None) => {} // No listener: the application owns cold-start handling.
            Err(e) => {
                eprintln!("SUPER DESKTOP: control request failed ({e}); not retrying `{action}`");
                std::process::exit(2);
            }
        }
    }
    let exe = std::env::current_exe().expect("client executable path");
    let mut command = std::process::Command::new(exe.with_file_name("super-desktop"));
    command.args(args);
    #[cfg(target_os = "linux")]
    {
        let library = "/usr/lib/libgtk4-layer-shell.so";
        if Path::new(library).exists() {
            let preload = std::env::var("LD_PRELOAD").unwrap_or_default();
            if !preload.split([':', ' ']).any(|item| item == library) {
                command.env(
                    "LD_PRELOAD",
                    if preload.is_empty() {
                        library.to_string()
                    } else {
                        format!("{library}:{preload}")
                    },
                );
            }
        }
    }
    eprintln!(
        "SUPER DESKTOP: could not start application: {}",
        command.exec()
    );
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_socket_is_safe_to_delegate() {
        assert!(request(
            Path::new("/nonexistent-super-desktop-test/control.sock"),
            "status"
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn connected_empty_reply_is_not_safe_to_replay() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut buf = [0; 7];
            server.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"toggle\n");
        });
        assert_eq!(
            exchange(client, "toggle").unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        worker.join().unwrap();
    }

    #[test]
    fn connected_reply_is_returned_without_replaying_the_command() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut buf = [0; 7];
            server.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"status\n");
            server.write_all(b"{\"visible\":true}").unwrap();
        });
        assert_eq!(exchange(client, "status").unwrap(), "{\"visible\":true}");
        worker.join().unwrap();
    }
}
