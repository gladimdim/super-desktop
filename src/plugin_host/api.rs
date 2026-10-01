//! Host API 1: answering a plugin's requests, and the session that owns one
//! plugin's process.
//!
//! The method list, required params and permissions come from
//! `skills/super-desktop-plugin/schemas/host-api.openrpc.json` (compiled in),
//! so the host cannot drift from the published contract. Methods this build
//! does not implement yet answer `unavailable` with a hint; `host.describe`
//! lists exactly the implemented ones.
//!
//! Everything here runs off the GTK thread. Effects that need GTK go through
//! the `Ui` trait, which the GTK side implements by posting to its main loop.
use super::manifest::Manifest;
use super::process::{Event, Process, Spawn};
use super::rpc::{self, RpcError};
use super::{llm, store};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEACTIVATE_TIMEOUT: Duration = Duration::from_secs(1);
pub const KILL_GRACE: Duration = Duration::from_secs(2);
const MAX_IN_FLIGHT: usize = 32;
const MAX_LLM_CALLS: usize = 2;
const NOTIFY_PER_MINUTE: usize = 6;
const MAX_LOG: u64 = 4 * 1024 * 1024;

/// Host methods this build answers. Keep in step with the OpenRPC document
/// (`plugin_spec_implemented_methods_are_in_the_contract`).
pub const IMPLEMENTED: [&str; 25] = [
    "host.describe",
    "log",
    "contrib.update",
    "settings.get",
    "settings.set",
    "storage.get",
    "storage.set",
    "ui.open",
    "ui.patch",
    "ui.close",
    "ui.notify",
    "llm.complete",
    "workspace.cards",
    "card.iconify",
    "card.restore",
    "card.expand",
    "card.collapse",
    "card.focus",
    "card.setRect",
    "card.close",
    "terminal.text",
    "terminal.send",
    "harness.launch",
    "title.set",
    "title.clear",
];

pub fn limits() -> Value {
    json!({
        "messageBytes": rpc::MAX_MESSAGE,
        "activateMs": ACTIVATE_TIMEOUT.as_millis() as u64,
        "deactivateMs": DEACTIVATE_TIMEOUT.as_millis() as u64,
        "hostRequestMs": 2000,
        "toolbarItems": 4,
        "cardButtons": 2,
        "shortcuts": 8,
        "views": 8,
        "badgeChars": 4,
        "storageBytes": store::MAX_STORAGE_BYTES,
        "llmPromptBytes": llm::MAX_PROMPT,
        "llmConcurrent": MAX_LLM_CALLS,
        "llmTimeoutMs": llm::TIMEOUT.as_millis() as u64,
        "notificationsPerMinute": NOTIFY_PER_MINUTE,
        "viewNodes": super::ui_model::MAX_NODES,
        "patchOps": super::ui_model::MAX_OPS,
        "inFlightRequests": MAX_IN_FLIGHT,
    })
}

/// The published contract, parsed once.
pub struct Contract {
    /// host method → (required params, permission)
    pub host: BTreeMap<String, (Vec<String>, Option<String>)>,
    pub plugin: Vec<String>,
}

pub fn contract() -> &'static Contract {
    static CONTRACT: OnceLock<Contract> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        let doc: Value = serde_json::from_str(include_str!("../../skills/super-desktop-plugin/schemas/host-api.openrpc.json"))
            .expect("host-api.openrpc.json is valid JSON");
        let mut host = BTreeMap::new();
        let mut plugin = Vec::new();
        for method in doc["methods"].as_array().expect("methods") {
            let name = method["name"].as_str().expect("name").to_string();
            if method["tags"][0]["name"] == "host" {
                let required = method["params"]
                    .as_array()
                    .map(|ps| ps.iter().filter(|p| p["required"] == true).filter_map(|p| p["name"].as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                host.insert(name, (required, method["x-permission"].as_str().map(str::to_string)));
            } else {
                plugin.push(name);
            }
        }
        Contract { host, plugin }
    })
}

/// What the GTK side does for a plugin.
pub trait Ui: Send + Sync + 'static {
    /// A desktop method, already checked. Blocks this worker until the GTK
    /// thread answers (with its own timeout).
    fn call(&self, plugin: &str, method: &str, params: Value) -> Result<Value, RpcError>;
    /// Lifecycle news; must not block.
    fn event(&self, plugin: &str, event: SessionEvent);
    /// A desktop notification, already checked and rate-limited. The app name
    /// says which plugin sent it, so none can pass for SUPER DESKTOP itself.
    fn notify(&self, app: &str, urgency: &str, title: &str, body: &str) {
        send_notification(app, urgency, title, body);
    }
}

