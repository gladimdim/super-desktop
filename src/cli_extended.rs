//! Strict option parsing for structured local commands.
use crate::cli::{render_reply, Output};
use crate::control::{self, Command, InputData, Reply, Request};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Read};
use std::os::unix::fs::OpenOptionsExt;

pub(crate) struct Options {
    pub words: Vec<String>,
    pub values: BTreeMap<String, String>,
    pub flags: BTreeSet<String>,
    pub json: bool,
}
impl Options {
    pub fn parse(args: &[String], values: &[&str], flags: &[&str]) -> Result<Self, &'static str> {
        let mut parsed = Self {
            words: vec![],
            values: BTreeMap::new(),
            flags: BTreeSet::new(),
            json: false,
        };
        let mut index = 0;
        while index < args.len() {
            let word = &args[index];
            if !word.starts_with('-') {
                parsed.words.push(word.clone());
                index += 1;
                continue;
            }
            let (key, inline) = word
                .split_once('=')
                .map_or((word.as_str(), None), |(a, b)| (a, Some(b)));
            if flags.contains(&key) {
                if inline.is_some() || !parsed.flags.insert(key.into()) {
                    return Err("Do not repeat flags or give flag values.");
                }
            } else if values.contains(&key)
                || matches!(key, "--format" | "--target" | "--request-id")
            {
                let value = match inline {
                    Some(value) => value,
                    None => {
                        index += 1;
                        args.get(index).ok_or("Missing option value.")?
                    }
                };
                if value.is_empty() || parsed.values.insert(key.into(), value.into()).is_some() {
                    return Err("Missing or repeated option value.");
                }
            } else {
                return Err("Unknown option. Read this command's --help.");
            }
            index += 1;
        }
        if let Some(format) = parsed.values.get("--format") {
            match format.as_str() {
                "json" => parsed.json = true,
                "text" => {}
                _ => return Err("Use --format text or json."),
            }
        }
        Ok(parsed)
    }
    pub fn required(&self, key: &str) -> Result<String, &'static str> {
        self.values
            .get(key)
            .cloned()
            .ok_or("A required option is missing. Read this command's --help.")
    }
    pub fn guard(&self) -> Result<(String, String, String), &'static str> {
        let epoch = self.required("--expect-epoch")?;
        let revision = self.required("--expect-revision")?;
        let pane = self.required("--expect-pane-identity")?;
        if !valid_id(&epoch, 64) || !opaque(&revision) || !opaque(&pane) {
            return Err("Copy epoch/revision from geometry and paneIdentity from runtime.");
        }
        Ok((epoch, revision, pane))
    }
    pub fn text(&self, max: usize) -> Result<String, &'static str> {
        let stdin = self.flags.contains("--stdin");
        let file = self.values.get("--file");
        if stdin == file.is_some() {
            return Err("Choose exactly one of --stdin or --file PATH.");
        }
        let mut bytes = vec![];
        if stdin {
            if std::io::stdin().is_terminal() {
                return Err("--stdin requires piped or redirected input.");
            }
            std::io::stdin()
                .take(max as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "Cannot read stdin.")?;
        } else {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(file.unwrap())
                .map_err(|_| "Cannot open input file.")?;
            if !file
                .metadata()
                .map_err(|_| "Cannot inspect input file.")?
                .is_file()
            {
                return Err("Input must be a regular file.");
            }
            file.take(max as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "Cannot read input file.")?;
        }
        if bytes.len() > max {
            return Err("Input exceeds this command's byte limit; no prefix was sent.");
        }
        String::from_utf8(bytes).map_err(|_| "Input must be UTF-8.")
    }
}
pub(crate) fn valid_id(id: &str, max: usize) -> bool {
    !id.is_empty()
        && id.len() <= max
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
pub(crate) fn opaque(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(crate) fn json_requested(args: &[String]) -> bool {
    args.iter().any(|a| a == "--format=json")
        || args
            .windows(2)
            .any(|a| a[0] == "--format" && a[1] == "json")
}
pub(crate) fn send(command: Command, options: &Options, method: &str) -> Reply {
    if options.values.get("--target").is_some_and(|t| t != "local") {
        return Reply::failure(
            "",
            "unsupported_target",
            "This command supports local only; no fallback was attempted.",
        );
    }
    let mut request = match control::new_request(command) {
        Ok(r) => r,
        Err(_) => return Reply::failure("", "unavailable", "OS randomness is unavailable."),
    };
    if request.command.is_mutation() {
        let Some(id) = options
            .values
            .get("--request-id")
            .filter(|id| valid_id(id, 64))
        else {
            return Reply::failure(
                "",
                "invalid_arguments",
                "Mutation requires a unique --request-id of 1-64 ASCII letters/digits/_/-.",
            );
        };
        request.request_id = id.clone();
    } else if options.values.contains_key("--request-id") {
        return Reply::failure(
            "",
            "invalid_arguments",
            "--request-id is only for mutations.",
        );
    }
    send_request(&request, method)
}
pub(crate) fn send_request(request: &Request, method: &str) -> Reply {
    let probe = match control::new_request(Command::Capabilities {}) {
        Ok(r) => r,
        Err(_) => {
            return Reply::failure(
                &request.request_id,
                "unavailable",
                "OS randomness is unavailable.",
            )
        }
    };
    let support = control::request_at(&control::runtime_dir(), &probe);
    if !support.ok {
        let mut r = support;
        r.request_id = request.request_id.clone();
        return r;
    }
    if !support
        .data
        .as_ref()
        .and_then(|d| d["methods"].as_array())
        .is_some_and(|methods| methods.iter().any(|m| m == method))
    {
        return Reply::failure(
            &request.request_id,
            "unsupported_command",
            "This daemon does not support this operation.",
        );
    }
    control::request_at(&control::runtime_dir(), request)
}

pub(crate) fn run(args: &[String]) -> Option<Output> {
    let action = args.get(1)?.as_str();
    if args.first()?.as_str() != "terminal"
        || !matches!(action, "send" | "keys" | "interrupt" | "prompt")
    {
        return None;
    }
    let json = json_requested(args);
    let build = || -> Result<(Command, Options), &'static str> {
        let mut values = vec![
            "--expect-epoch",
            "--expect-revision",
            "--expect-pane-identity",
        ];
        let mut flags = vec![];
        if matches!(action, "send" | "prompt") {
            values.push("--file");
            flags.push("--stdin");
        }
        if action == "send" {
            flags.push("--enter");
        }
        let options = Options::parse(args, &values, &flags)?;
        if options.words.len() < 3 || !valid_id(&options.words[2], 128) {
            return Err("Use an exact terminal card ID.");
        }
        let (expect_epoch, expect_revision, expect_pane_identity) = options.guard()?;
        if !options
            .values
            .get("--request-id")
            .is_some_and(|id| valid_id(id, 64))
        {
            return Err("Input requires a unique --request-id.");
        }
        let input = if action == "keys" {
            InputData::Keys {
                keys: options.words[3..].to_vec(),
            }
        } else {
            if options.words.len() != 3 {
                return Err("Unexpected positional arguments.");
            }
            match action {
                "interrupt" => InputData::Keys {
                    keys: vec!["Ctrl-C".into()],
                },
                "prompt" => InputData::Prompt {
                    text: options.text(control::MAX_INPUT)?,
                },
                _ => InputData::Send {
                    text: options.text(control::MAX_INPUT)?,
                    enter: options.flags.contains("--enter"),
                },
            }
        };
        input.validate()?;
        Ok((
            Command::Input {
                id: options.words[2].clone(),
                input,
                expect_epoch,
                expect_revision,
                expect_pane_identity,
            },
            options,
        ))
    };
    Some(match build() {
        Ok((command, options)) => render_reply(send(command, &options, "terminal.input"), json),
        Err(message) => render_reply(Reply::failure("", "invalid_arguments", message), json),
    })
}
