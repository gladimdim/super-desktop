//! Local, journaled input to the exact observed pane. No bridge input path changes.
use crate::control::{Command, InputData, Reply, Request};
use crate::control_close::UiResult;
use crate::control_terminal::{probe, read_process, read_process_input, Failure, Pane};
use serde_json::json;
use std::path::Path;
use std::process::Command as Process;
use std::sync::Arc;
use std::time::Instant;

const MARKER: &str = "@super_desktop_cli_input";
const CONFLICT: Failure = (
    "conflict",
    "The card, pane or session layout changed; input was refused.",
);

pub fn execute(
    root: &Path,
    request: &Request,
    deadline: Instant,
    mut inspect: impl FnMut() -> Result<UiResult, ()>,
) -> Reply {
    crate::control_journal::execute(root, request, |_| {
        let fail = |(code, message)| Reply::failure(&request.request_id, code, message);
        let Command::Input {
            input,
            expect_pane_identity,
            ..
        } = &request.command
        else {
            return fail(("invalid_request", "Expected terminal input."));
        };
        if let Err(message) = input.validate() {
            return fail(("invalid_arguments", message));
        }
        if expect_pane_identity.len() != 64
            || !expect_pane_identity.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return fail((
                "invalid_arguments",
                "Copy paneIdentity from terminal runtime.",
            ));
        }
        let target = match inspect() {
            Ok(Ok(Some(target))) => target,
            Ok(Err(reply)) => return reply,
            _ => return fail(("timeout", "Input target lookup expired before sending.")),
        };
        let task = Arc::clone(&target.task);
        task.with_idle_until(deadline, || {
            let session = &target.data.session_name;
            if !session.starts_with("sd_term_")
                || session.len() > 128
                || !session
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            {
                return fail((
                    "unsupported_terminal",
                    "The saved card does not identify a supported local session.",
                ));
            }
            let before = match probe(session, deadline) {
                Ok(pane) => pane,
                Err(error) => return fail(error),
            };
            if before.dead || &before.identity != expect_pane_identity {
                return fail(CONFLICT);
            }
            let nonce = match crate::control::new_request(Command::Status {}) {
                Ok(r) => r.request_id,
                Err(_) => return fail(("unavailable", "OS randomness is unavailable.")),
            };
            // The marker plus a second process probe binds even numeric IDs
            // reused after a server restart. No input occurs in this step.
            match guarded(session, &before, &nonce, None, deadline) {
                Ok(true) => {}
                Ok(false) => return fail(CONFLICT),
                Err(error) => return fail(error),
            }
            // Refuse an unsupported or occupied composer before staging private copies.
            if let Err(error) = input_command(input, &target.data.agent_type, &before, deadline) {
                return fail(error);
            }
            let prepared = match prepare_attachments(input, &target.data, root, &request.request_id) {
                Ok(input) => input,
                Err(error) => return fail(error),
            };
            let (payload, kind, bytes) =
                match input_command(&prepared, &target.data.agent_type, &before, deadline) {
                    Ok(value) => value,
                    Err(error) => return fail(error),
                };
            match probe(session, deadline) {
                Ok(current) if current.identity == before.identity && !current.dead => {}
                Ok(_) => return fail(CONFLICT),
                Err(error) => return fail(error),
            }
            match inspect() {
                Ok(Ok(Some(current)))
                    if current.data.id == target.data.id
                        && current.data.session_name == target.data.session_name
                        && current.data.created_at == target.data.created_at
                        && current.data.workspace_dir == target.data.workspace_dir
                        && Arc::ptr_eq(&current.task, &target.task) => {}
                Ok(Err(reply)) => return reply,
                Ok(_) => return fail(CONFLICT),
                Err(_) => return fail(("timeout", "Input target recheck expired before sending.")),
            }
            if Instant::now() >= deadline {
                return fail(("timeout", "Input expired before sending."));
            }
            match guarded(session, &before, &nonce, Some(&payload), deadline) {
                Ok(true) => Reply::success(
                    &request.request_id,
                    json!({"id":target.data.id,
                    "sessionName":session,"paneIdentity":before.identity,"kind":kind,"bytes":bytes,
                    "outcome":"delivered","submissionObserved":false,"completionObserved":false,
                    "turnId":null,"gridChanged":false,"attachments":match input {InputData::Prompt {attachments,..}=>attachments.len(),_=>0},"attachmentDelivery":match input {InputData::Prompt {attachments,..} if !attachments.is_empty()=>Some("path-references"),_=>None},"nativeImageConfirmation":false}),
                ),
                Ok(false) => fail(CONFLICT),
                // A failed process/acknowledgement cannot establish whether
                // some or all bytes reached the terminal. Never send again.
                Err(_) => Reply::unknown(&request.request_id),
            }
        })
        .unwrap_or_else(|| fail(("busy", "Terminal preparation or closing prevented input.")))
    })
}

