//! A stable way to reach the desktop's Wayland display from a process that
//! started before the desktop did.
//!
//! Settings → System launch starts a daemon at boot, without a display, and it
//! restores the saved cards. A process keeps the environment it started with,
//! so a harness restored that way could never read the clipboard after login:
//! Claude Code and Codex read a clipboard image through Wayland when Ctrl+V
//! arrives.
//!
//! The socket's name is not known at boot, so the headless daemon gives its
//! children a fixed `WAYLAND_DISPLAY` path in the runtime directory, and the
//! desktop daemon makes that path a symlink to the real socket. Wayland clients
//! connect when they need to, and accept an absolute path. Until the link
//! exists they fail exactly as they do with no display.
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

const LINK_NAME: &str = "super-desktop-wayland";
const VARIABLE: &str = "WAYLAND_DISPLAY";

fn link_in(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(LINK_NAME)
}

fn socket_exists(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.file_type().is_socket())
}

/// Make the link point at the display named by `display`, a `WAYLAND_DISPLAY`
/// value. Replaced atomically, so a client connecting now sees the old link or
/// the new one. False when there is no display to link to, or `display` is the
/// link itself (a daemon started from a restored card).
fn publish_in(runtime_dir: &Path, display: Option<&OsStr>) -> std::io::Result<bool> {
    let Some(display) = display.filter(|value| !value.is_empty()) else {
        return Ok(false);
    };
    let link = link_in(runtime_dir);
    // `join` keeps an absolute value as it is.
    let target = runtime_dir.join(display);
    if target == link || !socket_exists(&target) {
        return Ok(false);
    }
    let temporary = runtime_dir.join(format!("{LINK_NAME}.{}", std::process::id()));
    let _ = std::fs::remove_file(&temporary);
    std::os::unix::fs::symlink(&target, &temporary)?;
    std::fs::rename(&temporary, &link).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })?;
    Ok(true)
}

/// Whether `display`, a `WAYLAND_DISPLAY` value, can stand for a desktop: the
/// link only counts once it leads to a socket. Any other value is taken at its
/// word, as before.
fn wayland_present_in(runtime_dir: &Path, display: Option<&OsStr>) -> bool {
    let Some(display) = display.filter(|value| !value.is_empty()) else {
        return false;
    };
    let link = link_in(runtime_dir);
    Path::new(display) != link || socket_exists(&link)
}

/// Desktop daemon: lead the link to this session's display.
pub fn publish() {
    let display = std::env::var_os(VARIABLE);
    if let Err(error) = publish_in(&super::runtime::directory(), display.as_deref()) {
        eprintln!("SUPER DESKTOP: could not link the Wayland display: {error}");
    }
}

/// The `WAYLAND_DISPLAY` a headless daemon gives what it starts.
pub fn headless_value() -> OsString {
    link_in(&super::runtime::directory()).into_os_string()
}

/// Whether this process's environment names a usable Wayland display. Use this
/// instead of reading the variable: cards restored at boot carry the link.
pub fn wayland_present() -> bool {
    wayland_present_in(&super::runtime::directory(), std::env::var_os(VARIABLE).as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("sd-link-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The order at boot: the client's path is fixed before the socket or the
    /// link exists, and the same path connects to the compositor afterwards.
    #[test]
    fn a_path_given_before_the_desktop_reaches_it_afterwards() {
        let scratch = Scratch::new("boot");
        let link = link_in(&scratch.0);
        assert!(UnixStream::connect(&link).is_err(), "no desktop yet");

        let compositor = UnixListener::bind(scratch.0.join("wayland-1")).unwrap();
        assert!(publish_in(&scratch.0, Some(OsStr::new("wayland-1"))).unwrap());

        let mut client = UnixStream::connect(&link).unwrap();
        let (mut accepted, _) = compositor.accept().unwrap();
        client.write_all(b"x").unwrap();
        let mut byte = [0];
        accepted.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"x");
    }

    #[test]
    fn publishing_again_follows_a_new_socket_and_leaves_no_temporary() {
        let scratch = Scratch::new("again");
        let _first = UnixListener::bind(scratch.0.join("wayland-1")).unwrap();
        let second_path = scratch.0.join("wayland-2");
        let _second = UnixListener::bind(&second_path).unwrap();
        assert!(publish_in(&scratch.0, Some(OsStr::new("wayland-1"))).unwrap());
        // An absolute value works like a name in the runtime directory.
        assert!(publish_in(&scratch.0, Some(second_path.as_os_str())).unwrap());
        assert_eq!(std::fs::read_link(link_in(&scratch.0)).unwrap(), second_path);
        let names: Vec<_> = std::fs::read_dir(&scratch.0).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[test]
    fn nothing_is_linked_without_a_real_socket() {
        let scratch = Scratch::new("none");
        let link = link_in(&scratch.0);
        std::fs::write(scratch.0.join("not-a-socket"), b"").unwrap();
        for display in [None, Some(OsStr::new("")), Some(OsStr::new("wayland-9")), Some(OsStr::new("not-a-socket"))] {
            assert!(!publish_in(&scratch.0, display).unwrap(), "{display:?}");
            assert!(link.symlink_metadata().is_err(), "{display:?}");
        }
    }

    /// A daemon started from a restored card must not make the link its own
    /// target, which would cut off every card using it.
    #[test]
    fn a_daemon_whose_display_is_the_link_keeps_it() {
        let scratch = Scratch::new("self");
        let _compositor = UnixListener::bind(scratch.0.join("wayland-1")).unwrap();
        publish_in(&scratch.0, Some(OsStr::new("wayland-1"))).unwrap();
        let link = link_in(&scratch.0);
        assert!(!publish_in(&scratch.0, Some(link.as_os_str())).unwrap());
        assert!(!publish_in(&scratch.0, Some(OsStr::new(LINK_NAME))).unwrap());
        assert!(socket_exists(&link), "the link still reaches the compositor");
    }

    #[test]
    fn the_link_counts_as_a_display_only_once_it_leads_to_one() {
        let scratch = Scratch::new("present");
        let link = link_in(&scratch.0);
        assert!(!wayland_present_in(&scratch.0, None));
        assert!(!wayland_present_in(&scratch.0, Some(OsStr::new(""))));
        assert!(wayland_present_in(&scratch.0, Some(OsStr::new("wayland-1"))));
        assert!(!wayland_present_in(&scratch.0, Some(link.as_os_str())), "no link yet");
        let socket = scratch.0.join("wayland-1");
        let compositor = UnixListener::bind(&socket).unwrap();
        publish_in(&scratch.0, Some(OsStr::new("wayland-1"))).unwrap();
        assert!(wayland_present_in(&scratch.0, Some(link.as_os_str())));
        // The desktop ended and left the link behind.
        drop(compositor);
        std::fs::remove_file(&socket).unwrap();
        assert!(!wayland_present_in(&scratch.0, Some(link.as_os_str())));
    }
}
