//! `llm.complete`: one answer from an AI model, through an agent CLI the
//! user already installed and signed in to (Claude Code, else Codex). It runs
//! non-interactively, with every tool off, in an empty temporary folder, with
//! the prompt on stdin. Plugins never see credentials.
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub const MAX_PROMPT: usize = 64 * 1024;
pub const TIMEOUT: Duration = Duration::from_secs(120);
/// Answers being worked on at once, over all plugins.
const MAX_IN_FLIGHT: usize = 2;
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// The providers this build knows, in the order they are tried.
pub const PROVIDERS: [&str; 2] = ["claude", "codex"];

fn error(code: &str, message: impl Into<String>, hint: &str) -> Value {
    json!({"code": code, "message": message.into(), "hint": hint})
}

fn unavailable(message: String) -> Value {
    error("unavailable", message, "Check that the agent works in a terminal (claude -p hello), then try again.")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub prompt: String,
    pub system: Option<String>,
    pub max_tokens: u32,
    pub json: bool,
    pub fast: bool,
}

impl Request {
    pub fn from_params(params: &Value) -> Result<Self, Value> {
        let prompt = params.get("prompt").and_then(Value::as_str).unwrap_or("").to_owned();
        if prompt.trim().is_empty() {
            return Err(error("bad_params", "llm.complete needs {prompt}", "Pass the question as prompt: \"…\"."));
        }
        if prompt.len() > MAX_PROMPT {
            return Err(error("bad_params", "the prompt is longer than 64 KiB", "Send less: summarize or cut the input first."));
        }
        Ok(Self {
            prompt,
            system: params.get("system").and_then(Value::as_str).map(str::to_owned),
            max_tokens: params.get("maxTokens").and_then(Value::as_u64).map_or(1024, |n| n.clamp(16, 8192) as u32),
            json: params.get("json").and_then(Value::as_bool).unwrap_or(false),
            fast: params.get("fast").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// The first installed provider.
pub fn provider() -> Option<&'static str> {
    PROVIDERS.iter().copied().find(|p| crate::tmux::which(p).is_some())
}

/// One answer: `{text, provider, model}`. Blocks up to two minutes; call
/// it off the GTK thread.
pub fn complete(request: &Request) -> Result<Value, Value> {
    let Some(provider) = provider() else {
        return Err(error("unavailable", "no AI agent CLI is installed for plugins to use",
            "Install and sign in to Claude Code (claude) or Codex (codex)."));
    };
    if IN_FLIGHT.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        return Err(error("busy", "two answers are already being worked on", "Wait for them, then ask again."));
    }
    let result = run_provider(provider, request);
    IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    result
}

fn run_provider(provider: &str, request: &Request) -> Result<Value, Value> {
    let binary = crate::tmux::which(provider).ok_or_else(|| unavailable(format!("{provider} is gone")))?;
    let system = system_prompt(request);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let workdir = std::env::temp_dir().join(format!("sd-plugin-llm-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&workdir).map_err(|e| unavailable(format!("cannot create a work folder: {e}")))?;
    let answer_file = workdir.join("answer.txt");
    let mut command = Command::new(binary);
    let input = if provider == "claude" {
        command.args(["-p", "--output-format", "json", "--tools", "", "--no-session-persistence",
            "--setting-sources", "", "--strict-mcp-config", "--system-prompt", &system]);
        if request.fast {
            command.args(["--model", "haiku"]);
        }
        request.prompt.clone()
    } else {
        command.args(["exec", "--skip-git-repo-check", "--ephemeral", "--sandbox", "read-only", "--output-last-message"])
            .arg(&answer_file)
            .arg("-");
        format!("{system}\n\n{}", request.prompt)
    };
    let result = run(&mut command, &workdir, &input);
    let answer = result.and_then(|stdout| {
        if provider == "claude" {
            parse_claude(&stdout)
        } else {
            let text = std::fs::read_to_string(&answer_file).map_err(|_| unavailable("Codex gave no answer".into()))?;
            Ok((text, "codex default".to_owned()))
        }
    });
    let _ = std::fs::remove_dir_all(&workdir);
    let (text, model) = answer?;
    let text = if request.json { strip_fences(&text) } else { text.trim().to_owned() };
    Ok(json!({"text": text, "provider": provider, "model": model}))
}

fn system_prompt(request: &Request) -> String {
    let mut system = request.system.clone().unwrap_or_default();
    if request.json {
        system.push_str("\n\nAnswer with one JSON document and nothing else: no prose, no code fences.");
    }
    system.push_str(&format!("\n\nKeep the answer under {} tokens.", request.max_tokens));
    system.trim().to_owned()
}

fn run(command: &mut Command, dir: &std::path::Path, input: &str) -> Result<String, Value> {
    use std::os::unix::process::CommandExt;
    let mut child = command
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| unavailable(format!("cannot start the agent: {e}")))?;
    let mut stdin = child.stdin.take().ok_or_else(|| unavailable("no stdin".into()))?;
    let input = input.to_owned();
    std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().ok_or_else(|| unavailable("no stdout".into()))?;
    let mut stderr = child.stderr.take().ok_or_else(|| unavailable("no stderr".into()))?;
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if started.elapsed() > TIMEOUT {
            // SAFETY: signalling our own child's process group.
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            return Err(error("timeout", "the agent did not answer within two minutes", "Send a shorter prompt, or try again."));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if !status.success() {
        let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no error output").trim();
        return Err(unavailable(format!("the agent failed: {}", last.chars().take(300).collect::<String>())));
    }
    Ok(stdout)
}

fn parse_claude(stdout: &str) -> Result<(String, String), Value> {
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|_| unavailable("Claude Code answered with something that is not JSON".into()))?;
    if value["is_error"].as_bool() == Some(true) {
        return Err(unavailable(format!("Claude Code reported an error: {}", value["result"].as_str().unwrap_or("unknown"))));
    }
    let text = value["result"].as_str().ok_or_else(|| unavailable("Claude Code's answer has no result".into()))?.to_owned();
    let model = value["modelUsage"].as_object().and_then(|m| m.keys().next().cloned()).unwrap_or_else(|| "claude default".into());
    Ok((text, model))
}

/// Removes a ``` fence that models add despite being asked not to.
fn strip_fences(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.split_once('\n').map(|(_, body)| body).unwrap_or("");
        return rest.trim_end().strip_suffix("```").unwrap_or(rest).trim().to_owned();
    }
    trimmed.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_llm_parses_claude_and_strips_fences() {
        let (text, model) = parse_claude(r#"{"result":"{\"a\":1}","is_error":false,"modelUsage":{"claude-haiku-4-5":{}}}"#).unwrap();
        assert_eq!((text.as_str(), model.as_str()), ("{\"a\":1}", "claude-haiku-4-5"));
        assert_eq!(parse_claude(r#"{"result":"quota","is_error":true}"#).unwrap_err()["code"], "unavailable");
        assert_eq!(strip_fences("```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_fences(" {\"a\": 1} "), "{\"a\": 1}");
    }

    #[test]
    fn plugins_llm_checks_the_request() {
        assert_eq!(Request::from_params(&json!({})).unwrap_err()["code"], "bad_params");
        assert_eq!(Request::from_params(&json!({"prompt": "x".repeat(MAX_PROMPT + 1)})).unwrap_err()["code"], "bad_params");
        let r = Request::from_params(&json!({"prompt": "hi", "maxTokens": 5, "json": true})).unwrap();
        assert_eq!((r.max_tokens, r.json, r.fast), (16, true, false));
        assert!(system_prompt(&r).contains("one JSON document"));
    }
}