/// Observe the same conservative composer guard used by prompt delivery.
/// This constructs a candidate input but never sends it or writes metadata.
pub(crate) fn composer(agent:&str,pane:&Pane,deadline:Instant)->Result<(bool,&'static str),Failure>{
    if !matches!(agent,"claude"|"codex"|"grok") {
        return Err(("unsupported_composer","This launcher has no verified composer reader."));
    }
    if pane.dead { return Ok((false,"exited")); }
    match input_command(&InputData::Prompt {text:"probe".into(),attachments:vec![]},agent,pane,deadline){
        Ok(_)=>Ok((true,"empty")),
        Err((code @ ("unsupported_composer"|"composer_not_empty"),_))=>Ok((false,code)),
        Err(error)=>Err(error),
    }
}

fn prepare_attachments(input: &InputData, card: &crate::state::TerminalData, root: &Path, request: &str) -> Result<InputData, Failure> {
    use crate::prompt_attachments::{self as attachments, Attachment, Delivery, Kind};
    let InputData::Prompt {text, attachments: ids} = input else { return Ok(input.clone()); };
    if ids.is_empty() { return Ok(input.clone()); }
    let _slot=crate::assets::Transfer::acquire().ok_or(("busy","File workers are busy."))?;
    let mut files=Vec::new(); let mut total=0usize;
    for id in ids {
        let (asset,bytes)=crate::assets::cli::read(card,id).map_err(|_|("file_unavailable","Attachment changed, expired or is no longer readable; refresh terminal files."))?;
        total=total.saturating_add(bytes.len());
        if total>attachments::MAX_TOTAL { return Err(("invalid_arguments","Attachments together exceed 16 MiB.")); }
        files.push(Attachment {kind:Kind::File,name:attachments::safe_name(&asset.name,Kind::File),bytes});
    }
    let store=root.parent().ok_or(("unavailable","Private state directory unavailable."))?.join("cli-attachments");
    let key=attachments::request_key("local-cli",&card.id,request);
    let paths=attachments::stage(&store,&key,&files).map_err(|_|("attachment_storage","Cannot stage private attachments; storage may be full or contain a prior attempt. No input sent."))?;
    let prepared=InputData::Prompt {text:attachments::prompt_text(Delivery::Path,text,&files,&paths),attachments:vec![]};
    prepared.validate().map_err(|_|("invalid_arguments","Prompt plus attachment paths exceeds the input limit or contains unsupported controls. No input sent."))?;
    Ok(prepared)
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn input_command(
    input: &InputData,
    agent: &str,
    pane: &Pane,
    deadline: Instant,
) -> Result<(String, &'static str, usize), Failure> {
    let id = &pane.pane_id;
    Ok(match input {
        InputData::Send { text, enter } => (
            format!(
                "send-keys -t {id} -H {}{}",
                hex(text.as_bytes()),
                if *enter { " 0d" } else { "" }
            ),
            "send",
            text.len() + usize::from(*enter),
        ),
        InputData::Keys { keys } => (
            format!(
                "send-keys -t {id} {}",
                keys.iter()
                    .map(|key| crate::control::key_name(key).unwrap())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            "keys",
            0,
        ),
        InputData::Prompt { text, .. } => {
            // Composer readers are conservative: unsupported and unrecognized
            // frames are not assumed empty. Shell input must use send/keys.
            if !matches!(agent, "claude" | "codex" | "grok") {
                return Err(("unsupported_composer", "Verified composer inspection is unavailable for this launcher; use explicit send/keys if intended."));
            }
            let mut command = Process::new(crate::tmux::tmux_bin());
            command.args([
                "-N",
                "display-message",
                "-p",
                "-t",
                id,
                "#{pane_current_command}",
            ]);
            let current = read_process(command, 256, deadline)?;
            if current.limited
                || std::str::from_utf8(&current.bytes).ok().map(str::trim) != Some(agent)
            {
                return Err((
                    "unsupported_composer",
                    "The expected harness is not the foreground command; no prompt was sent.",
                ));
            }
            let mut process = Process::new(crate::tmux::tmux_bin());
            process.args(["-N", "capture-pane", "-p", "-e", "-t", id]);
            let output = read_process(process, 65536, deadline)?;
            if output.limited {
                return Err(("output_limit", "Composer capture exceeded its limit."));
            }
            let screen = std::str::from_utf8(&output.bytes)
                .map_err(|_| ("unavailable", "Composer is not valid UTF-8."))?;
            match crate::prompt_attachments::composer_has_draft(agent, screen) {
                Some(false) => {},
                Some(true) => return Err(("composer_not_empty", "The composer contains a draft or cannot be recognized as empty. No text was sent.")),
                None => return Err(("unsupported_composer", "The current screen does not expose a readable composer.")),
            }
            // Bracketed paste keeps multiline text together for these TUIs;
            // Enter is an explicit part of prompt submission.
            let bytes = format!("\x1b[200~{text}\x1b[201~\r");
            (
                format!("send-keys -t {id} -H {}", hex(bytes.as_bytes())),
                "prompt",
                bytes.len(),
            )
        }
    })
}

fn guarded(
    session: &str,
    pane: &Pane,
    nonce: &str,
    input: Option<&str>,
    deadline: Instant,
) -> Result<bool, Failure> {
    let mut terms = vec![
        format!("#{{==:#{{pid}},{}}}", pane.server_pid),
        format!("#{{==:#{{session_name}},{session}}}"),
        format!("#{{==:#{{session_id}},{}}}", pane.session_id),
        format!("#{{==:#{{pane_id}},{}}}", pane.pane_id),
        format!("#{{==:#{{pane_pid}},{}}}", pane.pid),
        "#{==:#{pane_dead},0}".into(),
        "#{==:#{pane_in_mode},0}".into(),
        "#{==:#{session_windows},1}".into(),
        "#{==:#{window_panes},1}".into(),
        "#{==:#{window_linked},0}".into(),
    ];
    if input.is_some() {
        terms.push(format!("#{{==:#{{{MARKER}}},{nonce}}}"));
    }
    let guard = terms
        .into_iter()
        .reduce(|a, b| format!("#{{&&:{a},{b}}}"))
        .unwrap();
    let command = match input {
        Some(input) => format!("{input} ; display-message -p SD_INPUT"),
        None => format!(
            "set-option -t '{}' {MARKER} {nonce} ; display-message -p SD_READY",
            pane.session_id
        ),
    };
    // Only validated identifiers, fixed key names and hex bytes enter tmux's
    // language. Stdin keeps text out of process argv and temporary files.
    let script = format!(
        "if-shell -F -t {} '{}' {{ {} }} {{ display-message -p SD_CONFLICT }}\n",
        pane.pane_id, guard, command
    );
    let mut process = Process::new(crate::tmux::tmux_bin());
    process.args(["-N", "source-file", "-"]);
    let output = read_process_input(process, Some(script.as_bytes()), 1024, deadline)?;
    match output.bytes.as_slice() {
        b"SD_READY\n" if input.is_none() && !output.limited => Ok(true),
        b"SD_INPUT\n" if input.is_some() && !output.limited => Ok(true),
        b"SD_CONFLICT\n" if !output.limited => Ok(false),
        _ => Err(("unavailable", "Input acknowledgement is unavailable.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_close::Target;
    use std::time::Duration;
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(3)
    }
    fn run(args: &[&str]) -> String {
        let output = Process::new("tmux").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn inspect(target: &Target) -> Result<UiResult, ()> {
        Ok(Ok(Some(Target {
            data: target.data.clone(),
            task: Arc::clone(&target.task),
        })))
    }
    fn request(target: &Target, id: &str, input: InputData) -> Request {
        Request {
            control_version: 1,
            request_id: id.into(),
            command: Command::Input {
                id: target.data.id.clone(),
                input,
                expect_epoch: "epoch".into(),
                expect_revision: "a".repeat(64),
                expect_pane_identity: probe(&target.data.session_name, deadline())
                    .unwrap()
                    .identity,
            },
        }
    }
    #[test]
    fn cli_input_private_tmux_integration() {
        let root = std::env::temp_dir().join(format!("sd-input-{}", std::process::id()));
        std::fs::create_dir_all(root.join("tmux")).unwrap();
        let output = Process::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "control_input::tests::cli_input_tmux_inner",
                "--nocapture",
            ])
            .env("SD_CLI_INPUT_ROOT", &root)
            .env("TMUX_TMPDIR", root.join("tmux"))
            .env("HOME", &root)
            .env("XDG_STATE_HOME", root.join("state"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    fn cli_input_tmux_inner() {
        let Some(root) = std::env::var_os("SD_CLI_INPUT_ROOT").map(std::path::PathBuf::from) else {
            return;
        };
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Process::new("tmux").arg("kill-server").output();
            }
        }
        let _cleanup = Cleanup;
        let session = "sd_term_input_test";
        let output_file = root.join("received");
        let shell = format!(
            "stty raw -echo; printf READY; exec cat > {}",
            shlex::try_quote(output_file.to_str().unwrap()).unwrap()
        );
        run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            session,
            &shell,
        ]);
        let until = deadline();
        while !run(&["capture-pane", "-p", "-t", session]).contains("READY") {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let target = Target {
            data: serde_json::from_value(json!({"id":session,"session_name":session,
            "agent_type":"shell","command":"cat","x":10,"y":80,"created_at":1.0}))
            .unwrap(),
            task: Arc::new(crate::session_task::SessionTask::default()),
        };
        let text = "Готово ✓\n' ; $(touch not-a-command) #{pane_id}\t";
        let r = request(
            &target,
            "input-one",
            InputData::Send {
                text: text.into(),
                enter: false,
            },
        );
        let original = probe(session, deadline()).unwrap();
        let journal = root.join("journal");
        let reply = execute(&journal, &r, deadline(), || inspect(&target));
        assert!(reply.ok, "{reply:?}");
        let until = deadline();
        while std::fs::read(&output_file).unwrap_or_default() != text.as_bytes() {
            assert!(Instant::now() < until, "{:?}", std::fs::read(&output_file));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            execute(&journal, &r, deadline(), || panic!(
                "duplicate input must not run"
            ))
            .ok
        );
        assert_eq!(std::fs::read(&output_file).unwrap(), text.as_bytes());
        assert_eq!(
            probe(session, deadline()).unwrap().identity,
            original.identity
        );
        assert!(
            run(&["list-clients", "-t", session]).trim().is_empty(),
            "input must not attach"
        );
        let receipt = std::fs::read_to_string(journal.join("input-one.json")).unwrap();
        assert!(!receipt.contains("not-a-command"));
        let mut changed = r.clone();
        if let Command::Input { input, .. } = &mut changed.command {
            *input = InputData::Keys {
                keys: vec!["Enter".into()],
            };
        }
        assert_eq!(
            execute(&journal, &changed, deadline(), || panic!("changed replay")).exit_code(),
            5
        );
        let keys = request(
            &target,
            "keys",
            InputData::Keys {
                keys: vec!["Enter".into(), "Tab".into()],
            },
        );
        assert!(execute(&journal, &keys, deadline(), || inspect(&target)).ok);
        let expected = format!("{text}\r\t").into_bytes();
        let until = deadline();
        while std::fs::read(&output_file).unwrap() != expected {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        // A local cat stub named claude exposes an empty composer without invoking a model.
        let stub=root.join("claude");
        std::fs::copy("/usr/bin/cat",&stub).unwrap();
        let received=root.join("prompt-received");
        let launch=format!("stty raw -echo; printf '❯ '; exec {} > {}",shlex::try_quote(stub.to_str().unwrap()).unwrap(),shlex::try_quote(received.to_str().unwrap()).unwrap());
        let prompt_session="sd_term_prompt_test";
        run(&["new-session","-d","-s",prompt_session,&launch]);
        let mut prompt_target=Target {data:target.data.clone(),task:Arc::new(crate::session_task::SessionTask::default())};
        prompt_target.data.id=prompt_session.into();prompt_target.data.session_name=prompt_session.into();prompt_target.data.agent_type="claude".into();prompt_target.data.workspace_dir=Some(root.to_string_lossy().into_owned());
        let until=deadline();
        while run(&["display-message","-p","-t",prompt_session,"#{pane_current_command}"]).trim()!="claude" { assert!(Instant::now()<until);std::thread::sleep(Duration::from_millis(10)); }
        let observed=composer("claude",&probe(prompt_session,deadline()).unwrap(),deadline()).unwrap();
        assert_eq!(observed,(true,"empty"));
        assert!(std::fs::read(&received).unwrap_or_default().is_empty(),"Composer observation must never type");
        std::fs::write(root.join("attachment.md"),"immutable snapshot").unwrap();
        let asset=crate::assets::cli::add(&prompt_target.data,"attachment.md").unwrap();
        let prompt=request(&prompt_target,"prompt-files",InputData::Prompt {text:"Read this file".into(),attachments:vec![asset.id.clone()]});
        let reply=execute(&journal,&prompt,deadline(),||inspect(&prompt_target));
        assert!(reply.ok,"{reply:?}");assert_eq!(reply.data.unwrap()["attachmentDelivery"],"path-references");
        let until=deadline();let delivered=loop {let data=std::fs::read(&received).unwrap_or_default();if data.ends_with(b"\r") {break data;}assert!(Instant::now()<until);std::thread::sleep(Duration::from_millis(10));};
        let text=String::from_utf8(delivered.clone()).unwrap();assert!(text.starts_with("\x1b[200~Read this file "));assert!(text.ends_with("\x1b[201~\r"));
        let key=crate::prompt_attachments::request_key("local-cli",prompt_session,"prompt-files");
        let copy=root.join("cli-attachments").join(key).join("attachment.md");assert_eq!(std::fs::read_to_string(&copy).unwrap(),"immutable snapshot");
        use std::os::unix::fs::MetadataExt;
        assert_eq!(std::fs::metadata(&copy).unwrap().mode() & 0o777,0o600);
        assert!(execute(&journal,&prompt,deadline(),||panic!("do not submit twice")).ok);
        assert_eq!(std::fs::read(&received).unwrap(),delivered);
        std::fs::write(root.join("attachment.md"),"changed").unwrap();
        let changed=request(&prompt_target,"prompt-stale-file",InputData::Prompt {text:"Never".into(),attachments:vec![asset.id]});
        assert!(!execute(&journal,&changed,deadline(),||inspect(&prompt_target)).ok);
        assert_eq!(std::fs::read(&received).unwrap(),delivered);
        assert_eq!(std::fs::read_to_string(&copy).unwrap(),"immutable snapshot");
        assert!(!std::fs::read_to_string(journal.join("prompt-files.json")).unwrap().contains("Read this file"));
        let mut second = request(
            &target,
            "stale-ui",
            InputData::Send {
                text: "NEVER".into(),
                enter: true,
            },
        );
        let mut calls = 0;
        let reply = execute(&journal, &second, deadline(), || {
            calls += 1;
            if calls == 2 {
                Ok(Err(Reply::failure("stale-ui", "conflict", "changed")))
            } else {
                inspect(&target)
            }
        });
        assert_eq!(reply.exit_code(), 5);
        assert_eq!(std::fs::read(&output_file).unwrap(), expected);
        second.request_id = "stale-pane".into();
        run(&["respawn-pane", "-k", "-t", session, &shell]);
        assert_eq!(
            execute(&journal, &second, deadline(), || inspect(&target)).exit_code(),
            5
        );
        // A marker cannot authorize input after the pane is respawned.
        let pane = probe(session, deadline()).unwrap();
        assert!(guarded(session, &pane, "nonce", None, deadline()).unwrap());
        run(&["respawn-pane", "-k", "-t", session, "sleep 60"]);
        assert!(!guarded(
            session,
            &pane,
            "nonce",
            Some("display-message -p NEVER"),
            deadline()
        )
        .unwrap());
        run(&["split-window", "-d", "-t", session, "sleep 60"]);
        assert!(
            probe(session, deadline()).is_err(),
            "ambiguous panes are refused"
        );
    }
    #[test]
    fn cli_input_validation_refuses_controls_and_unknown_keys() {
        for text in ["", "a\0b", "x\x1by", "\r", "\u{009b}"] {
            assert!(InputData::Send {
                text: text.into(),
                enter: false
            }
            .validate()
            .is_err());
        }
        assert!(InputData::Send {
            text: "x".repeat(4097),
            enter: false
        }
        .validate()
        .is_err());
        assert!(InputData::Send {
            text: "a\n\t✓".into(),
            enter: true
        }
        .validate()
        .is_ok());
        assert!(InputData::Keys {
            keys: vec!["C-c ; kill-server".into()]
        }
        .validate()
        .is_err());
        assert!(InputData::Keys {
            keys: vec!["Enter".into(); 33]
        }
        .validate()
        .is_err());
    }
}
