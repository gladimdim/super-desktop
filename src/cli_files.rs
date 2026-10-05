//! File reads remain data; export writes only an explicitly named new file.
use crate::cli::{render_reply, Output};
use crate::cli_extended::{json_requested, opaque, send, valid_id, Options};
use crate::control::{Command, FilesEdit as Edit, FilesQuery as Read, Reply};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

pub(crate) fn run(args: &[String]) -> Option<Output> {
    if args.first().map(String::as_str) != Some("terminal")
        || args.get(1).map(String::as_str) != Some("files")
    {
        return None;
    }
    let build = || -> Result<(Command, Options), &'static str> {
        let action = args
            .get(2)
            .map(String::as_str)
            .ok_or("Choose list/add/read/save/remove.")?;
        let edit = matches!(action, "add" | "save" | "remove");
        let mut values = vec![];
        let mut flags = vec![];
        if edit {
            values.extend(["--expect-epoch", "--expect-revision"]);
        }
        if action == "save" {
            values.push("--file");
            flags.push("--stdin");
        }
        if action == "read" {
            values.extend(["--offset", "--output"]);
        }
        let options = Options::parse(args, &values, &flags)?;
        let words: Vec<_> = options.words.iter().map(String::as_str).collect();
        let id = words
            .get(3)
            .filter(|id| valid_id(id, 128))
            .ok_or("Use an exact terminal card ID.")?
            .to_string();
        let query = match words.as_slice() {
            ["terminal", "files", "list", _] => Some(Read::List),
            ["terminal", "files", "read", _, asset] if opaque(asset) => {
                if options.values.contains_key("--output")
                    && options.values.contains_key("--offset")
                {
                    return Err("--output exports the whole file; omit --offset.");
                }
                Some(Read::Read {
                    asset: (*asset).into(),
                    offset: options.values.get("--offset").map_or(Ok(0), |v| {
                        v.parse::<u64>().map_err(|_| "Use an unsigned byte offset.")
                    })?,
                })
            }
            _ => None,
        };
        if let Some(query) = query {
            return Ok((Command::Files { id, query }, options));
        }
        let edit = match words.as_slice() {
            ["terminal", "files", "add", _, path] => Edit::Add {
                path: (*path).into(),
            },
            ["terminal", "files", "save", _, asset] if opaque(asset) => Edit::Save {
                asset: (*asset).into(),
                text: options.text(8192)?,
            },
            ["terminal", "files", "remove", _, asset] if opaque(asset) => Edit::Remove {
                asset: (*asset).into(),
            },
            _ => return Err(
                "Unknown file command or extra arguments. Copy an exact asset ID from list/add.",
            ),
        };
        let epoch = options.required("--expect-epoch")?;
        let revision = options.required("--expect-revision")?;
        if !valid_id(&epoch, 64) || !opaque(&revision) {
            return Err("Copy epoch/revision from terminal geometry.");
        }
        Ok((
            Command::FilesEdit {
                id,
                edit,
                expect_epoch: epoch,
                expect_revision: revision,
            },
            options,
        ))
    };
    Some(match build() {
        Ok((command, options)) => {
            let reply = if options.values.contains_key("--output") {
                export(command, &options)
            } else {
                let method = if command.is_mutation() {
                    "terminal.files.edit"
                } else {
                    "terminal.files.read"
                };
                send(command, &options, method)
            };
            render_reply(reply, options.json)
        }
        Err(message) => render_reply(
            Reply::failure("", "invalid_arguments", message),
            json_requested(args),
        ),
    })
}
fn export(command: Command, options: &Options) -> Reply {
    let fail = |message| Reply::failure("", "output_failed", message);
    let Command::Files {
        id,
        query: Read::Read { asset, .. },
    } = command
    else {
        return fail("Export requires a file read.");
    };
    let path = &options.values["--output"];
    let mut output = None;
    let mut offset = 0;
    let mut expected = None;
    let mut hash = Sha256::new();
    loop {
        let reply = send(
            Command::Files {
                id: id.clone(),
                query: Read::Read {
                    asset: asset.clone(),
                    offset,
                },
            },
            options,
            "terminal.files.read",
        );
        if !reply.ok {
            return if output.is_some() {
                fail("File export failed; the explicitly named output contains a partial file. It was not overwritten or removed.")
            } else {
                reply
            };
        }
        let Some(data) = reply.data else {
            return fail("Missing file chunk.");
        };
        let Some(digest) = data["sha256"].as_str().filter(|v| opaque(v)) else {
            return fail("Invalid file digest.");
        };
        if expected.as_ref().is_some_and(|v| v != digest) {
            return fail("File changed between chunks; output is partial.");
        }
        expected = Some(digest.to_string());
        let bytes = match data["bytes"]
            .as_str()
            .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
        {
            Some(v) if v.len() <= 65536 => v,
            _ => return fail("Invalid file chunk; output may be partial."),
        };
        if data["offset"] != offset
            || data["nextOffset"] != offset + bytes.len() as u64
            || offset + bytes.len() as u64 > 16 * 1024 * 1024
        {
            return fail("Invalid file chunk bounds.");
        }
        if output.is_none() {
            output =
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(path)
                {
                    Ok(file) => Some(file),
                    Err(_) => return fail(
                        "Cannot create output; existing files and symlinks are never overwritten.",
                    ),
                };
        }
        if output.as_mut().unwrap().write_all(&bytes).is_err() {
            return fail("Cannot write output; the file may be partial.");
        }
        hash.update(&bytes);
        offset += bytes.len() as u64;
        if data["eof"] == true {
            break;
        }
        if bytes.is_empty() {
            return fail("Empty non-final file chunk.");
        }
    }
    let digest = format!("{:x}", hash.finalize());
    if Some(&digest) != expected.as_ref() {
        return fail("Export digest mismatch; output must not be trusted.");
    }
    if output.unwrap().sync_all().is_err() {
        return fail("Cannot synchronize output; file durability is unknown.");
    }
    Reply::success(
        "",
        serde_json::json!({"path":path,"bytes":offset,"sha256":digest,"outcome":"exported"}),
    )
}