/// Answers `llm.complete` instead of the user's provider (`plugin test`).
pub type LlmOverride = Arc<dyn Fn(&llm::Request) -> Result<llm::Reply, RpcError> + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    /// The process ended without being asked to (crash or exit).
    Exited,
}

/// Per-plugin state shared by the workers answering its requests.
pub struct Context {
    pub manifest: Arc<Manifest>,
    pub ui: Arc<dyn Ui>,
    pub log: PathBuf,
    llm_calls: AtomicUsize,
    notifications: Mutex<VecDeque<Instant>>,
    pub llm: Option<LlmOverride>,
}

impl Context {
    pub fn new(manifest: Arc<Manifest>, ui: Arc<dyn Ui>) -> Self {
        let log = super::log_file(&manifest.id);
        Context { manifest, ui, log, llm_calls: AtomicUsize::new(0), notifications: Mutex::default(), llm: None }
    }

    pub fn log(&self, level: &str, message: &str) {
        append_log(&self.log, level, message);
    }
}

pub fn append_log(path: &Path, level: &str, message: &str) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let line: String = message.chars().take(4096).collect();
        let _ = writeln!(file, "{} {level} {}", chrono::Local::now().format("%H:%M:%S"), line.replace('\n', "\n    "));
    }
}

fn param<'a>(params: &'a Value, key: &str) -> &'a Value {
    params.get(key).unwrap_or(&Value::Null)
}

fn string(params: &Value, key: &str, max: usize, docs: &str) -> Result<String, RpcError> {
    match param(params, key) {
        Value::String(s) if s.len() <= max => Ok(s.clone()),
        Value::String(_) => Err(RpcError::invalid_params(format!("`{key}` is longer than {max} bytes"), docs)),
        _ => Err(RpcError::invalid_params(format!("`{key}` must be a string"), docs)),
    }
}

