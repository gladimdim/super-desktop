//! CLI frontend for typed settings and launcher preferences.
use crate::cli::{render_reply, Output};
use crate::cli_extended::{json_requested, opaque, send, valid_id, Options};
use crate::control::{Command, PreferencesEdit as Edit, PreferencesQuery as Query, Reply};

pub(crate) fn run(args: &[String]) -> Option<Output> {
    let words: Vec<_> = args
        .iter()
        .take_while(|a| !a.starts_with('-'))
        .map(String::as_str)
        .collect();
    let selected = matches!(
        words.as_slice(),
        ["settings", ..]
            | ["theme", "inspect" | "reload", ..]
            | ["usage", "inspect", ..]
            | ["harness", "args" | "custom" | "visibility" | "rescan", ..]
    );
    if !selected {
        return None;
    }
    if matches!(words.as_slice(),["settings","shortcut",..]) {
        let build=||->Result<Output,&'static str>{
            let o=Options::parse(args,&["--combo","--preview","--expect-epoch","--expect-revision"],&[])?;
            let apply=match o.words.iter().map(String::as_str).collect::<Vec<_>>().as_slice(){["settings","shortcut","preview"]=>false,["settings","shortcut","apply"]=>true,_=>return Err("Use settings shortcut preview or apply.")};
            let combo=o.required("--combo")?;
            if !apply&&o.values.keys().any(|k|matches!(k.as_str(),"--preview"|"--request-id"|"--expect-epoch"|"--expect-revision")){return Err("Preview only takes --combo, --format and --target.");}
            let (preview,epoch,revision)=if apply {let p=o.required("--preview")?;let e=o.required("--expect-epoch")?;let r=o.required("--expect-revision")?;if !opaque(&p)||!valid_id(&e,64)||!opaque(&r){return Err("Copy preview, epoch and revision from the preview response.");}(Some(p),Some(e),Some(r))}else{(None,None,None)};
            Ok(render_reply(send(Command::Shortcut {combo,preview,expect_epoch:epoch,expect_revision:revision},&o,"settings.shortcut"),o.json))
        };
        return Some(build().unwrap_or_else(|m|render_reply(Reply::failure("","invalid_arguments",m),json_requested(args))));
    }
    let build = || -> Result<(Command, Options), &'static str> {
        let read = matches!(
            words.as_slice(),
            ["settings", "list" | "get", ..]
                | ["theme", "inspect", ..]
                | ["usage", "inspect", ..]
                | ["harness", "args" | "custom", "get", ..]
        );
        let input = matches!(
            words.as_slice(),
            ["harness", "args" | "visibility", "set", ..]
                | ["harness", "custom", "add" | "update", ..]
        );
        let mut values = vec![];
        let mut flags = vec![];
        if !read {
            values.extend(["--expect-epoch", "--expect-revision"]);
        }
        if input {
            values.push("--file");
            flags.push("--stdin");
        }
        if matches!(words.as_slice(), ["settings", "set", ..]) {
            values.push("--value");
        }
        let options = Options::parse(args, &values, &flags)?;
        let words: Vec<_> = options.words.iter().map(String::as_str).collect();
        let query = match words.as_slice() {
            ["settings", "list"] => Some(Query::Settings { key: None }),
            ["settings", "get", key] => Some(Query::Settings {
                key: Some((*key).into()),
            }),
            ["harness", "args", "get", id] => Some(Query::HarnessArgs { id: (*id).into() }),
            ["harness", "custom", "get", id] => Some(Query::Custom { id: (*id).into() }),
            ["theme", "inspect"] => Some(Query::Theme),
            ["usage", "inspect"] => Some(Query::Usage),
            _ => None,
        };
        if let Some(query) = query {
            return Ok((Command::Preferences { query }, options));
        }
        let edit=match words.as_slice(){
            ["settings","set",key]=>Edit::Setting {key:(*key).into(),value:Some(serde_json::from_str(&options.required("--value")?).map_err(|_|"--value must be a JSON value; quote strings, for example '\"small\"'.")?)},
            ["settings","reset",key]=>Edit::Setting {key:(*key).into(),value:None},
            ["harness","args","set",id]=>Edit::HarnessArgs {id:(*id).into(),arguments:Some(serde_json::from_str(&options.text(12000)?).map_err(|_|"Input must be a JSON array of argument strings.")?)},
            ["harness","args","reset",id]=>Edit::HarnessArgs {id:(*id).into(),arguments:None},
            ["harness","custom",action @ ("add"|"update")]=>Edit::CustomPut {launcher:serde_json::from_str(&options.text(12000)?).map_err(|_|"Input must be a launcher object with id, name, icon, executable and arguments; no unknown fields.")?,create:*action=="add"},
            ["harness","custom","remove",id]=>Edit::CustomRemove {id:(*id).into()},
            ["harness","visibility","set"]=>Edit::Visibility {keys:Some(serde_json::from_str(&options.text(12000)?).map_err(|_|"Input must be a JSON array of harness IDs.")?)},
            ["harness","visibility","reset"]=>Edit::Visibility {keys:None},
            ["harness","rescan"]=>Edit::Rescan,
            ["theme","reload"]=>Edit::ThemeReload,
            _=>return Err("Unknown settings/harness command or extra arguments. Read --help."),
        };
        let epoch = options.required("--expect-epoch")?;
        let revision = options.required("--expect-revision")?;
        if !valid_id(&epoch, 64) || !opaque(&revision) {
            return Err("Copy epoch/revision from settings list or workspace inspect.");
        }
        Ok((
            Command::PreferencesEdit {
                edit,
                expect_epoch: epoch,
                expect_revision: revision,
            },
            options,
        ))
    };
    Some(match build() {
        Ok((command, options)) => {
            let method = if command.is_mutation() {
                "settings.edit"
            } else {
                "settings.read"
            };
            render_reply(send(command, &options, method), options.json)
        }
        Err(message) => render_reply(
            Reply::failure("", "invalid_arguments", message),
            json_requested(args),
        ),
    })
}
