//! Plugins on the desktop: the GTK-thread side of `plugin_host`.
//!
//! The manager keeps the active plugins, draws their contributions (toolbar
//! items, panels), runs their commands, and turns them off. Turning a plugin
//! off is done here, not by the plugin: its items and panels are removed, its
//! process group is killed, and what is left is checked (`footprint`).
//!
//! Plugin sessions run on worker threads (`plugin_host::api::Session`); when
//! they need GTK they post a `Job` here and wait for the answer with a
//! timeout. The GTK thread never waits on a plugin.
pub mod cards;
pub mod settings_page;
pub mod toolbar;
pub mod view;

use crate::plugin_host::api::{self, Session, SessionEvent};
use crate::plugin_host::manifest::Manifest;
use crate::plugin_host::rpc::{self, RpcError};
use crate::plugin_host::{cli, store};
use futures_util::StreamExt;
use gtk4::glib;
use gtk4::prelude::*;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use cards::card_inputs_changed;

/// How long a plugin's worker waits for the GTK thread.
const UI_TIMEOUT: Duration = Duration::from_secs(2);
/// Restart delays after the 1st and 2nd crash; the 3rd within
/// `CRASH_WINDOW` marks the plugin failed.
const RESTART_DELAYS: [u64; 2] = [1, 5];
const CRASH_WINDOW: Duration = Duration::from_secs(300);
const MAX_CRASHES: usize = 3;

enum Job {
    Call { plugin: String, method: String, params: Value, reply: std::sync::mpsc::Sender<Result<Value, RpcError>> },
    Event { plugin: String, event: SessionEvent },
    Started { plugin: String, generation: u64, result: Result<Arc<Session>, String> },
    BindsApplied { generation: u64, refused: Vec<(crate::shortcut::PluginBind, String)>, error: Option<String> },
}

/// The `Ui` the sessions see: posts jobs to the GTK main loop.
struct Bridge {
    jobs: std::sync::Mutex<futures_channel::mpsc::UnboundedSender<Job>>,
}

impl api::Ui for Bridge {
    fn call(&self, plugin: &str, method: &str, params: Value) -> Result<Value, RpcError> {
        let (reply, answer) = std::sync::mpsc::channel();
        let job = Job::Call { plugin: plugin.into(), method: method.into(), params, reply };
        if self.jobs.lock().unwrap_or_else(|e| e.into_inner()).unbounded_send(job).is_err() {
            return Err(RpcError::new(rpc::UNAVAILABLE, "SUPER DESKTOP is shutting down", "Stop.", "references/host-api.md#lifecycle"));
        }
        // `card.close` waits for the person to answer its confirmation.
        let timeout = if method == "card.close" { Duration::from_secs(120) } else { UI_TIMEOUT };
        answer.recv_timeout(timeout).unwrap_or_else(|_| {
            Err(RpcError::new(rpc::TIMEOUT, format!("{method}: the desktop did not answer in time"), "Retry once; the desktop may be busy.", "references/host-api.md#errors"))
        })
    }

    fn event(&self, plugin: &str, event: SessionEvent) {
        let _ = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).unbounded_send(Job::Event { plugin: plugin.into(), event });
    }
}

#[derive(Clone, Debug, PartialEq)]
enum State {
    /// Contributions drawn; the process starts on its first activation event.
    Idle,
    Starting,
    Running,
    Failed(String),
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::Idle => "on",
            State::Starting => "starting",
            State::Running => "running",
            State::Failed(_) => "failed",
        }
    }
}

struct OpenView {
    view: String,
    tree: Rc<view::Tree>,
    panel: Option<Rc<crate::floating_panel::MovablePanel>>,
    popover: Option<gtk4::Popover>,
}

struct Plugin {
    manifest: Arc<Manifest>,
    dir: PathBuf,
    state: State,
    session: Option<Arc<Session>>,
    /// Bumped on every start and stop, so a late `Started` is ignored.
    generation: u64,
    crashes: VecDeque<Instant>,
    /// Commands clicked before the process was ready.
    queued: Vec<Value>,
    views: BTreeMap<String, OpenView>,
}

/// Where plugin panels open: the overlay, its bar height, and the dialog
/// that must stay above them (same host as Files & links).
struct ViewHost {
    overlay: glib::WeakRef<gtk4::Overlay>,
    top: Rc<dyn Fn() -> i32>,
    ceiling: glib::WeakRef<gtk4::Widget>,
}

