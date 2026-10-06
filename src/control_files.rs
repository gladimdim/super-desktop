//! Bounded file operations on exact local cards, outside the GTK thread.
use crate::control::{Command, FilesEdit as Edit, FilesQuery as Read, Reply, Request};
use crate::state::TerminalData;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Instant;
pub struct Query {
    pub request: Request,
    pub responder: std::sync::mpsc::SyncSender<Result<TerminalData, Reply>>,
    pub deadline: Instant,
}

pub fn execute(
    root: &std::path::Path,
    request: &Request,
    deadline: Instant,
    mut target: impl FnMut() -> Result<TerminalData, Reply>,
) -> Reply {
    let mut run = || {
        let fail = |code, message| Reply::failure(&request.request_id, code, message);
        if Instant::now() >= deadline {
            return fail("timeout", "File request expired.");
        }
        let Some(_slot) = crate::assets::Transfer::acquire() else {
            return fail("busy", "File workers are busy.");
        };
        let card = match target() {
            Ok(c) => c,
            Err(r) => return r,
        };
        let data:Result<Value,String>=match &request.command {
            Command::Files {query:Read::List,..}=>{
                // Reuse the guarded observer, including exact pane identity and
                // deadlines. No terminal session is attached or started.
                let capture=Request {control_version:1,request_id:request.request_id.clone(),command:Command::Capture {id:card.id.clone(),history:true,lines:Some(300)}};
                let mut state=crate::state::AppState::default();state.terminals=vec![card.clone()];
                let reply=crate::control_terminal::execute(&capture,&state,deadline,|before|target().map(|after|same(before,&after)).map_err(|_|()));
                if !reply.ok{return reply;}
                let screen=reply.data.as_ref().and_then(|d|d["text"].as_str()).unwrap_or("");
                crate::assets::cli::list(&card,screen).map(|assets|json!({"id":card.id,"assets":assets,"outputSampled":true,"captureTruncated":reply.data.as_ref().unwrap()["truncated"]}))
            },
            Command::Files {query:Read::Read {asset,offset},..}=>crate::assets::cli::read(&card,asset).and_then(|(asset,bytes)|{
                let offset=usize::try_from(*offset).ok().filter(|n|*n<=bytes.len()).ok_or("invalid_offset")?;
                let end=(offset+65536).min(bytes.len());let chunk=&bytes[offset..end];
                Ok(json!({"id":card.id,"asset":asset,"offset":offset,"nextOffset":end,"eof":end==bytes.len(),"encoding":"base64","bytes":base64::engine::general_purpose::STANDARD.encode(chunk),"text":std::str::from_utf8(chunk).ok(),"sha256":format!("{:x}",Sha256::digest(&bytes))}))
            }),
            Command::FilesEdit {edit,..}=>{
                if let Edit::Save {text,..}=edit{if text.len()>8192{return fail("invalid_arguments","CLI Markdown saves accept at most 8192 UTF-8 bytes.");}}
                // Revalidate the GUI identity immediately before touching disk.
                match target(){Ok(current)if same(&card,&current)=>{},Ok(_)=>return fail("conflict","Terminal workspace changed before file edit."),Err(r)=>return r}
                match edit {
                    Edit::Add {path}=>crate::assets::cli::add(&card,path).map(|asset|json!({"id":card.id,"asset":asset,"outcome":"added"})),
                    Edit::Save {asset,text}=>crate::assets::cli::save(&card,asset,text).map(|asset|json!({"id":card.id,"asset":asset,"outcome":"saved"})),
                    Edit::Remove {asset}=>crate::assets::cli::remove(&card,asset).map(|asset|json!({"id":card.id,"asset":asset,"outcome":"reference_removed","fileDeleted":false})),
                }
            },
            _=>return fail("invalid_request","Expected terminal files operation."),
        };
        match data {
            Ok(data) => match target() {
                Ok(after) if same(&card, &after) => Reply::success(&request.request_id, data),
                _ if request.command.is_mutation() => Reply::unknown(&request.request_id),
                _ => fail(
                    "conflict",
                    "Terminal workspace changed; file content withheld.",
                ),
            },
            Err(error)
                if error == "write_failed"
                    || (request.command.is_mutation()
                        && error == "reference_history_unavailable") =>
            {
                Reply::unknown(&request.request_id)
            }
            Err(error) => fail(
                if error.contains("changed") {
                    "conflict"
                } else {
                    "file_unavailable"
                },
                &error,
            ),
        }
    };
    if request.command.is_mutation() {
        crate::control_journal::execute(root, request, |_| run())
    } else {
        run()
    }
}
fn same(a: &TerminalData, b: &TerminalData) -> bool {
    a.id == b.id
        && a.session_name == b.session_name
        && a.created_at == b.created_at
        && a.workspace_dir == b.workspace_dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn file_commands_preserve_policy_versions_and_receipts() {
        if std::env::var_os("SD_CLI_FILE_TEST").is_none() {
            let root = std::env::temp_dir().join(format!("sd-cli-files-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "control_files::tests::file_commands_preserve_policy_versions_and_receipts",
                    "--nocapture",
                ])
                .env("SD_CLI_FILE_TEST", "1")
                .env("HOME", &root)
                .env("XDG_CONFIG_HOME", &root)
                .env("XDG_STATE_HOME", &root)
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .output()
                .unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(output.status.success(), "{:?}", output);
            return;
        }
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        let workspace = home.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let card:TerminalData=serde_json::from_value(json!({"id":"sd_term_files","session_name":"sd_term_files","agent_type":"shell","command":"/bin/false","x":10,"y":100,"width":500,"height":300,"iconified":false,"created_at":1.0,"workspace_dir":workspace})).unwrap();
        let path = workspace.join("note.md");
        std::fs::write(&path, "original\nПривіт").unwrap();
        std::fs::write(workspace.join(".hidden.md"), "private").unwrap();
        symlink(&path, workspace.join("link.md")).unwrap();
        std::fs::hard_link(&path, workspace.join("hard.md")).unwrap();
        assert!(
            crate::assets::cli::add(&card, "note.md").is_err(),
            "hard-linked original is refused"
        );
        std::fs::remove_file(workspace.join("hard.md")).unwrap();
        for invalid in [".hidden.md", "link.md", "../outside.md", "/etc/passwd"] {
            assert!(
                crate::assets::cli::add(&card, invalid).is_err(),
                "{invalid}"
            );
        }
        let request = |edit, name: &str| Request {
            control_version: 1,
            request_id: name.into(),
            command: Command::FilesEdit {
                id: card.id.clone(),
                edit,
                expect_epoch: "epoch".into(),
                expect_revision: "a".repeat(64),
            },
        };
        let journal = home.join("journal");
        let apply = |r: &Request| {
            execute(
                &journal,
                r,
                Instant::now() + std::time::Duration::from_secs(3),
                || Ok(card.clone()),
            )
        };
        let added = apply(&request(
            Edit::Add {
                path: "note.md".into(),
            },
            "add",
        ));
        assert!(added.ok, "{added:?}");
        let id = added.data.unwrap()["asset"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            crate::assets::cli::read(&card, &id).unwrap().1,
            "original\nПривіт".as_bytes()
        );
        std::fs::write(&path, "external change").unwrap();
        let stale = apply(&request(
            Edit::Save {
                asset: id,
                text: "wrong overwrite".into(),
            },
            "stale",
        ));
        assert_eq!(stale.exit_code(), 5);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external change");
        let asset = crate::assets::cli::add(&card, "note.md").unwrap();
        let save = request(
            Edit::Save {
                asset: asset.id,
                text: "saved private content\n".into(),
            },
            "save",
        );
        let reply = apply(&save);
        assert!(reply.ok, "{reply:?}");
        assert!(apply(&save).ok);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "saved private content\n"
        );
        assert!(!std::fs::read_to_string(journal.join("save.json"))
            .unwrap()
            .contains("saved private content"));
        let asset = reply.data.unwrap()["asset"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            apply(&request(
                Edit::Remove {
                    asset: asset.clone()
                },
                "remove"
            ))
            .ok
        );
        assert!(path.exists());
        assert!(crate::assets::cli::read(&card, &asset).is_err());
        assert!(crate::assets::cli::list(&card, "").unwrap().is_empty());
        assert_eq!(
            crate::assets::cli::list(&card, "note.md").unwrap().len(),
            1,
            "output references can rediscover a removed hint"
        );
    }
}
