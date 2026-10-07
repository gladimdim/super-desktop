//! Bounded, on-demand file references. Never a filesystem browser or a URL proxy.
pub(crate) mod cli;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs::{File, Metadata};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const MAX_FILE: u64 = 16 * 1024 * 1024;
pub const MAX_TEXT: u64 = 512 * 1024;
const MAX_ASSETS: usize = 64;
const MAX_SESSIONS: usize = 64;

static TRANSFERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub struct Transfer;
impl Transfer {
    pub fn acquire() -> Option<Self> {
        use std::sync::atomic::Ordering;
        TRANSFERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 4).then_some(n + 1)
            })
            .ok()
            .map(|_| Self)
    }
}
impl Drop for Transfer {
    fn drop(&mut self) {
        TRANSFERS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub id: String,
    pub name: String,
    pub relative_path: String,
    pub mime_type: String,
    pub kind: String,
    pub size: u64,
    pub modified: i64,
}

#[derive(Clone)]
struct Entry {
    asset: Asset,
    root: PathBuf,
    relative: PathBuf,
    version: (u64, u64, u64, i64, i64, i64, i64),
}

#[derive(Default)]
struct Catalog {
    sessions: VecDeque<(String, Vec<Entry>)>,
}
static CATALOG: OnceLock<Mutex<Catalog>> = OnceLock::new();
fn catalog() -> &'static Mutex<Catalog> {
    CATALOG.get_or_init(Mutex::default)
}

pub fn file_type(path: &Path) -> Option<(&'static str, &'static str)> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => ("image/png", "image"),
            "jpg" | "jpeg" => ("image/jpeg", "image"),
            "webp" => ("image/webp", "image"),
            "gif" => ("image/gif", "image"),
            "pdf" => ("application/pdf", "pdf"),
            "md" | "markdown" => ("text/markdown", "markdown"),
            "txt" | "log" | "csv" | "tsv" | "json" | "yaml" | "yml" | "toml" | "rs" | "py"
            | "kt" | "java" | "js" | "ts" | "tsx" | "jsx" | "css" | "c" | "h" | "cpp" | "go"
            | "sh" | "xml" | "rst" => ("text/plain", "text"),
            _ => return None, // In particular: no HTML, SVG, executables or archives.
        },
    )
}

/// Additional desktop previews; the bridge keeps its existing format set.
fn desktop_file_type(path: &Path) -> Option<(&'static str, &'static str)> {
    file_type(path).or_else(|| Some(match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "wav" | "wave" => ("audio/wav", "audio"),
        "mp3" => ("audio/mpeg", "audio"),
        "ogg" | "oga" | "opus" => ("audio/ogg", "audio"),
        "flac" => ("audio/flac", "audio"),
        "m4a" => ("audio/mp4", "audio"),
        "aac" => ("audio/aac", "audio"),
        "aif" | "aiff" => ("audio/aiff", "audio"),
        "bmp" => ("image/bmp", "image"),
        "tif" | "tiff" => ("image/tiff", "image"),
        "jsonl" | "ndjson" | "json5" | "srt" | "vtt" | "ini" | "cfg"
        | "sql" | "lua" | "rb" | "swift" | "kts" | "scss" | "sass"
        | "vue" | "svelte" | "gradle" | "properties" | "glsl" | "vert"
        | "frag" | "gd" | "tscn" | "tres" => ("text/plain", "text"),
        _ => return None,
    }))
}

fn version(meta: &Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

/// Open every component relative to an already opened directory. No symlinks,
/// no FIFO/device blocking, and no check-then-open canonicalization race.
fn open_regular(root: &Path, relative: &Path) -> Result<File, String> {
    open_in(root, relative, false)
}

/// `open_regular`, with the file itself opened for writing when `write`.
fn open_in(root: &Path, relative: &Path, write: bool) -> Result<File, String> {
    if !root.is_absolute() || relative.is_absolute() {
        return Err("outside_workspace".into());
    }
    let mut dir = File::open("/").map_err(|_| "file_unavailable")?;
    let root_components: Vec<_> = root
        .components()
        .filter(|c| *c != Component::RootDir)
        .collect();
    let relative_components: Vec<_> = relative
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect();
    if relative_components.is_empty() {
        return Err("file_unavailable".into());
    }
    for (i, component) in root_components
        .iter()
        .chain(relative_components.iter())
        .enumerate()
    {
        let Component::Normal(name) = component else {
            return Err("outside_workspace".into());
        };
        if i >= root_components.len() && name.to_string_lossy().starts_with('.') {
            return Err("hidden_files_not_shared".into());
        }
        let name = std::ffi::CString::new(name.as_encoded_bytes()).map_err(|_| "invalid_path")?;
        let last = i + 1 == root_components.len() + relative_components.len();
        let flags = if last && write { libc::O_WRONLY } else { libc::O_RDONLY }
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if last { 0 } else { libc::O_DIRECTORY };
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err("file_unavailable_or_symlink".into());
        }
        dir = unsafe { File::from_raw_fd(fd) };
    }
    let meta = dir.metadata().map_err(|_| "file_unavailable")?;
    if !meta.is_file() {
        return Err("not_a_regular_file".into());
    }
    // Hard links can otherwise expose an unrelated sensitive file under a safe name.
    if meta.nlink() != 1 {
        return Err("hard_links_not_shared".into());
    }
    Ok(dir)
}

