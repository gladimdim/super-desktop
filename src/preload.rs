//! `libgtk4-layer-shell` must be preloaded into the GTK daemon (the client adds
//! it when delegating), but nothing else needs it: inherited by tmux, every
//! shell/agent in a card and every helper the daemon spawns, it made each of
//! them map GTK first (a `tmux` query cost ~34 ms of CPU instead of ~1.5 ms).
use std::path::Path;
use std::process::Command;

/// The primary Arch/generic path the client preloads for the layer-shell overlay.
pub const LAYER_SHELL_LIBRARY: &str = "/usr/lib/libgtk4-layer-shell.so";

const LAYER_SHELL_CANDIDATES: &[&str] = &[
    LAYER_SHELL_LIBRARY,
    "/usr/lib/libgtk4-layer-shell.so.0",
    "/usr/lib64/libgtk4-layer-shell.so",
    "/usr/lib64/libgtk4-layer-shell.so.0",
    "/usr/lib/x86_64-linux-gnu/libgtk4-layer-shell.so",
    "/usr/lib/x86_64-linux-gnu/libgtk4-layer-shell.so.0",
    "/usr/lib/aarch64-linux-gnu/libgtk4-layer-shell.so",
    "/usr/lib/aarch64-linux-gnu/libgtk4-layer-shell.so.0",
];

/// Return the first existing layer-shell shared library path from `candidates`.
pub fn find_layer_shell_library_in<'a>(candidates: &[&'a str]) -> Option<&'a str> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).exists())
}

/// Return the first existing layer-shell shared library path on this system.
pub fn find_layer_shell_library() -> Option<&'static str> {
    find_layer_shell_library_in(LAYER_SHELL_CANDIDATES)
}

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
/// daemon (cold start from `toggle`/`show`, or the client delegating to the
/// application). An inherited exact entry is left as it is.
pub fn preload_layer_shell(command: &mut Command) {
    let Some(library) = find_layer_shell_library() else {
        return;
    };
    let current = std::env::var("LD_PRELOAD").unwrap_or_default();
    if current.split([':', ' ']).any(is_layer_shell) {
        return;
    }
    let value = if current.is_empty() {
        library.to_string()
    } else {
        format!("{library}:{current}")
    };
    command.env("LD_PRELOAD", value);
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

    #[test]
    fn finds_first_existing_layer_shell_candidate_across_multiarch_dirs() {
        let dir = std::env::temp_dir().join(format!("sd-preload-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib64 = dir.join("usr/lib64");
        let gnu = dir.join("usr/lib/x86_64-linux-gnu");
        std::fs::create_dir_all(&lib64).unwrap();
        std::fs::create_dir_all(&gnu).unwrap();

        let missing = dir.join("usr/lib/libgtk4-layer-shell.so");
        let fedora_lib = lib64.join("libgtk4-layer-shell.so.0");
        let debian_lib = gnu.join("libgtk4-layer-shell.so.0");
        std::fs::write(&fedora_lib, b"").unwrap();
        std::fs::write(&debian_lib, b"").unwrap();

        let m_str = missing.to_string_lossy();
        let f_str = fedora_lib.to_string_lossy();
        let d_str = debian_lib.to_string_lossy();
        assert_eq!(
            find_layer_shell_library_in(&[&m_str, &f_str, &d_str]),
            Some(f_str.as_ref())
        );
        assert_eq!(
            find_layer_shell_library_in(&[&m_str, &d_str]),
            Some(d_str.as_ref())
        );
        assert_eq!(find_layer_shell_library_in(&[&m_str]), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