/// Answer one request from a plugin.
pub fn dispatch(ctx: &Context, method: &str, params: &Value) -> Result<Value, RpcError> {
    let contract = contract();
    let Some((required, permission)) = contract.host.get(method) else {
        let hint = if contract.plugin.iter().any(|m| m == method) {
            format!("`{method}` is sent by the host to the plugin, not the other way round.")
        } else {
            "Call host.describe for the methods this build answers.".to_string()
        };
        return Err(RpcError::new(rpc::METHOD_NOT_FOUND, format!("no host method `{method}`"), hint, "references/host-api.md#methods-the-plugin-calls"));
    };
    if let Some(permission) = permission {
        if !ctx.manifest.has_permission(permission) {
            return Err(RpcError::new(
                rpc::PERMISSION_DENIED,
                format!("{method} needs the {permission} permission"),
                format!("Add \"{permission}\" to permissions in super-desktop-plugin.json; users approve it again on update."),
                "references/manifest.md#permissions",
            ));
        }
    }
    for name in required {
        if params.get(name).is_none() {
            return Err(RpcError::invalid_params(format!("{method} needs `{name}`"), &docs_for(method)));
        }
    }
    if !IMPLEMENTED.contains(&method) {
        return Err(RpcError::new(
            rpc::UNAVAILABLE,
            format!("{method} is part of plugin API 1 but this build ({}) does not support it yet", crate::updates::running()),
            "Check host.describe().methods before calling optional features, and raise engines.superDesktop when you depend on one.",
            &docs_for(method),
        ));
    }
    let manifest = &ctx.manifest;
    match method {
        "host.describe" => Ok(json!({
            "apiVersion": super::API_VERSION,
            "hostVersion": crate::updates::running().to_string(),
            "pluginId": manifest.id,
            "permissions": manifest.permissions,
            "methods": IMPLEMENTED,
            "limits": limits(),
            "llm": llm::resolve(&store::Store::load().llm_provider).map(|p| json!({"provider": p})),
        })),
        "log" => {
            let level = param(params, "level").as_str().filter(|l| ["debug", "info", "warn", "error"].contains(l)).unwrap_or("info");
            ctx.log(level, param(params, "message").as_str().unwrap_or(""));
            Ok(json!({}))
        }
        "settings.get" => match param(params, "secret").as_str() {
            Some(key) => Ok(json!({ key: store::secret(manifest, key) })),
            None => Ok(Value::Object(store::settings(manifest))),
        },
        "settings.set" => {
            let key = string(params, "key", 48, &docs_for(method))?;
            store::set_setting(manifest, &key, param(params, "value").clone())
                .map_err(|why| RpcError::invalid_params(why, "references/manifest.md#contribution-points"))?;
            Ok(json!({}))
        }
        "storage.get" => {
            let key = string(params, "key", 128, &docs_for(method))?;
            Ok(json!({"value": store::storage_get(&manifest.id, &key)}))
        }
        "storage.set" => {
            let key = string(params, "key", 128, &docs_for(method))?;
            store::storage_set(&manifest.id, &key, param(params, "value").clone())
                .map_err(|why| RpcError::new(rpc::LIMIT_EXCEEDED, why, "Store less, or keep large files in dataDir.", &docs_for(method)))?;
            Ok(json!({}))
        }
        "ui.notify" => notify(ctx, params),
        "llm.complete" => llm_complete(ctx, params),
        "contrib.update" => {
            check_contrib_update(manifest, params)?;
            ctx.ui.call(&manifest.id, method, params.clone())
        }
        "ui.open" => {
            let view = string(params, "view", 96, &docs_for(method))?;
            if !manifest.contributes.views.iter().any(|v| v.id == view) {
                return Err(RpcError::new(rpc::NOT_FOUND, format!("view `{view}` is not declared"), "Declare it in contributes.views.", "references/ui.md#declaring-and-opening"));
            }
            if let Some(anchor) = param(params, "anchor").as_str() {
                let declared = manifest.contributes.toolbar.iter().any(|t| t.id == anchor) || manifest.contributes.card_buttons.iter().any(|b| b.id == anchor);
                if !declared {
                    return Err(RpcError::new(rpc::NOT_FOUND, format!("anchor `{anchor}` is not a declared toolbar item or card button"), "Leave anchor out, or use a declared id.", "references/ui.md#declaring-and-opening"));
                }
            }
            super::ui_model::check_tree(param(params, "model"))?;
            ctx.ui.call(&manifest.id, method, params.clone())
        }
        "ui.patch" => {
            string(params, "handle", 64, &docs_for(method))?;
            let ops = param(params, "ops").as_array().ok_or_else(|| RpcError::invalid_params("`ops` must be an array", "references/ui.md#patching"))?;
            if ops.len() > super::ui_model::MAX_OPS {
                return Err(RpcError::new(rpc::LIMIT_EXCEEDED, format!("more than {} ops", super::ui_model::MAX_OPS), "Split the patch.", "references/ui.md#patching"));
            }
            for op in ops {
                super::ui_model::check_op(op)?;
            }
            ctx.ui.call(&manifest.id, method, params.clone())
        }
        "ui.close" => {
            string(params, "handle", 64, &docs_for(method))?;
            ctx.ui.call(&manifest.id, method, params.clone())
        }
        "workspace.cards" => ctx.ui.call(&manifest.id, method, params.clone()),
        "card.restore" | "card.expand" | "card.collapse" | "card.focus" | "card.close" => {
            string(params, "card", 128, &docs_for(method))?;
            ctx.ui.call(&manifest.id, method, params.clone())
        }
        "card.iconify" | "card.setRect" => {
            string(params, "card", 128, &docs_for(method))?;
            let (key, fields): (&str, &[&str]) = if method == "card.setRect" { ("rect", &["x", "y", "w", "h"]) } else { ("at", &["x", "y"]) };
            if let Some(value) = params.get(key) {
                let finite = fields.iter().all(|f| value[*f].as_f64().is_some_and(f64::is_finite));
                if !finite {
                    return Err(RpcError::invalid_params(format!("`{key}` needs finite numbers {}", fields.join(", ")), &docs_for(method)));
                }
            }
            ctx.ui.call(&manifest.id, method, params.clone())
        }
        "terminal.text" | "terminal.send" => {
            string(params, "card", 128, &docs_for(method))?;
            // The card's session comes from the desktop; tmux runs here, on
            // this worker, never on the GTK thread.
            let session = ctx.ui.call(&manifest.id, "card.session", json!({"card": params["card"]}))?;
            let session = session["session"].as_str().unwrap_or_default().to_string();
            if method == "terminal.text" {
                let lines = match params.get("lines") {
                    None => 50,
                    Some(v) => v.as_u64().filter(|n| (1..=200).contains(n)).ok_or_else(|| RpcError::invalid_params("`lines` is 1–200", &docs_for(method)))? as usize,
                };
                let screen = crate::tmux::capture_visible_screen(&session)
                    .ok_or_else(|| RpcError::new(rpc::UNAVAILABLE, "the terminal could not be read", "The session may have ended; read workspace.cards again.", &docs_for(method)))?;
                let all: Vec<&str> = screen.trim_end().lines().collect();
                let text = all[all.len().saturating_sub(lines)..].join("\n");
                Ok(json!({"text": text}))
            } else {
                let text = string(params, "text", 16 * 1024, &docs_for(method))?;
                let enter = param(params, "enter").as_bool().unwrap_or(false);
                crate::tmux::send_keys(&session, &text, enter)
                    .map_err(|why| RpcError::new(rpc::UNAVAILABLE, format!("typing failed: {why}"), "The session may have ended; read workspace.cards again.", &docs_for(method)))?;
                ctx.log("info", &format!("terminal.send: {} characters to {session}{}", text.chars().count(), if enter { " + Enter" } else { "" }));
                Ok(json!({}))
            }
        }
        "harness.launch" => {
            let agent = string(params, "agent", 96, &docs_for(method))?;
            string(params, "folder", 4096, &docs_for(method))?;
            if let Some(prompt) = params.get("prompt").filter(|p| !p.is_null()) {
                let prompt = prompt.as_str().filter(|p| p.len() <= 16 * 1024 && !p.contains('\0'))
                    .ok_or_else(|| RpcError::invalid_params("`prompt` is text of at most 16 KiB", &docs_for(method)))?;
                if crate::tmux::initial_prompt_args(&agent, prompt).is_none() {
                    return Err(RpcError::new(
                        rpc::UNAVAILABLE,
                        format!("`{agent}` cannot be started with a prompt"),
                        "Use claude, codex, opencode or gemini, or launch without a prompt and send it with terminal.send once the harness is ready.",
                        &docs_for(method),
                    ));
                }
            }
            ctx.ui.call(&manifest.id, method, params.clone())
        }
        "title.clear" => {
            string(params, "card", 128, &docs_for(method))?;
            ctx.ui.call(&manifest.id, method, params.clone())
        }
        "title.set" => {
            let docs = docs_for(method);
            string(params, "card", 128, &docs)?;
            match params.get("text") {
                None | Some(Value::Null) => {}
                Some(Value::String(t)) if t.chars().count() <= 200 => {}
                Some(_) => return Err(RpcError::invalid_params("`text` is a string of at most 200 characters, or null", &docs)),
            }
            for key in ["chipsBefore", "chipsAfter"] {
                if let Some(chips) = params.get(key) {
                    let chips = chips.as_array().filter(|c| c.len() <= 3).ok_or_else(|| RpcError::invalid_params(format!("`{key}` is an array of at most 3 chips"), &docs))?;
                    for chip in chips {
                        let text_ok = chip["text"].as_str().is_some_and(|t| (1..=24).contains(&t.chars().count()));
                        let tone_ok = chip.get("tone").is_none_or(|t| t.as_str().is_some_and(|t| ["neutral", "accent", "success", "warning", "error"].contains(&t)));
                        let tip_ok = chip.get("tooltip").is_none_or(|t| t.as_str().is_some_and(|t| t.chars().count() <= 120));
                        if !(text_ok && tone_ok && tip_ok) {
                            return Err(RpcError::invalid_params(format!("each chip in `{key}` is {{text: 1–24 characters, tone?, tooltip?}}"), &docs));
                        }
                    }
                }
            }
            ctx.ui.call(&manifest.id, method, params.clone())
        }
        _ => unreachable!("every IMPLEMENTED method is matched"),
    }
}

