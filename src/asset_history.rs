//! Shared references only: persisted paths never grant access to file contents.
use serde::{Deserialize, Serialize};
use std::fs::{DirBuilder, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path};
use std::time::{Duration, Instant};

const MAX_BYTES: usize = 2 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct Record {
    session: String,
    root: String,
    paths: Vec<String>,
}

fn open_at(dir: &File, name: &str, flags: i32) -> std::io::Result<File> {
    let name = std::ffi::CString::new(name)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn private(file: &File, directory: bool) -> std::io::Result<()> {
    let m = file.metadata()?;
    if m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || if directory {
            !m.is_dir()
        } else {
            !m.is_file() || m.nlink() != 1
        }
    {
        return Err(std::io::Error::other("unsafe asset history permissions"));
    }
    Ok(())
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && Path::new(path)
            .components()
            .any(|c| matches!(c, Component::Normal(_)))
        && Path::new(path).components().all(|c| match c {
            Component::CurDir => true,
            Component::Normal(name) => !name.to_string_lossy().starts_with('.'),
            _ => false,
        })
}

pub fn merge(session: &str, root: &Path, paths: Vec<String>) -> std::io::Result<Vec<String>> {
    let home = std::env::var_os("HOME").ok_or_else(|| std::io::Error::other("HOME unavailable"))?;
    merge_at(
        &Path::new(&home).join(".local/state/super-desktop/file-assets"),
        session,
        root,
        paths,
    )
}

fn merge_at(
    directory: &Path,
    session: &str,
    root: &Path,
    paths: Vec<String>,
) -> std::io::Result<Vec<String>> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(directory)?;
    private(&dir, true)?;
    let lock = open_at(&dir, "lock", libc::O_RDWR | libc::O_CREAT)?;
    private(&lock, false)?;
    let start = Instant::now();
    loop {
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK)
            || start.elapsed() > Duration::from_secs(2)
        {
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // The lock descriptor stays alive through rename, serializing independent processes.
    let mut records: Vec<Record> = match open_at(&dir, "references.json", libc::O_RDONLY) {
        Ok(file) => {
            private(&file, false)?;
            let mut bytes = Vec::new();
            file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > MAX_BYTES {
                return Err(std::io::Error::other("asset history too large"));
            }
            serde_json::from_slice(&bytes).map_err(std::io::Error::other)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let root = root.to_string_lossy().into_owned();
    let old = records
        .iter()
        .position(|r| r.session == session && r.root == root)
        .map(|i| records.remove(i).paths)
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let paths: Vec<_> = paths
        .into_iter()
        .chain(old)
        .filter(|p| valid_path(p) && seen.insert(p.clone()))
        .take(64)
        .collect();
    records.push(Record {
        session: session.into(),
        root,
        paths: paths.clone(),
    });
    while records.len() > 64 {
        records.remove(0);
    }
    let bytes = loop {
        let bytes = serde_json::to_vec(&records).map_err(std::io::Error::other)?;
        if bytes.len() <= MAX_BYTES {
            break bytes;
        }
        records.remove(0);
    };
    // Fixed temporary name is safe under flock; never truncate or follow an existing file.
    // Remove only an owned, private regular file left by an interrupted previous write.
    if let Ok(file) = open_at(&dir, "references.tmp", libc::O_RDONLY) {
        private(&file, false)?;
        if unsafe { libc::unlinkat(dir.as_raw_fd(), c"references.tmp".as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    let mut file = open_at(
        &dir,
        "references.tmp",
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if unsafe {
        libc::renameat(
            dir.as_raw_fd(),
            c"references.tmp".as_ptr(),
            dir.as_raw_fd(),
            c"references.json".as_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    dir.sync_all()?;
    Ok(paths)
}

use std::os::unix::fs::OpenOptionsExt;

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "sd-history-{}-{}",
                std::process::id(),
                crate::tmux::unique_session_name()
            )))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn persists_merges_and_scopes_references() {
        let f = Fixture::new();
        let root = Path::new("/workspace");
        merge_at(&f.0, "terminal", root, vec!["a.png".into()]).unwrap();
        assert_eq!(
            merge_at(&f.0, "terminal", root, vec!["b.pdf".into(), "a.png".into()]).unwrap(),
            ["b.pdf", "a.png"]
        );
        assert_eq!(
            merge_at(&f.0, "terminal", root, vec![]).unwrap(),
            ["b.pdf", "a.png"]
        );
        assert!(merge_at(&f.0, "terminal", Path::new("/elsewhere"), vec![])
            .unwrap()
            .is_empty());
        assert!(merge_at(&f.0, "other", root, vec![]).unwrap().is_empty());
    }
    #[test]
    fn concurrent_writers_do_not_lose_references() {
        let f = Fixture::new();
        std::thread::scope(|scope| {
            for i in 0..16 {
                let dir = &f.0;
                scope.spawn(move || {
                    merge_at(dir, "t", Path::new("/w"), vec![format!("{i}.png")]).unwrap();
                });
            }
        });
        assert_eq!(
            merge_at(&f.0, "t", Path::new("/w"), vec![]).unwrap().len(),
            16
        );
    }
    #[test]
    fn bounds_paths_and_rejects_symlink_storage() {
        let f = Fixture::new();
        let paths = (0..100).map(|i| format!("{i}.png")).collect();
        assert_eq!(
            merge_at(&f.0, "t", Path::new("/w"), paths).unwrap().len(),
            64
        );
        for path in ["../a.png", "/a.png", ".secret/a.png", "a/../b.png", ""] {
            assert!(!valid_path(path));
        }
        assert!(valid_path("./images/a.png"));
        std::fs::remove_file(f.0.join("references.json")).unwrap();
        std::os::unix::fs::symlink("missing", f.0.join("references.json")).unwrap();
        assert!(merge_at(&f.0, "t", Path::new("/w"), vec![]).is_err());
        assert!(!f.0.join("missing").exists());
    }

    #[test]
    fn rejects_public_storage_and_bounds_terminal_history() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        for i in 0..65 {
            merge_at(
                &f.0,
                &format!("t{i}"),
                Path::new("/w"),
                vec!["a.png".into()],
            )
            .unwrap();
        }
        let records: Vec<Record> =
            serde_json::from_slice(&std::fs::read(f.0.join("references.json")).unwrap()).unwrap();
        assert_eq!(records.len(), 64);
        assert_eq!(records[0].session, "t1");
        std::fs::set_permissions(
            f.0.join("references.json"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(merge_at(&f.0, "t", Path::new("/w"), vec![]).is_err());
    }
}
