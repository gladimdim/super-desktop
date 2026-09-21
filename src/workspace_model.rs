//! The daemon's local workspace, independent of whichever machine is viewed.
//!
//! Existing local widgets temporarily share the state handle. Snapshot revisions
//! describe published content, not pointer-motion events: reads coalesce edits,
//! and identical content never advances the revision. Later command handlers
//! must publish current state before checking an expected revision.
use crate::desktop_protocol::{
    Canvas, CardLayout, DesktopCard, HarnessType, LocalWorkspaceSnapshot, TerminalSize,
};
use crate::state::AppState;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

pub const MAX_DESKTOP_CARDS: usize = 256;
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const RUNTIME_MAX_AGE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct CardPresentation {
    pub title: String,
    pub expanded: bool,
}

pub struct LocalWorkspace {
    state: Rc<RefCell<AppState>>,
    epoch: String,
    published: RefCell<Option<LocalWorkspaceSnapshot>>,
    runtime: RefCell<Option<RuntimePoller>>,
}

impl LocalWorkspace {
    pub fn new(state: AppState) -> Self {
        let mut bytes = [0; 16];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .expect("OS randomness required for workspace epoch");
        Self::with_epoch(state, bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    fn with_epoch(mut state: AppState, epoch: String) -> Self {
        crate::state::normalize_terminal_order(&mut state);
        Self {
            state: Rc::new(RefCell::new(state)),
            epoch,
            published: RefCell::new(None),
            runtime: RefCell::new(None),
        }
    }

    pub fn state(&self) -> Rc<RefCell<AppState>> {
        Rc::clone(&self.state)
    }

    pub fn snapshot(
        &self,
        canvas: Canvas,
        presentation: &HashMap<String, CardPresentation>,
    ) -> Result<LocalWorkspaceSnapshot, &'static str> {
        // No tmux, harness detection, path validation or disk reads on GTK.
        let runtime = self
            .runtime
            .borrow_mut()
            .get_or_insert_with(RuntimePoller::new)
            .current();
        self.snapshot_with_runtime(canvas, presentation, &runtime)
    }

    fn snapshot_with_runtime(
        &self,
        canvas: Canvas,
        presentation: &HashMap<String, CardPresentation>,
        runtime: &Runtime,
    ) -> Result<LocalWorkspaceSnapshot, &'static str> {
        let state = self.state.borrow();
        if state.terminals.len() > MAX_DESKTOP_CARDS {
            return Err("too_many_desktop_cards");
        }
        let home = crate::state::home_dir_string();
        let workspace = state.workspace_dir.clone().unwrap_or_else(|| home.clone());
        let harness_types: Vec<_> = crate::tmux::HARNESS_KEYS
            .iter()
            .map(|id| HarnessType {
                id: (*id).into(),
                name: crate::tmux::get_agent_config(id).name.into(),
                available: runtime.harnesses.as_ref().map(|keys| keys.contains(*id)),
            })
            .collect();
        let visible_harnesses = harness_types
            .iter()
            .filter(|h| {
                h.available != Some(false)
                    && state
                        .visible_harnesses
                        .as_ref()
                        .is_none_or(|keys| keys.contains(&h.id))
            })
            .map(|h| h.id.clone())
            .collect();
        let order: HashMap<_, _> = state
            .terminal_order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i as u32))
            .collect();
        let mut ids = HashSet::new();
        let mut cards = Vec::with_capacity(state.terminals.len());
        for (index, card) in state.terminals.iter().enumerate() {
            if !ids.insert(&card.id) {
                return Err("duplicate_card_id");
            }
            let view = presentation.get(&card.id);
            let pane = runtime
                .sessions
                .as_ref()
                .and_then(|sessions| sessions.get(&card.session_name));
            let alive = runtime
                .sessions
                .as_ref()
                .map(|_| pane.is_some_and(|p| p.alive));
            cards.push(DesktopCard {
                card_id: card.id.clone(),
                session_name: card.session_name.clone(),
                agent_type: card.agent_type.clone(),
                title: view
                    .map(|v| v.title.clone())
                    .unwrap_or_else(|| crate::tmux::get_agent_config(&card.agent_type).name.into()),
                status: match alive {
                    Some(true) => "RUNNING",
                    Some(false) => "EXITED",
                    None => "UNKNOWN",
                }
                .into(),
                session_alive: alive,
                workspace: card
                    .workspace_dir
                    .clone()
                    .unwrap_or_else(|| workspace.clone()),
                revision: 0,
                layout: CardLayout {
                    x: card.x,
                    y: card.y,
                    width: card.width.max(1) as u32,
                    height: card.height.max(1) as u32,
                    restored_width: card.restored_width.max(1) as u32,
                    restored_height: card.restored_height.max(1) as u32,
                    iconified: card.iconified,
                    icon_x: card.icon_x,
                    icon_y: card.icon_y,
                    tag: card.tag,
                },
                stacking_order: order.get(card.id.as_str()).copied().unwrap_or(index as u32),
                expanded: view.is_some_and(|v| v.expanded),
                terminal_size: pane.map(|p| p.size),
            });
        }
        cards.sort_by_key(|c| c.stacking_order);
        let mut next = LocalWorkspaceSnapshot {
            epoch: self.epoch.clone(),
            revision: 0,
            canvas,
            workspace,
            home_directory: home,
            visible_harnesses,
            harness_types,
            cards,
        };
        let mut published = self.published.borrow_mut();
        let next_revision = published
            .as_ref()
            .map_or(1, |previous| previous.revision + 1);
        for card in &mut next.cards {
            if let Some(old) = published
                .as_ref()
                .and_then(|p| p.cards.iter().find(|c| c.card_id == card.card_id))
            {
                card.revision = old.revision;
                if card != old {
                    card.revision = next_revision;
                }
            } else {
                card.revision = next_revision;
            }
        }
        next.revision = published.as_ref().map_or(0, |p| p.revision);
        if published.as_ref() != Some(&next) {
            next.revision = next_revision;
            *published = Some(next.clone());
        }
        Ok(next)
    }
}

