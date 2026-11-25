//! MCP access to client-side workflows which have no single daemon method.
use crate::cli::COMMANDS;
use crate::cli_extended::valid_id;
use crate::control::Reply;
use crate::mcp_settings::Config;
use serde_json::{json, Value};
use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) const NAMES: &[&str] = &[
    "app_action",
    "plugin_command",
    "follow_terminal",
    "attach_terminal",
    "workspace_events",
    "peer_events",
    "peer_attach",
    "desktop_doctor",
    "audit_export",
    "shortcut_preview",
    "shortcut_apply",
];

pub(crate) fn allowed(name: &str, c: &Config) -> bool {
    match name {
        "plugin_command" => c.plugins,
        "follow_terminal" | "attach_terminal" => c.read_output,
        "workspace_events" => c.layout && c.read_notes,
        "peer_events" | "peer_attach" => c.administration && c.read_output,
        "app_action" | "desktop_doctor" | "audit_export" | "shortcut_preview"
        | "shortcut_apply" => c.administration,
        _ => false,
    }
}

fn commands(name: &str) -> Vec<&'static str> {
    match name {
        "plugin_command" => COMMANDS
            .iter()
            .filter_map(|s| s.name.starts_with("plugin ").then_some(s.name))
            .collect(),
        "app_action" => vec![
            "app start",
            "app stop",
            "app restart",
            "app show",
            "app hide",
            "app toggle",
        ],
        "follow_terminal" => vec!["terminal follow"],
        "attach_terminal" => vec!["terminal attach"],
        "workspace_events" => vec!["events"],
        "peer_events" => vec!["peer-events"],
        "peer_attach" => vec!["peer-attach"],
        "desktop_doctor" => vec!["doctor"],
        "audit_export" => vec!["audit export"],
        "shortcut_preview" => vec!["settings shortcut preview"],
        "shortcut_apply" => vec!["settings shortcut apply"],
        _ => vec![],
    }
}

fn requires_id(name: &str) -> bool {
    matches!(
        name,
        "app_action" | "attach_terminal" | "shortcut_apply" | "plugin_command" | "audit_export"
    )
}

pub(crate) fn tool(name: &str) -> Value {
    let names = commands(name);
    let usage: Vec<_> = names
        .iter()
        .filter_map(|n| COMMANDS.iter().find(|s| s.name == *n))
        .map(|s| s.usage)
        .collect();
    let description=format!("Run the selected SUPER DESKTOP workflow. Pass positional arguments and options as separate strings in arguments. Output is bounded to 128 KiB. Syntax: {}",usage.join("\n"));
    let mut props = json!({"command":{"type":"string","enum":names},"arguments":{"type":"array","maxItems":64,"items":{"type":"string","maxLength":4096}}});
    let mut required = vec!["command"];
    if requires_id(name) {
        props["requestId"] = json!({"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"});
        required.push("requestId");
    }
    let writes = requires_id(name);
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":props,"required":required,"additionalProperties":false},
        "outputSchema":schemars::schema_for!(Reply),"annotations":{"readOnlyHint":!writes,"destructiveHint":writes,"openWorldHint":true,"idempotentHint":!writes}})
}

struct Invocation {
    words: Vec<String>,
    id: String,
}
fn parse(name: &str, args: &Value) -> Result<Invocation, &'static str> {
    let map = args.as_object().ok_or("Expected arguments object")?;
    if map
        .keys()
        .any(|k| !matches!(k.as_str(), "command" | "arguments" | "requestId"))
    {
        return Err("Unknown argument");
    }
    let cmd = args["command"]
        .as_str()
        .filter(|s| commands(name).contains(s))
        .ok_or("Choose a command from this tool's schema")?;
    let id = if requires_id(name) {
        args["requestId"]
            .as_str()
            .filter(|id| valid_id(id, 64))
            .ok_or("Supply a unique requestId")?
    } else if map.contains_key("requestId") {
        return Err("Unexpected requestId");
    } else {
        "mcp"
    };
    let mut words: Vec<_> = cmd.split(' ').map(str::to_owned).collect();
    if let Some(values) = map.get("arguments") {
        let array = values
            .as_array()
            .filter(|a| a.len() <= 64)
            .ok_or("Use at most 64 arguments")?;
        for v in array {
            let word = v
                .as_str()
                .filter(|s| s.len() <= 4096 && !s.contains('\0'))
                .ok_or("Invalid argument")?;
            if matches!(
                word.split('=').next(),
                Some(
                    "--request-id"
                        | "--raw"
                        | "--interactive"
                        | "--stdin"
                        | "--format"
                        | "--help"
                        | "-h"
                )
            ) {
                return Err("MCP supplies the request ID and output format. Interactive input is unavailable.");
            }
            words.push(word.into());
        }
    }
    if matches!(name, "peer_attach" | "peer_events") {
        if let Some(index) = words.iter().position(|word| word == "--seconds") {
            if !words
                .get(index + 1)
                .and_then(|v| v.parse::<u16>().ok())
                .is_some_and(|n| (1..=300).contains(&n))
            {
                return Err("Use --seconds with a duration from 1 to 300 seconds.");
            }
        } else {
            words.extend(["--seconds".into(), "10".into()]);
        }
    }
    if requires_id(name) && !matches!(name, "plugin_command" | "audit_export") {
        words.push(format!("--request-id={id}"));
    }
    match name {
        "plugin_command" if !matches!(cmd, "plugin path" | "plugin logs") => {
            words.push("--json".into())
        }
        "follow_terminal" | "workspace_events" | "attach_terminal" => {
            words.push("--format=jsonl".into())
        }
        "plugin_command" | "peer_events" | "peer_attach" => {}
        _ => words.push("--format=json".into()),
    }
    Ok(Invocation {
        words,
        id: id.into(),
    })
}

