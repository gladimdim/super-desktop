//! Guarded local close. No legacy name-based cleanup is scheduled.
use crate::control::{Command, Reply, Request};
use crate::control_terminal::{probe, read_process, Failure, Pane};
use crate::state::TerminalData;
use serde_json::json;
use std::path::Path;
use std::process::Command as Process;
use std::sync::Arc;
use std::time::Instant;

pub struct Target {
    pub data: TerminalData,
    pub task: Arc<crate::session_task::SessionTask>,
}

pub enum Action {
    Inspect,
    Remove(Target),
}

pub type UiResult = Result<Option<Target>, Reply>;
pub struct Query {
    pub request: Request,
    pub action: Action,
    pub responder: std::sync::mpsc::SyncSender<UiResult>,
    pub deadline: Instant,
}

const MARKER: &str = "@super_desktop_cli_close";
const CONFLICT: Failure = (
    "conflict",
    "The live terminal identity or session layout changed. Nothing was closed.",
);

pub fn execute(
    root: &Path,
    request: &Request,
    deadline: Instant,
    mut ui: impl FnMut(Action) -> Result<UiResult, ()>,
) -> Reply {
    crate::control_journal::execute(root, request, |_| {
        let Command::Close {
            expect_pane_identity,
            ..
        } = &request.command
        else {
            return Reply::failure(
                &request.request_id,
                "invalid_request",
                "Expected terminal close.",
            );
        };
        if expect_pane_identity.len() != 64
            || !expect_pane_identity.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Reply::failure(
                &request.request_id,
                "invalid_arguments",
                "Copy paneIdentity from terminal runtime.",
            );
        }
        let target = match ui(Action::Inspect) {
            Ok(Ok(Some(target))) => target,
            Ok(Err(reply)) => return reply,
            _ => {
                return Reply::failure(
                    &request.request_id,
                    "timeout",
                    "Close target lookup failed before removal.",
                )
            }
        };
        let task = Arc::clone(&target.task);
        task.with_idle_until(deadline, || {
            let fail = |(code, message)| Reply::failure(&request.request_id, code, message);
            let session = &target.data.session_name;
            if !session.starts_with("sd_term_") || session.len() > 128
                || !session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return fail(("unsupported_terminal", "The card does not identify a supported local session."));
            }
            let before = match probe(session, deadline) {
                Ok(pane) => pane,
                Err(error) => return fail(error),
            };
            if &before.identity != expect_pane_identity { return fail(CONFLICT); }
            let nonce = match crate::control::new_request(Command::Status {}) {
                Ok(request) => request.request_id,
                Err(_) => return fail(("unavailable", "OS randomness is unavailable.")),
            };
            // Bind this operation to the existing server/session, then repeat
            // the /proc identity probe. A restarted server cannot inherit the
            // nonce, even when it recycles tmux's numeric session/pane IDs.
            match guarded_command(session, &before, &nonce, false, deadline) {
                Ok(true) => {},
                Ok(false) => return fail(CONFLICT),
                Err(error) => return fail(error),
            }
            match probe(session, deadline) {
                Ok(current) if current.identity == before.identity && current.dead == before.dead => {},
                Ok(_) => return fail(CONFLICT),
                Err(error) => return fail(error),
            }
            if Instant::now() >= deadline { return fail(("timeout", "Close expired before card removal.")); }
            // Revalidate the card on GTK, cancel queued preparation, detach and
            // remove it before killing. No callback can subsequently recreate
            // its session. A failure after this point is a partial/unknown
            // outcome, never permission to kill by name or replay the close.
            match ui(Action::Remove(Target { data: target.data.clone(), task: Arc::clone(&target.task) })) {
                Ok(Ok(None)) => {},
                Ok(Err(reply)) => return reply,
                _ => return Reply::unknown(&request.request_id),
            }
            finish_removed(request, session, &before, &nonce, deadline)
        }).unwrap_or_else(|| Reply::failure(&request.request_id, "conflict", "Terminal preparation is busy or the card is already closing; no CLI close was applied."))
    })
}