pub struct Manager {
    plugins: RefCell<BTreeMap<String, Plugin>>,
    bridge: Arc<Bridge>,
    bar: Rc<toolbar::PluginBar>,
    host: RefCell<Option<ViewHost>>,
    show_overlay: RefCell<Option<Rc<dyn Fn()>>>,
    overlay_shown: Cell<bool>,
    next_handle: Cell<u64>,
    /// Told when the set of plugins or their state changes (Settings page).
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    /// Bumped per bind update; only the newest result is kept.
    binds_generation: Cell<u64>,
    /// Shortcut id → why it is not active (taken, invalid), for Settings.
    shortcut_problems: RefCell<BTreeMap<String, String>>,
    /// Overlay-scope shortcuts: normalized combo → (plugin, command).
    overlay_keys: RefCell<BTreeMap<String, (String, String)>>,
    /// The local workspace (cards, screen, commands), set by the window.
    workspace: RefCell<Option<Rc<dyn cards::Workspace>>>,
    /// card id → plugin id → that plugin's title text and chips.
    titles: RefCell<BTreeMap<String, BTreeMap<String, cards::TitleState>>>,
}

thread_local! {
    static MANAGER: RefCell<Option<Rc<Manager>>> = const { RefCell::new(None) };
}

/// The manager, created on first use on the GTK thread.
pub fn manager() -> Rc<Manager> {
    if let Some(manager) = MANAGER.with(|m| m.borrow().clone()) {
        return manager;
    }
    let (sender, mut jobs) = futures_channel::mpsc::unbounded::<Job>();
    let manager = Rc::new(Manager {
        plugins: RefCell::default(),
        bridge: Arc::new(Bridge { jobs: std::sync::Mutex::new(sender) }),
        bar: toolbar::PluginBar::new(Rc::new(|plugin: &str, item: &str| manager().toolbar_clicked(plugin, item))),
        host: RefCell::new(None),
        show_overlay: RefCell::new(None),
        overlay_shown: Cell::new(false),
        next_handle: Cell::new(1),
        listeners: RefCell::default(),
        binds_generation: Cell::new(0),
        shortcut_problems: RefCell::default(),
        overlay_keys: RefCell::default(),
        workspace: RefCell::new(None),
        titles: RefCell::default(),
    });
    MANAGER.with(|m| m.replace(Some(Rc::clone(&manager))));
    let weak = Rc::downgrade(&manager);
    glib::MainContext::default().spawn_local(async move {
        while let Some(job) = jobs.next().await {
            let Some(manager) = weak.upgrade() else { break };
            manager.handle(job);
        }
    });
    manager
}

impl Manager {
    /// The toolbar group the local top bar places after its launchers.
    pub fn toolbar(&self) -> Rc<toolbar::PluginBar> {
        Rc::clone(&self.bar)
    }

    pub fn set_view_host(&self, overlay: &gtk4::Overlay, top: Rc<dyn Fn() -> i32>, ceiling: &impl IsA<gtk4::Widget>) {
        self.host.replace(Some(ViewHost { overlay: overlay.downgrade(), top, ceiling: ceiling.upcast_ref::<gtk4::Widget>().downgrade() }));
    }

    pub fn set_show_overlay(&self, show: Rc<dyn Fn()>) {
        self.show_overlay.replace(Some(show));
    }

    pub fn on_change(&self, listener: Rc<dyn Fn()>) {
        self.listeners.borrow_mut().push(listener);
    }

