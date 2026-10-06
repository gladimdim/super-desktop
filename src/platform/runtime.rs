//! One control-socket location shared by the desktop and its command client.
use std::path::{Path, PathBuf};

pub fn directory() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_directory)
}

#[cfg(target_os = "linux")]
fn default_directory() -> PathBuf {
    PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
}

#[cfg(target_os = "macos")]
fn default_directory() -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    // Darwin supplies a short, per-user directory independently of the
    // Finder/login-shell environment. Do not fall back to a shared /tmp socket.
    let length = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, std::ptr::null_mut(), 0) };
    assert!(
        length > 1,
        "could not locate the macOS user temporary directory"
    );
    let mut bytes = vec![0u8; length];
    let written = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    assert!(
        written > 1 && written <= length,
        "could not read the macOS user temporary directory"
    );
    bytes.truncate(written - 1);
    let directory = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    // The OS-provided path can use /var -> /private/var. Resolve that trusted
    // default so owner-only control can reject symlinks in explicit paths.
    directory.canonicalize().unwrap_or(directory)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("SUPER DESKTOP supports Linux and macOS runtime directories");

pub fn socket_path() -> PathBuf {
    socket_path_in(&directory())
}

pub fn socket_path_in(directory: &Path) -> PathBuf {
    directory.join("super-desktop.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_socket_name_is_unchanged() {
        assert_eq!(
            socket_path_in(Path::new("/private/runtime")),
            Path::new("/private/runtime/super-desktop.sock")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_default_is_the_existing_user_runtime_path() {
        assert_eq!(
            default_directory(),
            PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_default_is_private_and_fits_a_unix_socket() {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        let directory = default_directory();
        let metadata = std::fs::metadata(&directory).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.uid(), unsafe { libc::getuid() });
        assert_eq!(metadata.mode() & 0o077, 0);
        let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        assert!(socket_path_in(&directory).as_os_str().as_bytes().len() < address.sun_path.len());
    }
}