fn finish_removed(
    request: &Request,
    session: &str,
    before: &Pane,
    nonce: &str,
    deadline: Instant,
) -> Reply {
    if !matches!(
        guarded_command(session, before, nonce, true, deadline),
        Ok(true)
    ) {
        let mut reply = Reply::unknown(&request.request_id);
        reply.error.as_mut().unwrap().message = "The card was removed, but closing its exact tmux session was not confirmed. The session may still run. Inspect this request; do not retry under a new ID or kill a replacement by name.".into();
        return reply;
    }
    if crate::state::flush_state_saves_checked().is_err() {
        return Reply::unknown(&request.request_id);
    }
    Reply::success(
        &request.request_id,
        json!({"id":card_id(request),"sessionName":session,
        "sessionId":before.session_id,"paneIdentity":before.identity,"outcome":"closed",
        "cardRemoved":true,"sessionClosed":true,"processExitObserved":false}),
    )
}

pub fn card_id(request: &Request) -> &str {
    match &request.command {
        Command::Close { id, .. } => id,
        _ => "",
    }
}

fn all(terms: impl IntoIterator<Item = String>) -> String {
    terms
        .into_iter()
        .reduce(|left, right| format!("#{{&&:{left},{right}}}"))
        .unwrap()
}

/// All values interpolated here are validated names, numeric IDs, or random
/// hexadecimal tokens generated by this process. No shell command is invoked:
/// tmux evaluates -F and executes its own command list without an async wait.
fn guarded_command(
    session: &str,
    pane: &Pane,
    nonce: &str,
    kill: bool,
    deadline: Instant,
) -> Result<bool, Failure> {
    let mut terms = vec![
        format!("#{{==:#{{pid}},{}}}", pane.server_pid),
        format!("#{{==:#{{session_name}},{session}}}"),
        format!("#{{==:#{{session_id}},{}}}", pane.session_id),
        format!("#{{==:#{{pane_id}},{}}}", pane.pane_id),
        format!("#{{==:#{{pane_pid}},{}}}", pane.pid),
        format!("#{{==:#{{pane_dead}},{}}}", u8::from(pane.dead)),
        "#{==:#{session_windows},1}".into(),
        "#{==:#{window_panes},1}".into(),
        "#{==:#{window_linked},0}".into(),
    ];
    if kill {
        terms.push(format!("#{{==:#{{{MARKER}}},{nonce}}}"));
    }
    let command = if kill {
        format!(
            "kill-session -t '{}' ; display-message -p SD_CLOSED",
            pane.session_id
        )
    } else {
        format!(
            "set-option -t '{}' {MARKER} {nonce} ; display-message -p SD_PREPARED",
            pane.session_id
        )
    };
    let mut process = Process::new(crate::tmux::tmux_bin());
    process.args([
        "-N",
        "if-shell",
        "-F",
        "-t",
        &pane.pane_id,
        &all(terms),
        &command,
        "display-message -p SD_CONFLICT",
    ]);
    let output = read_process(process, 1024, deadline)?;
    if output.limited {
        return Err(("unavailable", "Unexpected close acknowledgement."));
    }
    match output.bytes.as_slice() {
        b"SD_PREPARED\n" if !kill => Ok(true),
        b"SD_CLOSED\n" if kill => Ok(true),
        b"SD_CONFLICT\n" => Ok(false),
        _ => Err(("unavailable", "Unexpected close acknowledgement.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Duration;
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(3)
    }
    fn run(args: &[&str]) -> String {
        let output = Process::new("tmux").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn alive(session: &str) -> bool {
        Process::new("tmux")
            .args(["has-session", "-t", &format!("={session}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }
    fn create(session: &str) -> Target {
        run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            session,
            "/bin/sleep 120",
        ]);
        Target { data: serde_json::from_value(json!({"id":session,"session_name":session,"agent_type":"shell",
            "command":"/bin/sleep 120","x":20,"y":80,"width":640,"height":480,"created_at":1.0,"tag":0})).unwrap(),
            task: Arc::new(crate::session_task::SessionTask::default()) }
    }
    fn request(target: &Target, id: &str) -> Request {
        Request {
            control_version: 1,
            request_id: id.into(),
            command: Command::Close {
                id: target.data.id.clone(),
                expect_epoch: "epoch".into(),
                expect_revision: "a".repeat(64),
                expect_pane_identity: probe(&target.data.session_name, deadline())
                    .unwrap()
                    .identity,
            },
        }
    }
    fn inspect(target: &Target) -> UiResult {
        Ok(Some(Target {
            data: target.data.clone(),
            task: Arc::clone(&target.task),
        }))
    }
    #[test]
    fn cli_close_private_tmux_integration() {
        let root = std::env::temp_dir().join(format!("sd-close-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("tmux")).unwrap();
        let output = Process::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "control_close::tests::cli_close_tmux_inner",
                "--nocapture",
            ])
            .env("SD_CLI_CLOSE_ROOT", &root)
            .env("TMUX_TMPDIR", root.join("tmux"))
            .env("HOME", &root)
            .env("XDG_STATE_HOME", root.join("state"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .env_remove("LD_PRELOAD")
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    fn cli_close_tmux_inner() {
        let Some(root) = std::env::var_os("SD_CLI_CLOSE_ROOT").map(std::path::PathBuf::from) else {
            return;
        };
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Process::new("tmux")
                    .arg("kill-server")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _cleanup = Cleanup;
        let keeper = create("sd_term_keeper");
        let journal = root.join("journal");

        let target = create("sd_term_close");
        let r = request(&target, "close-1");
        let mut removed = false;
        let reply = execute(&journal, &r, deadline(), |action| {
            Ok(match action {
                Action::Inspect => inspect(&target),
                Action::Remove(data) => {
                    assert_eq!(data.data.id, target.data.id);
                    removed = true;
                    target.task.close();
                    Ok(None)
                }
            })
        });
        assert!(reply.ok, "{reply:?}");
        assert!(removed && !alive(&target.data.session_name) && alive(&keeper.data.session_name));
        assert!(target.task.is_closed());
        assert!(!target
            .task
            .prepare(|| panic!("closed card cannot resurrect")));
        let replacement = create("sd_term_close");
        assert!(
            execute(&journal, &r, deadline(), |_| panic!(
                "receipt replay must not close replacement"
            ))
            .ok
        );
        assert!(alive(&replacement.data.session_name));
        let mut changed = r.clone();
        if let Command::Close {
            expect_pane_identity,
            ..
        } = &mut changed.command
        {
            *expect_pane_identity = "0".repeat(64);
        }
        assert_eq!(
            execute(&journal, &changed, deadline(), |_| panic!(
                "changed receipt payload"
            ))
            .exit_code(),
            5
        );
        assert_eq!(
            crate::control_journal::inspect(&journal, "read", "close-1")
                .data
                .unwrap()["cardId"],
            target.data.id
        );

        let stale = create("sd_term_stale");
        let r = request(&stale, "stale");
        run(&[
            "respawn-pane",
            "-k",
            "-t",
            "=sd_term_stale:",
            "/bin/sleep 120",
        ]);
        let reply = execute(&journal, &r, deadline(), |action| {
            Ok(match action {
                Action::Inspect => inspect(&stale),
                _ => panic!("stale target must not remove card"),
            })
        });
        assert_eq!(reply.exit_code(), 5, "{reply:?}");
        assert!(alive(&stale.data.session_name));

        let refused = create("sd_term_refused");
        let r = request(&refused, "revision-conflict");
        let reply = execute(&journal, &r, deadline(), |action| {
            Ok(match action {
                Action::Inspect => inspect(&refused),
                Action::Remove(_) => Err(Reply::failure(
                    &r.request_id,
                    "conflict",
                    "stale GTK revision",
                )),
            })
        });
        assert_eq!(reply.exit_code(), 5);
        assert!(alive(&refused.data.session_name) && !refused.task.is_closed());

        let multiple = create("sd_term_multiple");
        let r = request(&multiple, "multiple");
        run(&[
            "split-window",
            "-d",
            "-t",
            "=sd_term_multiple:",
            "/bin/sleep 120",
        ]);
        let reply = execute(&journal, &r, deadline(), |action| {
            Ok(match action {
                Action::Inspect => inspect(&multiple),
                _ => panic!("multiple panes"),
            })
        });
        assert_eq!(reply.error.unwrap().code, "unsupported_terminal");
        assert!(alive(&multiple.data.session_name));

        let linked = create("sd_term_linked");
        let r = request(&linked, "linked");
        run(&[
            "link-window",
            "-s",
            "=sd_term_linked:0",
            "-t",
            "=sd_term_keeper:5",
        ]);
        let reply = execute(&journal, &r, deadline(), |action| {
            Ok(match action {
                Action::Inspect => inspect(&linked),
                _ => panic!("shared window"),
            })
        });
        assert_eq!(reply.exit_code(), 5, "{reply:?}");
        assert!(alive(&linked.data.session_name));

        let partial = create("sd_term_partial");
        let r = request(&partial, "partial");
        let reply = execute(&journal, &r, deadline(), |action| {
            Ok(match action {
                Action::Inspect => inspect(&partial),
                Action::Remove(_) => {
                    partial.task.close();
                    run(&["kill-session", "-t", "=sd_term_partial"]);
                    create("sd_term_partial");
                    Ok(None)
                }
            })
        });
        assert_eq!(reply.error.unwrap().outcome, "unknown");
        assert!(alive("sd_term_partial"));
        assert_eq!(
            execute(&journal, &r, deadline(), |_| panic!(
                "unknown must not replay"
            ))
            .error
            .unwrap()
            .outcome,
            "unknown"
        );

        let lost = create("sd_term_lost");
        let r = request(&lost, "lost-ui-reply");
        let reply = execute(&journal, &r, deadline(), |action| match action {
            Action::Inspect => Ok(inspect(&lost)),
            Action::Remove(_) => {
                lost.task.close();
                Err(())
            }
        });
        assert_eq!(reply.error.unwrap().outcome, "unknown");
        assert!(alive("sd_term_lost"));

        let missing = create("sd_term_missing");
        let r = request(&missing, "missing");
        run(&["kill-session", "-t", "=sd_term_missing"]);
        let reply = execute(&journal, &r, deadline(), |action| {
            Ok(match action {
                Action::Inspect => inspect(&missing),
                _ => panic!("missing session must not remove card"),
            })
        });
        assert_eq!(reply.exit_code(), 3);

        let dead = create("sd_term_dead");
        run(&[
            "set-option",
            "-w",
            "-t",
            "sd_term_dead",
            "remain-on-exit",
            "on",
        ]);
        run(&["respawn-pane", "-k", "-t", "=sd_term_dead:", "/bin/true"]);
        let until = deadline();
        while !probe("sd_term_dead", deadline()).is_ok_and(|pane| pane.dead) {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let r = request(&dead, "dead");
        let reply = execute(&journal, &r, deadline(), |action| {
            Ok(match action {
                Action::Inspect => inspect(&dead),
                Action::Remove(_) => {
                    dead.task.close();
                    Ok(None)
                }
            })
        });
        assert!(reply.ok, "{reply:?}");
        assert!(!alive("sd_term_dead"));

        let foreign = create("foreign_session");
        let r = request(&foreign, "foreign");
        let reply = execute(&journal, &r, deadline(), |action| {
            Ok(match action {
                Action::Inspect => inspect(&foreign),
                _ => panic!("foreign session"),
            })
        });
        assert_eq!(reply.error.unwrap().code, "unsupported_terminal");
        assert!(alive("foreign_session"));

        create("sd_term_late_respawn");
        let before = probe("sd_term_late_respawn", deadline()).unwrap();
        assert!(
            guarded_command("sd_term_late_respawn", &before, "nonce", false, deadline()).unwrap()
        );
        run(&[
            "respawn-pane",
            "-k",
            "-t",
            "=sd_term_late_respawn:",
            "/bin/sleep 120",
        ]);
        assert!(
            !guarded_command("sd_term_late_respawn", &before, "nonce", true, deadline()).unwrap()
        );
        assert!(alive("sd_term_late_respawn"));

        let final_one = create("sd_term_restart");
        let before = probe("sd_term_restart", deadline()).unwrap();
        assert!(guarded_command("sd_term_restart", &before, "nonce", false, deadline()).unwrap());
        let pid = before.server_pid;
        run(&["kill-server"]);
        let until = deadline();
        while let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            if stat
                .rsplit_once(')')
                .unwrap()
                .1
                .trim_start()
                .starts_with('Z')
            {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let restarted = create(&final_one.data.session_name);
        // Even with the current numeric IDs/PID substituted, an old nonce is
        // absent on the restarted server. The kill must remain refused.
        let current = probe("sd_term_restart", deadline()).unwrap();
        assert!(!guarded_command("sd_term_restart", &current, "nonce", true, deadline()).unwrap());
        assert!(alive("sd_term_restart"));
        let r = request(&restarted, "last-session");
        let reply = execute(&journal, &r, deadline(), |action| {
            Ok(match action {
                Action::Inspect => inspect(&restarted),
                Action::Remove(_) => {
                    restarted.task.close();
                    Ok(None)
                }
            })
        });
        assert!(reply.ok, "last session: {reply:?}");
        assert!(!alive("sd_term_restart"));
    }
}
