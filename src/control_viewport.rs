//! Temporary local sizing clients. No persistent tmux options are changed.
use crate::control::{Command, Reply, Request, ViewportAction as Action};
use crate::control_close::{Target, UiResult};
use crate::control_terminal::probe;
use crate::desktop_protocol::TerminalSize;
use crate::state::TerminalData;
use crate::tmux_control::Control;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
struct State {
    control: Option<Control>,
    expires: Instant,
    requested: TerminalSize,
    effective: TerminalSize,
}
struct Lease {
    id: String,
    card: TerminalData,
    identity: String,
    state: Mutex<State>,
}
static LEASES: OnceLock<Mutex<BTreeMap<String, Arc<Lease>>>> = OnceLock::new();
fn leases() -> &'static Mutex<BTreeMap<String, Arc<Lease>>> {
    LEASES.get_or_init(Mutex::default)
}
fn describe(lease: &Lease, state: &State) -> Value {
    json!({"leaseId":lease.id,"id":lease.card.id,"paneIdentity":lease.identity,
    "requested":state.requested,"effective":state.effective,"remainingMs":state.expires.saturating_duration_since(Instant::now()).as_millis(),
    "active":state.control.is_some(),"exclusive":false,"expiryCleanup":"background-client-detach","persistentOptionsChanged":false,"effectiveGridSource":"last-lease-operation","liveGridCommand":"terminal runtime"})
}
fn drop_lease(lease: &Lease) {
    let control = lease.state.lock().unwrap().control.take();
    drop(control);
    leases().lock().unwrap().remove(&lease.id);
}
fn watch(lease: Arc<Lease>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(250));
        let mut state = match lease.state.try_lock() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if state.control.is_none() {
            drop(state);
            leases().lock().unwrap().remove(&lease.id);
            break;
        }
        if Instant::now() >= state.expires || !state.control.as_ref().unwrap().is_healthy() {
            drop(state);
            drop_lease(&lease);
            break;
        }
        let alive = probe(
            &lease.card.session_name,
            Instant::now() + Duration::from_millis(500),
        )
        .is_ok_and(|p| !p.dead && p.identity == lease.identity);
        if !alive {
            let client = state.control.take();
            drop(state);
            drop(client);
            leases().lock().unwrap().remove(&lease.id);
            break;
        }
    });
}
fn matching(control: &mut Control, card: &TerminalData, identity: &str, deadline: Instant) -> bool {
    let Ok(pane) = probe(&card.session_name, deadline) else {
        return false;
    };
    if pane.dead || pane.identity != identity {
        return false;
    }
    let expected = format!(
        "{} {} {} {} {} 0",
        card.session_name, pane.session_id, pane.pane_id, pane.pid, pane.server_pid
    );
    control
        .pane_format("#{session_name} #{session_id} #{pane_id} #{pane_pid} #{pid} #{window_linked}")
        .is_ok_and(|v| v.trim() == expected)
        && control.phone_viewport_available().unwrap_or(false)
}
pub fn execute(
    root: &std::path::Path,
    request: &Request,
    deadline: Instant,
    mut target: impl FnMut() -> Result<UiResult, ()>,
) -> Reply {
    let fail = |code, message| Reply::failure(&request.request_id, code, message);
    if let Command::Viewports { id } = &request.command {
        let list = leases()
            .lock()
            .unwrap()
            .values()
            .filter(|l| l.card.id == *id)
            .cloned()
            .collect::<Vec<_>>();
        return Reply::success(
            &request.request_id,
            json!({"id":id,"leases":list.iter().map(|l|match l.state.try_lock(){Ok(s)=>describe(l,&s),Err(_)=>json!({"leaseId":l.id,"id":l.card.id,"paneIdentity":l.identity,"state":"busy"})}).collect::<Vec<_>>(),"scope":"local-cli"}),
        );
    }
    crate::control_journal::execute(root, request, |_| {
        let Command::Viewport {
            id,
            action,
            expect_pane_identity,
            ..
        } = &request.command
        else {
            return fail("invalid_request", "Expected viewport operation.");
        };
        if let Action::Release { lease } = action {
            let existing = leases().lock().unwrap().get(lease).cloned();
            let Some(existing) = existing.filter(|l| l.card.id == *id) else {
                return fail("not_found", "No local CLI lease has that ID for this card.");
            };
            drop_lease(&existing);
            return Reply::success(
                &request.request_id,
                json!({"id":id,"leaseId":lease,"outcome":"released","persistentOptionsChanged":false}),
            );
        }
        let (columns, rows, ttl) = match action {
            Action::Acquire { columns, rows, ttl }
            | Action::Set {
                columns, rows, ttl, ..
            } => (*columns, *rows, *ttl),
            _ => unreachable!(),
        };
        if !(20..=500).contains(&columns) || !(5..=300).contains(&rows) || !(1..=300).contains(&ttl)
        {
            return fail(
                "invalid_arguments",
                "Use 20..500 columns, 5..300 rows and 1..300 seconds TTL.",
            );
        }
        let Some(identity) = expect_pane_identity
            .as_ref()
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            return fail(
                "invalid_arguments",
                "Copy paneIdentity from terminal runtime.",
            );
        };
        let owner = match target() {
            Ok(Ok(Some(t))) => t,
            Ok(Err(r)) => return r,
            _ => return fail("timeout", "Viewport target lookup failed."),
        };
        if owner.data.id != *id {
            return fail("conflict", "Card identity changed.");
        }
        let task = Arc::clone(&owner.task);
        task.with_idle_until(deadline,||{
            if Instant::now()>=deadline{return fail("timeout","Viewport request expired.");}
            match action {
                Action::Acquire {..}=>{
                    {let map=leases().lock().unwrap();if map.values().any(|l|l.card.id==*id){return fail("conflict","This card already has a local CLI viewport lease.");}if map.len()>=4{return fail("busy","At most four local viewport leases may be active.");}}
                    let before=match probe(&owner.data.session_name,deadline){Ok(p)if !p.dead&&p.identity==*identity=>p,_=>return fail("conflict","Pane identity changed or the pane exited.")};
                    let mut control=match Control::open_cli(&owner.data.session_name){Ok(c)=>c,Err(_)=>return fail("unavailable","Cannot attach a local sizing client.")};
                    if !matching(&mut control,&owner.data,identity,deadline){return fail("unsupported_terminal","Viewport requires the same live single unlinked pane/window and latest sizing policy.");}
                    match target(){Ok(Ok(Some(t)))if same(&owner,&t)=>{},Ok(Err(r))=>return r,_=>return fail("conflict","Card changed before sizing.")}
                    if Instant::now()>=deadline{return fail("timeout","Viewport expired before resizing.");}
                    if control.set_phone_viewport(columns,rows).is_err(){return Reply::unknown(&request.request_id);}
                    let effective=match control.pane_grid(){Ok(Some(g))=>g,_=>return Reply::unknown(&request.request_id)};
                    if !probe(&owner.data.session_name,Instant::now()+Duration::from_millis(500)).is_ok_and(|p|p.identity==before.identity&&!p.dead){return Reply::unknown(&request.request_id);}
                    let lease=Arc::new(Lease {id:format!("viewport-{}",request.request_id),card:owner.data.clone(),identity:identity.clone(),state:Mutex::new(State {control:Some(control),expires:Instant::now()+Duration::from_secs(ttl.into()),requested:TerminalSize {columns,rows},effective})});
                    let data=describe(&lease,&lease.state.lock().unwrap());leases().lock().unwrap().insert(lease.id.clone(),lease.clone());watch(lease);
                    Reply::success(&request.request_id,data)
                },
                Action::Set {lease,..}=>{
                    let current=leases().lock().unwrap().get(lease).cloned();let Some(lease)=current.filter(|l|l.card.id==*id&&l.identity==*identity&&l.card.created_at==owner.data.created_at)else{return fail("not_found","Lease missing, expired or belongs to another pane.");};
                    let mut state=match lease.state.try_lock(){Ok(s)=>s,Err(_)=>return fail("busy","Viewport lease is being checked; no resize attempted.")};
                    if Instant::now()>=state.expires{return fail("not_found","Lease expired.");}
                    let Some(control)=state.control.as_mut()else{return fail("not_found","Lease detached.");};
                    if !matching(control,&owner.data,identity,deadline){return fail("conflict","Pane or sizing policy changed.");}
                    match target(){Ok(Ok(Some(t)))if same(&owner,&t)=>{},Ok(Err(r))=>return r,_=>return fail("conflict","Card changed before resizing.")}
                    if Instant::now()>=deadline{return fail("timeout","Viewport expired before resizing.");}
                    if control.set_phone_viewport(columns,rows).is_err(){return Reply::unknown(&request.request_id);}
                    let effective=match control.pane_grid(){Ok(Some(g))=>g,_=>return Reply::unknown(&request.request_id)};
                    if !probe(&owner.data.session_name,Instant::now()+Duration::from_millis(500)).is_ok_and(|p|p.identity==*identity&&!p.dead){let client=state.control.take();drop(client);return Reply::unknown(&request.request_id);}
                    state.requested=TerminalSize {columns,rows};state.effective=effective;state.expires=Instant::now()+Duration::from_secs(ttl.into());
                    Reply::success(&request.request_id,describe(&lease,&state))
                },_=>unreachable!()
            }
        }).unwrap_or_else(||fail("conflict","Terminal preparation or close is in progress."))
    })
}
fn same(a: &Target, b: &Target) -> bool {
    a.data.id == b.data.id
        && a.data.session_name == b.data.session_name
        && a.data.created_at == b.data.created_at
        && Arc::ptr_eq(&a.task, &b.task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command as Process, Stdio};
    #[test]
    fn viewport_owns_only_its_client_and_expires() {
        if std::env::var_os("SD_VIEWPORT_TEST").is_none() {
            let root = std::env::temp_dir().join(format!("sd-viewport-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let output = Process::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "control_viewport::tests::viewport_owns_only_its_client_and_expires",
                    "--nocapture",
                ])
                .env("SD_VIEWPORT_TEST", "1")
                .env("HOME", &root)
                .env("TMUX_TMPDIR", &root)
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .output()
                .unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(output.status.success(), "{output:?}");
            return;
        }
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Process::new("tmux")
                    .args(["kill-server"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _cleanup = Cleanup;
        let tmux = |args: &[&str]| {
            let output = Process::new("tmux").args(args).output().unwrap();
            assert!(output.status.success(), "{args:?}: {output:?}");
            String::from_utf8(output.stdout).unwrap()
        };
        let session = "sd_term_viewport";
        tmux(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            "80",
            "-y",
            "24",
            "sleep 60",
        ]);
        tmux(&["set-window-option", "-t", session, "window-size", "latest"]);
        let deadline = || Instant::now() + Duration::from_secs(3);
        let before = probe(session, deadline()).unwrap();
        let card:TerminalData=serde_json::from_value(json!({"id":session,"session_name":session,"agent_type":"shell","command":"sleep 60","x":10,"y":100,"width":500,"height":300,"iconified":false,"created_at":1.0})).unwrap();
        let task = Arc::new(crate::session_task::SessionTask::default());
        let target = || {
            Ok(Ok(Some(Target {
                data: card.clone(),
                task: task.clone(),
            })))
        };
        let mut desktop = Control::open_cli(session).unwrap();
        desktop.set_phone_viewport(80, 24).unwrap();
        let request = |action, name: &str| Request {
            control_version: 1,
            request_id: name.into(),
            command: Command::Viewport {
                id: session.into(),
                action,
                expect_epoch: Some("epoch".into()),
                expect_revision: Some("a".repeat(64)),
                expect_pane_identity: Some(before.identity.clone()),
            },
        };
        let root = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("journal");
        let acquire = request(
            Action::Acquire {
                columns: 160,
                rows: 40,
                ttl: 10,
            },
            "acquire",
        );
        let reply = execute(&root, &acquire, deadline(), target);
        assert!(reply.ok, "{reply:?}");
        let lease = reply.data.unwrap()["leaseId"].as_str().unwrap().to_string();
        assert_eq!(
            desktop.pane_grid().unwrap().unwrap(),
            TerminalSize {
                columns: 160,
                rows: 40
            }
        );
        assert_eq!(
            probe(session, deadline()).unwrap().identity,
            before.identity
        );
        assert!(
            execute(&root, &acquire, deadline(), || panic!(
                "duplicate must not attach"
            ))
            .ok
        );
        let resized = execute(
            &root,
            &request(
                Action::Set {
                    lease: lease.clone(),
                    columns: 120,
                    rows: 35,
                    ttl: 10,
                },
                "set",
            ),
            deadline(),
            target,
        );
        assert!(resized.ok, "{resized:?}");
        assert_eq!(
            desktop.pane_grid().unwrap().unwrap(),
            TerminalSize {
                columns: 120,
                rows: 35
            }
        );
        let released = execute(
            &root,
            &request(Action::Release { lease }, "release"),
            deadline(),
            || panic!("release must not depend on card existence"),
        );
        assert!(released.ok);
        assert_eq!(
            desktop.pane_grid().unwrap().unwrap(),
            TerminalSize {
                columns: 80,
                rows: 24
            }
        );
        let expires = execute(
            &root,
            &request(
                Action::Acquire {
                    columns: 100,
                    rows: 30,
                    ttl: 1,
                },
                "expires",
            ),
            deadline(),
            target,
        );
        assert!(expires.ok, "{expires:?}");
        let until = Instant::now() + Duration::from_secs(4);
        while !leases().lock().unwrap().is_empty() {
            assert!(Instant::now() < until, "lease did not expire");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            desktop.pane_grid().unwrap().unwrap(),
            TerminalSize {
                columns: 80,
                rows: 24
            }
        );
        assert_eq!(
            probe(session, deadline()).unwrap().identity,
            before.identity
        );
        assert!(
            execute(
                &root,
                &request(
                    Action::Acquire {
                        columns: 100,
                        rows: 30,
                        ttl: 1
                    },
                    "expires"
                ),
                deadline(),
                || panic!("expired receipt must not reacquire")
            )
            .ok
        );
        assert!(leases().lock().unwrap().is_empty());
        let mut wrong = request(
            Action::Acquire {
                columns: 100,
                rows: 30,
                ttl: 1,
            },
            "wrong",
        );
        if let Command::Viewport {
            expect_pane_identity,
            ..
        } = &mut wrong.command
        {
            *expect_pane_identity = Some("0".repeat(64));
        }
        assert_eq!(execute(&root, &wrong, deadline(), target).exit_code(), 5);
        tmux(&["set-window-option", "-t", session, "window-size", "manual"]);
        assert_eq!(
            execute(
                &root,
                &request(
                    Action::Acquire {
                        columns: 100,
                        rows: 30,
                        ttl: 1
                    },
                    "manual"
                ),
                deadline(),
                target
            )
            .exit_code(),
            6
        );
        assert!(leases().lock().unwrap().is_empty());
        assert_eq!(
            tmux(&["show-window-options", "-t", session, "-v", "window-size"]).trim(),
            "manual"
        );
        tmux(&["set-window-option", "-t", session, "window-size", "latest"]);
        assert!(
            execute(
                &root,
                &request(
                    Action::Acquire {
                        columns: 100,
                        rows: 30,
                        ttl: 30
                    },
                    "replaced"
                ),
                deadline(),
                target
            )
            .ok
        );
        tmux(&["respawn-pane", "-k", "-t", session, "sleep 60"]);
        let until = Instant::now() + Duration::from_secs(4);
        while !leases().lock().unwrap().is_empty() {
            assert!(Instant::now() < until, "replaced pane retained a lease");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_ne!(
            probe(session, deadline()).unwrap().identity,
            before.identity
        );
    }
}
