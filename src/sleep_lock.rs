//! Charger-only logind inhibitors. The daemon owns the file descriptor, so
//! exiting or crashing releases the lock without a helper process or config edits.
use gio::prelude::*;
use glib::variant::ToVariant;
use gtk4::{gio, glib};
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Power {
    External,
    Battery,
    Unknown,
}

fn power_source(root: &Path) -> Power {
    let Ok(entries) = fs::read_dir(root) else {
        return Power::Unknown;
    };
    let mut offline = false;
    for entry in entries.flatten() {
        let path = entry.path();
        let read = |name| {
            fs::read_to_string(path.join(name))
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        // A connected peripheral's battery/charger must not keep the laptop awake.
        if read("scope") == "Device" {
            continue;
        }
        match read("type").as_str() {
            "Mains" | "USB" | "USB_C" | "USB_PD" | "USB_PD_DRP" | "USB_DCP" | "USB_CDP"
            | "USB_ACA" | "Wireless" => match read("online").as_str() {
                "1" => return Power::External,
                "0" => offline = true,
                _ => {}
            },
            "Battery" if read("status") == "Discharging" => offline = true,
            _ => {}
        }
    }
    if offline {
        Power::Battery
    } else {
        Power::Unknown
    }
}

#[derive(Default)]
struct Lease<L> {
    held: Option<L>,
}
impl<L> Lease<L> {
    fn update(
        &mut self,
        enabled: bool,
        power: Power,
        acquire: impl FnOnce() -> Result<L, String>,
    ) -> Result<bool, String> {
        if !enabled || power != Power::External {
            self.held = None;
        } else if self.held.is_none() {
            self.held = Some(acquire()?);
        }
        Ok(self.held.is_some())
    }
}

fn acquire() -> Result<OwnedFd, String> {
    let bus = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    let parameters = (
        "sleep:handle-lid-switch",
        "SUPER DESKTOP",
        "Keep AI harnesses available while on charger power",
        "block",
    )
        .to_variant();
    let (reply, descriptors) = bus
        .call_with_unix_fd_list_sync(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "Inhibit",
            Some(&parameters),
            None,
            gio::DBusCallFlags::NONE,
            3000,
            None::<&gio::UnixFDList>,
            gio::Cancellable::NONE,
        )
        .map_err(|e| e.to_string())?;
    let (handle,) = reply
        .get::<(glib::variant::Handle,)>()
        .ok_or("Invalid sleep-lock response")?;
    descriptors
        .ok_or("Sleep lock did not return a file descriptor")?
        .get(handle.0)
        .map_err(|e| e.to_string())
}

fn alive(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: 0,
        revents: 0,
    };
    // logind owns the read end of this inhibitor pipe. A restart closes it.
    unsafe {
        libc::poll(&mut poll, 1, 0) >= 0
            && poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0
    }
}

struct Runtime {
    updates: mpsc::Sender<bool>,
    status: Arc<Mutex<String>>,
}
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn status() -> String {
    RUNTIME
        .get()
        .map(|r| r.status.lock().unwrap().clone())
        .unwrap_or_else(|| "Off — normal sleep behavior.".into())
}