/// `references/host-api.md` section for a method (GitHub heading anchors).
fn docs_for(method: &str) -> String {
    let anchor = match method {
        "ui.open" | "ui.patch" | "ui.close" => "uiopen--uipatch--uiclose".to_string(),
        "settings.get" | "settings.set" => "settingsget--settingsset".to_string(),
        "storage.get" | "storage.set" => "storageget--storageset".to_string(),
        "terminal.text" | "terminal.send" => "terminaltext--terminalsend".to_string(),
        "title.set" | "title.clear" => "titleset--titleclear".to_string(),
        m if m.starts_with("card.") => "card".to_string(),
        m => m.replace('.', "").to_lowercase(),
    };
    format!("references/host-api.md#{anchor}")
}

fn check_contrib_update(manifest: &Manifest, params: &Value) -> Result<(), RpcError> {
    const DOCS: &str = "references/host-api.md#contribupdate";
    let id = string(params, "id", 96, DOCS)?;
    let c = &manifest.contributes;
    let declared = c.toolbar.iter().any(|t| t.id == id) || c.card_buttons.iter().any(|b| b.id == id) || c.commands.iter().any(|m| m.id == id);
    if !declared {
        return Err(RpcError::new(rpc::NOT_FOUND, format!("`{id}` is not a declared toolbar item, card button or command"), "Contribution ids cannot be added at run time; declare it in the manifest.", DOCS));
    }
    for (key, max) in [("label", 24), ("tooltip", 120), ("icon", 256)] {
        if let Some(value) = params.get(key) {
            if !value.as_str().is_some_and(|s| s.chars().count() <= max) {
                return Err(RpcError::invalid_params(format!("`{key}` must be a string of at most {max} characters"), DOCS));
            }
        }
    }
    if let Some(badge) = params.get("badge") {
        if !(badge.is_null() || badge.as_str().is_some_and(|s| s.chars().count() <= 4)) {
            return Err(RpcError::invalid_params("`badge` is at most 4 characters, or null", DOCS));
        }
    }
    for key in ["enabled", "visible"] {
        if params.get(key).is_some_and(|v| !v.is_boolean()) {
            return Err(RpcError::invalid_params(format!("`{key}` must be true or false"), DOCS));
        }
    }
    Ok(())
}

