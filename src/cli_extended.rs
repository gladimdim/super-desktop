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

pub(crate) fn seconds(value: &str) -> Result<std::time::Duration, &'static str> {
    let (number, multiplier) = if let Some(v) = value.strip_suffix('m') {
        (v, 60)
    } else {
        (value.strip_suffix('s').unwrap_or(value), 1)
    };
    let n = number
        .parse::<u64>()
        .ok()
        .filter(|n| (1..=3600).contains(n))
        .ok_or("Use 1-3600 seconds or a duration such as 5m.")?;
    let n = n
        .checked_mul(multiplier)
        .filter(|n| *n <= 3600)
        .ok_or("Duration must not exceed one hour.")?;
    Ok(std::time::Duration::from_secs(n))
}

fn observe_args(args: &[String], wait: bool) -> Result<Options, &'static str> {
    let options = Options::parse(
        args,
        if wait {
            &["--until", "--after", "--timeout", "--expect-pane-identity"]
        } else {
            &[]
        },
        &[],
    )?;
    if options.words.len() != 3 || !valid_id(&options.words[2], 128) {
        return Err("Use an exact terminal card ID.");
    }
    if options.values.contains_key("--request-id") {
        return Err("Observation does not accept --request-id.");
    }
    Ok(options)
}

fn wait_terminal(options: &Options) -> Reply {
    let fail = |code, message| Reply::failure("", code, message);
    let until = match options.required("--until") {
        Ok(v)
            if matches!(
                v.as_str(),
                "completed" | "exited" | "working" | "idle" | "error" | "waiting"
            ) =>
        {
            v
        }
        _ => {
            return fail(
                "invalid_arguments",
                "Use --until completed|exited|working|idle|error|waiting.",
            )
        }
    };
    let expected = match options.required("--expect-pane-identity") {
        Ok(v) if opaque(&v) => v,
        _ => {
            return fail(
                "invalid_arguments",
                "Wait requires --expect-pane-identity from terminal status/runtime.",
            )
        }
    };
    let after = options.values.get("--after");
    if until == "completed" {
        if !after.is_some_and(|v| v == "none" || opaque(v)) {
            return fail("invalid_arguments","Completion wait needs --after with the last observed completionId, or none for an observed null baseline.");
        }
    } else if after.is_some() {
        return fail("invalid_arguments", "--after is only for completed waits.");
    }
    let timeout = match options
        .values
        .get("--timeout")
        .map_or(Ok(std::time::Duration::from_secs(30)), |v| seconds(v))
    {
        Ok(value) => value,
        Err(message) => return fail("invalid_arguments", message),
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let reply = send(
            Command::Lifecycle {
                id: options.words[2].clone(),
            },
            options,
            "terminal.status",
        );
        if !reply.ok {
            return reply;
        }
        let Some(data) = reply.data.as_ref() else {
            return fail("invalid_response", "Missing terminal status.");
        };
        if data["paneIdentity"] != expected {
            return fail("conflict", "The pane was replaced while waiting.");
        }
        let state = data["lifecycle"].as_str().unwrap_or("unknown");
        if until == "completed" && data["completion"]["supported"] != true {
            return fail(
                "unsupported_completion",
                "This pane has no attributable native completion support.",
            );
        }
        let completed = data["completion"]["state"] == "completed"
            && data["completion"]["completionId"]
                .as_str()
                .is_some_and(|id| Some(id) != after.map(String::as_str));
        if (until == "completed" && completed) || (until != "completed" && state == until) {
            return reply;
        }
        if state == "exited" {
            return fail(
                "terminal_not_running",
                "The pane exited before the requested condition.",
            );
        }
        if until != "exited" && until != "completed" && data["nativeMetadataObserved"] != true {
            return fail(
                "unsupported_completion",
                "This condition needs native lifecycle metadata; silence is not evidence.",
            );
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return fail(
                "timeout",
                "The requested terminal condition was not observed before the deadline.",
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(250).min(deadline - now));
    }
}

pub(crate) fn observe(args: &[String]) -> Option<Output> {
    if args.first()?.as_str() != "terminal" || !matches!(args.get(1)?.as_str(), "status" | "wait") {
        return None;
    }
    let waiting = args[1] == "wait";
    let json = json_requested(args);
    Some(match observe_args(args, waiting) {
        Err(message) => render_reply(Reply::failure("", "invalid_arguments", message), json),
        Ok(options) => render_reply(
            if waiting {
                wait_terminal(&options)
            } else {
                send(
                    Command::Lifecycle {
                        id: options.words[2].clone(),
                    },
                    &options,
                    "terminal.status",
                )
            },
            json,
        ),
    })
}

/// A bounded stream of replacement screen snapshots. Polling cannot promise
/// every intervening output byte, so this never labels snapshots as deltas.
pub(crate) fn stream(args: &[String]) -> Option<i32> {
    use std::io::Write;
    if args.first()?.as_str() != "terminal" || args.get(1)?.as_str() != "follow" {
        return None;
    }
    let mut parse_args = args.to_vec();
    for i in 0..parse_args.len() {
        if parse_args[i] == "--format=jsonl" {
            parse_args[i] = "--format=json".into();
        } else if parse_args[i] == "jsonl" && i > 0 && parse_args[i - 1] == "--format" {
            parse_args[i] = "json".into();
        }
    }
    let emit = |value: &serde_json::Value| -> Result<usize, ()> {
        let line = serde_json::to_string(value)
            .map_err(|_| ())?
            .chars()
            .map(|c| {
                if c.is_control() {
                    format!("\\u{:04x}", c as u32)
                } else {
                    c.to_string()
                }
            })
            .collect::<String>()
            + "\n";
        let mut out = std::io::stdout().lock();
        out.write_all(line.as_bytes())
            .and_then(|_| out.flush())
            .map_err(|_| ())?;
        Ok(line.len())
    };
    let fail = |code, message| {
        let reply = Reply::failure("", code, message);
        if emit(&serde_json::to_value(&reply).unwrap()).is_err() {
            8
        } else {
            reply.exit_code()
        }
    };
    let options = match Options::parse(
        &parse_args,
        &["--seconds", "--interval-ms", "--expect-pane-identity"],
        &[],
    ) {
        Ok(options) => options,
        Err(message) => return Some(fail("invalid_arguments", message)),
    };
    if options.words.len() != 3
        || !valid_id(&options.words[2], 128)
        || options.values.contains_key("--request-id")
    {
        return Some(fail(
            "invalid_arguments",
            "Follow requires an exact card ID and no request ID.",
        ));
    }
    if args
        .iter()
        .any(|a| a.starts_with("--format=") && a != "--format=jsonl")
        || args
            .windows(2)
            .any(|a| a[0] == "--format" && a[1] != "jsonl")
    {
        return Some(fail(
            "invalid_arguments",
            "Follow emits JSONL screen snapshots.",
        ));
    }
    let duration = match options
        .values
        .get("--seconds")
        .map_or(Ok(std::time::Duration::from_secs(10)), |v| seconds(v))
    {
        Ok(v) => v,
        Err(message) => return Some(fail("invalid_arguments", message)),
    };
    let interval = match options
        .values
        .get("--interval-ms")
        .map_or(Some(500), |v| v.parse::<u64>().ok())
        .filter(|v| (200..=10000).contains(v))
    {
        Some(v) => std::time::Duration::from_millis(v),
        None => return Some(fail("invalid_arguments", "Use --interval-ms 200-10000.")),
    };
    let mut identity = options.values.get("--expect-pane-identity").cloned();
    if identity.as_ref().is_some_and(|v| !opaque(v)) {
        return Some(fail(
            "invalid_arguments",
            "Copy paneIdentity from terminal runtime.",
        ));
    }
    let stream_id = match control::new_request(Command::Status {}) {
        Ok(r) => r.request_id,
        Err(_) => return Some(fail("unavailable", "OS randomness unavailable.")),
    };
    let deadline = std::time::Instant::now() + duration;
    let mut previous = None;
    let mut sequence = 0;
    let mut total = 0;
    loop {
        let reply = send(
            Command::Capture {
                id: options.words[2].clone(),
                history: false,
                lines: None,
            },
            &options,
            "terminal.capture",
        );
        if !reply.ok {
            return Some(if emit(&serde_json::to_value(&reply).unwrap()).is_ok() {
                reply.exit_code()
            } else {
                8
            });
        }
        let Some(data) = reply.data else {
            return Some(fail("invalid_response", "Missing capture snapshot."));
        };
        let Some(pane) = data["runtime"]["paneIdentity"].as_str() else {
            return Some(fail("invalid_response", "Missing capture pane identity."));
        };
        if identity.as_ref().is_some_and(|id| id != pane) {
            return Some(fail("conflict", "The pane was replaced; the stream ended."));
        }
        identity = Some(pane.into());
        let fingerprint = serde_json::json!([
            data["text"],
            data["runtime"]["columns"],
            data["runtime"]["rows"],
            data["truncated"]
        ]);
        if previous.as_ref() != Some(&fingerprint) {
            sequence += 1;
            let event = serde_json::json!({"schemaVersion":1,"ok":true,"target":"local","type":"snapshot","streamId":stream_id,
                "sequence":sequence,"mayHaveGaps":true,"data":data});
            match emit(&event) {
                Ok(n) => total += n,
                Err(()) => return Some(8),
            }
            previous = Some(fingerprint);
        }
        if total >= 4 * 1024 * 1024 || sequence >= 4096 || std::time::Instant::now() >= deadline {
            return Some(if emit(&serde_json::json!({"schemaVersion":1,"ok":true,"type":"end","streamId":stream_id,"sequence":sequence+1,
                "reason":if total>=4*1024*1024 || sequence>=4096 {"limit"} else {"duration"}})).is_ok() {0} else {8});
        }
        std::thread::sleep(
            interval.min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
    }
}