#[derive(Clone, Debug)]
struct Pane {
    size: TerminalSize,
    alive: bool,
}

#[derive(Clone, Default)]
struct Runtime {
    // None means unknown/unavailable, never a fabricated exited session.
    sessions: Option<HashMap<String, Pane>>,
    harnesses: Option<HashSet<String>>,
}

struct RuntimePoller {
    request: SyncSender<()>,
    result: Receiver<(Instant, Runtime)>,
    cached: Runtime,
    updated: Option<Instant>,
    requested: Option<Instant>,
    in_flight: bool,
}

impl RuntimePoller {
    fn new() -> Self {
        let (request, requests) = mpsc::sync_channel(1);
        let (results, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("desktop-runtime".into())
            .spawn(move || {
                while requests.recv().is_ok() {
                    let runtime = collect_runtime();
                    if results.send((Instant::now(), runtime)).is_err() {
                        break;
                    }
                }
            })
            .expect("desktop runtime worker");
        Self {
            request,
            result,
            cached: Runtime::default(),
            updated: None,
            requested: None,
            in_flight: false,
        }
    }

    fn current(&mut self) -> Runtime {
        if let Ok((updated, runtime)) = self.result.try_recv() {
            self.cached = runtime;
            self.updated = Some(updated);
            self.in_flight = false;
        }
        if !self.in_flight
            && self
                .requested
                .is_none_or(|t| t.elapsed() >= REFRESH_INTERVAL)
        {
            if self.request.try_send(()).is_ok() {
                self.in_flight = true;
                self.requested = Some(Instant::now());
            }
        }
        if self.updated.is_some_and(|t| t.elapsed() <= RUNTIME_MAX_AGE) {
            self.cached.clone()
        } else {
            Runtime::default()
        }
    }
}

fn collect_runtime() -> Runtime {
    let mut command = Command::new(crate::tmux::tmux_bin());
    command.args(["list-panes", "-a", "-F", "#{session_name}\t#{window_active}\t#{pane_active}\t#{pane_width}\t#{pane_height}\t#{pane_dead}"])
        .env_remove("TMUX").env_remove("TMUX_PANE");
    Runtime {
        sessions: bounded_output(command).and_then(|bytes| parse_panes(&bytes)),
        harnesses: Some(
            crate::tmux::detect_harnesses()
                .iter()
                .map(|h| h.key.to_string())
                .collect(),
        ),
    }
}

fn parse_panes(bytes: &[u8]) -> Option<HashMap<String, Pane>> {
    let mut sessions = HashMap::new();
    for line in std::str::from_utf8(bytes).ok()?.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 6 {
            return None;
        }
        if !fields[0].starts_with("sd_term_") {
            continue;
        }
        if fields[1] != "1" || fields[2] != "1" {
            continue;
        }
        let size = TerminalSize {
            columns: fields[3].parse().ok()?,
            rows: fields[4].parse().ok()?,
        }
        .validate()
        .ok()?;
        sessions.insert(
            fields[0].into(),
            Pane {
                size,
                alive: fields[5] == "0",
            },
        );
    }
    Some(sessions)
}

