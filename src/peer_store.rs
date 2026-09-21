//! Owner-only outgoing credentials, atomic replacement and advisory locking.
use crate::peer_client::{Peer, PeerError, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

pub struct PeerStore {
    directory: File,
    _lock: File,
}
#[derive(Serialize, Deserialize)]
struct Registry {
    version: u32,
    peers: Vec<Peer>,
}
fn unique_name() -> String {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("OS randomness unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn failure(_: std::io::Error) -> PeerError {
    PeerError("peer_store_io_error")
}
fn private(file: &File, directory: bool) -> Result<()> {
    let m = file.metadata().map_err(failure)?;
    if m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || (if directory {
            !m.is_dir()
        } else {
            !m.is_file() || m.nlink() != 1
        })
    {
        return Err(PeerError("unsafe_peer_store_permissions"));
    }
    Ok(())
}
impl PeerStore {
    pub fn default_store() -> Result<Self> {
        let path = std::env::var_os("SUPER_DESKTOP_PEERS_STATE_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|p| Path::new(&p).join(".local/state/super-desktop"))
            })
            .ok_or(PeerError("peer_store_directory_missing"))?;
        Self::open(&path)
    }
    pub fn open(path: &Path) -> Result<Self> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(failure)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
            .map_err(failure)?;
        private(&directory, true)?;
        let mut store = Self {
            directory,
            _lock: File::open("/dev/null").map_err(failure)?,
        };
        let lock = store.file("peers.lock", libc::O_RDWR | libc::O_CREAT)?;
        private(&lock, false)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(PeerError("peer_store_busy"));
        }
        store._lock = lock;
        Ok(store)
    }
    fn file(&self, name: &str, flags: i32) -> Result<File> {
        let name = std::ffi::CString::new(name).unwrap();
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                return Err(PeerError("peer_store_not_found"));
            }
            return Err(PeerError("peer_store_io_error"));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub fn peers(&self) -> Result<Vec<Peer>> {
        let file = match self.file("peers.json", libc::O_RDONLY) {
            Err(PeerError("peer_store_not_found")) => return Ok(vec![]),
            other => other?,
        };
        private(&file, false)?;
        let mut bytes = Vec::new();
        file.take(131073).read_to_end(&mut bytes).map_err(failure)?;
        if bytes.len() > 131072 {
            return Err(PeerError("invalid_peer_store"));
        }
        let registry: Registry =
            serde_json::from_slice(&bytes).map_err(|_| PeerError("invalid_peer_store"))?;
        if registry.version != 1 || registry.peers.len() > 64 {
            return Err(PeerError("invalid_peer_store"));
        }
        let mut ids = std::collections::HashSet::new();
        for peer in &registry.peers {
            peer.validate()?;
            if !ids.insert(&peer.machine_id) {
                return Err(PeerError("invalid_peer_store"));
            }
        }
        Ok(registry.peers)
    }
    pub fn get(&self, id: &str) -> Result<Peer> {
        self.peers()?
            .into_iter()
            .find(|p| p.machine_id == id)
            .ok_or(PeerError("peer_not_found"))
    }
    pub fn upsert(&self, peer: Peer) -> Result<()> {
        peer.validate()?;
        let mut peers = self.peers()?;
        peers.retain(|p| p.machine_id != peer.machine_id);
        peers.push(peer);
        if peers.len() > 64 {
            return Err(PeerError("peer_limit_reached"));
        }
        self.write(peers)
    }
    pub fn forget(&self, id: &str) -> Result<()> {
        let mut peers = self.peers()?;
        let before = peers.len();
        peers.retain(|p| p.machine_id != id);
        if before == peers.len() {
            return Err(PeerError("peer_not_found"));
        }
        self.write(peers)
    }
    fn write(&self, peers: Vec<Peer>) -> Result<()> {
        let name = format!("peers-{}.tmp", unique_name());
        let mut file = self.file(&name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
        let source = std::ffi::CString::new(name).unwrap();
        let result = (|| {
            let bytes = serde_json::to_vec(&Registry { version: 1, peers })
                .map_err(|_| PeerError("invalid_peer_store"))?;
            file.write_all(&bytes).map_err(failure)?;
            file.sync_all().map_err(failure)?;
            if unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    source.as_ptr(),
                    self.directory.as_raw_fd(),
                    c"peers.json".as_ptr(),
                )
            } != 0
            {
                return Err(PeerError("peer_store_io_error"));
            }
            self.directory.sync_all().map_err(failure)
        })();
        if result.is_err() {
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), source.as_ptr(), 0);
            }
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn private_atomic_registry_and_locking() {
        let path = std::env::temp_dir().join(format!("sd-peers-{}", unique_name()));
        let store = PeerStore::open(&path).unwrap();
        assert!(PeerStore::open(&path).is_err());
        store.upsert(crate::peer_client::test_peer('a')).unwrap();
        store.upsert(crate::peer_client::test_peer('b')).unwrap();
        assert_eq!(store.peers().unwrap().len(), 2);
        assert_eq!(
            std::fs::metadata(path.join("peers.json")).unwrap().mode() & 0o777,
            0o600
        );
        store.forget(&"a".repeat(32)).unwrap();
        assert_eq!(store.peers().unwrap().len(), 1);
        std::fs::set_permissions(
            path.join("peers.json"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(store.peers().is_err());
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn rejects_symlinks_and_preserves_corrupt_records() {
        let path = std::env::temp_dir().join(format!("sd-peers-{}", unique_name()));
        let store = PeerStore::open(&path).unwrap();
        std::os::unix::fs::symlink("/dev/null", path.join("peers.json")).unwrap();
        assert!(store.upsert(crate::peer_client::test_peer('a')).is_err());
        std::fs::remove_file(path.join("peers.json")).unwrap();
        let mut file = store
            .file("peers.json", libc::O_CREAT | libc::O_WRONLY)
            .unwrap();
        file.write_all(b"broken").unwrap();
        assert!(store.upsert(crate::peer_client::test_peer('a')).is_err());
        assert_eq!(std::fs::read(path.join("peers.json")).unwrap(), b"broken");
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
}
