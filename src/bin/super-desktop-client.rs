//! Small, GTK-free IPC entry point. Never replay a command after connecting:
//! a timed-out toggle may already have been applied by the daemon.
use std::os::unix::{net::UnixStream, process::CommandExt};
use std::path::Path;
use super_desktop::cli_legacy::{self, Dial};
use super_desktop::{cli, harness_record, platform, preload};

fn request(path: &Path, command: &str) -> std::io::Result<Option<String>> {
    match cli_legacy::dial(UnixStream::connect(path)) {
        Dial::Connected(stream) => exchange(stream, command).map(Some),
        Dial::Missing | Dial::Refused => Ok(None),
        Dial::Failed(e) => Err(e),
    }
}

fn exchange(mut stream: UnixStream, command: &str) -> std::io::Result<String> {
    stream.set_read_timeout(Some(cli_legacy::TIMEOUT))?;
    stream.set_write_timeout(Some(cli_legacy::TIMEOUT))?;
    cli_legacy::send(&mut stream, command)?;
    let reply = cli_legacy::receive(&mut stream, 64 * 1024)?;
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
    if let Some(code) = cli::run(&args) {
        std::process::exit(code);
    }
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
                } else if let Some(text) = serde_json::from_str(&reply)
                    .ok()
                    .and_then(|value| cli_legacy::visibility_text(action, &value))
                {
                    println!("{text}");
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
    preload::preload_layer_shell(&mut command);
    eprintln!(
        "SUPER DESKTOP: could not start application: {}",
        command.exec()
    );
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

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
