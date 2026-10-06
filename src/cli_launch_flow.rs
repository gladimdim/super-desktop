//! Client-side launch composition. Every effect has its own durable receipt;
//! the parent reservation prevents replay of partially completed workflows.
use crate::cli_extended::{valid_id, Options};
use crate::control::{self, Command, InputData, Reply, Request};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Serialize)]
pub(crate) struct Flow {
    launch: Command,
    width: Option<u32>,
    height: Option<u32>,
    center: bool,
    prompt: Option<String>,
    show: bool,
    ready_timeout_ms: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    capture_completion_baseline: bool,
}
impl Flow {
    pub(crate) fn parse(launch: Command, options: &Options) -> Result<Option<Self>, &'static str> {
        let dimension = |key| {
            options
                .values
                .get(key)
                .map(|v| {
                    v.parse::<u32>()
                        .ok()
                        .filter(|n| (1..=32768).contains(n))
                        .ok_or("Dimensions must be 1-32768 logical pixels.")
                })
                .transpose()
        };
        let width = dimension("--width")?;
        let height = dimension("--height")?;
        if width.is_some() != height.is_some() {
            return Err("Supply both --width and --height.");
        }
        let sources = usize::from(options.values.contains_key("--prompt"))
            + usize::from(options.values.contains_key("--prompt-file"))
            + usize::from(options.flags.contains("--prompt-stdin"));
        if sources > 1 {
            return Err("Choose one of --prompt, --prompt-file or --prompt-stdin.");
        }
        let prompt = if let Some(text) = options.values.get("--prompt") {
            Some(text.clone())
        } else if let Some(path) = options.values.get("--prompt-file") {
            Some(
                Options::parse(&["--file".into(), path.clone()], &["--file"], &[])?
                    .text(control::MAX_INPUT)?,
            )
        } else if options.flags.contains("--prompt-stdin") {
            Some(Options::parse(&["--stdin".into()], &[], &["--stdin"])?.text(control::MAX_INPUT)?)
        } else {
            None
        };
        if let Some(text) = &prompt {
            InputData::Prompt {
                text: text.clone(),
                attachments: vec![],
            }
            .validate()?;
            if !matches!(&launch,Command::Launch {harness,..} if matches!(harness.as_str(),"claude"|"codex"|"grok"))
            {
                return Err("Initial prompts require a direct Claude, Codex or Grok launcher.");
            }
        }
        if prompt.is_none() && options.values.contains_key("--ready-timeout") {
            return Err("--ready-timeout requires an initial prompt.");
        }
        let wait = options
            .values
            .get("--ready-timeout")
            .map(|s| crate::cli_extended::seconds(s))
            .transpose()?
            .unwrap_or(Duration::from_secs(30));
        if wait > Duration::from_secs(300) {
            return Err("Composer readiness timeout is at most 300s.");
        }
        let center = options.flags.contains("--center");
        let show = options.flags.contains("--show");
        Ok(if width.is_some() || center || prompt.is_some() || show {
            Some(Self {
                launch,
                width,
                height,
                center,
                prompt,
                show,
                ready_timeout_ms: wait.as_millis() as u64,
                capture_completion_baseline: false,
            })
        } else {
            None
        })
    }

    pub(crate) fn with_completion_baseline(mut self) -> Self {
        self.capture_completion_baseline = true;
        self
    }

    pub(crate) fn run(self, options: &Options) -> Reply {
        self.run_with_permission(options, || true)
    }

    pub(crate) fn run_with_permission(
        self,
        options: &Options,
        mut permitted: impl FnMut() -> bool,
    ) -> Reply {
        if options.values.get("--target").is_some_and(|s| s != "local") {
            return Reply::failure(
                "",
                "unsupported_target",
                "Launch workflows target local only.",
            );
        }
        let Some(id) = options
            .values
            .get("--request-id")
            .filter(|s| valid_id(s, 64))
        else {
            return Reply::failure(
                "",
                "invalid_arguments",
                "A unique --request-id is required.",
            );
        };
        execute(
            &crate::control_journal::root(),
            id,
            &self,
            |request| {
                if permitted() {
                    control::request_at(&control::runtime_dir(), request)
                } else {
                    Reply::failure(
                        &request.request_id,
                        "mcp_disabled",
                        "MCP launch or prompt access was disabled during this workflow.",
                    )
                }
            },
            |id| {
                let exe = match std::env::current_exe() {
                    Ok(p) => p.with_file_name("super-desktop"),
                    Err(_) => {
                        return Reply::failure(
                            id,
                            "unavailable",
                            "Cannot locate application executable.",
                        )
                    }
                };
                crate::cli_application::execute(
                    &crate::control_journal::root(),
                    &crate::platform::runtime::socket_path(),
                    &exe,
                    "show",
                    id,
                )
            },
        )
    }
}

