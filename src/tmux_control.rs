//! A private control client for a phone terminal. It never resizes the desktop
//! pane, and closes only its own client when the phone disconnects.
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub struct Control {
    child: Child,
    input: ChildStdin,
    lines: Receiver<String>,
    pane: String,
    healthy: bool,
}

impl Control {
    pub fn open(session: &str) -> Result<Self, String> {
        let mut child = Command::new("tmux")
            .args([
                "-C",
                "attach-session",
                "-f",
                "ignore-size,no-output",
                "-t",
                &format!("={session}"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::sync_channel(512);
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            input,
            lines,
            pane: String::new(),
            healthy: true,
        };
        client.response()?; // attach-session acknowledgement
        let pane = client.command("display-message -p '#{pane_id}'")?;
        let pane = pane.trim();
        if !pane.starts_with('%') || !pane[1..].chars().all(|c| c.is_ascii_digit()) {
            return Err("Invalid tmux pane".into());
        }
        client.pane = pane.to_string();
        Ok(client)
    }

    fn response(&mut self) -> Result<String, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut block: Option<String> = None;
        let mut output = String::new();
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| "tmux control connection lost or timed out".to_string())?;
            if let Some(tag) = block.as_ref() {
                if line == format!("%end {tag}") {
                    return Ok(output);
                }
                if line == format!("%error {tag}") {
                    return Err(output);
                }
                if output.len() + line.len() > 1 << 20 {
                    return Err("tmux response too large".into());
                }
                output.push_str(&line);
                output.push('\n');
            } else if let Some(tag) = line.strip_prefix("%begin ") {
                block = Some(tag.to_string());
            } else if line.starts_with("%exit") {
                return Err("Terminal session ended".into());
            }
        }
    }

    fn command(&mut self, command: &str) -> Result<String, String> {
        if !self.healthy {
            return Err("tmux connection must be reopened".into());
        }
        self.healthy = false;
        writeln!(self.input, "{command}").map_err(|e| e.to_string())?;
        self.input.flush().map_err(|e| e.to_string())?;
        let result = self.response();
        self.healthy = result.is_ok();
        result
    }

    pub fn send(&mut self, text: &str, enter: bool) -> Result<(), String> {
        if !text.is_empty() {
            // Hex bytes keep all user input out of the tmux command language.
            let bytes = text
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            self.command(&format!("send-keys -t {} -H {bytes}", self.pane))?;
        }
        if enter {
            self.command(&format!("send-keys -t {} Enter", self.pane))?;
        }
        Ok(())
    }

    pub fn capture(&mut self) -> Result<String, String> {
        self.command(&format!("capture-pane -p -e -S -300 -t {}", self.pane))
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
