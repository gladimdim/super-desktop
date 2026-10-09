//! Durable mutation receipts. An interrupted request is never re-executed.
use crate::cli_extended::valid_id;
use crate::control::{self, Reply, Request};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const MAX_ENTRIES: usize = 4096;
const MAX_ENTRY_BYTES: u64 = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    version: u32,
    request_hash: String,
    card_id: String,
    reply: Option<Reply>,
}

pub fn root() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("super-desktop/cli-requests")
}

fn entry_path(root: &Path, id: &str) -> io::Result<PathBuf> {
    if !valid_id(id, 64) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid request ID",
        ));
    }
    Ok(root.join(format!("{id}.json")))
}

fn read(path: &Path) -> io::Result<Option<Entry>> {
    match control::private_file(path, false) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(m) if m.len() > MAX_ENTRY_BYTES => return Err(io::Error::other("oversized receipt")),
        Ok(_) => {}
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_ENTRY_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ENTRY_BYTES {
        return Err(io::Error::other("oversized receipt"));
    }
    let entry: Entry = serde_json::from_slice(&bytes)?;
    if entry.version != 1 {
        return Err(io::Error::other("unsupported receipt"));
    }
    Ok(Some(entry))
}

fn write_new(path: &Path, entry: &Entry) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    serde_json::to_writer(&mut file, entry)?;
    file.flush()?;
    file.sync_all()
}

struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn lock(root: &Path) -> io::Result<Lock> {
    if !root.is_absolute() {
        return Err(io::Error::other("journal path must be absolute"));
    }
    let mut current = PathBuf::new();
    for component in root.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(io::Error::other("invalid journal path"));
        }
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
            Ok(_) => return Err(io::Error::other("unsafe journal path")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::DirBuilder::new().mode(0o700).create(&current)?;
                File::open(current.parent().unwrap())?.sync_all()?;
            }
            Err(e) => return Err(e),
        }
    }
    control::private_dir(root)?;
    let path = root.join("journal.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&path)?;
    control::private_file(&path, false)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "mutation journal busy",
        ));
    }
    Ok(Lock(file))
}

fn journal_error(id: &str, _error: io::Error) -> Reply {
    // A competing execution or unreadable receipt may represent an earlier
    // mutation. Refusing this connection cannot establish its original outcome.
    let mut reply = Reply::unknown(id);
    reply.error.as_mut().unwrap().message = "The private mutation journal is busy, unavailable or invalid. No new mutation was attempted; an earlier outcome cannot be established. Inspect this ID; do not retry with a new ID.".into();
    reply
}

/// Serializes mutations across threads/processes and saves intent before any
/// side effect. There is deliberately no automatic pruning of IDs.
pub fn execute(root: &Path, request: &Request, apply: impl FnOnce(&str) -> Reply) -> Reply {
    execute_operation(root,&request.request_id,&request.command,|random|match &request.command {
            control::Command::WorkspaceEdit {
                edit: control::WorkspaceEdit::NoteCreate { .. },
                ..
            } => format!("note_cli_{}", request.request_id),
            control::Command::WorkspaceEdit {
                edit:
                    control::WorkspaceEdit::NoteUpdate { id, .. }
                    | control::WorkspaceEdit::NoteDelete { id }
                    | control::WorkspaceEdit::NoteMove { id, .. }
                    | control::WorkspaceEdit::NoteResize { id, .. }
                    | control::WorkspaceEdit::NoteTag { id, .. },
                ..
            } => id.clone(),
            control::Command::ConnectionInvite {..} => "connection:invitation".into(),
            control::Command::ConnectionDecide {id,..} | control::Command::ConnectionRevoke {id} => format!("connection:{id}"),
            control::Command::PeerAdd {..} => "peer:pairing".into(),
            control::Command::PeerCommand {id,..} | control::Command::PeerForget {id}=>format!("peer:{id}"),
            control::Command::UpdatesCheck {} | control::Command::UpdatesInstall {..} => "updates".into(),
            control::Command::WorkspaceEdit { .. } => "workspace".into(),
            control::Command::PreferencesEdit { .. } | control::Command::Shortcut {..} => "settings".into(),
            control::Command::Forget {id,..}
            | control::Command::Attach { id, .. }
            | control::Command::Viewport { id, .. }
            | control::Command::CardAction { id, .. }
            | control::Command::FilesEdit { id, .. }
            | control::Command::Input { id, .. }
            | control::Command::Mode { id, .. }
            | control::Command::Move { id, .. }
            | control::Command::Resize { id, .. }
            | control::Command::Close { id, .. } => id.clone(),
            _ => format!("sd_term_cli_{random}"),
        },apply)
}

