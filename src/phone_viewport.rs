//! A temporary sizing client owned by one opted-in mobile terminal stream.
use crate::tmux_control::Control;
use std::time::{Duration, Instant};

pub const LEASE_TIME: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Info {
    pub version: u8,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Grid {
    columns: u16,
    rows: u16,
}

enum Request {
    Resize(Grid),
    Release,
    Invalid,
    Ignore,
}

fn request(text: &str) -> Request {
    let Ok(body) = serde_json::from_str::<serde_json::Value>(text) else {
        return Request::Ignore;
    };
    if body["type"] != "viewport" {
        return Request::Ignore;
    }
    if body["release"] == true {
        return Request::Release;
    }
    match (body["columns"].as_u64(), body["rows"].as_u64()) {
        (Some(columns @ 20..=500), Some(rows @ 5..=300)) => Request::Resize(Grid {
            columns: columns as u16,
            rows: rows as u16,
        }),
        _ => Request::Invalid,
    }
}

pub struct Lease {
    control: Option<Control>,
    grid: Option<Grid>,
    renewed: Instant,
    pub info: Info,
}

impl Lease {
    pub fn new() -> Self {
        Self {
            control: None,
            grid: None,
            renewed: Instant::now(),
            info: Info {
                version: 1,
                active: false,
                reason: None,
            },
        }
    }

    fn release(&mut self, reason: Option<&'static str>) {
        self.control = None;
        self.grid = None;
        self.info.active = false;
        self.info.reason = reason;
    }

    pub fn expire(&mut self, now: Instant) {
        if self.control.is_some() && now.saturating_duration_since(self.renewed) >= LEASE_TIME {
            self.release(Some("expired"));
        }
    }

    pub fn on_message(&mut self, text: &str, id: &str) {
        self.apply(text, Instant::now(), || Control::open(id));
    }

    fn apply(&mut self, text: &str, now: Instant, open: impl FnOnce() -> Result<Control, String>) {
        let grid = match request(text) {
            Request::Ignore => return,
            Request::Release => {
                self.release(None);
                return;
            }
            Request::Invalid => {
                self.info.reason = Some("invalid_size");
                return;
            }
            Request::Resize(grid) => grid,
        };
        if self.control.is_none() {
            match open() {
                Ok(control) => self.control = Some(control),
                Err(_) => {
                    self.release(Some("terminal_unavailable"));
                    return;
                }
            }
        }
        let control = self.control.as_mut().unwrap();
        match control.phone_viewport_available() {
            Ok(true) => {}
            Ok(false) => {
                self.release(Some("unsupported_layout"));
                return;
            }
            Err(_) => {
                self.release(Some("terminal_unavailable"));
                return;
            }
        }
        // A heartbeat renews the lease without competing with another phone
        // or the PC for tmux's latest-active-client sizing ownership.
        if self.grid != Some(grid) && control.set_phone_viewport(grid.columns, grid.rows).is_err() {
            self.release(Some("terminal_unavailable"));
            return;
        }
        self.grid = Some(grid);
        self.renewed = now;
        self.info.active = true;
        self.info.reason = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct Server(String);
    impl Server {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let server = Self(format!(
                "sd-viewport-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            server.run(&[
                "new-session",
                "-d",
                "-s",
                "sd_term_viewport",
                "-x",
                "120",
                "-y",
                "40",
                "/bin/sleep",
                "1000",
            ]);
            server
        }
        fn command(&self) -> Command {
            let mut command = Command::new("tmux");
            command
                .args(["-L", &self.0, "-f", "/dev/null"])
                .env_remove("TMUX")
                .env_remove("TMUX_PANE");
            command
        }
        fn run(&self, args: &[&str]) -> String {
            let output = self.command().args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        fn open(&self) -> Result<Control, String> {
            Control::open_for_test("sd_term_viewport", self.command())
        }
        fn grid(&self) -> String {
            self.run(&[
                "display-message",
                "-p",
                "-t",
                "sd_term_viewport",
                "#{pane_width}x#{pane_height}",
            ])
        }
        fn wait_grid(&self, expected: &str) {
            let until = Instant::now() + Duration::from_secs(3);
            loop {
                let actual = self.grid();
                if actual == expected {
                    return;
                }
                assert!(Instant::now() < until, "expected {expected}, got {actual}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.command().arg("kill-server").output();
        }
    }

    #[test]
    fn viewport_messages_are_optional_bounded_and_ignore_unknown_fields() {
        assert!(matches!(
            request(r#"{"type":"viewport","columns":42,"rows":30,"future":1}"#),
            Request::Resize(Grid {
                columns: 42,
                rows: 30
            })
        ));
        for text in [r#"{"text":"hi"}"#, "not json", r#"{"type":"future"}"#] {
            assert!(matches!(request(text), Request::Ignore));
        }
        for (columns, rows) in [(19, 30), (501, 30), (80, 4), (80, 301), (-1, 30)] {
            assert!(matches!(
                request(&format!(
                    r#"{{"type":"viewport","columns":{columns},"rows":{rows}}}"#
                )),
                Request::Invalid
            ));
        }
        assert!(matches!(
            request(r#"{"type":"viewport","columns":"42","rows":30}"#),
            Request::Invalid
        ));
        assert!(matches!(
            request(r#"{"type":"viewport","release":true}"#),
            Request::Release
        ));
    }

    #[test]
    fn viewport_resizes_releases_expires_and_preserves_the_session() {
        let server = Server::new();
        let mut desktop = server.open().unwrap();
        desktop.set_phone_viewport(120, 40).unwrap();
        server.wait_grid("120x40");
        let pid = server.run(&[
            "display-message",
            "-p",
            "-t",
            "sd_term_viewport",
            "#{pane_pid}",
        ]);
        // Existing input/watch clients remain size-neutral.
        let _old_phone = server.open().unwrap();
        server.wait_grid("120x40");
        let mut lease = Lease::new();
        let now = Instant::now();
        let phone = r#"{"type":"viewport","columns":42,"rows":30}"#;
        lease.apply(phone, now, || server.open());
        server.wait_grid("42x30");
        assert!(lease.info.active);
        lease.apply(
            r#"{"type":"viewport","columns":80,"rows":20}"#,
            now,
            || unreachable!(),
        );
        server.wait_grid("80x20");
        lease.apply(
            r#"{"type":"viewport","release":true}"#,
            now,
            || unreachable!(),
        );
        server.wait_grid("120x40");
        assert!(!lease.info.active);
        lease.apply(phone, now, || server.open());
        server.wait_grid("42x30");
        lease.apply(phone, now + Duration::from_secs(10), || unreachable!());
        lease.expire(now + Duration::from_secs(31));
        assert!(lease.info.active, "heartbeats renew the lease");
        lease.expire(now + Duration::from_secs(40));
        server.wait_grid("120x40");
        assert_eq!(lease.info.reason, Some("expired"));
        lease.apply(phone, now, || server.open());
        server.wait_grid("42x30");
        drop(lease);
        server.wait_grid("120x40");
        assert_eq!(
            server.run(&[
                "display-message",
                "-p",
                "-t",
                "sd_term_viewport",
                "#{pane_pid}"
            ]),
            pid
        );
        assert_eq!(
            server.run(&[
                "show-options",
                "-w",
                "-v",
                "-t",
                "sd_term_viewport",
                "window-size"
            ]),
            "",
            "no local option override was introduced"
        );
        assert_eq!(
            server.run(&[
                "display-message",
                "-p",
                "-t",
                "sd_term_viewport",
                "#{window-size}"
            ]),
            "latest"
        );
    }

    #[test]
    fn viewport_heartbeats_do_not_fight_another_phone() {
        let server = Server::new();
        let mut desktop = server.open().unwrap();
        desktop.set_phone_viewport(120, 40).unwrap();
        server.wait_grid("120x40");
        let mut first = Lease::new();
        let mut second = Lease::new();
        let now = Instant::now();
        let narrow = r#"{"type":"viewport","columns":42,"rows":30}"#;
        first.apply(narrow, now, || server.open());
        server.wait_grid("42x30");
        second.apply(r#"{"type":"viewport","columns":70,"rows":25}"#, now, || {
            server.open()
        });
        server.wait_grid("70x25");
        first.apply(narrow, now + Duration::from_secs(10), || unreachable!());
        server.wait_grid("70x25");
        drop(second);
        server.wait_grid("42x30");
        drop(first);
        server.wait_grid("120x40");
    }

    #[test]
    fn viewport_respects_custom_size_policies_and_split_layouts() {
        let server = Server::new();
        let mut desktop = server.open().unwrap();
        desktop.set_phone_viewport(120, 40).unwrap();
        server.wait_grid("120x40");
        let mut lease = Lease::new();
        let message = r#"{"type":"viewport","columns":42,"rows":30}"#;
        for policy in ["manual", "largest", "smallest"] {
            server.run(&[
                "set-option",
                "-w",
                "-t",
                "sd_term_viewport",
                "window-size",
                policy,
            ]);
            lease.apply(message, Instant::now(), || server.open());
            assert_eq!(lease.info.reason, Some("unsupported_layout"));
            assert!(!lease.info.active);
            server.wait_grid("120x40");
        }
        server.run(&[
            "set-option",
            "-w",
            "-t",
            "sd_term_viewport",
            "window-size",
            "latest",
        ]);
        server.run(&[
            "split-window",
            "-d",
            "-t",
            "sd_term_viewport",
            "/bin/sleep",
            "1000",
        ]);
        lease.apply(message, Instant::now(), || server.open());
        assert_eq!(lease.info.reason, Some("unsupported_layout"));
    }
}