pub fn set_enabled(enabled: bool) {
    let runtime = RUNTIME.get_or_init(|| {
        let (updates, receiver) = mpsc::channel();
        let status = Arc::new(Mutex::new("Checking power source…".to_string()));
        let shared = Arc::clone(&status);
        std::thread::Builder::new()
            .name("sleep-lock".into())
            .spawn(move || {
                let mut enabled = false;
                let mut lease = Lease::<OwnedFd> { held: None };
                loop {
                    match receiver.recv_timeout(Duration::from_secs(2)) {
                        Ok(value) => enabled = value,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    while let Ok(value) = receiver.try_recv() {
                        enabled = value;
                    }
                    let power = power_source(Path::new("/sys/class/power_supply"));
                    if lease.held.as_ref().is_some_and(|fd| !alive(fd)) {
                        lease.held = None;
                    }
                    let result = lease.update(enabled, power, acquire);
                    // Power may have changed while logind was processing the request.
                    if lease.held.is_some()
                        && power_source(Path::new("/sys/class/power_supply")) != Power::External
                    {
                        lease.held = None;
                    }
                    *shared.lock().unwrap() = match result {
                        Err(error) => format!("Sleep lock unavailable: {error}"),
                        _ if !enabled => "Off — normal sleep behavior.".into(),
                        _ if lease.held.is_some() => {
                            "Active on charger — sleep and lid-close suspend are blocked.".into()
                        }
                        _ if power == Power::Unknown => {
                            "Waiting — charger power could not be detected.".into()
                        }
                        _ => "On battery — normal sleep behavior restored.".into(),
                    };
                }
            })
            .expect("Cannot start sleep-lock worker");
        Runtime { updates, status }
    });
    let _ = runtime.updates.send(enabled);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn lock_is_charger_only_and_released_on_disable_or_unknown_power() {
        struct Token(Rc<Cell<u32>>);
        impl Drop for Token {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let dropped = Rc::new(Cell::new(0));
        let calls = Cell::new(0);
        let acquire = || {
            calls.set(calls.get() + 1);
            Ok(Token(Rc::clone(&dropped)))
        };
        let mut lease = Lease { held: None };
        assert!(!lease.update(false, Power::External, acquire).unwrap());
        assert!(!lease.update(true, Power::Battery, acquire).unwrap());
        assert!(lease.update(true, Power::External, acquire).unwrap());
        assert!(lease.update(true, Power::External, acquire).unwrap());
        assert_eq!(calls.get(), 1);
        assert!(!lease.update(true, Power::Battery, acquire).unwrap());
        assert_eq!(dropped.get(), 1);
        assert!(lease.update(true, Power::External, acquire).unwrap());
        assert!(!lease.update(false, Power::External, acquire).unwrap());
        assert_eq!(dropped.get(), 2);
        assert!(lease.update(true, Power::External, acquire).unwrap());
        assert!(!lease.update(true, Power::Unknown, acquire).unwrap());
        assert_eq!(dropped.get(), 3);
    }

    #[test]
    fn acquisition_failure_does_not_claim_a_lock_and_can_retry() {
        let mut lease = Lease::<u8> { held: None };
        assert!(lease
            .update(true, Power::External, || Err("denied".into()))
            .is_err());
        assert!(lease.held.is_none());
        assert!(lease.update(true, Power::External, || Ok(1)).unwrap());
    }

    #[test]
    fn reads_ac_and_usb_power_without_confusing_peripherals() {
        let root = std::env::temp_dir().join(format!("sd-power-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let supply = |name: &str, kind: &str, online: &str, scope: &str| {
            let path = root.join(name);
            fs::create_dir_all(&path).unwrap();
            for (file, value) in [("type", kind), ("online", online), ("scope", scope)] {
                fs::write(path.join(file), value).unwrap();
            }
        };
        assert_eq!(power_source(&root), Power::Unknown);
        supply("mouse", "USB", "1", "Device");
        assert_eq!(power_source(&root), Power::Unknown);
        supply("AC0", "Mains", "0", "");
        assert_eq!(power_source(&root), Power::Battery);
        supply("AC0", "Mains", "1", "");
        assert_eq!(power_source(&root), Power::External);
        supply("AC0", "Mains", "0", "");
        supply("usb", "USB_PD", "1", "System");
        assert_eq!(power_source(&root), Power::External);
        supply("usb", "USB_PD", "broken", "System");
        assert_eq!(power_source(&root), Power::Battery);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(power_source(&root), Power::Unknown);
    }

    #[test]
    fn old_state_defaults_off_and_preference_round_trips() {
        let mut value = serde_json::to_value(crate::state::AppState::default()).unwrap();
        value.as_object_mut().unwrap().remove("sleep_lock_on_ac");
        let mut state: crate::state::AppState = serde_json::from_value(value).unwrap();
        assert!(!state.sleep_lock_on_ac);
        state.sleep_lock_on_ac = true;
        let restored: crate::state::AppState =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(restored.sleep_lock_on_ac);
    }

    #[test]
    #[ignore = "briefly acquires a real logind inhibitor; run explicitly on a desktop"]
    fn live_logind_lock_acquires_and_releases() {
        let fd = acquire().expect("logind should allow a desktop sleep inhibitor");
        assert!(alive(&fd));
        let output = std::process::Command::new("systemd-inhibit")
            .arg("--list")
            .arg("--no-pager")
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&output.stdout);
        assert!(output.contains("SUPER DESKTOP"), "{output}");
        drop(fd);
    }
}
