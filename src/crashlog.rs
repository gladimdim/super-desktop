//! Crash log: the one place a background panic can be read afterwards.
//!
//! Why this exists: release builds use `panic = "abort"`, and the daemon is
//! spawned by the shortcut's client with `stdio → /dev/null`. A panic therefore
//! killed the overlay silently — the only trace was a `systemd-coredump` entry
//! whose stack was unsymbolized (the binary is stripped, and by the time anyone
//! looks, it has usually been rebuilt). Six of today's twelve core dumps were
//! exactly that: `RefCell already borrowed` with no way to tell where from.
//!
//! The hook runs *before* the abort, so message, location, thread and a
//! backtrace all survive. `Cargo.toml` keeps the release symbol table
//! (`strip = "debuginfo"`) so those backtraces name our functions.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Keep the log bounded; it only ever holds panic records, but a crash loop
/// (daemon respawned by every shortcut press) must not fill the disk.
const MAX_BYTES: u64 = 512 * 1024;

/// `~/.config/super-desktop/panic.log` — next to `state.json`.
pub fn log_path() -> PathBuf {
    let mut path = crate::state::get_state_path();
    path.set_file_name("panic.log");
    path
}

/// Append `text`, rotating to `panic.log.1` once it grows past [`MAX_BYTES`].
///
/// Every failure is ignored on purpose: logging a panic must never be the thing
/// that turns one crash into two.
pub fn append(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

/// Record panics (and the abort that follows) in the crash log and on stderr.
///
/// Called once per process, from `main`, so clients log their panics too.
pub fn install_panic_hook() {
    let path = log_path();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<panic payload was not a string>".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        let thread = std::thread::current()
            .name()
            .unwrap_or("<unnamed thread>")
            .to_string();

        let entry = format_entry(
            &thread,
            &location,
            &message,
            chrono::Utc::now().to_rfc3339().as_str(),
            std::process::id(),
            &std::backtrace::Backtrace::force_capture().to_string(),
        );

        // Foreground runs (a terminal, a test) show it immediately …
        eprintln!("SUPER DESKTOP panic at {location}: {message}");
        // … and the log is the only place the spawned daemon's story survives.
        append(&path, &entry);
    }));
}

/// The exact text written for one panic — split out so it can be asserted
/// without installing the process-wide hook.
pub fn format_entry(
    thread: &str,
    location: &str,
    message: &str,
    when: &str,
    pid: u32,
    backtrace: &str,
) -> String {
    format!(
        "\n=== panic ===\nsuper-desktop aborted (panic = \"abort\")\nwhen: {when}\npid: {pid}\nthread: {thread}\nwhere: {location}\nmessage: {message}\nbacktrace:\n{backtrace}\n"
    )
}

/// Note something that is not a crash but explains later behaviour (e.g. a
/// skipped window build), using the same log as panics.
pub fn note(what: &str) {
    append(
        &log_path(),
        &format!(
            "[{}] {what} (pid {})\n",
            chrono::Utc::now().to_rfc3339(),
            std::process::id(),
        ),
    );
}

/// Mark the start of a long-lived instance, so a later panic in the log can be
/// pinned to the run that produced it.
pub fn note_start(kind: &str) {
    append(
        &log_path(),
        &format!(
            "[{}] super-desktop {kind} started (pid {})\n",
            chrono::Utc::now().to_rfc3339(),
            std::process::id(),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sd-crashlog-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("panic.log")
    }

    #[test]
    fn test_append_creates_dirs_and_keeps_every_entry() {
        let path = temp_log("append");
        append(&path, "first\n");
        append(&path, "second\n");

        let content = fs::read_to_string(&path).expect("log must exist");
        assert_eq!(content, "first\nsecond\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_append_rotates_when_the_log_grows_past_the_cap() {
        let path = temp_log("rotate");
        let rotated = path.with_extension("log.1");

        // Filling the log does not rotate it: the cap is checked when the next
        // entry arrives, so the log can sit slightly over the cap in between.
        append(&path, &"x".repeat(MAX_BYTES as usize + 1));
        assert!(!rotated.exists(), "rotation happens on the following append");

        append(&path, "after rotation\n");
        assert!(rotated.is_file(), "oversized log must be rotated to .log.1");
        assert!(
            fs::metadata(&rotated).map(|m| m.len()).unwrap_or(0) > MAX_BYTES,
            "the rotated file keeps the oversized run"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "after rotation\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_format_entry_carries_everything_needed_to_find_the_crash() {
        let entry = format_entry(
            "main",
            "src/window.rs:1204:9",
            "already borrowed: BorrowMutError",
            "2026-09-17T20:02:40+03:00",
            241674,
            "  0: super_desktop::window::SuperDesktopWindow::show_again",
        );

        for needle in [
            "src/window.rs:1204:9",
            "already borrowed: BorrowMutError",
            "thread: main",
            "pid: 241674",
            "2026-09-17T20:02:40+03:00",
            "show_again",
        ] {
            assert!(entry.contains(needle), "entry is missing {needle:?}:\n{entry}");
        }
    }

    #[test]
    fn test_log_lives_next_to_state_json() {
        let path = log_path();
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("panic.log"));
        assert_eq!(
            path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()),
            Some("super-desktop"),
            "got: {}",
            path.display()
        );
    }
}
