//! `llm.complete`: one completion from the AI provider the user chose
//! (`plugins.json` → `llmProvider`), run on the plugin's worker thread.
//!
//! Providers are harness CLIs the user already installed and signed in to,
//! run non-interactively with every tool disabled, in an empty temporary
//! directory, with the prompt on stdin. The plugin never sees credentials.
use super::rpc::{self, RpcError};
use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MAX_PROMPT: usize = 64 * 1024;
pub const TIMEOUT: Duration = Duration::from_secs(120);
const DOCS: &str = "references/host-api.md#llmcomplete";

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub prompt: String,
    pub system: Option<String>,
    pub max_tokens: u32,
    pub json: bool,
    pub fast: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub text: String,
    pub provider: String,
    pub model: String,
}

/// Providers this build knows, in the order `auto` tries them.
pub const PROVIDERS: [&str; 2] = ["claude", "codex"];

pub fn available() -> Vec<&'static str> {
    PROVIDERS.iter().copied().filter(|p| crate::tmux::which(p).is_some()).collect()
}

/// The provider `choice` resolves to now, if any.
pub fn resolve(choice: &str) -> Option<&'static str> {
    let installed = available();
    if choice == "auto" {
        installed.first().copied()
    } else {
        installed.into_iter().find(|p| *p == choice)
    }
}

pub fn complete(choice: &str, request: &Request) -> Result<Reply, RpcError> {
    let Some(provider) = resolve(choice) else {
        return Err(RpcError::new(
            rpc::UNAVAILABLE,
            if choice == "auto" { "no AI provider is installed".to_string() } else { format!("the chosen AI provider `{choice}` is not installed") },
            "Install and sign in to Claude Code or Codex, then choose it in Settings → Plugins → AI provider.",
            DOCS,
        ));
    };
    let system = system_prompt(request);
    let (mut command, output_file) = match provider {
        "claude" => {
            let mut c = Command::new(crate::tmux::which("claude").expect("resolved above"));
            c.args(["-p", "--output-format", "json", "--tools", "", "--no-session-persistence", "--setting-sources", "", "--strict-mcp-config"]);
            if request.fast {
                c.args(["--model", "haiku"]);
            }
            c.args(["--system-prompt", &system]);
            (c, None)
        }
        _ => {
            let out = std::env::temp_dir().join(format!("sd-llm-{}-{}.txt", std::process::id(), nanos()));
            let mut c = Command::new(crate::tmux::which("codex").expect("resolved above"));
            c.args(["exec", "--skip-git-repo-check", "--ephemeral", "--sandbox", "read-only", "--output-last-message"]).arg(&out).arg("-");
            (c, Some(out))
        }
    };
    let workdir = std::env::temp_dir().join(format!("sd-llm-cwd-{}-{}", std::process::id(), nanos()));
    std::fs::create_dir_all(&workdir).map_err(|e| unavailable(format!("cannot create a work directory: {e}")))?;
    let stdin_text = if provider == "claude" { request.prompt.clone() } else { format!("{system}\n\n{}", request.prompt) };
    let result = run(&mut command, &workdir, &stdin_text);
    let _ = std::fs::remove_dir_all(&workdir);
    let stdout = result?;
    let (text, model) = match provider {
        "claude" => parse_claude(&stdout)?,
        _ => {
            let path = output_file.expect("codex writes a file");
            let text = std::fs::read_to_string(&path).map_err(|_| unavailable("codex produced no answer".into()))?;
            let _ = std::fs::remove_file(&path);
            (text, "codex default".to_string())
        }
    };
    let text = if request.json { strip_fences(&text) } else { text.trim().to_string() };
    Ok(Reply { text, provider: provider.to_string(), model })
}

fn system_prompt(request: &Request) -> String {
    let mut system = request.system.clone().unwrap_or_default();
    if request.json {
        system.push_str("\n\nAnswer with one JSON document and nothing else: no prose, no code fences.");
    }
    system.push_str(&format!("\n\nKeep the answer under {} tokens.", request.max_tokens));
    system.trim().to_string()
}

fn run(command: &mut Command, dir: &std::path::Path, input: &str) -> Result<String, RpcError> {
    use std::os::unix::process::CommandExt;
    let mut child = command
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| unavailable(format!("cannot start the AI provider: {e}")))?;
    let mut stdin = child.stdin.take().expect("piped");
    let input = input.to_string();
    std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
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
            // SAFETY: our own child's process group.
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            return Err(RpcError::new(rpc::TIMEOUT, "the AI provider did not answer within 120 s", "Send a shorter prompt, or try again.", DOCS));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if !status.success() {
        let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no error output").trim();
        return Err(unavailable(format!("the AI provider failed: {}", last.chars().take(300).collect::<String>())));
    }
    Ok(stdout)
}

fn parse_claude(stdout: &str) -> Result<(String, String), RpcError> {
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|_| unavailable("Claude Code answered with something that is not JSON".into()))?;
    if value["is_error"].as_bool() == Some(true) {
        return Err(unavailable(format!("Claude Code reported an error: {}", value["result"].as_str().unwrap_or("unknown"))));
    }
    let text = value["result"].as_str().ok_or_else(|| unavailable("Claude Code's answer has no result".into()))?.to_string();
    let model = value["modelUsage"].as_object().and_then(|m| m.keys().next().cloned()).unwrap_or_else(|| "claude default".into());
    Ok((text, model))
}

/// Remove a surrounding ``` fence that models add despite instructions.
fn strip_fences(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.split_once('\n').map(|(_, body)| body).unwrap_or("");
        return rest.trim_end().strip_suffix("```").unwrap_or(rest).trim().to_string();
    }
    trimmed.to_string()
}

fn unavailable(reason: String) -> RpcError {
    RpcError::new(rpc::UNAVAILABLE, reason, "Check that the provider works in a terminal, or pick another in Settings → Plugins → AI provider.", DOCS)
}

fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_llm_parses_claude_and_strips_fences() {
        let (text, model) = parse_claude(r#"{"result":"{\"a\":1}","is_error":false,"modelUsage":{"claude-haiku-4-5-20251001":{}}}"#).unwrap();
        assert_eq!((text.as_str(), model.as_str()), ("{\"a\":1}", "claude-haiku-4-5-20251001"));
        assert_eq!(parse_claude(r#"{"result":"quota","is_error":true}"#).unwrap_err().code, rpc::UNAVAILABLE);
        assert_eq!(strip_fences("```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_fences(" {\"a\": 1} "), "{\"a\": 1}");
    }

    #[test]
    fn plugin_llm_unknown_provider_is_unavailable_with_a_hint() {
        let error = complete("nonexistent", &Request { prompt: "x".into(), system: None, max_tokens: 10, json: false, fast: false }).unwrap_err();
        assert_eq!(error.code, rpc::UNAVAILABLE);
        assert!(error.data.unwrap()["hint"].as_str().unwrap().contains("Settings → Plugins"));
    }
}