/// Owner-client orchestration shares the same ID namespace and durable journal.
pub fn execute_operation(root:&Path,id:&str,payload:&impl Serialize,target:impl FnOnce(&str)->String,apply:impl FnOnce(&str)->Reply)->Reply {
    execute_reserved(root,id,payload,target,apply,false)
}

/// Reserve a workflow ID durably, then release the global lock so its child
/// mutations can use the same journal. Only the reservation owner writes the
/// result: concurrent/restarted callers see pending and never enter apply.
pub fn execute_workflow(root:&Path,id:&str,payload:&impl Serialize,target:impl FnOnce(&str)->String,apply:impl FnOnce(&str)->Reply)->Reply {
    execute_reserved(root,id,payload,target,apply,true)
}

fn execute_reserved(root:&Path,id:&str,payload:&impl Serialize,target:impl FnOnce(&str)->String,apply:impl FnOnce(&str)->Reply,release:bool)->Reply {
    let _lock = match lock(root) {
        Ok(lock) => lock,
        Err(e) => return journal_error(id, e),
    };
    let path = match entry_path(root, id) {
        Ok(path) => path,
        Err(e) => return journal_error(id, e),
    };
    let hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(payload).unwrap())
    );
    match read(&path) {
        Ok(Some(entry)) if entry.request_hash != hash => {
            return Reply::failure(
                id,
                "conflict",
                "This request ID already describes a different mutation.",
            )
        }
        Ok(Some(entry)) => return entry.reply.unwrap_or_else(|| Reply::unknown(id)),
        Ok(None) => {}
        Err(e) => return journal_error(id, e),
    }
    let count = match fs::read_dir(root).and_then(|entries| entries.collect::<io::Result<Vec<_>>>())
    {
        Ok(entries) => entries.len(),
        Err(e) => return journal_error(id, e),
    };
    if count >= MAX_ENTRIES + 1 {
        return Reply::failure(
            id,
            "journal_full",
            "Mutation journal is full. Existing IDs remain inspectable; no new mutation was attempted.",
        );
    }
    // Fresh random identity, recorded before launch. A request replay never
    // claims an existing session based solely on its name or prefix.
    let random = match control::new_request(control::Command::Status {}) {
        Ok(r) => r.request_id,
        Err(e) => return journal_error(id, e),
    };
    let mut entry = Entry {
        version: 1,
        request_hash: hash,
        card_id: target(&random),
        reply: None,
    };
    let durable = write_new(&path, &entry).and_then(|()| File::open(root)?.sync_all());
    if let Err(e) = durable {
        return journal_error(id, e);
    }
    let held = if release { drop(_lock); None } else { Some(_lock) };
    let reply = apply(&entry.card_id);
    let _held = held;
    entry.reply = Some(reply.clone());
    let temporary = root.join(format!(".{id}.{}.tmp", std::process::id()));
    // A leftover temporary file is evidence of an interrupted write, never
    // something to truncate or trust. Keep the pending receipt on any failure.
    let saved = write_new(&temporary, &entry)
        .and_then(|()| fs::rename(&temporary, &path))
        .and_then(|()| File::open(root)?.sync_all());
    if saved.is_err() {
        return Reply::unknown(id);
    }
    reply
}

pub fn inspect(root: &Path, request_id: &str, id: &str) -> Reply {
    let path = match entry_path(root, id) {
        Ok(path) => path,
        Err(_) => return Reply::failure(request_id, "invalid_arguments", "Invalid request ID."),
    };
    if !root.exists() {
        return Reply::failure(request_id, "not_found", "No mutation receipt has that ID.");
    }
    if let Err(e) = control::private_dir(root) {
        return journal_error(request_id, e);
    }
    match read(&path) {
        Ok(Some(entry)) => Reply::success(
            request_id,
            serde_json::json!({"id":id,"cardId":entry.card_id,
            "state":if entry.reply.is_some() {"recorded"} else {"unknown"},
            "result":entry.reply.unwrap_or_else(|| Reply::unknown(id))}),
        ),
        Ok(None) => Reply::failure(request_id, "not_found", "No mutation receipt has that ID."),
        Err(e) => journal_error(request_id, e),
    }
}