fn register(root: &Path, session: &str, reference: &str) -> Result<Entry, String> {
    register_for(root, session, reference, false)
}

fn register_for(root: &Path, session: &str, reference: &str, desktop: bool) -> Result<Entry, String> {
    let reference = reference.trim();
    if reference.len() > 4096
        || reference.chars().any(char::is_control)
        || reference.contains("://")
    {
        return Err("invalid_path".into());
    }
    let path = Path::new(reference);
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .map_err(|_| "outside_workspace")?
            .to_path_buf()
    } else {
        path.to_path_buf()
    };
    let (mime, kind) = if desktop { desktop_file_type(&relative) } else { file_type(&relative) }
        .ok_or("unsupported_file_type")?;
    let file = open_regular(root, &relative)?;
    let meta = file.metadata().map_err(|_| "file_unavailable")?;
    if meta.len()
        > if kind == "text" || kind == "markdown" {
            MAX_TEXT
        } else {
            MAX_FILE
        }
    {
        return Err("file_too_large".into());
    }
    let ver = version(&meta);
    let id = format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{session}\0{}\0{}\0{ver:?}",
                root.display(),
                relative.display()
            )
            .as_bytes()
        )
    );
    Ok(Entry {
        asset: Asset {
            id,
            name: relative.file_name().unwrap().to_string_lossy().into(),
            relative_path: relative.to_string_lossy().into(),
            mime_type: mime.into(),
            kind: kind.into(),
            size: meta.len(),
            modified: meta.mtime(),
        },
        root: root.into(),
        relative,
        version: ver,
    })
}

fn workspace(session: &str) -> Result<PathBuf, String> {
    let state = crate::state::load_state();
    let term = state
        .terminals
        .iter()
        .find(|t| t.session_name == session)
        .ok_or("unknown_terminal")?;
    // Never fall back to HOME when the recorded workspace disappears.
    let root = term
        .workspace_dir
        .as_deref()
        .ok_or("workspace_not_recorded")?;
    let root = std::fs::canonicalize(root).map_err(|_| "workspace_unavailable")?;
    if root == Path::new("/") {
        return Err("root_workspace_not_shared".into());
    }
    Ok(root)
}

/// Conservative candidates: quoted paths, Markdown targets, and plain words.
/// Ambiguous/wrapped references can be added manually through the same policy.
fn candidates(text: &str) -> Vec<String> {
    let text: String = text.chars().take(128 * 1024).collect();
    let mut result = Vec::new();
    for delimiter in ['`', '"', '\''] {
        result.extend(text.split(delimiter).skip(1).step_by(2).map(str::to_string));
    }
    for part in text.split("](").skip(1) {
        if let Some((path, _)) = part.split_once(')') {
            result.push(path.into());
        }
    }
    result.extend(text.split_whitespace().map(|word| {
        word.trim_matches(|c: char| "()[]{}<>\"'`,;".contains(c))
            .trim_end_matches('.')
            .to_string()
    }));
    result.retain(|s| s.len() <= 4096 && file_type(Path::new(s)).is_some());
    result.truncate(256);
    result
}

pub fn list(session: &str, explicit: Option<&str>) -> Result<Vec<Asset>, String> {
    list_for(session, explicit, false)
}

pub fn list_desktop(session: &str, explicit: Option<&str>) -> Result<Vec<Asset>, String> {
    list_for(session, explicit, true)
}

fn catalog_key(session: &str, desktop: bool) -> String {
    if desktop { format!("desktop:{session}") } else { session.to_owned() }
}