    fn changed(&self) {
        let listeners = self.listeners.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    /// Make the running set match `plugins.json`: turn on what is active
    /// there and off what is not.
    pub fn sync(self: &Rc<Self>) {
        let store = store::Store::load();
        let wanted: Vec<store::Installed> = store.plugins.iter().filter(|p| p.active).cloned().collect();
        let running: Vec<String> = self.plugins.borrow().keys().cloned().collect();
        for id in running {
            let still = wanted.iter().find(|p| p.id == id);
            let moved = still.is_some_and(|p| self.plugins.borrow().get(&id).is_some_and(|q| q.dir != p.dir));
            if still.is_none() || moved {
                self.deactivate(&id);
            }
        }
        for installed in wanted {
            if !self.plugins.borrow().contains_key(&installed.id) {
                self.activate(&installed);
            }
        }
        self.apply_shortcuts();
        self.changed();
    }

    /// Global shortcuts of every active plugin, as `bindings.lua` binds; the
    /// user's overrides from `plugins.json` apply. Written on a worker thread
    /// (it runs `hyprctl`), only when the block changes.
    pub fn apply_shortcuts(self: &Rc<Self>) {
        let store = store::Store::load();
        let plugins: Vec<(String, Arc<Manifest>)> = self.plugins.borrow().iter().map(|(id, p)| (id.clone(), Arc::clone(&p.manifest))).collect();
        let desired = crate::plugin_host::binds::desired(&plugins, &store);
        let (binds, overlay, problems) = (desired.global, desired.overlay, desired.problems);
        self.overlay_keys.replace(overlay);
        self.shortcut_problems.replace(problems);
        let generation = self.binds_generation.get() + 1;
        self.binds_generation.set(generation);
        let jobs = self.bridge.jobs.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let _ = std::thread::Builder::new().name("plugin-binds".into()).spawn(move || {
            let (refused, error) = match crate::shortcut::apply_plugin_binds(&binds) {
                Ok(refused) => (refused, None),
                Err(error) => (Vec::new(), Some(error)),
            };
            let _ = jobs.unbounded_send(Job::BindsApplied { generation, refused, error });
        });
    }

    /// A key pressed in the overlay, as Hyprland spells it. `in_terminal`:
    /// a terminal has focus, which keeps every combination without SUPER.
    pub fn overlay_shortcut(self: &Rc<Self>, combo: &str, in_terminal: bool) -> bool {
        let key = crate::plugin_host::binds::normalize(combo);
        if in_terminal && !key.split(" + ").any(|part| part == "SUPER") {
            return false;
        }
        let target = self.overlay_keys.borrow().get(&key).cloned();
        match target {
            Some((plugin, command)) => {
                let _ = self.run_command(&plugin, &command, json!({"source": "shortcut"}));
                true
            }
            None => false,
        }
    }

    /// Why a shortcut is not active, if it is not.
    pub fn shortcut_problem(&self, shortcut: &str) -> Option<String> {
        self.shortcut_problems.borrow().get(shortcut).cloned()
    }

    fn activate(self: &Rc<Self>, installed: &store::Installed) {
        let running: Vec<(String, Arc<Manifest>)> = self.plugins.borrow().iter().map(|(id, p)| (id.clone(), Arc::clone(&p.manifest))).collect();
        let manifest = match cli::installed_manifest(installed)
            .and_then(|m| cli::check_activatable(installed, &m).map(|_| m))
            .and_then(|m| cli::check_exclusive(&m, &running).map(|_| m))
        {
            Ok(manifest) => Arc::new(manifest),
            Err(why) => {
                api::append_log(&crate::plugin_host::log_file(&installed.id), "error", &format!("not turned on: {why}"));
                eprintln!("SUPER DESKTOP: plugin {} not turned on: {why}", installed.id);
                return;
            }
        };
        let id = manifest.id.clone();
        for item in &manifest.contributes.toolbar {
            self.bar.add(&id, &installed.dir, item);
        }
        let starts_now = manifest.main.is_some()
            && (manifest.activation.iter().any(|a| a == "onStartup")
                || (self.overlay_shown.get() && manifest.activation.iter().any(|a| a == "onOverlayShown")));
        self.plugins.borrow_mut().insert(
            id.clone(),
            Plugin {
                manifest,
                dir: installed.dir.clone(),
                state: State::Idle,
                session: None,
                generation: 0,
                crashes: VecDeque::new(),
                queued: Vec::new(),
                views: BTreeMap::new(),
            },
        );
        self.decorate_all();
        if starts_now {
            self.start(&id);
        }
    }

    /// Start the plugin's process on a worker thread.
    fn start(self: &Rc<Self>, id: &str) {
        let (manifest, dir, generation) = {
            let mut plugins = self.plugins.borrow_mut();
            let Some(plugin) = plugins.get_mut(id) else { return };
            if plugin.manifest.main.is_none() || matches!(plugin.state, State::Starting | State::Running) {
                return;
            }
            plugin.state = State::Starting;
            plugin.generation += 1;
            (Arc::clone(&plugin.manifest), plugin.dir.clone(), plugin.generation)
        };
        let bridge: Arc<dyn api::Ui> = self.bridge.clone();
        let jobs = self.bridge.jobs.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let plugin = id.to_string();
        let _ = std::thread::Builder::new().name(format!("plugin-start-{id}")).spawn(move || {
            let result = Session::start(manifest, &dir, bridge).map(Arc::new);
            let _ = jobs.unbounded_send(Job::Started { plugin, generation, result });
        });
        self.changed();
    }

    /// Turn a plugin off. Everything it added goes; its process group is
    /// stopped on a worker thread.
    pub fn deactivate(self: &Rc<Self>, id: &str) {
        let Some(plugin) = self.plugins.borrow_mut().remove(id) else { return };
        for (_, open) in plugin.views {
            close_view_widgets(&open);
        }
        self.bar.remove_plugin(id);
        self.forget_cards_of(id);
        self.apply_shortcuts();
        if let Some(session) = plugin.session {
            let _ = std::thread::Builder::new().name(format!("plugin-stop-{id}")).spawn(move || session.stop());
        }
        api::append_log(&crate::plugin_host::log_file(id), "info", "turned off");
        self.changed();
    }

    /// Everything a plugin still has in the desktop. Empty after `deactivate`
    /// (checked by the `plugin_roundtrip_` tests).
    pub fn footprint(&self, id: &str) -> Vec<String> {
        let mut left = Vec::new();
        if self.plugins.borrow().contains_key(id) {
            left.push("registered".into());
        }
        if self.bar.button_count_of(id) > 0 {
            left.push("toolbar items".into());
        }
        if self.overlay_keys.borrow().values().any(|(plugin, _)| plugin == id) {
            left.push("overlay shortcuts".into());
        }
        left.extend(self.cards_footprint(id));
        let lua = std::fs::read_to_string(crate::shortcut::bindings_path()).unwrap_or_default();
        if crate::shortcut::plugin_bind_lines(&lua).iter().any(|line| line.contains(&format!("plugin run {id} "))) {
            left.push("global shortcuts in bindings.lua".into());
        }
        left
    }

    fn handle(self: &Rc<Self>, job: Job) {
        match job {
            Job::Call { plugin, method, params, reply } => {
                if method == "card.close" && self.plugins.borrow().contains_key(&plugin) {
                    self.confirm_close(&plugin, &params, reply);
                    return;
                }
                let result = self.call(&plugin, &method, &params);
                let _ = reply.send(result);
            }
            Job::Started { plugin, generation, result } => self.started(&plugin, generation, result),
            Job::Event { plugin, event: SessionEvent::Exited } => self.crashed(&plugin),
            Job::BindsApplied { generation, refused, error } => {
                if generation != self.binds_generation.get() {
                    return;
                }
                if let Some(error) = error {
                    // Every global shortcut is off: say so next to each one.
                    let globals: Vec<(String, String)> = self
                        .plugins
                        .borrow()
                        .iter()
                        .flat_map(|(id, p)| p.manifest.contributes.shortcuts.iter().filter(|s| s.scope == "global").map(move |s| (id.clone(), s.id.clone())))
                        .collect();
                    for (plugin, shortcut) in globals {
                        api::append_log(&crate::plugin_host::log_file(&plugin), "warn", &format!("shortcut not bound: {error}"));
                        self.shortcut_problems.borrow_mut().entry(shortcut).or_insert_with(|| error.clone());
                    }
                }
                for (bind, holder) in refused {
                    let plugin = bind.command.split_whitespace().nth(3).unwrap_or_default().to_string();
                    let shortcut = self
                        .plugins
                        .borrow()
                        .get(&plugin)
                        .and_then(|p| p.manifest.contributes.shortcuts.iter().find(|s| bind.command.ends_with(&format!(" {}", s.command))).map(|s| s.id.clone()));
                    let why = format!("{} is used by Hyprland for \u{201c}{holder}\u{201d}; choose another in Settings → Plugins", bind.combo);
                    api::append_log(&crate::plugin_host::log_file(&plugin), "warn", &format!("shortcut not bound: {why}"));
                    if let Some(shortcut) = shortcut {
                        self.shortcut_problems.borrow_mut().insert(shortcut, why);
                    }
                }
                self.changed();
            }
        }
    }

    fn started(self: &Rc<Self>, id: &str, generation: u64, result: Result<Arc<Session>, String>) {
        let queued = {
            let mut plugins = self.plugins.borrow_mut();
            let Some(plugin) = plugins.get_mut(id).filter(|p| p.generation == generation) else {
                // Turned off (or restarted) while starting: stop this one.
                if let Ok(session) = result {
                    std::thread::spawn(move || session.stop());
                }
                return;
            };
            match result {
                Ok(session) => {
                    plugin.state = State::Running;
                    plugin.session = Some(session);
                    std::mem::take(&mut plugin.queued)
                }
                Err(why) => {
                    plugin.state = State::Failed(why.clone());
                    drop(plugins);
                    self.bar.set_failed(id, Some(&why));
                    self.changed();
                    return;
                }
            }
        };
        self.bar.set_failed(id, None);
        if let Some(session) = self.session(id) {
            if self.overlay_shown.get() {
                session.notify("overlay.shown", json!({}));
            }
            if let Some(workspace) = self.workspace.borrow().clone() {
                self.send_title_inputs(Some(id), &workspace.cards());
            }
            for command in queued {
                session.notify("command", command);
            }
        }
        self.changed();
    }

    fn crashed(self: &Rc<Self>, id: &str) {
        let delay = {
            let mut plugins = self.plugins.borrow_mut();
            let Some(plugin) = plugins.get_mut(id) else { return };
            plugin.session = None;
            plugin.generation += 1;
            let now = Instant::now();
            plugin.crashes.push_back(now);
            while plugin.crashes.front().is_some_and(|t| now.duration_since(*t) > CRASH_WINDOW) {
                plugin.crashes.pop_front();
            }
            // Views of the dead process can no longer answer.
            for (_, open) in std::mem::take(&mut plugin.views) {
                close_view_widgets(&open);
            }
            if plugin.crashes.len() >= MAX_CRASHES {
                let why = format!("stopped after {MAX_CRASHES} crashes; see `super-desktop plugin logs {id}`");
                plugin.state = State::Failed(why.clone());
                drop(plugins);
                self.bar.set_failed(id, Some(&why));
                self.changed();
                return;
            }
            plugin.state = State::Idle;
            RESTART_DELAYS[plugin.crashes.len() - 1]
        };
        self.bar.set_failed(id, Some("restarting…"));
        let weak = Rc::downgrade(self);
        let plugin = id.to_string();
        glib::timeout_add_seconds_local_once(delay as u32, move || {
            if let Some(manager) = weak.upgrade() {
                manager.start(&plugin);
            }
        });
        self.changed();
    }

    fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.plugins.borrow().get(id).and_then(|p| p.session.clone())
    }

