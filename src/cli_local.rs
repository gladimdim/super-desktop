//! The first structured local commands: capabilities, app status, terminal
//! list/inspect/geometry/runtime/capture/move/resize/close and mode changes,
//! harness list/inspect, request inspect, and launch forms that place options
//! between command words. Their grammar predates `Options::parse` and keeps
//! its own messages: empty --cwd/--request-id values are accepted, --format,
//! --target and --lines are checked where they appear, and a repeated or
//! valued flag is an unknown option.
use crate::cli::{render_reply, Output};
use crate::cli_extended::{json_requested, opaque, send_request, valid_id, Options};
use crate::control::{self, Command, ModeAction, Reply};

/// Guard and geometry options, in the order the guarded commands list them.
const GUARDS: [&str; 7] = [
    "--x",
    "--y",
    "--width",
    "--height",
    "--expect-epoch",
    "--expect-revision",
    "--expect-pane-identity",
];
const FLAGS: [&str; 4] = ["--clamp", "--all", "--allow-unsafe-harness", "--allow-download"];

type Failure = (&'static str, &'static str);

fn invalid(message: &'static str) -> Failure {
    ("invalid_arguments", message)
}

/// An inline `--key=value`, else the next argument.
fn value<'a>(args: &'a [String], index: &mut usize, inline: Option<&'a str>) -> Option<&'a str> {
    inline.or_else(|| {
        *index += 1;
        args.get(*index).map(String::as_str)
    })
}

fn parse(args: &[String]) -> Result<Options, Failure> {
    let mut o = Options {
        words: vec![],
        values: Default::default(),
        flags: Default::default(),
        json: json_requested(args),
        attachments: vec![],
    };
    let mut index = 0;
    while index < args.len() {
        let word = args[index].as_str();
        let (key, inline) = word.split_once('=').map_or((word, None), |(k, v)| (k, Some(v)));
        match key {
            _ if GUARDS.contains(&key) => {
                let value = value(args, &mut index, inline)
                    .filter(|v| !v.is_empty())
                    .ok_or(invalid("Missing geometry option value."))?;
                if o.values.insert(key.into(), value.into()).is_some() {
                    return Err(invalid("Do not repeat geometry options."));
                }
            }
            "--lines" => {
                let value = value(args, &mut index, inline)
                    .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                    .filter(|v| v.parse::<u32>().is_ok_and(|n| (1..=2000).contains(&n)))
                    .ok_or(invalid("--lines requires an integer from 1 to 2000."))?;
                if o.values.insert(key.into(), value.into()).is_some() {
                    return Err(invalid("Use --lines once."));
                }
            }
            "--cwd" | "--request-id" => {
                let value = value(args, &mut index, inline).ok_or(invalid("Missing option value."))?;
                if o.values.insert(key.into(), value.into()).is_some() {
                    return Err(invalid("Do not repeat --cwd or --request-id."));
                }
            }
            "--format" => {
                let value = value(args, &mut index, inline).ok_or(invalid("Missing option value."))?;
                if !matches!(value, "text" | "json") || o.values.insert(key.into(), value.into()).is_some() {
                    return Err(invalid("Use --format text or --format json once."));
                }
            }
            "--target" => {
                let value = value(args, &mut index, inline).ok_or(invalid("Missing option value."))?;
                if o.values.insert(key.into(), value.into()).is_some() {
                    return Err(invalid("Use --target once."));
                }
                if value != "local" {
                    return Err(("unsupported_target", "This command supports --target local only; no local fallback was attempted."));
                }
            }
            "--screen" | "--history" if inline.is_none() => {
                if o.flags.contains("--screen") || o.flags.contains("--history") {
                    return Err(invalid("Choose --screen or --history once."));
                }
                o.flags.insert(key.into());
            }
            _ if inline.is_none() && FLAGS.contains(&key) && o.flags.insert(key.into()) => {}
            _ if word.starts_with('-') => return Err(invalid("Unknown option. Use this command's --help.")),
            _ => o.words.push(word.into()),
        }
        index += 1;
    }
    Ok(o)
}