fn notify(ctx: &Context, params: &Value) -> Result<Value, RpcError> {
    const DOCS: &str = "references/host-api.md#uinotify";
    let title = string(params, "title", 80, DOCS)?;
    let body = match params.get("body") {
        None => String::new(),
        Some(_) => string(params, "body", 400, DOCS)?,
    };
    let urgency = param(params, "urgency").as_str().unwrap_or("normal");
    if !["low", "normal", "critical"].contains(&urgency) {
        return Err(RpcError::invalid_params("`urgency` is low, normal or critical", DOCS));
    }
    {
        let mut sent = ctx.notifications.lock().unwrap_or_else(|e| e.into_inner());
        while sent.front().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            sent.pop_front();
        }
        if sent.len() >= NOTIFY_PER_MINUTE {
            return Err(RpcError::new(rpc::LIMIT_EXCEEDED, "more than 6 notifications in a minute", "Combine notifications; show progress in a view instead.", DOCS));
        }
        sent.push_back(Instant::now());
    }
    // The app name says which plugin this is, so a plugin cannot pass for
    // SUPER DESKTOP's own notices (pairing, updates).
    let app = format!("SUPER DESKTOP · {}", ctx.manifest.name);
    ctx.ui.notify(&app, urgency, &title, &body);
    Ok(json!({}))
}

#[cfg(not(test))]
fn send_notification(app: &str, urgency: &str, title: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", app, "-u", urgency, "--", title, body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| child.wait());
}

/// Tests never put notifications on the user's desktop.
#[cfg(test)]
fn send_notification(_app: &str, _urgency: &str, _title: &str, _body: &str) {}

fn llm_complete(ctx: &Context, params: &Value) -> Result<Value, RpcError> {
    const DOCS: &str = "references/host-api.md#llmcomplete";
    let prompt = string(params, "prompt", llm::MAX_PROMPT, DOCS)?;
    let system = match params.get("system") {
        None | Some(Value::Null) => None,
        Some(_) => Some(string(params, "system", 16 * 1024, DOCS)?),
    };
    let max_tokens = match params.get("maxTokens") {
        None => 1024,
        Some(v) => v.as_u64().filter(|n| (1..=8192).contains(n)).ok_or_else(|| RpcError::invalid_params("`maxTokens` is 1–8192", DOCS))? as u32,
    };
    let json_mode = param(params, "json").as_bool().unwrap_or(false);
    let fast = match param(params, "tier").as_str() {
        None | Some("default") => false,
        Some("fast") => true,
        Some(_) => return Err(RpcError::invalid_params("`tier` is default or fast", DOCS)),
    };
    if ctx.llm_calls.fetch_add(1, Ordering::SeqCst) >= MAX_LLM_CALLS {
        ctx.llm_calls.fetch_sub(1, Ordering::SeqCst);
        return Err(RpcError::new(rpc::LIMIT_EXCEEDED, "2 llm.complete calls are already running", "Wait for one to finish; queue the rest.", DOCS));
    }
    let started = Instant::now();
    let request = llm::Request { prompt: prompt.clone(), system, max_tokens, json: json_mode, fast };
    let result = match &ctx.llm {
        Some(answer) => answer(&request),
        None => llm::complete(&store::Store::load().llm_provider, &request),
    };
    ctx.llm_calls.fetch_sub(1, Ordering::SeqCst);
    // Sizes and timing only: prompts can hold the user's code.
    match &result {
        Ok(reply) => ctx.log("info", &format!("llm.complete via {} ({}): {} bytes in, {} bytes out, {} ms", reply.provider, reply.model, prompt.len(), reply.text.len(), started.elapsed().as_millis())),
        Err(error) => ctx.log("warn", &format!("llm.complete failed after {} ms: {}", started.elapsed().as_millis(), error.reason())),
    }
    result.map(|reply| json!({"text": reply.text, "provider": reply.provider, "model": reply.model}))
}