    /// Run a declared command. Starts the plugin first if needed.
    pub fn run_command(self: &Rc<Self>, id: &str, command: &str, context: Value) -> Result<(), String> {
        let message = json!({"command": command, "context": context});
        let state = {
            let mut plugins = self.plugins.borrow_mut();
            let plugin = plugins.get_mut(id).ok_or_else(|| format!("plugin `{id}` is not on"))?;
            if !plugin.manifest.contributes.commands.iter().any(|c| c.id == command) {
                return Err(format!("`{command}` is not a command of {id}"));
            }
            if plugin.state != State::Running {
                if let State::Failed(why) = &plugin.state {
                    return Err(why.clone());
                }
                plugin.queued.push(message.clone());
            }
            plugin.state.clone()
        };
        match state {
            State::Running => {
                if let Some(session) = self.session(id) {
                    session.notify("command", message);
                }
            }
            State::Idle => self.start(id),
            _ => {}
        }
        Ok(())
    }

    fn toolbar_clicked(self: &Rc<Self>, id: &str, item: &str) {
        let target = {
            let plugins = self.plugins.borrow();
            let Some(plugin) = plugins.get(id) else { return };
            plugin.manifest.contributes.toolbar.iter().find(|t| t.id == item).map(|t| (t.command.clone(), t.view.clone()))
        };
        match target {
            Some((Some(command), _)) => {
                let _ = self.run_command(id, &command, json!({"source": "toolbar"}));
            }
            Some((None, Some(view))) => {
                let handle = self.open_view(id, &view, &view::placeholder(), Some(item));
                if let Ok(handle) = handle {
                    let message = json!({"handle": handle, "view": view, "anchor": item});
                    match self.session(id) {
                        Some(session) => {
                            session.notify("view.opened", message);
                        }
                        None => {
                            // Not running yet: start it; it is told once ready.
                            self.start(id);
                            let weak = Rc::downgrade(self);
                            let plugin = id.to_string();
                            glib::timeout_add_local_once(Duration::from_millis(50), move || {
                                if let Some(manager) = weak.upgrade() {
                                    manager.notify_when_running(&plugin, "view.opened", message, 200);
                                }
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Send a notification once the plugin is running (polled briefly).
    fn notify_when_running(self: &Rc<Self>, id: &str, method: &'static str, params: Value, tries: u32) {
        if let Some(session) = self.session(id) {
            session.notify(method, params);
            return;
        }
        let starting = self.plugins.borrow().get(id).is_some_and(|p| p.state == State::Starting);
        if starting && tries > 0 {
            let weak = Rc::downgrade(self);
            let plugin = id.to_string();
            glib::timeout_add_local_once(Duration::from_millis(50), move || {
                if let Some(manager) = weak.upgrade() {
                    manager.notify_when_running(&plugin, method, params, tries - 1);
                }
            });
        }
    }

    /// A plugin's `Ui::call`, already checked by `api::dispatch`.
    fn call(self: &Rc<Self>, id: &str, method: &str, params: &Value) -> Result<Value, RpcError> {
        if !self.plugins.borrow().contains_key(id) {
            return Err(RpcError::new(rpc::CANCELLED, "the plugin is being turned off", "Stop.", "references/host-api.md#lifecycle"));
        }
        match method {
            "contrib.update" => {
                let target = params["id"].as_str().unwrap_or_default();
                // Commands have no widget of their own yet; their state is accepted.
                self.bar.update(target, params);
                Ok(json!({}))
            }
            "ui.open" => {
                let view = params["view"].as_str().unwrap_or_default();
                let handle = self.open_view(id, view, &params["model"], params["anchor"].as_str()).map_err(|why| RpcError::new(rpc::UNAVAILABLE, why, "Try again when SUPER DESKTOP is shown.", "references/ui.md#declaring-and-opening"))?;
                Ok(json!({"handle": handle}))
            }
            "ui.patch" => {
                let handle = params["handle"].as_str().unwrap_or_default();
                let tree = self.plugins.borrow().get(id).and_then(|p| p.views.get(handle)).map(|v| Rc::clone(&v.tree));
                let tree = tree.ok_or_else(|| RpcError::new(rpc::NOT_FOUND, format!("no open view `{handle}`"), "The view was closed; open it again with ui.open.", "references/ui.md#patching"))?;
                let ops = params["ops"].as_array().cloned().unwrap_or_default();
                tree.patch(&ops).map_err(|why| RpcError::invalid_params(why, "references/ui.md#patching"))?;
                Ok(json!({}))
            }
            "ui.close" => {
                let handle = params["handle"].as_str().unwrap_or_default();
                if !self.close_view(id, handle) {
                    return Err(RpcError::new(rpc::NOT_FOUND, format!("no open view `{handle}`"), "It is already closed.", "references/ui.md#declaring-and-opening"));
                }
                Ok(json!({}))
            }
            m if m == "workspace.cards" || m == "harness.launch" || m.starts_with("card.") || m.starts_with("title.") => self.card_call(id, method, params),
            _ => Err(RpcError::new(rpc::METHOD_NOT_FOUND, format!("{method} is not a desktop call"), "Report this as a SUPER DESKTOP bug.", "references/host-api.md#errors")),
        }
    }

    /// Open (or refresh and raise) a declared view. Shows the overlay.
    fn open_view(self: &Rc<Self>, id: &str, view_id: &str, model: &Value, anchor: Option<&str>) -> Result<String, String> {
        let (manifest, dir) = {
            let plugins = self.plugins.borrow();
            let plugin = plugins.get(id).ok_or("the plugin is off")?;
            (Arc::clone(&plugin.manifest), plugin.dir.clone())
        };
        let declared = manifest.contributes.views.iter().find(|v| v.id == view_id).cloned().ok_or("view is not declared")?;
        // Already open: new content, raised, same handle.
        let existing = self.plugins.borrow().get(id).and_then(|p| p.views.iter().find(|(_, v)| v.view == view_id).map(|(h, v)| (h.clone(), Rc::clone(&v.tree), v.panel.clone(), v.popover.clone())));
        if !self.overlay_shown.get() {
            if let Some(show) = self.show_overlay.borrow().clone() {
                show();
            }
        }
        if let Some((handle, tree, panel, popover)) = existing {
            tree.set_model(model);
            let ceiling = self.host.borrow().as_ref().and_then(|h| h.ceiling.upgrade());
            if let Some(panel) = panel {
                panel.raise(ceiling.as_ref());
            }
            if let Some(popover) = popover {
                popover.popup();
            }
            return Ok(handle);
        }
        let handle = format!("v{}", self.next_handle.get());
        self.next_handle.set(self.next_handle.get() + 1);
        let on_event: view::OnEvent = {
            let (plugin, handle, view_id) = (id.to_string(), handle.clone(), view_id.to_string());
            Rc::new(move |node: &str, event: &str, value: Value| {
                if let Some(session) = manager().session(&plugin) {
                    session.notify("view.event", json!({"handle": handle, "view": view_id, "node": node, "event": event, "value": value}));
                }
            })
        };
        let tree = view::Tree::new(&dir, on_event);
        tree.set_model(model);
        let anchor_button = anchor.and_then(|a| self.bar.button(a)).filter(|b| b.is_mapped());
        let mut open = OpenView { view: view_id.to_string(), tree: Rc::clone(&tree), panel: None, popover: None };
        if declared.kind == "popover" && anchor_button.is_some() {
            let popover = gtk4::Popover::new();
            popover.add_css_class("plugin-popover");
            let scroll = gtk4::ScrolledWindow::builder().hscrollbar_policy(gtk4::PolicyType::Never).propagate_natural_height(true).max_content_height(declared.height.unwrap_or(480) as i32).child(&tree.root).build();
            scroll.set_size_request(declared.width.unwrap_or(360) as i32, -1);
            popover.set_child(Some(&scroll));
            popover.set_parent(anchor_button.as_ref().expect("checked"));
            let (plugin, closed) = (id.to_string(), handle.clone());
            popover.connect_closed(move |_| {
                let manager = manager();
                manager.forget_view(&plugin, &closed);
            });
            popover.popup();
            open.popover = Some(popover);
        } else {
            let host = self.host.borrow();
            let host = host.as_ref().ok_or("SUPER DESKTOP is not shown yet")?;
            let overlay = host.overlay.upgrade().ok_or("SUPER DESKTOP is not shown yet")?;
            let chrome = view::chrome(&manifest.name, &declared.title, &tree);
            let panel = crate::floating_panel::MovablePanel::install(
                &overlay,
                &chrome.widget,
                crate::floating_panel::PanelLayout {
                    default_size: (declared.width.unwrap_or(560) as i32, declared.height.unwrap_or(420) as i32),
                    min_size: (240, 160),
                    saved_pos: None,
                    saved_size: None,
                },
                Rc::clone(&host.top),
                Rc::new(|_, _| {}),
            );
            panel.raise(host.ceiling.upgrade().as_ref());
            let (plugin, closed) = (id.to_string(), handle.clone());
            chrome.close.connect_clicked(move |_| {
                manager().close_view(&plugin, &closed);
            });
            open.panel = Some(panel);
        }
        if let Some(plugin) = self.plugins.borrow_mut().get_mut(id) {
            plugin.views.insert(handle.clone(), open);
        }
        Ok(handle)
    }

    /// Close a view and tell the plugin. `false` when it was not open.
    fn close_view(&self, id: &str, handle: &str) -> bool {
        let open = self.plugins.borrow_mut().get_mut(id).and_then(|p| p.views.remove(handle));
        let Some(open) = open else { return false };
        close_view_widgets(&open);
        if let Some(session) = self.session(id) {
            session.notify("view.closed", json!({"handle": handle}));
        }
        true
    }

    /// A popover closed itself (click outside, Escape, overlay hidden).
    fn forget_view(&self, id: &str, handle: &str) {
        let open = self.plugins.borrow_mut().get_mut(id).and_then(|p| p.views.remove(handle));
        if let Some(open) = open {
            if let Some(popover) = &open.popover {
                // Unparent after the closed signal has finished.
                let popover = popover.clone();
                glib::idle_add_local_once(move || popover.unparent());
            }
            if let Some(session) = self.session(id) {
                session.notify("view.closed", json!({"handle": handle}));
            }
        }
    }

    /// Open views and their nodes' state.
    pub fn views(&self, id: &str) -> Result<Value, String> {
        let plugins = self.plugins.borrow();
        let plugin = plugins.get(id).ok_or_else(|| format!("plugin `{id}` is not on"))?;
        Ok(Value::Array(plugin.views.iter().map(|(handle, v)| json!({"handle": handle, "view": v.view, "nodes": v.tree.snapshot()})).collect()))
    }

    /// Operate a node in an open view (the only one, or `view`).
    pub fn interact(&self, id: &str, view: Option<&str>, node: &str, event: &str, value: &Value) -> Result<(), String> {
        let tree = {
            let plugins = self.plugins.borrow();
            let plugin = plugins.get(id).ok_or_else(|| format!("plugin `{id}` is not on"))?;
            let mut matching = plugin.views.iter().filter(|(handle, v)| view.is_none_or(|w| w == v.view || w == handle.as_str()));
            let (_, open) = matching.next().ok_or("no open view (open one first, e.g. with plugin run)")?;
            if view.is_none() && matching.next().is_some() {
                return Err("several views are open: name one with --view".into());
            }
            Rc::clone(&open.tree)
        };
        tree.interact(node, event, value)
    }

    pub fn overlay_shown(self: &Rc<Self>) {
        self.overlay_shown.set(true);
        let ids: Vec<(String, bool)> = self
            .plugins
            .borrow()
            .iter()
            .map(|(id, p)| (id.clone(), p.state == State::Idle && p.manifest.activation.iter().any(|a| a == "onOverlayShown")))
            .collect();
        for (id, start) in ids {
            if start {
                self.start(&id);
            } else if let Some(session) = self.session(&id) {
                session.notify("overlay.shown", json!({}));
            }
        }
    }

    pub fn overlay_hidden(&self) {
        self.overlay_shown.set(false);
        let mut popovers = Vec::new();
        for plugin in self.plugins.borrow().values() {
            popovers.extend(plugin.views.values().filter_map(|v| v.popover.clone()));
            if let Some(session) = &plugin.session {
                session.notify("overlay.hidden", json!({}));
            }
        }
        // Popovers are their own surfaces: they must not outlive the overlay.
        for popover in popovers {
            popover.popdown();
        }
    }

    /// Settings page rows: (id, name, version, state, detail).
    pub fn rows(&self) -> Vec<(String, String, String, String, String)> {
        self.plugins
            .borrow()
            .iter()
            .map(|(id, p)| {
                let detail = match &p.state {
                    State::Failed(why) => why.clone(),
                    _ => p.manifest.description.clone(),
                };
                (id.clone(), p.manifest.name.clone(), p.manifest.version.clone(), p.state.name().to_string(), detail)
            })
            .collect()
    }

    pub fn manifest(&self, id: &str) -> Option<Arc<Manifest>> {
        self.plugins.borrow().get(id).map(|p| Arc::clone(&p.manifest))
    }

    /// Settings changed from the Settings page.
    pub fn settings_changed(&self, id: &str) {
        if let (Some(session), Some(manifest)) = (self.session(id), self.manifest(id)) {
            session.notify("settings.changed", json!({"values": store::settings(&manifest)}));
        }
    }

    pub fn status(&self) -> Value {
        let plugins: serde_json::Map<String, Value> = self
            .plugins
            .borrow()
            .iter()
            .map(|(id, p)| {
                let failed = if let State::Failed(why) = &p.state { Some(why.clone()) } else { None };
                (
                    id.clone(),
                    json!({
                        "state": p.state.name(),
                        "pid": p.session.as_ref().map(|s| s.process.pid),
                        "views": p.views.len(),
                        "toolbarItems": self.bar.button_count_of(id),
                        "error": failed,
                    }),
                )
            })
            .collect();
        json!({"ok": true, "plugins": plugins})
    }
}

fn close_view_widgets(open: &OpenView) {
    if let Some(panel) = &open.panel {
        panel.remove();
    }
    if let Some(popover) = &open.popover {
        popover.popdown();
        popover.unparent();
    }
}

/// The daemon's `plugin <json>` IPC verb (sent by `super-desktop plugin …`).
pub fn handle_ipc(payload: &str) -> String {
    let manager = manager();
    let request: Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(_) => return json!({"ok": false, "error": "invalid plugin request"}).to_string(),
    };
    let id = request["id"].as_str().unwrap_or_default();
    let reply = match request["op"].as_str().unwrap_or_default() {
        "sync" => {
            manager.sync();
            json!({"ok": true})
        }
        "status" => manager.status(),
        // What a plugin still has in the desktop; empty once it is off.
        "footprint" => json!({"ok": true, "footprint": manager.footprint(id)}),
        "reload" => {
            manager.deactivate(id);
            manager.sync();
            match manager.plugins.borrow().contains_key(id) {
                true => json!({"ok": true}),
                false => json!({"ok": false, "error": format!("{id} is not on (see super-desktop plugin logs {id})")}),
            }
        }
        "cards" => {
            let (w, h, top) = manager.workspace.borrow().as_ref().map(|ws| ws.screen()).unwrap_or((0, 0, 0));
            json!({"ok": true, "screen": {"w": w, "h": h, "top": top}, "cards": manager.cards_snapshot()})
        }
        "press" => match manager.press(request["card"].as_str().unwrap_or_default(), request["control"].as_str().unwrap_or_default()) {
            Ok(()) => json!({"ok": true}),
            Err(why) => json!({"ok": false, "error": why}),
        },
        "views" => match manager.views(id) {
            Ok(views) => json!({"ok": true, "views": views}),
            Err(why) => json!({"ok": false, "error": why}),
        },
        "interact" => {
            let view = request["view"].as_str();
            let result = manager.interact(id, view, request["node"].as_str().unwrap_or_default(), request["event"].as_str().unwrap_or_default(), &request["value"]);
            match result {
                Ok(()) => json!({"ok": true}),
                Err(why) => json!({"ok": false, "error": why}),
            }
        }
        "run" => {
            let command = request["command"].as_str().unwrap_or_default();
            let mut context = json!({"source": "cli"});
            if !request["args"].is_null() {
                context["args"] = request["args"].clone();
            }
            // A shortcut runs a command from anywhere: its view shows the overlay.
            match manager.run_command(id, command, context) {
                Ok(()) => json!({"ok": true}),
                Err(why) => json!({"ok": false, "error": why}),
            }
        }
        _ => json!({"ok": false, "error": "unknown plugin op"}),
    };
    reply.to_string()
}