/// Checks the options against the command, in their original order.
fn command(o: &Options) -> Result<Command, Failure> {
    let words: Vec<&str> = o.words.iter().map(String::as_str).collect();
    let value = |key: &str| o.values.get(key).map(String::as_str);
    let flag = |key: &str| o.flags.contains(key);
    let launching = matches!(words[..], ["harness", "launch", _] | ["terminal", "create"]);
    let moving = matches!(words[..], ["terminal", "move", _]);
    let resizing = matches!(words[..], ["terminal", "resize", _]);
    let closing = matches!(words[..], ["terminal", "close", _]);
    let changing_mode = matches!(words[..], ["terminal", "minimize" | "restore" | "expand" | "collapse", _]);
    let guarded = moving || resizing || closing || changing_mode;
    let given: Vec<&str> = GUARDS.into_iter().filter(|key| o.values.contains_key(*key)).collect();
    if (!guarded && !given.is_empty()) || (!(moving || resizing) && flag("--clamp")) {
        return Err(invalid("Guard options are for move/resize/close/mode changes; --clamp is only for move/resize."));
    }
    let request_id = value("--request-id");
    if !launching && !guarded && request_id.is_some() {
        return Err(invalid("--request-id is only for mutation commands."));
    }
    if (launching || guarded) && !request_id.is_some_and(|id| valid_id(id, 64)) {
        return Err(invalid("Mutation requires --request-id with 1-64 ASCII letters, digits, '_' or '-'."));
    }
    if guarded {
        let required: &[&str] = if changing_mode {
            &["--expect-epoch", "--expect-revision"]
        } else if closing {
            &["--expect-epoch", "--expect-revision", "--expect-pane-identity"]
        } else if moving {
            &["--x", "--y", "--expect-epoch", "--expect-revision"]
        } else {
            &["--width", "--height", "--expect-epoch", "--expect-revision"]
        };
        if given != required {
            return Err(invalid("Move/resize require both coordinates/dimensions. All guarded operations require epoch/revision from terminal geometry; close also requires pane identity from terminal runtime."));
        }
        if !valid_id(&o.values["--expect-epoch"], 64) || !opaque(&o.values["--expect-revision"]) {
            return Err(invalid("Use the epoch and opaque revision returned by terminal geometry."));
        }
        if closing && !opaque(&o.values["--expect-pane-identity"]) {
            return Err(invalid("Copy paneIdentity from terminal runtime."));
        }
        for key in if moving || resizing { &required[..2] } else { &[] } {
            let text = &o.values[*key];
            let Some(number) = text
                .parse::<i32>()
                .ok()
                .filter(|n| n.abs_diff(0) <= 32768 && (moving || *n > 0))
            else {
                return Err(invalid("Coordinates must be within -32768..32768; dimensions within 1..32768."));
            };
            if number.to_string() != *text {
                return Err(invalid("Use canonical decimal integers for geometry."));
            }
        }
    }
    let lines = value("--lines").map(|n| n.parse().unwrap());
    if (!matches!(words[..], ["terminal", "capture", _]) && (flag("--screen") || flag("--history") || lines.is_some()))
        || (lines.is_some() && !flag("--history"))
    {
        return Err(invalid("Screen/history options are only for terminal capture; --lines requires --history."));
    }
    let cwd = value("--cwd");
    let (allow_unsafe_harness, allow_download) = (flag("--allow-unsafe-harness"), flag("--allow-download"));
    if !launching && (cwd.is_some() || allow_unsafe_harness || allow_download) {
        return Err(invalid("Launch options are only accepted by launch/create commands."));
    }
    if launching && !cwd.is_some_and(|path| std::path::Path::new(path).is_absolute() && path.len() <= 4096 && !path.contains('\0')) {
        return Err(invalid("Launch requires --cwd with an absolute directory path of at most 4096 bytes."));
    }
    let guard = |key: &str| o.values[key].clone();
    let coordinate = |key: &str| o.values[key].parse().unwrap();
    Ok(match words[..] {
        ["harness", "list"] => Command::Harnesses { all: flag("--all") },
        _ if flag("--all") => return Err(invalid("Invalid command or arguments. Use --help for accepted syntax.")),
        ["terminal", action @ ("minimize" | "restore" | "expand" | "collapse"), id] if valid_id(id, 128) => Command::Mode {
            id: id.into(),
            action: match action {
                "minimize" => ModeAction::Minimize,
                "restore" => ModeAction::Restore,
                "expand" => ModeAction::Expand,
                _ => ModeAction::Collapse,
            },
            expect_epoch: guard("--expect-epoch"),
            expect_revision: guard("--expect-revision"),
        },
        ["terminal", "close", id] if valid_id(id, 128) => Command::Close {
            id: id.into(),
            expect_epoch: guard("--expect-epoch"),
            expect_revision: guard("--expect-revision"),
            expect_pane_identity: guard("--expect-pane-identity"),
        },
        ["terminal", "geometry", id] if valid_id(id, 128) => Command::Geometry { id: id.into() },
        ["terminal", "move", id] if valid_id(id, 128) => Command::Move {
            id: id.into(),
            x: coordinate("--x"),
            y: coordinate("--y"),
            clamp: flag("--clamp"),
            expect_epoch: guard("--expect-epoch"),
            expect_revision: guard("--expect-revision"),
        },
        ["terminal", "resize", id] if valid_id(id, 128) => Command::Resize {
            id: id.into(),
            width: o.values["--width"].parse().unwrap(),
            height: o.values["--height"].parse().unwrap(),
            clamp: flag("--clamp"),
            expect_epoch: guard("--expect-epoch"),
            expect_revision: guard("--expect-revision"),
        },
        ["terminal", "capture", id] if valid_id(id, 128) => Command::Capture {
            id: id.into(),
            history: flag("--history"),
            lines,
        },
        ["terminal", "runtime", id] if valid_id(id, 128) => Command::Runtime { id: id.into() },
        ["harness", "launch", id] if valid_id(id, 128) => Command::Launch {
            arguments: None,
            harness: id.into(),
            cwd: guard("--cwd"),
            allow_unsafe_harness,
            allow_download,
        },
        ["terminal", "create"] if !allow_download => Command::Launch {
            arguments: None,
            harness: "shell".into(),
            cwd: guard("--cwd"),
            allow_unsafe_harness,
            allow_download: false,
        },
        ["request", "inspect", id] if valid_id(id, 64) => Command::InspectRequest { id: id.into() },
        ["capabilities"] => Command::Capabilities {},
        ["app", "status"] => Command::Status {},
        ["terminal", "list"] => Command::Terminals {},
        ["terminal", "inspect", id] if valid_id(id, 128) => Command::Terminal { id: id.into() },
        ["harness", "inspect", id] if valid_id(id, 128) => Command::Harness { id: id.into() },
        _ => return Err(invalid("Invalid command or arguments. Use --help for accepted syntax.")),
    })
}

pub(crate) fn run(args: &[String]) -> Output {
    let json = json_requested(args);
    let (command, o) = match parse(args).and_then(|o| command(&o).map(|command| (command, o))) {
        Ok(parsed) => parsed,
        Err((code, message)) => return render_reply(Reply::failure("", code, message), json),
    };
    let Ok(mut request) = control::new_request(command) else {
        return render_reply(Reply::failure("", "unavailable", "OS randomness is unavailable."), json);
    };
    if let Some(id) = o.values.get("--request-id") {
        request.request_id = id.clone();
    }
    // The first daemons' methods are sent directly; newer ones only after
    // the daemon advertises them.
    let first = matches!(
        request.command,
        Command::Capabilities {}
            | Command::Status {}
            | Command::Terminals {}
            | Command::Terminal { .. }
            | Command::Harnesses { .. }
            | Command::Harness { .. }
    );
    render_reply(
        if first {
            control::request_at(&control::runtime_dir(), &request)
        } else {
            send_request(&request)
        },
        json,
    )
}