/// A stuck tmux server must not accumulate workers or block GTK/IPC. Drain the
/// pipe while running (it can exceed pipe capacity), with output/time bounds.
fn bounded_output(mut command: Command) -> Option<Vec<u8>> {
    struct Probe(Child);
    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Probe(
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?,
    );
    let mut stdout = child.0.stdout.take()?;
    let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(750);
    let mut output = Vec::new();
    while Instant::now() < deadline {
        let mut chunk = [0; 8192];
        match stdout.read(&mut chunk) {
            Ok(0) => {
                if let Some(status) = child.0.try_wait().ok()? {
                    return status.success().then_some(output);
                }
            }
            Ok(n) => {
                output.extend_from_slice(&chunk[..n]);
                if output.len() > 512 * 1024 {
                    return None;
                }
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Canvas {
        Canvas {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            top_inset: 56,
        }
    }
    fn card(id: &str) -> crate::state::TerminalData {
        serde_json::from_value(serde_json::json!({"id":id,"session_name":format!("sd_term_{id}"),"agent_type":"shell","command":"DO_NOT_EXPORT", "x":100,"y":120,"created_at":0})).unwrap()
    }
    fn model() -> LocalWorkspace {
        let mut state = AppState::default();
        state.terminals = vec![card("a"), card("b")];
        LocalWorkspace::with_epoch(state, "epoch-one".into())
    }
    fn snapshot(model: &LocalWorkspace) -> LocalWorkspaceSnapshot {
        model
            .snapshot_with_runtime(canvas(), &HashMap::new(), &Runtime::default())
            .unwrap()
    }

    #[test]
    fn snapshots_read_live_shared_state_and_revision_only_changed_cards() {
        let model = model();
        let first = snapshot(&model);
        assert_eq!(first, snapshot(&model));
        model.state().borrow_mut().terminals[0].x = 400;
        let changed = snapshot(&model);
        assert_eq!(changed.revision, first.revision + 1);
        assert_eq!(changed.cards[0].revision, changed.revision);
        assert_eq!(changed.cards[1].revision, first.cards[1].revision);
        assert_eq!(changed.cards[0].layout.x, 400);
        // Local preferences and notes are not desktop-terminal content.
        model.state().borrow_mut().sleep_lock_on_ac = true;
        model.state().borrow_mut().notes[0].text = "PRIVATE_NOTE".into();
        assert_eq!(changed, snapshot(&model));
        let json = serde_json::to_string(&changed).unwrap();
        assert!(!json.contains("DO_NOT_EXPORT") && !json.contains("PRIVATE_NOTE"));
    }

    #[test]
    fn disappearance_and_reappearance_never_reuse_card_revisions() {
        let model = model();
        let first = snapshot(&model);
        let removed = model.state().borrow_mut().terminals.remove(0);
        let missing = snapshot(&model);
        assert_eq!(missing.cards.len(), 1);
        model.state().borrow_mut().terminals.push(removed);
        let restored = snapshot(&model);
        assert!(restored.cards[0].revision > first.cards[0].revision);
        assert!(restored.revision > missing.revision);
        assert_ne!(LocalWorkspace::new(AppState::default()).epoch, model.epoch);
    }

    #[test]
    fn runtime_unknown_is_distinct_from_missing_and_only_owned_cards_export() {
        let model = model();
        assert_eq!(snapshot(&model).cards[0].session_alive, None);
        let sessions =
            parse_panes(b"sd_term_a\t1\t1\t120\t40\t0\nforeign\t1\t1\t80\t24\t0\n").unwrap();
        let snapshot = model
            .snapshot_with_runtime(
                canvas(),
                &HashMap::new(),
                &Runtime {
                    sessions: Some(sessions),
                    harnesses: None,
                },
            )
            .unwrap();
        assert_eq!(snapshot.cards.len(), 2);
        assert_eq!(snapshot.cards[0].session_alive, Some(true));
        assert_eq!(snapshot.cards[1].session_alive, Some(false));
        assert_eq!(
            snapshot.cards[0].terminal_size,
            Some(TerminalSize {
                columns: 120,
                rows: 40
            })
        );
        assert!(parse_panes(b"malformed").is_none());
    }

    #[test]
    fn stacking_migration_repairs_deleted_duplicates_and_missing_entries() {
        let model = model();
        let state = model.state();
        state.borrow_mut().terminal_order = vec!["b".into(), "gone".into(), "b".into()];
        assert!(crate::state::normalize_terminal_order(
            &mut state.borrow_mut()
        ));
        assert_eq!(state.borrow().terminal_order, ["b", "a"]);
        assert!(!crate::state::normalize_terminal_order(
            &mut state.borrow_mut()
        ));
        assert_eq!(snapshot(&model).cards[0].card_id, "b");
    }

    #[test]
    fn runtime_probe_is_bounded_and_handles_more_than_a_pipe_buffer() {
        let mut large = Command::new("python3");
        large.args(["-c", "import sys; sys.stdout.write('x'*100000)"]);
        assert_eq!(bounded_output(large).unwrap().len(), 100000);
        let mut excessive = Command::new("python3");
        excessive.args(["-c", "import sys; sys.stdout.write('x'*600000)"]);
        assert!(bounded_output(excessive).is_none());
        let mut hung = Command::new("sleep");
        hung.arg("5");
        let start = Instant::now();
        assert!(bounded_output(hung).is_none());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn runtime_requests_coalesce_and_queued_old_results_stay_unknown() {
        let (request, requests) = mpsc::sync_channel(1);
        let (results, result) = mpsc::sync_channel(1);
        let mut poller = RuntimePoller {
            request,
            result,
            cached: Runtime::default(),
            updated: None,
            requested: None,
            in_flight: false,
        };
        assert!(poller.current().sessions.is_none());
        assert!(requests.try_recv().is_ok());
        assert!(poller.current().sessions.is_none());
        assert!(
            requests.try_recv().is_err(),
            "must not queue duplicate probes"
        );
        let old = Instant::now() - Duration::from_secs(10);
        results
            .send((
                old,
                Runtime {
                    sessions: Some(HashMap::new()),
                    harnesses: None,
                },
            ))
            .unwrap();
        assert!(
            poller.current().sessions.is_none(),
            "an old queued result is not fresh"
        );
        results
            .send((
                Instant::now(),
                Runtime {
                    sessions: Some(HashMap::new()),
                    harnesses: None,
                },
            ))
            .unwrap();
        assert!(poller.current().sessions.is_some());
    }
}