/// One plugin's running process and the workers answering it.
pub struct Session {
    pub id: String,
    pub process: Process,
    pub ctx: Arc<Context>,
    stopping: Arc<std::sync::atomic::AtomicBool>,
}

impl Session {
    /// Start the process and activate it. Blocks up to `ACTIVATE_TIMEOUT`:
    /// call it from a worker thread.
    pub fn start(manifest: Arc<Manifest>, dir: &Path, ui: Arc<dyn Ui>) -> Result<Session, String> {
        Self::start_with(manifest, dir, ui, None)
    }

    /// `start`, with `llm.complete` answered by `llm` (`plugin test`).
    pub fn start_with(manifest: Arc<Manifest>, dir: &Path, ui: Arc<dyn Ui>, llm: Option<LlmOverride>) -> Result<Session, String> {
        let main = manifest.main.as_ref().ok_or("the plugin has no process component")?;
        let id = manifest.id.clone();
        let data = super::data_dir(&id);
        std::fs::create_dir_all(&data).map_err(|e| format!("cannot create {}: {e}", data.display()))?;
        let mut env: Vec<(String, String)> = main.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        env.extend([
            ("SD_PLUGIN_ID".into(), id.clone()),
            ("SD_PLUGIN_API".into(), super::API_VERSION.to_string()),
            ("SD_PLUGIN_DIR".into(), dir.display().to_string()),
            ("SD_PLUGIN_DATA".into(), data.display().to_string()),
            ("PYTHONUNBUFFERED".into(), "1".into()),
        ]);
        let mut ctx = Context::new(Arc::clone(&manifest), ui);
        ctx.llm = llm;
        let ctx = Arc::new(ctx);
        ctx.log("info", &format!("starting {} {} from {}", manifest.id, manifest.version, dir.display()));
        let (process, events) = Process::spawn(Spawn { command: &main.command, dir, env, log: ctx.log.clone() })
            .map_err(|e| format!("cannot start `{}`: {e}", main.command.join(" ")))?;
        let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let process = process.clone();
            let ctx = Arc::clone(&ctx);
            let stopping = Arc::clone(&stopping);
            std::thread::Builder::new()
                .name(format!("plugin-{id}"))
                .spawn(move || serve(events, process, ctx, stopping))
                .map_err(|e| e.to_string())?;
        }
        let params = json!({
            "apiVersion": super::API_VERSION,
            "hostVersion": crate::updates::running().to_string(),
            "pluginId": id,
            "pluginDir": dir.display().to_string(),
            "dataDir": data.display().to_string(),
            "settings": store::settings(&manifest),
            "permissions": manifest.permissions,
        });
        match process.request("activate", params, ACTIVATE_TIMEOUT) {
            Ok(_) => {
                ctx.log("info", "activated");
                Ok(Session { id, process, ctx, stopping })
            }
            Err(error) => {
                stopping.store(true, Ordering::SeqCst);
                process.kill(Duration::from_millis(200));
                // What the process said last is usually the reason (a syntax
                // error, a missing module).
                std::thread::sleep(Duration::from_millis(100));
                let said: Vec<String> = std::fs::read_to_string(&ctx.log)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|l| l.split_once(" stderr ").map(|(_, t)| t.trim().to_string()))
                    .collect();
                let tail = said[said.len().saturating_sub(3)..].join(" | ");
                let why = if tail.is_empty() { error.reason() } else { format!("{} — it said: {tail}", error.reason()) };
                ctx.log("error", &format!("activate failed: {why}"));
                Err(format!("activate failed: {why}"))
            }
        }
    }

    pub fn notify(&self, method: &str, params: Value) -> bool {
        self.process.notify(method, params)
    }

    /// `deactivate` with its 1 s, then the process group goes. Blocks up to
    /// about 3 s: call it from a worker thread.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.process.request("deactivate", json!({}), DEACTIVATE_TIMEOUT);
        self.process.kill(KILL_GRACE);
        self.ctx.log("info", "stopped");
    }
}

