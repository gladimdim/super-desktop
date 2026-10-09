//! Structured workspace and note command parsing shared by both frontends.
use crate::cli::{render_reply, Output};
use crate::cli_extended::{opaque, respond, send, valid_id, Options};
use crate::control::{Command, WorkspaceEdit as Edit, WorkspaceQuery as Query};

pub(crate) fn run(args: &[String]) -> Option<Output> {
    if !matches!(args.first().map(String::as_str), Some("note" | "workspace")) {
        return None;
    }
    let build = || -> Result<(Command, Options), &'static str> {
        let words: Vec<_> = args
            .iter()
            .take_while(|a| !a.starts_with('-'))
            .map(String::as_str)
            .collect();
        let mut edit = matches!(
            words.get(1),
            Some(
                &"create"
                    | &"update"
                    | &"delete"
                    | &"move"
                    | &"resize"
                    | &"tag"
                    | &"set"
                    | &"arrange"
                    | &"layout"
            )
        );
        if words.get(1) == Some(&"layout") {
            edit = words.get(2) == Some(&"apply");
        }
        let mut values = vec![];
        let mut flags = vec![];
        if edit {
            values.extend(["--expect-epoch", "--expect-revision"]);
        }
        match words.as_slice() {
            ["note", "create", ..] => {
                values.extend(["--file", "--x", "--y", "--width", "--height", "--tag"]);
                flags.push("--stdin");
            }
            ["note", "update", ..] => {
                values.push("--file");
                flags.push("--stdin");
            }
            ["note", "move", ..] => values.extend(["--x", "--y"]),
            ["note", "resize", ..] => values.extend(["--width", "--height"]),
            ["workspace", "layout", "validate" | "apply", ..] => {
                values.push("--file");
                flags.push("--stdin");
            }
            _ => {}
        }
        let options = Options::parse(args, &values, &flags)?;
        let words: Vec<_> = options.words.iter().map(String::as_str).collect();
        let num = |flag: &str, default: Option<i32>| -> Result<i32, &'static str> {
            match options.values.get(flag) {
                Some(v) => v
                    .parse::<i32>()
                    .map_err(|_| "Use integer coordinates, dimensions and tags."),
                None => default.ok_or("Required coordinate or dimension is missing."),
            }
        };
        let query = match words.as_slice() {
            ["workspace", "inspect"] => Some(Query::Inspect),
            ["workspace", "folders"] => Some(Query::Folders),
            ["workspace", "layout", "export"] => Some(Query::Layout),
            ["workspace", "layout", "validate"] => Some(Query::ValidateLayout {layout:serde_json::from_str(&options.text(12000)?).map_err(|_|"Invalid layout JSON; use the data.layout object from export, with no unknown fields.")?}),
            ["note", "list"] => Some(Query::Notes),
            ["note", "inspect", id] if valid_id(id, 128) => Some(Query::Note { id: (*id).into() }),
            _ => None,
        };
        if let Some(query) = query {
            return Ok((Command::Workspace { query }, options));
        }
        let operation = match words.as_slice() {
            ["note", "create"] => Edit::NoteCreate {
                text: options.text(4096)?,
                x: num("--x", Some(80))?,
                y: num("--y", Some(140))?,
                width: num("--width", Some(260))?,
                height: num("--height", Some(200))?,
                tag: u8::try_from(num("--tag", Some(0))?)
                    .ok()
                    .filter(|t| *t <= 8)
                    .ok_or("Tag must be 0..8.")?,
            },
            ["note", "update", id] if valid_id(id, 128) => Edit::NoteUpdate {
                id: (*id).into(),
                text: options.text(4096)?,
            },
            ["note", "delete", id] if valid_id(id, 128) => Edit::NoteDelete { id: (*id).into() },
            ["note", "move", id] if valid_id(id, 128) => Edit::NoteMove {
                id: (*id).into(),
                x: num("--x", None)?,
                y: num("--y", None)?,
            },
            ["note", "resize", id] if valid_id(id, 128) => Edit::NoteResize {
                id: (*id).into(),
                width: num("--width", None)?,
                height: num("--height", None)?,
            },
            ["note", "tag", "set", id, tag] if valid_id(id, 128) => Edit::NoteTag {
                id: (*id).into(),
                tag: tag
                    .parse::<u8>()
                    .ok()
                    .filter(|t| *t <= 8)
                    .ok_or("Tag must be 0..8.")?,
            },
            ["workspace", "arrange"] => Edit::Arrange,
            ["workspace", "layout", "apply"] => Edit::Layout {layout:serde_json::from_str(&options.text(12000)?).map_err(|_|"Invalid layout JSON; use the data.layout object from export, with no unknown fields.")?},
            ["workspace", "set", path] => Edit::Folder {
                path: (*path).into(),
            },
            _ => {
                return Err("Unknown workspace/note command or unexpected arguments. Read --help.")
            }
        };
        let epoch = options.required("--expect-epoch")?;
        let revision = options.required("--expect-revision")?;
        if !valid_id(&epoch, 64) || !opaque(&revision) {
            return Err("Copy epoch and revision from workspace inspect or note inspect.");
        }
        Ok((
            Command::WorkspaceEdit {
                edit: operation,
                expect_epoch: epoch,
                expect_revision: revision,
            },
            options,
        ))
    };
    respond(args, || {
        let (command, options) = build()?;
        Ok(render_reply(send(command, &options), options.json))
    })
}
