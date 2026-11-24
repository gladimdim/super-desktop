//! Desktop notifications when background or hidden cards finish or need input.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const COOLDOWN: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub critical: bool,
}

#[derive(Default)]
pub struct Notifier {
    last_status: HashMap<String, String>,
    last_sent: HashMap<String, Instant>,
}

impl Notifier {
    /// Observe a status transition for `session`. Emits a notification only on
    /// `WORKING` -> `FINISHED | WAITING | ERROR` when `should_notify` is true
    /// and outside the per-session cooldown window.
    pub fn observe(
        &mut self,
        session: &str,
        card_title: &str,
        new_status: &str,
        should_notify: bool,
        now: Instant,
    ) -> Option<Notification> {
        let prev = self.last_status.insert(session.to_string(), new_status.to_string());
        if !should_notify || prev.as_deref() != Some("WORKING") {
            return None;
        }
        let (title, critical) = match new_status {
            "FINISHED" => ("Agent finished", false),
            "WAITING" => ("Needs input", true),
            "ERROR" => ("Agent error", true),
            _ => return None,
        };
        if self
            .last_sent
            .get(session)
            .is_some_and(|sent| now.saturating_duration_since(*sent) < COOLDOWN)
        {
            return None;
        }
        self.last_sent.insert(session.to_string(), now);
        let body = crate::harness_record::clean(card_title);
        Some(Notification {
            title: title.to_string(),
            body,
            critical,
        })
    }

    pub fn forget(&mut self, session: &str) {
        self.last_status.remove(session);
        self.last_sent.remove(session);
    }
}

#[cfg(not(test))]
fn escape_pango(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn dispatch(notification: Notification) {
    #[cfg(test)]
    {
        let _ = notification;
        return;
    }
    #[cfg(not(test))]
    {
        if std::env::var_os("SD_DISABLE_NOTIFICATIONS").as_deref() == Some(std::ffi::OsStr::new("1"))
        {
            return;
        }
        let has_display = std::env::var_os("WAYLAND_DISPLAY")
            .filter(|v| !v.is_empty())
            .is_some()
            || std::env::var_os("DISPLAY")
                .filter(|v| !v.is_empty())
                .is_some();
        if !has_display {
            return;
        }
        std::thread::spawn(move || {
            let urgency = if notification.critical { "critical" } else { "normal" };
            let title = escape_pango(&notification.title);
            let body = escape_pango(&notification.body);
            let _ = std::process::Command::new("notify-send")
                .args([
                    "-a",
                    "SUPER DESKTOP",
                    "-u",
                    urgency,
                    "--",
                    &title,
                    &body,
                ])
                .output();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifier_emits_only_on_working_to_attention_transitions_with_cooldown() {
        let mut n = Notifier::default();
        let t0 = Instant::now();

        assert_eq!(n.observe("s1", "Claude", "IDLE", true, t0), None);
        assert_eq!(n.observe("s1", "Claude", "WORKING", true, t0 + Duration::from_secs(1)), None);
        assert_eq!(n.observe("s1", "Claude", "WORKING", true, t0 + Duration::from_secs(2)), None);

        // Suppressed when card is focused (`should_notify == false`).
        assert_eq!(
            n.observe("s1", "Claude · Fix bug", "FINISHED", false, t0 + Duration::from_secs(3)),
            None
        );

        // Real transition when unfocused or hidden.
        assert_eq!(n.observe("s1", "Claude · Fix bug", "WORKING", true, t0 + Duration::from_secs(4)), None);
        assert_eq!(
            n.observe("s1", "Claude · Fix bug", "FINISHED", true, t0 + Duration::from_secs(5)),
            Some(Notification {
                title: "Agent finished".into(),
                body: "Claude · Fix bug".into(),
                critical: false,
            })
        );

        // Cooldown blocks immediate back-to-back alerts within 5s.
        assert_eq!(n.observe("s1", "Claude", "WORKING", true, t0 + Duration::from_secs(6)), None);
        assert_eq!(
            n.observe("s1", "Claude · Fix bug", "WAITING", true, t0 + Duration::from_secs(7)),
            None
        );

        // After cooldown expires, WAITING and ERROR fire cleanly as critical.
        assert_eq!(n.observe("s1", "Claude", "WORKING", true, t0 + Duration::from_secs(11)), None);
        assert_eq!(
            n.observe("s1", "Claude · Confirm", "WAITING", true, t0 + Duration::from_secs(12)),
            Some(Notification {
                title: "Needs input".into(),
                body: "Claude · Confirm".into(),
                critical: true,
            })
        );

        n.forget("s2");
        assert_eq!(n.observe("s2", "Codex", "WORKING", true, t0 + Duration::from_secs(1)), None);
        assert_eq!(
            n.observe("s2", "Codex · Test failed", "ERROR", true, t0 + Duration::from_secs(2)),
            Some(Notification {
                title: "Agent error".into(),
                body: "Codex · Test failed".into(),
                critical: true,
            })
        );
    }
}