fn serve(events: std::sync::mpsc::Receiver<Event>, process: Process, ctx: Arc<Context>, stopping: Arc<std::sync::atomic::AtomicBool>) {
    let in_flight = Arc::new(AtomicUsize::new(0));
    while let Ok(event) = events.recv() {
        match event {
            Event::Request { id, method, params } => {
                if stopping.load(Ordering::SeqCst) {
                    process.respond(&id, Err(RpcError::new(rpc::CANCELLED, "the plugin is being turned off", "Stop and return.", "references/host-api.md#lifecycle")));
                    continue;
                }
                if in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    process.respond(&id, Err(RpcError::new(rpc::LIMIT_EXCEEDED, "too many requests at once", "Wait for answers before sending more.", "references/host-api.md#transport")));
                    continue;
                }
                let (process, ctx, in_flight) = (process.clone(), Arc::clone(&ctx), Arc::clone(&in_flight));
                std::thread::spawn(move || {
                    let result = dispatch(&ctx, &method, &params);
                    if let Err(error) = &result {
                        ctx.log("warn", &format!("{method}: {} — {}", error.message, error.reason()));
                    }
                    process.respond(&id, result);
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Event::Notification { method, params } => {
                if method == "log" {
                    let _ = dispatch(&ctx, "log", &params);
                } else {
                    ctx.log("warn", &format!("ignored notification `{method}` (host methods expect an id)"));
                }
            }
            Event::ProtocolError(why) => ctx.log("error", &format!("protocol: {why}")),
            Event::Exited => {
                if !stopping.load(Ordering::SeqCst) {
                    ctx.log("error", "the process exited");
                    ctx.ui.event(&ctx.manifest.id, SessionEvent::Exited);
                }
                break;
            }
        }
    }
}

/// Settings as `Map`, for callers outside this module.
pub fn settings_of(manifest: &Manifest) -> Map<String, Value> {
    store::settings(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeUi {
        calls: Mutex<Vec<(String, Value)>>,
        events: Mutex<Vec<SessionEvent>>,
    }

    impl Ui for FakeUi {
        fn call(&self, _plugin: &str, method: &str, params: Value) -> Result<Value, RpcError> {
            self.calls.lock().unwrap().push((method.to_string(), params));
            Ok(if method == "ui.open" { json!({"handle": "h1"}) } else { json!({}) })
        }
        fn event(&self, _plugin: &str, event: SessionEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn manifest() -> Arc<Manifest> {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/skills/super-desktop-plugin/examples/git-flush/super-desktop-plugin.json")).unwrap();
        Arc::new(super::super::manifest::validate_text(&text, None).manifest.unwrap())
    }

    fn ctx() -> (Context, Arc<FakeUi>) {
        let ui = Arc::new(FakeUi { calls: Mutex::default(), events: Mutex::default() });
        let mut ctx = Context::new(manifest(), ui.clone());
        ctx.log = std::env::temp_dir().join(format!("sd-api-{}.log", std::process::id()));
        (ctx, ui)
    }

    #[test]
    fn plugin_spec_implemented_methods_are_in_the_contract() {
        let contract = contract();
        for method in IMPLEMENTED {
            assert!(contract.host.contains_key(method), "{method} is not in host-api.openrpc.json");
        }
        assert!(contract.plugin.iter().any(|m| m == "activate") && contract.host.len() >= 26);
    }

    #[test]
    fn plugin_api_checks_before_acting() {
        let (ctx, ui) = ctx();
        let code = |method: &str, params: Value| dispatch(&ctx, method, &params).unwrap_err().code;
        assert_eq!(code("nope", json!({})), rpc::METHOD_NOT_FOUND);
        assert_eq!(code("activate", json!({})), rpc::METHOD_NOT_FOUND);
        assert_eq!(code("terminal.send", json!({"card": "c", "text": "x"})), rpc::PERMISSION_DENIED);
        assert_eq!(code("ui.open", json!({"model": {}})), rpc::INVALID_PARAMS);
        assert_eq!(code("ui.open", json!({"view": "git-flush.nope", "model": {"type": "spinner", "id": "s"}})), rpc::NOT_FOUND);
        assert_eq!(code("ui.open", json!({"view": "git-flush.repos", "model": {"type": "div", "id": "s"}})), rpc::INVALID_PARAMS);
        assert_eq!(code("contrib.update", json!({"id": "git-flush.other"})), rpc::NOT_FOUND);
        assert_eq!(code("contrib.update", json!({"id": "git-flush.button", "badge": "12345"})), rpc::INVALID_PARAMS);
        assert_eq!(code("llm.complete", json!({"prompt": "x", "tier": "huge"})), rpc::INVALID_PARAMS);
        assert!(ui.calls.lock().unwrap().is_empty(), "nothing reached the UI");
        let opened = dispatch(&ctx, "ui.open", &json!({"view": "git-flush.repos", "model": {"type": "spinner", "id": "s"}, "anchor": "git-flush.button"})).unwrap();
        assert_eq!(opened["handle"], "h1");
        dispatch(&ctx, "contrib.update", &json!({"id": "git-flush.button", "badge": null})).unwrap();
        assert_eq!(ui.calls.lock().unwrap().len(), 2);
        let described = dispatch(&ctx, "host.describe", &json!({})).unwrap();
        assert_eq!(described["apiVersion"], 1);
        assert_eq!(described["methods"].as_array().unwrap().len(), IMPLEMENTED.len());
        let _ = std::fs::remove_file(&ctx.log);
    }

    #[test]
    fn plugin_api_every_error_has_hint_and_docs() {
        let (ctx, _) = ctx();
        for (method, params) in [
            ("nope", json!({})),
            ("terminal.send", json!({"card": "c", "text": "x"})),
            ("ui.open", json!({})),
            ("ui.patch", json!({"handle": "h", "ops": [{"op": "move", "id": "x"}]})),
        ] {
            let error = dispatch(&ctx, method, &params).unwrap_err();
            let data = error.data.expect("data");
            assert!(!data["hint"].as_str().unwrap().is_empty(), "{method}");
            assert!(data["docs"].as_str().unwrap().starts_with("references/"), "{method}");
        }
    }

    /// GitHub's heading anchor: lowercase, punctuation dropped, spaces → `-`.
    fn slug(heading: &str) -> String {
        heading
            .trim()
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
            .map(|c| if c == ' ' { '-' } else { c })
            .collect()
    }

    /// Every `docs` pointer the host can send names an existing reference
    /// file and heading, so an agent following it lands on the right section.
    #[test]
    fn plugin_spec_docs_pointers_resolve() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut pointers: Vec<String> = contract().host.keys().map(|m| docs_for(m)).collect();
        for file in ["api.rs", "manifest.rs", "rpc.rs", "llm.rs", "ui_model.rs", "testing.rs", "../plugin_ui/mod.rs", "../plugin_ui/cards.rs"] {
            let source = std::fs::read_to_string(root.join("src/plugin_host").join(file)).unwrap();
            for (at, _) in source.match_indices("\"references/") {
                let rest = &source[at + 1..];
                let pointer = &rest[..rest.find('"').unwrap()];
                // `format!` templates are covered by `docs_for` above.
                if !pointer.contains('{') && pointer.contains(".md") {
                    pointers.push(pointer.to_string());
                }
            }
        }
        assert!(pointers.len() > 40);
        for pointer in pointers {
            let (file, anchor) = pointer.split_once('#').map(|(f, a)| (f, Some(a))).unwrap_or((&pointer, None));
            let text = std::fs::read_to_string(root.join("skills/super-desktop-plugin").join(file)).unwrap_or_else(|_| panic!("{pointer}: no such file"));
            if let Some(anchor) = anchor {
                let found = text.lines().filter_map(|l| l.strip_prefix('#')).map(|h| slug(h.trim_start_matches('#'))).any(|s| s == anchor);
                assert!(found, "{pointer}: no heading with that anchor");
            }
        }
    }

    #[test]
    fn plugin_api_notifications_are_rate_limited() {
        let (ctx, _) = ctx();
        for _ in 0..NOTIFY_PER_MINUTE {
            dispatch(&ctx, "ui.notify", &json!({"title": "t", "urgency": "low"})).ok();
        }
        assert_eq!(dispatch(&ctx, "ui.notify", &json!({"title": "t"})).unwrap_err().code, rpc::LIMIT_EXCEEDED);
    }
}