fn list_for(session: &str, explicit: Option<&str>, desktop: bool) -> Result<Vec<Asset>, String> {
    let root = workspace(session)?;
    let key = catalog_key(session, desktop);
    let register = |root: &Path, session: &str, reference: &str| register_for(root, session, reference, desktop);
    let mut found = Vec::new();
    if let Some(path) = explicit {
        found.push(register(&root, session, path)?);
    }
    // This runs only when opening/refreshing the drawer, never per frame/keystroke.
    let screen = if desktop {
        crate::tmux::capture_file_references(session)
    } else {
        crate::tmux::capture_pane_history(session)
    }.unwrap_or_default();
    let paths = if desktop {
        crate::asset_references::candidates(&screen, |path| desktop_file_type(Path::new(path)).is_some())
    } else {
        candidates(&screen)
    };
    for path in paths {
        if let Ok(entry) = register(&root, session, &path) {
            found.push(entry);
        }
        if found.len() >= MAX_ASSETS {
            break;
        }
    }
    // Persist only path hints. Re-registering below enforces the same workspace,
    // symlink, hidden-file and size policy for references added on either device.
    let references = found
        .iter()
        .map(|e| e.asset.relative_path.clone())
        .collect();
    match crate::asset_history::merge(session, &root, references) {
        Ok(paths) => {
            for path in paths {
                if let Ok(entry) = register(&root, session, &path) {
                    found.push(entry);
                }
            }
        }
        Err(error) => eprintln!("File reference history unavailable: {error}"),
    }
    let mut catalog = catalog().lock().unwrap();
    let old = catalog
        .sessions
        .iter()
        .position(|(id, _)| id == &key)
        .and_then(|i| catalog.sessions.remove(i))
        .map(|(_, items)| items)
        .unwrap_or_default();
    // Refresh previously referenced paths too, so a rewritten asset gets a new ID.
    for entry in old {
        if entry.root == root {
            if let Ok(entry) = register(&root, session, &entry.asset.relative_path) {
                found.push(entry);
            }
        }
    }
    let mut paths = std::collections::HashSet::new();
    found.retain(|entry| paths.insert(entry.relative.clone()));
    found.truncate(MAX_ASSETS);
    let result = found.iter().map(|e| e.asset.clone()).collect();
    catalog.sessions.push_back((key, found));
    while catalog.sessions.len() > MAX_SESSIONS {
        catalog.sessions.pop_front();
    }
    Ok(result)
}

fn listed(session: &str, id: &str, desktop: bool) -> Result<Entry, String> {
    let key = catalog_key(session, desktop);
    let root = workspace(session)?;
    let entry = {
        let catalog = catalog().lock().unwrap();
        catalog
            .sessions
            .iter()
            .find(|(s, _)| s == &key)
            .and_then(|(_, entries)| entries.iter().find(|e| e.asset.id == id))
            .cloned()
            .ok_or("asset_expired_refresh_list")?
    };
    if entry.root != root {
        return Err("workspace_changed_refresh_list".into());
    }
    Ok(entry)
}

pub fn read(session: &str, id: &str) -> Result<(Asset, Vec<u8>), String> {
    read_entry(&listed(session, id, false)?)
}

pub fn read_desktop(session: &str, id: &str) -> Result<(Asset, Vec<u8>), String> {
    read_entry(&listed(session, id, true)?)
}

/// Replace a listed Markdown file's contents with `text`, as edited on this
/// desktop. The file must still be the one that was read: a change on disk
/// since then is refused rather than overwritten. Returns the file's new
/// listing (with a new ID), which replaces the old one.
pub fn write_markdown(session: &str, id: &str, text: &str) -> Result<Asset, String> {
    let entry = listed(session, id, true)?;
    let saved = write_entry(session, &entry, text)?;
    let asset = saved.asset.clone();
    let mut catalog = catalog().lock().unwrap();
    if let Some(slot) = catalog
        .sessions
        .iter_mut()
        .find(|(s, _)| s == &catalog_key(session, true))
        .and_then(|(_, entries)| entries.iter_mut().find(|e| e.asset.id == id))
    {
        *slot = saved;
    }
    Ok(asset)
}

