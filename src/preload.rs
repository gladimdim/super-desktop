//! `libgtk4-layer-shell` must be preloaded into the GTK daemon (the client adds
//! it when delegating), but nothing else needs it: inherited by tmux, every
//! shell/agent in a card and every helper the daemon spawns, it made each of
//! them map GTK first (a `tmux` query cost ~34 ms of CPU instead of ~1.5 ms).
use std::path::Path;
use std::process::Command;

/// The library the client preloads for the layer-shell overlay.
pub const LAYER_SHELL_LIBRARY: &str = "/usr/lib/libgtk4-layer-shell.so";

fn is_layer_shell(entry: &str) -> bool {
    Path::new(entry)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("libgtk4-layer-shell.so"))
}

/// What `LD_PRELOAD` should become without the layer-shell entry.
///
/// `None`: no layer-shell entry, leave the variable alone. `Some(None)`: the
/// variable only held layer-shell entries, remove it. `Some(Some(rest))`: keep
/// the user's other entries, in their order.
pub fn strip_layer_shell(value: &str) -> Option<Option<String>> {
    // ld.so accepts both colons and spaces as separators.
    let entries: Vec<&str> = value.split([':', ' ']).filter(|e| !e.is_empty()).collect();
    if !entries.iter().any(|entry| is_layer_shell(entry)) {
        return None;
    }
    let rest: Vec<&str> = entries.into_iter().filter(|e| !is_layer_shell(e)).collect();
    Some((!rest.is_empty()).then(|| rest.join(":")))
}

/// Drop layer-shell from this process's environment so children do not
/// inherit it. The library is already mapped by the time `main` runs, and
/// GTK has not been initialised yet.
///
/// Must run first in `main`, before any thread exists: environment mutation
/// is not thread-safe (edition 2021 still exposes it as a safe function).
pub fn strip_from_process_env() {
    let Some(value) = std::env::var_os("LD_PRELOAD") else {
        return;
    };
    match strip_layer_shell(&value.to_string_lossy()) {
        None => {}
        Some(None) => std::env::remove_var("LD_PRELOAD"),
        Some(Some(rest)) => std::env::set_var("LD_PRELOAD", rest),
    }
}

/// Explicitly preload layer-shell for a child that is itself the overlay
/// daemon (cold start from `toggle`/`show`).
pub fn preload_layer_shell(command: &mut Command) {
    if !Path::new(LAYER_SHELL_LIBRARY).exists() {
        return;
    }
    let current = std::env::var("LD_PRELOAD").unwrap_or_default();
    let value = if current.is_empty() {
        LAYER_SHELL_LIBRARY.to_string()
    } else {
        format!("{LAYER_SHELL_LIBRARY}:{current}")
    };
    command.env("LD_PRELOAD", value);
}

/// A tmux server started from a preloaded environment copies `LD_PRELOAD`
/// into its global environment, and every new pane inherits it. Remove only
/// the layer-shell entry there; blocking (runs tmux), so call it off the UI
/// thread.
pub fn clean_tmux_global_env() {
    let tmux = crate::tmux::tmux_bin();
    let Ok(output) = Command::new(&tmux)
        .args(["show-environment", "-g", "LD_PRELOAD"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return; // No server, or the variable is not set.
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(value) = text.trim_end_matches('\n').strip_prefix("LD_PRELOAD=") else {
        return; // `-LD_PRELOAD` (already removed) or unexpected output.
    };
    match strip_layer_shell(value) {
        None => {}
        Some(None) => {
            let _ = Command::new(&tmux)
                .args(["set-environment", "-g", "-u", "LD_PRELOAD"])
                .output();
        }
        Some(Some(rest)) => {
            let _ = Command::new(&tmux)
                .args(["set-environment", "-g", "LD_PRELOAD", &rest])
                .output();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_only_the_layer_shell_entry() {
        assert_eq!(strip_layer_shell(LAYER_SHELL_LIBRARY), Some(None));
        assert_eq!(
            strip_layer_shell("/usr/lib/libgtk4-layer-shell.so.1.0.4"),
            Some(None)
        );
        assert_eq!(
            strip_layer_shell("/usr/lib/libgtk4-layer-shell.so:/opt/libmine.so"),
            Some(Some("/opt/libmine.so".into()))
        );
        assert_eq!(
            strip_layer_shell("/a/liba.so /usr/lib/libgtk4-layer-shell.so:/b/libb.so"),
            Some(Some("/a/liba.so:/b/libb.so".into()))
        );
        assert_eq!(strip_layer_shell("libgtk4-layer-shell.so::"), Some(None));
        // Unrelated preloads are never touched, including look-alike names.
        assert_eq!(strip_layer_shell("/opt/libmine.so"), None);
        assert_eq!(strip_layer_shell("/opt/libgtk4-layer-shell-extra.so"), None);
        assert_eq!(strip_layer_shell(""), None);
    }
}