/// Stable, bounded receipt metadata; never includes result payloads or credentials.
pub fn list(root:&Path,request_id:&str,after:Option<&str>,limit:u16,expected:Option<&str>)->Reply {
    use std::os::unix::fs::MetadataExt;
    let fail=|code,message|Reply::failure(request_id,code,message);
    if !(1..=100).contains(&limit) || after.is_some_and(|s|!valid_id(s, 64)) { return fail("invalid_arguments","Use limit 1-100 and a returned cursor."); }
    let collect=||->io::Result<Vec<(String,u64,i64,i64,u64)>> {
        match control::private_dir(root) { Err(e) if e.kind()==io::ErrorKind::NotFound=>return Ok(vec![]),Err(e)=>return Err(e),Ok(_)=>{} }
        let mut files=vec![];
        for file in fs::read_dir(root)? {
            let file=file?;let name=file.file_name();let Some(id)=name.to_str().and_then(|n|n.strip_suffix(".json")) else {continue;};
            if !valid_id(id, 64) {return Err(io::Error::other("invalid receipt name"));}
            let m=control::private_file(&file.path(),false)?;
            files.push((id.to_owned(),m.ino(),m.mtime(),m.mtime_nsec(),m.len()));
            if files.len()>MAX_ENTRIES {return Err(io::Error::other("too many receipts"));}
        }
        files.sort();Ok(files)
    };
    let files=match collect(){Ok(v)=>v,Err(_)=>return fail("unavailable","Private receipt inventory is unavailable.")};
    let revision=format!("{:x}",Sha256::digest(serde_json::to_vec(&files).unwrap()));
    if expected.is_some_and(|v|v!=revision) {return fail("conflict","Receipt inventory changed; restart pagination.");}
    let mut entries=vec![];
    let eligible:Vec<_>=files.iter().filter(|f|after.is_none_or(|a|f.0.as_str()>a)).collect();
    for (id,_,modified,_,_) in eligible.iter().take(limit as usize).copied() {
        let entry=match read(&root.join(format!("{id}.json"))){Ok(Some(v))=>v,_=>return fail("unavailable","A receipt is unreadable; inventory is incomplete.")};
        entries.push(serde_json::json!({"id":id,"cardId":entry.card_id,"state":if entry.reply.is_some(){"recorded"}else{"unknown"},"ok":entry.reply.as_ref().map(|r|r.ok),"outcome":entry.reply.as_ref().and_then(|r|r.error.as_ref()).map(|e|e.outcome.as_str()),"modifiedUnixSeconds":modified}));
    }
    if collect().ok().as_ref()!=Some(&files){return fail("conflict","Receipt inventory changed during this read.");}
    let next=if eligible.len()>entries.len(){entries.last().map(|e|e["id"].clone())}else{None};
    Reply::success(request_id,serde_json::json!({"entries":entries,"revision":revision,"total":files.len(),"nextCursor":next,"order":"request-id","contentIncluded":false,"scope":"local-cli-mutations"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .canonicalize()
                .expect("resolve system temporary directory")
                .join(format!(
                "sd-receipt-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn request() -> Request {
        Request {
            control_version: 1,
            request_id: "launch-1".into(),
            command: control::Command::Launch {
                arguments: None,
                harness: "shell".into(),
                cwd: "/private/path".into(),
                allow_unsafe_harness: false,
                allow_download: false,
            },
        }
    }

    #[test]
    fn audit_pages_redact_content_guard_revisions_and_refuse_symlinks() {
        let root=Temp::new();let request=request();
        assert!(list(&root.0,"read",None,1,None).ok);
        for n in 0..3 {let mut r=request.clone();r.request_id=format!("audit-{n}");assert!(execute(&root.0,&r,|_|Reply::success(&r.request_id,serde_json::json!({"secret":"PRIVATE TEXT"}))).ok);}
        let first=list(&root.0,"read",None,1,None).data.unwrap();
        assert_eq!(first["total"],3);assert_eq!(first["nextCursor"],"audit-0");assert!(!first.to_string().contains("PRIVATE TEXT"));
        let revision=first["revision"].as_str().unwrap();
        let second=list(&root.0,"read",Some("audit-0"),2,Some(revision)).data.unwrap();assert_eq!(second["entries"].as_array().unwrap().len(),2);assert!(second["nextCursor"].is_null());
        assert!(execute(&root.0,&request,|_|Reply::success("launch-1",serde_json::json!({}))).ok);
        assert_eq!(list(&root.0,"read",Some("audit-0"),2,Some(revision)).exit_code(),5);
        std::os::unix::fs::symlink(root.0.join("audit-0.json"),root.0.join("linked.json")).unwrap();
        assert!(!list(&root.0,"read",None,100,None).ok);
    }
    #[test]
    fn cli_launch_receipt_replays_result_and_rejects_reused_id_with_changed_payload() {
        let root = Temp::new();
        let request = request();
        let result = execute(&root.0, &request, |card| {
            Reply::success(&request.request_id, serde_json::json!({"id":card}))
        });
        assert!(result.ok);
        let again = execute(&root.0, &request, |_| panic!("must not launch twice"));
        assert_eq!(
            serde_json::to_value(&again).unwrap(),
            serde_json::to_value(&result).unwrap()
        );
        let mut changed = request.clone();
        if let control::Command::Launch { cwd, .. } = &mut changed.command {
            *cwd = "/different".into();
        }
        assert_eq!(
            execute(&root.0, &changed, |_| panic!("conflict must not execute")).exit_code(),
            5
        );
        let disk = fs::read_to_string(root.0.join("launch-1.json")).unwrap();
        assert!(!disk.contains("/private/path"));
        let read = inspect(&root.0, "inspect-1", "launch-1");
        assert!(read.ok);
        assert_eq!(read.data.unwrap()["state"], "recorded");
    }

    #[test]
    fn cli_launch_crash_between_intent_and_result_is_unknown_and_never_replayed() {
        let root = Temp::new();
        let request = request();
        let interrupted =
            std::panic::catch_unwind(|| execute(&root.0, &request, |_| panic!("simulated crash")));
        assert!(interrupted.is_err());
        let reply = execute(&root.0, &request, |_| {
            panic!("pending receipt must not replay")
        });
        assert_eq!(reply.error.unwrap().outcome, "unknown");
        assert_eq!(
            inspect(&root.0, "inspect", "launch-1").data.unwrap()["state"],
            "unknown"
        );
    }

    #[test]
    fn cli_launch_corrupt_receipt_and_symlink_fail_closed() {
        let root = Temp::new();
        let request = request();
        {
            let _guard = lock(&root.0).unwrap();
        }
        let path = entry_path(&root.0, "launch-1").unwrap();
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(b"not json").unwrap();
        drop(file);
        assert!(!execute(&root.0, &request, |_| panic!("corrupt receipt")).ok);
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("missing", &path).unwrap();
        assert!(!execute(&root.0, &request, |_| panic!("symlink receipt")).ok);
        assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
    }

    #[test]
    fn cli_launch_concurrent_request_does_not_run_a_second_operation() {
        let root = Temp::new();
        let request = request();
        execute(&root.0, &request, |_| {
            let duplicate = execute(&root.0, &request, |_| panic!("concurrent execution"));
            assert_eq!(duplicate.error.unwrap().outcome, "unknown");
            Reply::success(&request.request_id, serde_json::json!({}))
        });
    }

    #[test]
    fn cli_launch_journal_rejects_symlink_parent_and_full_storage() {
        let root = Temp::new();
        fs::DirBuilder::new().mode(0o700).create(&root.0).unwrap();
        let real = root.0.join("real");
        fs::create_dir(&real).unwrap();
        let link = root.0.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let request = request();
        let refused = execute(&link.join("receipts"), &request, |_| {
            panic!("symlink parent")
        });
        assert_eq!(refused.error.unwrap().outcome, "unknown");
        assert!(
            !real.join("receipts").exists(),
            "must not create through a symlink"
        );
        for n in 0..MAX_ENTRIES {
            fs::write(root.0.join(format!("existing-{n}")), "").unwrap();
        }
        let full = execute(&root.0, &request, |_| panic!("full journal"));
        assert_eq!(full.error.unwrap().code, "journal_full");
        assert!(!root.0.join("launch-1.json").exists());
    }
}