fn drain(mut input: impl Read) -> io::Result<(Vec<u8>, bool)> {
    let mut result = Vec::new();
    let mut bytes = [0; 8192];
    let mut truncated = false;
    loop {
        let n = input.read(&mut bytes)?;
        if n == 0 {
            return Ok((result, truncated));
        }
        let kept = n.min((128 * 1024usize).saturating_sub(result.len()));
        result.extend_from_slice(&bytes[..kept]);
        truncated |= kept < n;
    }
}

fn execute(inv: &Invocation, name: &str) -> Reply {
    let id = &inv.id;
    let fail = |msg| Reply::failure(id, "unavailable", msg);
    let Ok(exe) = std::env::current_exe() else {
        return fail("Could not locate the desktop CLI.");
    };
    let mut child = match Command::new(exe)
        .args(&inv.words)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return fail("Could not start CLI workflow."),
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        let _ = tx.send((true, drain(stdout)));
    });
    std::thread::spawn(move || {
        let _ = tx2.send((false, drain(stderr)));
    });
    let deadline = Instant::now() + Duration::from_secs(310);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return if requires_id(name) {
                    Reply::unknown(id)
                } else {
                    fail("CLI workflow timed out.")
                };
            }
        }
    };
    let mut output = String::new();
    let mut errors = String::new();
    let mut truncated = false;
    for _ in 0..2 {
        let Ok((is_stdout, Ok((bytes, cut)))) = rx.recv_timeout(Duration::from_secs(2)) else {
            return if requires_id(name) {
                Reply::unknown(id)
            } else {
                fail("CLI output unavailable.")
            };
        };
        truncated |= cut;
        let target = if is_stdout { &mut output } else { &mut errors };
        *target = if is_stdout && name == "peer_attach" {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(bytes)
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
    }
    if !matches!(name, "plugin_command" | "peer_events" | "peer_attach") {
        if let Ok(reply) = serde_json::from_str::<Reply>(&output) {
            return reply;
        }
    }
    let mut reply = Reply::success(
        id,
        json!({"exitCode":status.code(),"stdout":output,"stdoutEncoding":if name=="peer_attach" {"base64"} else {"utf-8"},"stderr":errors,"truncated":truncated}),
    );
    if !status.success() {
        reply.ok = false;
        reply.error = Reply::failure(
            id,
            "workflow_failed",
            "CLI workflow failed. Inspect its exit code and output.",
        )
        .error;
    }
    reply
}

pub(crate) fn call(name: &str, args: &Value) -> Result<Value, &'static str> {
    let inv = parse(name, args)?;
    let permitted = || crate::mcp_settings::load().is_ok_and(|c| c.allows(name));
    let run = || {
        if permitted() {
            execute(&inv, name)
        } else {
            Reply::failure(
                &inv.id,
                "mcp_disabled",
                "This workflow is disabled in Settings → MCP.",
            )
        }
    };
    let changes_plugin = name == "plugin_command"
        && !matches!(inv.words[1].as_str(), "list" | "path" | "logs" | "search");
    let reply = if !permitted() {
        Reply::failure(
            &inv.id,
            "mcp_disabled",
            "This workflow is disabled in Settings → MCP.",
        )
    } else if changes_plugin || name == "audit_export" {
        // Store the outcome only. Plugin diagnostics may include local file contents.
        let mut first = None;
        let stored = crate::control_journal::execute_workflow(
            &crate::control_journal::root(),
            &inv.id,
            &json!({"mcpWorkflow":name,"arguments":inv.words}),
            |_| inv.words[0].clone(),
            |_| {
                let result = run();
                let mut receipt = result.clone();
                receipt.data = Some(json!({"command":inv.words[1],"ok":result.ok}));
                first = Some(result);
                receipt
            },
        );
        if stored.ok {
            first.unwrap_or(stored)
        } else {
            stored
        }
    } else {
        run()
    };
    Ok(json!(reply))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mcp_workflows_bind_prefix_and_format_and_refuse_interactive_input() {
        for command in [
            "mcp policy replace",
            "terminal send",
            "app restart",
            "plugin approve",
        ] {
            assert!(parse("follow_terminal", &json!({"command":command})).is_err());
        }
        for flag in [
            "--stdin",
            "--interactive",
            "--raw",
            "--format=raw",
            "--request-id=fresh",
        ] {
            assert!(parse(
                "attach_terminal",
                &json!({"command":"terminal attach","arguments":[flag],"requestId":"a"})
            )
            .is_err());
        }
        let invocation = parse(
            "attach_terminal",
            &json!({"command":"terminal attach","arguments":["card"],"requestId":"attach-1"}),
        )
        .unwrap();
        assert_eq!(
            invocation.words,
            [
                "terminal",
                "attach",
                "card",
                "--request-id=attach-1",
                "--format=jsonl"
            ]
        );
    }
}
