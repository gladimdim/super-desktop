//! Opens an AI agent's card in a plugin folder and, when asked, types the
//! user's request into it once the agent is ready, through the same path a
//! prompt from the phone takes. The request is never put on the command
//! line: a card's command is reused when it is resumed later.
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(400);
/// For an agent whose prompt box cannot be read: the screen has to be still
/// for this long, and at least `QUIET_START` after launch.
const QUIET: Duration = Duration::from_millis(1500);
const QUIET_START: Duration = Duration::from_secs(4);

/// Opens `harness` in `folder`; types `request` when given. Returns at once.
pub fn open(harness: &str, folder: &std::path::Path, request: Option<String>) -> Result<(), String> {
    let hooks = super::api::hooks().ok_or("SUPER DESKTOP is still starting")?;
    let folder = folder.to_str().ok_or("the plugin folder's path is not UTF-8")?;
    let session = (hooks.open_card)(harness, folder)?;
    if let Some(request) = request.filter(|r| !r.trim().is_empty()) {
        let harness = harness.to_owned();
        std::thread::Builder::new()
            .name("sd-plugin-request".into())
            .spawn(move || {
                if let Err(e) = type_when_ready(&session, &harness, &request) {
                    eprintln!("SUPER DESKTOP: the plugin request was not typed into {session}: {e}");
                }
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn type_when_ready(session: &str, harness: &str, request: &str) -> Result<(), String> {
    let start = Instant::now();
    let mut control: Option<crate::tmux_control::Control> = None;
    let mut last = String::new();
    let mut still_since = Instant::now();
    loop {
        if start.elapsed() > READY_TIMEOUT {
            return Err("the agent did not become ready in time".into());
        }
        std::thread::sleep(POLL);
        if !crate::tmux::session_exists(session) {
            continue;
        }
        let status = crate::tmux::inspect_status(session, harness);
        if status.status == "EXITED" {
            return Err("the agent exited".into());
        }
        if control.is_none() {
            control = crate::tmux_control::Control::open(session).ok();
        }
        let Some(screen) = control.as_mut().and_then(|c| c.capture().ok()) else {
            control = None;
            continue;
        };
        let ready = match crate::prompt_attachments::composer_has_draft(&status.cmd, &screen) {
            // An empty prompt box. (A trust question or a menu reads as a
            // draft: wait for the user to answer it.)
            Some(false) => true,
            Some(true) => false,
            None => {
                if screen != last {
                    last = screen;
                    still_since = Instant::now();
                }
                !last.trim().is_empty() && start.elapsed() > QUIET_START && still_since.elapsed() > QUIET
            }
        };
        if ready {
            drop(control);
            let request_id = format!("plugin-{}", start.elapsed().as_nanos());
            return crate::prompt_attachments::submit(session, "plugin-create", request, &request_id, &[], || true);
        }
    }
}