fn text(data: &Value, key: &str) -> Result<String, Reply> {
    data[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            Reply::failure(
                "",
                "invalid_response",
                "Missing launch workflow response field.",
            )
        })
}
fn guards(data: &Value) -> Result<(String, String), Reply> {
    Ok((text(data, "epoch")?, text(data, "revision")?))
}
fn centered(data: &Value) -> Result<(i32, i32), Reply> {
    let failure = || {
        Reply::failure(
            "",
            "invalid_response",
            "Invalid logical canvas or card dimensions.",
        )
    };
    let number = |v: &Value| {
        v.as_i64()
            .filter(|n| (-32768..=32768).contains(n))
            .ok_or_else(failure)
    };
    let width = number(&data["canvas"]["width"])?;
    let height = number(&data["canvas"]["height"])?;
    let w = number(&data["rect"]["width"])?;
    let h = number(&data["rect"]["height"])?;
    if width < w || height < h || w <= 0 || h <= 0 {
        return Err(failure());
    }
    // Card coordinates are local to the allocated canvas, not physical outputs.
    Ok((((width - w) / 2) as i32, ((height - h) / 2) as i32))
}

fn execute(
    root: &Path,
    id: &str,
    flow: &Flow,
    mut send: impl FnMut(&Request) -> Reply,
    mut show: impl FnMut(&str) -> Reply,
) -> Reply {
    let prefix = format!(
        "launch-{}",
        &format!("{:x}", Sha256::digest(id.as_bytes()))[..32]
    );
    let request_ids = json!({"launch":format!("{prefix}-create"),"resize":format!("{prefix}-resize"),"move":format!("{prefix}-move"),"prompt":format!("{prefix}-prompt"),"show":format!("{prefix}-show")});
    crate::control_journal::execute_workflow(
        root,
        id,
        &json!({"method":"client.harness.launch.workflow","flow":flow}),
        |_| format!("{prefix}-create"),
        |_| {
            let mut card: Option<String> = None;
            let mut completed: Vec<&str> = vec![];
            let mut stage = "preflight";
            let mut geometry = Value::Null;
            let mut prompt_receipt = Value::Null;
            let mut completion_baseline = Value::Null;
            let mut readiness = "not_observed";
            let mut call = |step: &str, command: Command| -> Result<Value, Reply> {
                let request = Request {
                    control_version: control::VERSION,
                    request_id: format!("{prefix}-{step}"),
                    command,
                };
                let reply = send(&request);
                if reply.ok {
                    reply.data.ok_or_else(|| {
                        Reply::failure(id, "invalid_response", "Missing operation result.")
                    })
                } else {
                    Err(reply)
                }
            };
            let result = (|| -> Result<(), Reply> {
                let capabilities = call("capabilities", Command::Capabilities {})?;
                let mut required = vec!["harness.launch", "terminal.geometry"];
                if flow.capture_completion_baseline {
                    required.push("terminal.status");
                }
                if flow.width.is_some() {
                    required.push("terminal.resize");
                }
                if flow.center {
                    required.push("terminal.move");
                }
                if flow.prompt.is_some() {
                    required.extend(["terminal.composer", "terminal.input"]);
                }
                if !capabilities["methods"]
                    .as_array()
                    .is_some_and(|methods| required.iter().all(|m| methods.iter().any(|v| v == m)))
                {
                    return Err(Reply::failure(id,"unsupported_command","Daemon lacks launch workflow methods; update/restart it before launching. No session created."));
                }
                stage = "launch";
                let launched = call("create", flow.launch.clone())?;
                card = Some(text(&launched, "id")?);
                completed.push("launch");
                let card = card.as_ref().unwrap();
                if flow.show {
                    stage = "show";
                    let reply = show(request_ids["show"].as_str().unwrap());
                    if !reply.ok {
                        return Err(reply);
                    }
                    completed.push("show");
                }
                stage = "geometry";
                geometry = call("geometry", Command::Geometry { id: card.clone() })?;
                if let (Some(width), Some(height)) = (flow.width, flow.height) {
                    stage = "resize";
                    let (expect_epoch, expect_revision) = guards(&geometry)?;
                    geometry = call(
                        "resize",
                        Command::Resize {
                            id: card.clone(),
                            width,
                            height,
                            clamp: false,
                            expect_epoch,
                            expect_revision,
                        },
                    )?;
                    completed.push("resize");
                }
                if flow.center {
                    stage = "move";
                    let (x, y) = centered(&geometry)?;
                    let (expect_epoch, expect_revision) = guards(&geometry)?;
                    geometry = call(
                        "move",
                        Command::Move {
                            id: card.clone(),
                            x,
                            y,
                            clamp: false,
                            expect_epoch,
                            expect_revision,
                        },
                    )?;
                    completed.push("move");
                }
                if let Some(prompt) = &flow.prompt {
                    stage = "readiness";
                    let until = Instant::now() + Duration::from_millis(flow.ready_timeout_ms);
                    let mut identity = None;
                    let ready = loop {
                        if Instant::now() >= until {
                            return Err(Reply::failure(id,"timeout","Composer readiness timed out; the created card remains open and no prompt was sent. Inspect its screen for trust/login dialogs or drafts."));
                        }
                        let observed = call("composer", Command::Composer { id: card.clone() })?;
                        if Instant::now() >= until {
                            return Err(Reply::failure(id,"timeout","Composer observation exceeded the readiness deadline; no prompt was sent."));
                        }
                        let current = text(&observed, "paneIdentity")?;
                        if identity.as_ref().is_some_and(|old| old != &current) {
                            return Err(Reply::failure(
                                id,
                                "conflict",
                                "The launched pane changed while waiting; no prompt was sent.",
                            ));
                        }
                        identity = Some(current);
                        if observed["reason"] == "exited" {
                            return Err(Reply::failure(
                                id,
                                "terminal_not_running",
                                "The launched harness exited before becoming ready.",
                            ));
                        }
                        if observed["ready"] == true {
                            break observed;
                        }
                        std::thread::sleep(
                            Duration::from_millis(100)
                                .min(until.saturating_duration_since(Instant::now())),
                        );
                    };
                    readiness = "recognized_empty_composer";
                    if flow.capture_completion_baseline {
                        stage = "completionBaseline";
                        let observed = call("baseline", Command::Lifecycle { id: card.clone() })?;
                        if observed["paneIdentity"] != ready["paneIdentity"] {
                            return Err(Reply::failure(
                                id,
                                "conflict",
                                "Pane changed before prompt submission.",
                            ));
                        }
                        let previous = &observed["completion"]["completionId"];
                        if !previous.is_null()
                            && !previous.as_str().is_some_and(|s| {
                                s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
                            })
                        {
                            return Err(Reply::failure(
                                id,
                                "invalid_response",
                                "Invalid completion baseline.",
                            ));
                        }
                        completion_baseline = json!({"observed":true,"supported":observed["completion"]["supported"]==true,"completionId":previous,"paneIdentity":observed["paneIdentity"]});
                    }
                    stage = "prompt";
                    // Keep the last geometry guard: user edits or display changes during
                    // startup must cause a conflict, not silently become new consent.
                    let (expect_epoch, expect_revision) = guards(&geometry)?;
                    prompt_receipt = call(
                        "prompt",
                        Command::Input {
                            id: card.clone(),
                            input: InputData::Prompt {
                                text: prompt.clone(),
                                attachments: vec![],
                            },
                            expect_epoch,
                            expect_revision,
                            expect_pane_identity: text(&ready, "paneIdentity")?,
                        },
                    )?;
                    completed.push("prompt");
                }
                Ok(())
            })();
            let mut reply = match result {
                Ok(()) => Reply::success(id, json!({})),
                Err(mut failed) => {
                    failed.request_id = id.into();
                    if card.is_some() {
                        if let Some(error) = &mut failed.error {
                            if error.outcome != "unknown" {
                                error.outcome = "partial".into();
                            }
                        }
                    }
                    failed
                }
            };
            reply.data = Some(
                json!({"id":card,"outcome":if reply.ok {"configured"}else{reply.error.as_ref().map(|e|e.outcome.as_str()).unwrap_or("unknown")},"completedSteps":completed,"failedStep":if reply.ok {None}else{Some(stage)},"requestIds":request_ids,"geometry":geometry,"readiness":readiness,"prompt":prompt_receipt,"submissionObserved":false,"completionObserved":false}),
            );
            if flow.capture_completion_baseline {
                reply.data.as_mut().unwrap()["completionBaseline"] = completion_baseline;
            }
            reply
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture {
        root: std::path::PathBuf,
        effects: Vec<String>,
        polls: usize,
        geometry: Value,
        fail: Option<&'static str>,
        old: bool,
    }
    impl Fixture {
        fn new() -> Self {
            Self {
                root: std::env::temp_dir()
                    .canonicalize()
                    .expect("resolve system temporary directory")
                    .join(format!(
                        "sd-launch-flow-{}-{}",
                        std::process::id(),
                        NEXT.fetch_add(1, Ordering::Relaxed)
                    )),
                effects: vec![],
                polls: 0,
                geometry: json!({"epoch":"epoch","revision":"one","canvas":{"width":1000,"height":800},"rect":{"x":30,"y":80,"width":640,"height":480}}),
                fail: None,
                old: false,
            }
        }
        fn send(&mut self, r: &Request) -> Reply {
            if let Command::Capabilities {} = r.command {
                return Reply::success(
                    &r.request_id,
                    json!({"methods":if self.old {vec!["harness.launch"]}else{control::METHODS.to_vec()}}),
                );
            }
            if let Command::Geometry { .. } = r.command {
                return Reply::success(&r.request_id, self.geometry.clone());
            }
            if let Command::Composer { .. } = r.command {
                self.polls += 1;
                return Reply::success(
                    &r.request_id,
                    json!({"paneIdentity":if self.fail==Some("pane")&&self.polls>1 {"changed"}else{"pane"},"ready":self.polls>=2&&self.fail!=Some("readiness"),"reason":"empty"}),
                );
            }
            if let Command::Lifecycle { .. } = r.command {
                return Reply::success(
                    &r.request_id,
                    json!({"paneIdentity":if self.fail==Some("baseline-pane") {"changed"}else{"pane"},"completion":{"supported":true,"completionId":"a".repeat(64)}}),
                );
            }
            let root = self.root.clone();
            crate::control_journal::execute(&root, r, |_| {
                let stage = match &r.command {
                    Command::Launch { .. } => "launch",
                    Command::Resize {
                        width,
                        height,
                        expect_revision,
                        ..
                    } => {
                        assert_eq!(
                            expect_revision,
                            &self.geometry["revision"].as_str().unwrap()
                        );
                        assert_eq!((*width, *height), (600, 300));
                        self.geometry["rect"]["width"] = json!(width);
                        self.geometry["rect"]["height"] = json!(height);
                        self.geometry["revision"] = json!("two");
                        "resize"
                    }
                    Command::Move {
                        x,
                        y,
                        expect_revision,
                        ..
                    } => {
                        assert_eq!(expect_revision, "two");
                        assert_eq!((*x, *y), (200, 250));
                        self.geometry["revision"] = json!("three");
                        self.geometry["rect"]["x"] = json!(x);
                        self.geometry["rect"]["y"] = json!(y);
                        "move"
                    }
                    Command::Input {
                        input: InputData::Prompt { text, .. },
                        expect_revision,
                        expect_pane_identity,
                        ..
                    } => {
                        assert_eq!(text, "Hello world");
                        assert_eq!(expect_revision, "three");
                        assert_eq!(expect_pane_identity, "pane");
                        "prompt"
                    }
                    _ => panic!("Unexpected command"),
                };
                self.effects.push(stage.into());
                if self.fail == Some(stage) {
                    return Reply::unknown(&r.request_id);
                }
                Reply::success(
                    &r.request_id,
                    match stage {
                        "launch" => json!({"id":"card"}),
                        "prompt" => json!({"outcome":"delivered","submissionObserved":false}),
                        _ => self.geometry.clone(),
                    },
                )
            })
        }
        fn run(&mut self, id: &str, flow: &Flow) -> Reply {
            let root = self.root.clone();
            execute(
                &root,
                id,
                flow,
                |r| self.send(r),
                |_| panic!("show not requested"),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn flow() -> Flow {
        Flow {
            launch: Command::Launch {
                harness: "claude".into(),
                cwd: "/tmp".into(),
                arguments: None,
                allow_unsafe_harness: true,
                allow_download: false,
            },
            width: Some(600),
            height: Some(300),
            center: true,
            prompt: Some("Hello world".into()),
            show: false,
            ready_timeout_ms: 1000,
            capture_completion_baseline: false,
        }
    }
    #[test]
    fn configured_launch_records_pre_prompt_baseline_and_replays_it() {
        let mut fixture = Fixture::new();
        let ordinary = flow();
        assert!(serde_json::to_value(&ordinary)
            .unwrap()
            .get("capture_completion_baseline")
            .is_none());
        let flow = ordinary.with_completion_baseline();
        let first = fixture.run("baseline", &flow);
        assert!(first.ok, "{first:?}");
        assert_eq!(
            first.data.as_ref().unwrap()["completionBaseline"]["completionId"],
            "a".repeat(64)
        );
        let effects = fixture.effects.clone();
        let replay = fixture.run("baseline", &flow);
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
        assert_eq!(fixture.effects, effects);
        let mut changed = Fixture::new();
        changed.fail = Some("baseline-pane");
        let result = changed.run("changed", &flow);
        assert!(!result.ok);
        assert!(!changed.effects.contains(&"prompt".to_string()));
    }

    #[test]
    fn configured_launch_centers_waits_and_replays_without_repeating_effects() {
        let mut fixture = Fixture::new();
        let flow = flow();
        let reply = fixture.run("parent", &flow);
        assert!(reply.ok, "{reply:?}");
        assert_eq!(fixture.effects, ["launch", "resize", "move", "prompt"]);
        assert_eq!(fixture.polls, 2);
        assert_eq!(reply.data.as_ref().unwrap()["geometry"]["rect"]["x"], 200);
        assert!(fixture.run("parent", &flow).ok);
        assert_eq!(fixture.effects.len(), 4);
        assert!(crate::control_journal::inspect(&fixture.root, "read", "parent").ok);
        let mut changed = flow;
        changed.prompt = Some("different".into());
        assert_eq!(fixture.run("parent", &changed).exit_code(), 5);
        for file in std::fs::read_dir(&fixture.root).unwrap() {
            let p = file.unwrap().path();
            if p.extension().is_some_and(|s| s == "json") {
                assert!(!std::fs::read_to_string(p).unwrap().contains("Hello world"));
            }
        }
    }
    #[test]
    fn configured_launch_partial_unknown_is_inspectable_and_never_replayed() {
        let mut fixture = Fixture::new();
        fixture.fail = Some("prompt");
        let flow = flow();
        let reply = fixture.run("partial", &flow);
        assert!(!reply.ok);
        assert_eq!(reply.error.as_ref().unwrap().outcome, "unknown");
        assert_eq!(reply.data.as_ref().unwrap()["id"], "card");
        assert_eq!(reply.data.as_ref().unwrap()["failedStep"], "prompt");
        let child = reply.data.as_ref().unwrap()["requestIds"]["prompt"]
            .as_str()
            .unwrap();
        assert!(crate::control_journal::inspect(&fixture.root, "read", child).ok);
        assert!(!fixture.run("partial", &flow).ok);
        assert_eq!(fixture.effects.iter().filter(|s| *s == "prompt").count(), 1);
    }
    #[test]
    fn configured_launch_refuses_old_daemon_and_changed_pane_without_input() {
        let mut old = Fixture::new();
        old.old = true;
        assert!(!old.run("old", &flow()).ok);
        assert!(old.effects.is_empty());
        let mut changed = Fixture::new();
        changed.fail = Some("pane");
        assert_eq!(changed.run("changed", &flow()).exit_code(), 5);
        assert_eq!(changed.effects, ["launch", "resize", "move"]);
        let mut timed = Fixture::new();
        timed.fail = Some("readiness");
        let mut flow = flow();
        flow.ready_timeout_ms = 20;
        let reply = timed.run("timed", &flow);
        assert_eq!(reply.exit_code(), 7);
        assert_eq!(reply.error.unwrap().outcome, "partial");
        assert!(!timed.effects.iter().any(|s| s == "prompt"));
    }
    #[test]
    fn pending_workflow_allows_children_but_never_a_second_parent() {
        let fixture = Fixture::new();
        let payload = json!({"test":"reservation"});
        let reply = crate::control_journal::execute_workflow(
            &fixture.root,
            "parent",
            &payload,
            |_| "child".into(),
            |_| {
                let again = crate::control_journal::execute_workflow(
                    &fixture.root,
                    "parent",
                    &payload,
                    |_| panic!(),
                    |_| panic!("pending parent repeated"),
                );
                assert_eq!(again.exit_code(), 7);
                crate::control_journal::execute_operation(
                    &fixture.root,
                    "child",
                    &payload,
                    |_| "target".into(),
                    |_| Reply::success("child", json!({})),
                )
            },
        );
        assert!(reply.ok);
    }
}
