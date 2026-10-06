//! Finder launches do not inherit an interactive shell's executable search path.
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

fn search_path(original: &OsStr, home: &Path, executable: Option<&Path>) -> OsString {
    let mut paths = Vec::new();
    if let Some(parent) = executable.and_then(Path::parent) {
        paths.push(parent.to_path_buf());
    }
    paths.extend(std::env::split_paths(original).filter(|path| !path.as_os_str().is_empty()));
    for path in [
        home.join(".local/bin"),
        home.join(".cargo/bin"),
        "/opt/homebrew/bin".into(),
        "/usr/local/bin".into(),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
    ] {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    std::env::join_paths(paths).unwrap_or_else(|_| original.to_os_string())
}

/// Call once at process entry, before any threads exist.
pub fn prepare() {
    let home = std::env::var_os("HOME").unwrap_or_default();
    let original = std::env::var_os("PATH").unwrap_or_default();
    let executable = std::env::current_exe().ok();
    std::env::set_var(
        "PATH",
        search_path(&original, Path::new(&home), executable.as_deref()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finder_path_finds_helpers_and_keeps_user_command_precedence() {
        let path = search_path(
            OsStr::new("/custom/bin:/usr/bin"),
            Path::new("/Users/test"),
            Some(Path::new("/app/bin/super-desktop")),
        );
        let paths: Vec<_> = std::env::split_paths(&path).collect();
        assert_eq!(paths[0], Path::new("/app/bin"));
        assert_eq!(paths[1], Path::new("/custom/bin"));
        assert!(paths.contains(&"/opt/homebrew/bin".into()));
        assert_eq!(
            paths.iter().filter(|p| *p == Path::new("/usr/bin")).count(),
            1
        );
    }
}