fn write_entry(session: &str, entry: &Entry, text: &str) -> Result<Entry, String> {
    use std::io::Write;
    if entry.asset.kind != "markdown" {
        return Err("only_markdown_is_editable".into());
    }
    if text.len() as u64 > MAX_TEXT {
        return Err("file_too_large".into());
    }
    if text.contains('\0') {
        return Err("file_content_does_not_match_type".into());
    }
    let mut file = open_in(&entry.root, &entry.relative, true)?;
    if version(&file.metadata().map_err(|_| "file_unavailable")?) != entry.version {
        return Err("file_changed_on_disk_refresh_list".into());
    }
    file.set_len(0).map_err(|_| "write_failed")?;
    file.write_all(text.as_bytes()).map_err(|_| "write_failed")?;
    file.sync_data().map_err(|_| "write_failed")?;
    drop(file);
    register(&entry.root, session, &entry.asset.relative_path)
}

fn read_entry(entry: &Entry) -> Result<(Asset, Vec<u8>), String> {
    let mut file = open_regular(&entry.root, &entry.relative)?;
    if version(&file.metadata().map_err(|_| "file_unavailable")?) != entry.version {
        return Err("file_changed_refresh_list".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(entry.asset.size + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "read_failed")?;
    if bytes.len() as u64 != entry.asset.size
        || version(&file.metadata().map_err(|_| "file_unavailable")?) != entry.version
    {
        return Err("file_changed_refresh_list".into());
    }
    let valid = match entry.asset.mime_type.as_str() {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => gif_frames(&bytes).is_ok(),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "image/bmp" => bytes.starts_with(b"BM"),
        "image/tiff" => bytes.starts_with(b"II\x2a\0") || bytes.starts_with(b"MM\0\x2a"),
        "audio/wav" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE"),
        "audio/ogg" => bytes.starts_with(b"OggS"),
        "audio/flac" => bytes.starts_with(b"fLaC"),
        "audio/mp4" => bytes.get(4..8) == Some(b"ftyp"),
        "audio/aiff" => bytes.starts_with(b"FORM") && matches!(bytes.get(8..12), Some(b"AIFF" | b"AIFC")),
        "audio/mpeg" => bytes.starts_with(b"ID3") || bytes.first() == Some(&0xff)
            && bytes.get(1).is_some_and(|b| b & 0xe6 == 0xe2),
        "audio/aac" => bytes.first() == Some(&0xff) && bytes.get(1).is_some_and(|b| b & 0xf6 == 0xf0),
        _ => std::str::from_utf8(&bytes).is_ok() && !bytes.contains(&0),
    };
    if !valid {
        return Err("file_content_does_not_match_type".into());
    }
    Ok((entry.asset.clone(), bytes))
}

/// Validate animation dimensions/frame count before either client decodes GIF.
pub fn gif_frames(bytes: &[u8]) -> Result<usize, String> {
    if bytes.len() < 13 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Err("Invalid GIF".into());
    }
    let width = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    let height = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    if width == 0 || height == 0 {
        return Err("Invalid GIF dimensions".into());
    }
    let table_size = |packed: u8| {
        if packed & 0x80 != 0 {
            3usize << ((packed & 7) + 1)
        } else {
            0
        }
    };
    let mut pos = 13 + table_size(bytes[10]);
    let mut count = 0;
    let mut ended = false;
    while pos < bytes.len() {
        let tag = bytes[pos];
        pos += 1;
        match tag {
            0x3b => {
                ended = true;
                break;
            }
            0x21 => {
                pos += 1;
            }
            0x2c => {
                if pos + 9 > bytes.len() {
                    return Err("Invalid GIF".into());
                }
                let fw = u16::from_le_bytes([bytes[pos + 4], bytes[pos + 5]]) as usize;
                let fh = u16::from_le_bytes([bytes[pos + 6], bytes[pos + 7]]) as usize;
                let x = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]) as usize;
                let y = u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
                if fw == 0 || fh == 0 || x + fw > width || y + fh > height {
                    return Err("Invalid GIF frame".into());
                }
                pos += 9 + table_size(bytes[pos + 8]) + 1;
                count += 1;
                if count > 200 || width.saturating_mul(height).saturating_mul(count) > 16_000_000 {
                    return Err("GIF exceeds preview memory limit".into());
                }
            }
            _ => return Err("Invalid GIF block".into()),
        }
        loop {
            let size = *bytes.get(pos).ok_or("Invalid GIF data")? as usize;
            pos += 1;
            if size == 0 {
                break;
            }
            pos += size;
        }
    }
    if count == 0 || !ended {
        return Err("GIF has no complete frames".into());
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "sd-assets-{}-{}",
                std::process::id(),
                crate::tmux::unique_session_name()
            ));
            std::fs::create_dir(&path).unwrap();
            // Production resolves the workspace before the no-symlink walk.
            // macOS's temporary directory commonly starts with /var -> /private/var.
            Self(std::fs::canonicalize(path).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn recognizes_supported_references_and_no_web_content() {
        let paths = candidates("Wrote `images/my preview.png` and [report](docs/report.pdf) and README.md https://example/a.png");
        assert!(paths.contains(&"images/my preview.png".into()));
        assert!(paths.contains(&"docs/report.pdf".into()));
        for name in ["x.html", "x.svg", "x.exe", "x.zip", ".env"] {
            assert!(file_type(Path::new(name)).is_none());
        }
    }
    #[test]
    fn denies_traversal_symlinks_hidden_files_and_hard_links() {
        let dir = Fixture::new();
        std::fs::write(dir.0.join("ok.md"), "# Safe").unwrap();
        symlink("ok.md", dir.0.join("link.md")).unwrap();
        for path in [
            "../ok.md",
            "/etc/passwd",
            "link.md",
            "https://example/a.png",
        ] {
            assert!(register(&dir.0, "s", path).is_err(), "{path}");
        }
        std::fs::create_dir(dir.0.join(".private")).unwrap();
        std::fs::write(dir.0.join(".private/key.txt"), "secret").unwrap();
        assert!(register(&dir.0, "s", ".private/key.txt").is_err());
        let entry = register(&dir.0, "s", "ok.md").unwrap();
        assert_eq!(read_entry(&entry).unwrap().1, b"# Safe");
        std::fs::hard_link(dir.0.join("ok.md"), dir.0.join("hard.md")).unwrap();
        assert!(register(&dir.0, "s", "hard.md").is_err());
    }
    #[test]
    fn detects_replacement_and_rejects_oversized_or_mistyped_files() {
        let dir = Fixture::new();
        std::fs::write(dir.0.join("note.md"), "old").unwrap();
        let entry = register(&dir.0, "s", "note.md").unwrap();
        std::fs::write(dir.0.join("note.md"), "new contents").unwrap();
        assert!(read_entry(&entry).is_err());
        std::fs::write(dir.0.join("fake.png"), "not a PNG").unwrap();
        assert!(read_entry(&register(&dir.0, "s", "fake.png").unwrap()).is_err());
        File::create(dir.0.join("huge.txt"))
            .unwrap()
            .set_len(MAX_TEXT + 1)
            .unwrap();
        assert!(register(&dir.0, "s", "huge.txt").is_err());
    }
    #[test]
    fn desktop_references_find_existing_wrapped_files_without_relaxing_access() {
        let dir = Fixture::new();
        std::fs::create_dir_all(dir.0.join("game/assets/art/menu")).unwrap();
        for name in ["prisoners_at_dawn_v1.png", "prisoners_at_dawn_v1.prompt.json"] {
            std::fs::write(dir.0.join("game/assets/art/menu").join(name), "fixture").unwrap();
        }
        std::fs::write(dir.0.join("hidden.png"), "must not substitute for .hidden.png").unwrap();
        symlink("game/assets/art/menu/prisoners_at_dawn_v1.png", dir.0.join("link.png")).unwrap();
        let text = "Created with imagegen: image (game/assets/art/menu/\n  prisoners_at_dawn_v1.png) · prompt (game/assets/art/menu/\n  prisoners_at_dawn_v1.prompt.json). .hidden.png link.png ../outside.png https://example.org/hidden.png";
        let found: Vec<_> = crate::asset_references::candidates(text, |p| desktop_file_type(Path::new(p)).is_some())
            .iter().filter_map(|path| register_for(&dir.0, "s", path, true).ok())
            .map(|entry| entry.asset.relative_path).collect();
        assert_eq!(found, ["game/assets/art/menu/prisoners_at_dawn_v1.png", "game/assets/art/menu/prisoners_at_dawn_v1.prompt.json"]);
    }

    #[test]
    fn desktop_references_find_files_a_harness_wrapped_mid_name() {
        let dir = Fixture::new();
        std::fs::create_dir_all(dir.0.join("src/assets")).unwrap();
        for name in ["src/assets/cli.rs", "src/assets/view.rs", "notes.md"] {
            std::fs::write(dir.0.join(name), "fixture").unwrap();
        }
        // Claude Code's tool output, re-wrapped by the harness for a narrow card.
        let text = "⏺ Update(src/assets/cl\n       i.rs)\n  ⎿  Added 3 lines to src/assets/vi\n     ew.rs and notes.md\n  view.rs";
        let found: Vec<_> = crate::asset_references::candidates(text, |p| desktop_file_type(Path::new(p)).is_some())
            .iter().filter_map(|path| register_for(&dir.0, "s", path, true).ok())
            .map(|entry| entry.asset.relative_path).collect();
        assert_eq!(found, ["src/assets/cli.rs", "notes.md", "src/assets/view.rs"]);
    }

    #[test]
    fn desktop_audio_and_extra_formats_preserve_bridge_types_and_limits() {
        let dir = Fixture::new();
        for (name, bytes) in [
            ("sound.wav", b"RIFF\0\0\0\0WAVE".as_slice()),
            ("sound.mp3", b"ID3".as_slice()),
            ("sound.ogg", b"OggS".as_slice()),
            ("sound.opus", b"OggS".as_slice()),
            ("sound.flac", b"fLaC".as_slice()),
            ("sound.m4a", b"\0\0\0\x18ftypM4A ".as_slice()),
            ("sound.aac", b"\xff\xf1".as_slice()),
            ("sound.aiff", b"FORM\0\0\0\0AIFF".as_slice()),
            ("art.bmp", b"BM".as_slice()),
            ("art.tiff", b"II\x2a\0".as_slice()),
            ("events.jsonl", b"{}\n".as_slice()),
            ("menu.gd", b"extends Node".as_slice()),
        ] {
            std::fs::write(dir.0.join(name), bytes).unwrap();
            assert!(register(&dir.0, "s", name).is_err(), "bridge: {name}");
            let entry = register_for(&dir.0, "s", name, true).unwrap();
            assert_eq!(read_entry(&entry).unwrap().1, bytes, "desktop: {name}");
            std::fs::write(dir.0.join(name), b"\0invalid format").unwrap();
            let entry = register_for(&dir.0, "s", name, true).unwrap();
            assert!(read_entry(&entry).is_err(), "mismatch: {name}");
        }
        File::create(dir.0.join("large.wav")).unwrap().set_len(MAX_FILE + 1).unwrap();
        assert!(register_for(&dir.0, "s", "large.wav", true).is_err());
        File::create(dir.0.join("large.jsonl")).unwrap().set_len(MAX_TEXT + 1).unwrap();
        assert!(register_for(&dir.0, "s", "large.jsonl", true).is_err());
        for path in ["app.exe", "page.html", "vector.svg", "archive.zip"] {
            assert!(desktop_file_type(Path::new(path)).is_none());
        }
        assert_ne!(catalog_key("s", true), catalog_key("s", false));
    }

    #[test]
    fn saves_edited_markdown_only_over_the_file_that_was_read() {
        let dir = Fixture::new();
        std::fs::write(dir.0.join("plan.md"), "# Old").unwrap();
        let entry = register(&dir.0, "s", "plan.md").unwrap();
        let saved = write_entry("s", &entry, "# New\n").unwrap();
        assert_eq!(std::fs::read(dir.0.join("plan.md")).unwrap(), b"# New\n");
        assert_ne!(saved.asset.id, entry.asset.id);
        assert_eq!(read_entry(&saved).unwrap().1, b"# New\n");
        // The old listing no longer matches the file: no blind overwrite.
        assert!(write_entry("s", &entry, "# Lost").is_err());
        assert_eq!(std::fs::read(dir.0.join("plan.md")).unwrap(), b"# New\n");
        // A change made elsewhere after reading is refused too.
        std::fs::write(dir.0.join("plan.md"), "# Changed elsewhere").unwrap();
        assert!(write_entry("s", &saved, "# Mine").is_err());
        assert_eq!(std::fs::read(dir.0.join("plan.md")).unwrap(), b"# Changed elsewhere");
        // Only Markdown, bounded, and never through a symlink.
        std::fs::write(dir.0.join("notes.txt"), "x").unwrap();
        let text = register(&dir.0, "s", "notes.txt").unwrap();
        assert!(write_entry("s", &text, "y").is_err());
        let fresh = register(&dir.0, "s", "plan.md").unwrap();
        assert!(write_entry("s", &fresh, &"a".repeat(MAX_TEXT as usize + 1)).is_err());
        assert!(write_entry("s", &fresh, "nul\0byte").is_err());
        std::fs::remove_file(dir.0.join("plan.md")).unwrap();
        std::fs::write(dir.0.join("target.md"), "safe").unwrap();
        symlink("target.md", dir.0.join("plan.md")).unwrap();
        assert!(write_entry("s", &fresh, "# Through a link").is_err());
        assert_eq!(std::fs::read(dir.0.join("target.md")).unwrap(), b"safe");
    }
}
