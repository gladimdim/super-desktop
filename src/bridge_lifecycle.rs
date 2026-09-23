//! Daemon-owned bridge supervision, independent of the settings page and GTK.
use super::*;

const CHECK_INTERVAL: Duration = Duration::from_secs(5);
const FAILURE_LIMIT: u8 = 3;

pub(super) struct Lifecycle {
    enabled: bool,
    failures: u8,
}

impl Lifecycle {
    const fn new() -> Self {
        // The first check starts a missing bridge immediately.
        Self {
            enabled: true,
            failures: FAILURE_LIMIT - 1,
        }
    }

    pub(super) fn enable(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.failures = 0;
    }

    fn check(
        &mut self,
        healthy: impl FnOnce() -> bool,
        start: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if healthy() {
            self.failures = 0;
        } else {
            self.failures += 1;
            if self.failures >= FAILURE_LIMIT {
                self.failures = 0;
                return start();
            }
        }
        Ok(())
    }
}

// Serialize manual Start/Stop, pairing, and recovery, so a watchdog cannot
// race Stop or launch a second child while another start is in progress.
pub(super) static LIFECYCLE: Mutex<Lifecycle> = Mutex::new(Lifecycle::new());

pub fn supervise() {
    loop {
        let result = LIFECYCLE.lock().unwrap().check(
            || bridge_running(BRIDGE_PORT),
            || {
                eprintln!("SUPER DESKTOP: starting/recovering the bridge");
                start_bridge_inner()
            },
        );
        if let Err(error) = result {
            eprintln!("SUPER DESKTOP: bridge recovery failed (will retry): {error}");
        }
        std::thread::sleep(CHECK_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn recovers_after_failure_without_a_settings_page() {
        let mut lifecycle = Lifecycle::new();
        let starts = Cell::new(0);
        let start = || {
            starts.set(starts.get() + 1);
            Ok(())
        };
        lifecycle.check(|| false, start).unwrap();
        assert_eq!(starts.get(), 1, "start immediately on daemon launch");
        lifecycle.check(|| true, start).unwrap();
        lifecycle.check(|| false, start).unwrap();
        lifecycle.check(|| false, start).unwrap();
        assert_eq!(starts.get(), 1, "tolerate transient missed pings");
        lifecycle.check(|| true, start).unwrap();
        for _ in 0..2 {
            lifecycle.check(|| false, start).unwrap();
        }
        assert_eq!(starts.get(), 1, "healthy pings reset the failure streak");
        lifecycle.check(|| false, start).unwrap();
        assert_eq!(starts.get(), 2, "recover a sustained failure");
    }

    #[test]
    fn explicit_stop_stays_stopped_until_start() {
        let mut lifecycle = Lifecycle::new();
        lifecycle.enable(false);
        for _ in 0..10 {
            lifecycle
                .check(
                    || panic!("stopped bridge must not be probed"),
                    || panic!("stopped bridge must not restart"),
                )
                .unwrap();
        }
        lifecycle.enable(true);
        lifecycle
            .check(|| true, || panic!("healthy bridge must stay running"))
            .unwrap();
        for _ in 0..2 {
            lifecycle.check(|| false, || panic!("too early")).unwrap();
        }
        assert!(lifecycle
            .check(|| false, || Err("bind failed".into()))
            .is_err());
        for _ in 0..2 {
            lifecycle
                .check(|| false, || panic!("retry must be delayed"))
                .unwrap();
        }
        let recovered = Cell::new(false);
        lifecycle
            .check(
                || false,
                || {
                    recovered.set(true);
                    Ok(())
                },
            )
            .unwrap();
        assert!(
            recovered.get(),
            "retry failed starts without user interaction"
        );
    }
}
