//! `super-desktop plugin …`: the commands people and coding agents use to
//! check, install and drive plugins (`skills/super-desktop-plugin/references/testing.md`).
//!
//! Store changes are written here and the daemon, if running, is told to
//! sync; it starts and stops plugins on its own threads.
use super::manifest::{self, Manifest};
use super::store::{self, Installed, Source};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: super-desktop plugin <command>

  describe [--json]            what this build supports (API, contribution points, methods, limits)
  validate [DIR] [--json]      check a plugin's manifest and files
  link DIR [--yes]             install a plugin folder in place (development) and grant its permissions
  list [--json]                installed plugins and their state
  activate ID                  turn a plugin on
  deactivate ID                turn a plugin off; everything it added goes away
  reload ID                    turn it off and on again (after editing it)
  run ID COMMAND [JSON]        run one of its commands, as if clicked
  views ID [--json]            its open views and every node's state (text, value, visible, enabled)
  interact ID NODE EVENT [VALUE] [--view=VIEW]
                               operate a node as a user would: click, change, submit (VALUE is JSON)
  logs ID [--follow]           its log: stderr, log calls, host errors
  remove ID [--purge]          uninstall; --purge also deletes its settings and data
  disable-all                  turn every plugin off (works without the daemon)";

pub fn run(args: &[String]) -> i32 {
    let json_out = args.iter().any(|a| a == "--json");
    let positional: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with("--")).collect();
    let result = match positional.first().copied() {
        Some("describe") => describe(json_out),
        Some("validate") => validate(Path::new(positional.get(1).copied().unwrap_or(".")), json_out),
        Some("link") => match positional.get(1) {
            Some(dir) => link(Path::new(dir), args.iter().any(|a| a == "--yes")),
            None => Err("plugin link DIR".into()),
        },
        Some("list") => list(json_out),
        Some("activate") => with_id(&positional, |id| set_active(id, true)),
        Some("deactivate") => with_id(&positional, |id| set_active(id, false)),
        Some("reload") => with_id(&positional, |id| daemon(json!({"op": "reload", "id": id})).map(|_| println!("Reloaded {id}."))),
        Some("run") => match (positional.get(1), positional.get(2)) {
            (Some(id), Some(command)) => {
                let args = match positional.get(3) {
                    Some(text) => serde_json::from_str::<Value>(text).map_err(|e| format!("JSON argument: {e}")),
                    None => Ok(Value::Null),
                };
                args.and_then(|args| daemon(json!({"op": "run", "id": id, "command": command, "args": args}))).map(|_| ())
            }
            _ => Err("plugin run ID COMMAND [JSON]".into()),
        },
        Some("logs") => with_id(&positional, |id| logs(id, args.iter().any(|a| a == "--follow"))),
        Some("views") => with_id(&positional, |id| {
            let reply = daemon(json!({"op": "views", "id": id}))?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&reply["views"]).unwrap_or_default());
            } else {
                for view in reply["views"].as_array().into_iter().flatten() {
                    println!("{} ({})", view["view"].as_str().unwrap_or(""), view["handle"].as_str().unwrap_or(""));
                    for (node, state) in view["nodes"].as_object().into_iter().flatten() {
                        println!("  {node:<24} {state}");
                    }
                }
            }
            Ok(())
        }),
        Some("interact") => match (positional.get(1), positional.get(2), positional.get(3)) {
            (Some(id), Some(node), Some(event)) => {
                let value = match positional.get(4) {
                    Some(text) => serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.to_string())),
                    None => Value::Null,
                };
                let view = args.iter().find_map(|a| a.strip_prefix("--view="));
                daemon(json!({"op": "interact", "id": id, "node": node, "event": event, "value": value, "view": view})).map(|_| ())
            }
            _ => Err("plugin interact ID NODE EVENT [VALUE] [--view=VIEW]".into()),
        },
        Some("remove") => with_id(&positional, |id| remove(id, args.iter().any(|a| a == "--purge"))),
        Some("disable-all") => disable_all(),
        _ => {
            println!("{USAGE}");
            return if positional.is_empty() { 0 } else { 2 };
        }
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("super-desktop plugin: {error}");
            1
        }
    }
}

fn with_id(positional: &[&str], action: impl FnOnce(&str) -> Result<(), String>) -> Result<(), String> {
    match positional.get(1) {
        Some(id) => action(id),
        None => Err(format!("plugin {} ID", positional[0])),
    }
}

pub fn describe_value() -> Value {
    let contract = super::api::contract();
    json!({
        "apiVersion": super::API_VERSION,
        "hostVersion": crate::updates::running().to_string(),
        "manifestVersion": 1,
        "contributionPoints": {
            "supported": manifest::SUPPORTED_CONTRIBUTIONS,
            "notYetSupported": manifest::ALL_CONTRIBUTIONS.iter().filter(|c| !manifest::SUPPORTED_CONTRIBUTIONS.contains(c)).collect::<Vec<_>>(),
        },
        "permissions": manifest::PERMISSIONS,
        "methods": {
            "implemented": super::api::IMPLEMENTED,
            "notYetImplemented": contract.host.keys().filter(|m| !super::api::IMPLEMENTED.contains(&m.as_str())).collect::<Vec<_>>(),
            "sentToPlugins": contract.plugin,
        },
        "limits": super::api::limits(),
        "llmProviders": {"installed": super::llm::available(), "chosen": store::Store::load().llm_provider},
        "sandbox": "not enforced by this build: plugins run as you",
        "commands": ["describe", "validate", "link", "list", "activate", "deactivate", "reload", "run", "views", "interact", "logs", "remove", "disable-all"],
        "skills": manifest::skills_dir().map(|d| d.display().to_string()),
        "schemas": manifest::skills_dir().map(|d| json!({
            "manifest": d.join("schemas/manifest.schema.json"),
            "hostApi": d.join("schemas/host-api.openrpc.json"),
            "ui": d.join("schemas/ui.schema.json"),
        })),
    })
}

fn describe(json_out: bool) -> Result<(), String> {
    let value = describe_value();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
        return Ok(());
    }
    println!("SUPER DESKTOP {} · plugin API {}", value["hostVersion"].as_str().unwrap_or("?"), super::API_VERSION);
    println!("Contribution points: {}", join(&value["contributionPoints"]["supported"]));
    println!("Not yet: {}", join(&value["contributionPoints"]["notYetSupported"]));
    println!("Host methods: {}", join(&value["methods"]["implemented"]));
    println!("AI providers installed: {}", join(&value["llmProviders"]["installed"]));
    if let Some(skills) = value["skills"].as_str() {
        println!("Skills and schemas: {skills}");
    }
    println!("Plugins run as you; the sandbox is not enforced by this build.");
    Ok(())
}

fn join(list: &Value) -> String {
    list.as_array().map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

fn validate(dir: &Path, json_out: bool) -> Result<(), String> {
    let loaded = manifest::load_dir(dir);
    if json_out {
        println!("{}", serde_json::to_string_pretty(&loaded.report).unwrap_or_default());
    } else {
        for (kind, problems) in [("error", &loaded.report.errors), ("warning", &loaded.report.warnings)] {
            for p in problems {
                let at = if p.path.is_empty() { String::new() } else { format!("{}: ", p.path) };
                println!("{kind}: {at}{}\n  fix: {}\n  see: {}", p.message, p.hint, p.docs);
            }
        }
        if loaded.report.ok {
            println!("{} is valid ({} warning(s)).", dir.join(manifest::FILE_NAME).display(), loaded.report.warnings.len());
        }
    }
    if loaded.report.ok { Ok(()) } else { Err(format!("{} error(s)", loaded.report.errors.len())) }
}

/// Plain-words consent lines for permissions, as shown at install.
pub fn permission_words(permission: &str) -> &'static str {
    match permission {
        "ui.toolbar" => "adds buttons to the top bar",
        "ui.cardButtons" => "adds buttons to terminal cards",
        "ui.cardControls" => "replaces the minimize/expand/close buttons",
        "ui.titles" => "changes card titles shown on this PC",
        "ui.popup" => "opens its own panels",
        "ui.notify" => "shows desktop notifications",
        "shortcuts.global" => "adds keyboard shortcuts that work everywhere",
        "shortcuts.overlay" => "adds keyboard shortcuts inside SUPER DESKTOP",
        "cards.read" => "sees your cards, their folders and prompts",
        "cards.control" => "moves, resizes, minimizes and closes cards",
        "terminal.read" => "reads what your terminals show",
        "terminal.write" => "TYPES INTO YOUR TERMINALS",
        "harness.provide" => "adds AI harnesses and their config files",
        "harness.launch" => "starts harnesses in new cards",
        "layout.renderer" => "replaces how cards are laid out",
        "llm" => "uses your AI provider",
        _ => "unknown permission",
    }
}

/// Load an installed plugin's manifest from its directory.
pub fn installed_manifest(installed: &Installed) -> Result<Manifest, String> {
    let loaded = manifest::load_dir(&installed.dir);
    match loaded.manifest {
        Some(manifest) if loaded.report.ok => Ok(manifest),
        _ => Err(loaded.report.errors.first().map(|p| format!("{}: {}", p.path, p.message)).unwrap_or_else(|| "invalid manifest".into())),
    }
}

fn link(dir: &Path, yes: bool) -> Result<(), String> {
    let dir: PathBuf = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    let loaded = manifest::load_dir(&dir);
    let Some(manifest) = loaded.manifest.filter(|_| loaded.report.ok) else {
        validate(&dir, false).ok();
        return Err("fix the manifest first (super-desktop plugin validate)".into());
    };
    if let Some(existing) = store::Store::load().get(&manifest.id) {
        if existing.dir != dir {
            return Err(format!("a plugin with id `{}` is already installed from {}; remove it first", manifest.id, existing.dir.display()));
        }
    }
    println!("{} {} ({})", manifest.name, manifest.version, manifest.id);
    println!("Runs as you, from {}. It:", dir.display());
    for permission in &manifest.permissions {
        println!("  - {}", permission_words(permission));
    }
    for shortcut in &manifest.contributes.shortcuts {
        println!("  - shortcut {} ({})", shortcut.default, shortcut.scope);
    }
    if !yes && !confirm("Link and allow these? [y/N] ") {
        return Err("not linked".into());
    }
    store::update(|s| {
        let entry = Installed {
            id: manifest.id.clone(),
            dir: dir.clone(),
            source: Source::Linked { path: dir.clone() },
            version: manifest.version.clone(),
            active: false,
            granted: manifest.permissions.clone(),
            shortcuts: Default::default(),
            extra: Default::default(),
        };
        match s.get_mut(&manifest.id) {
            Some(existing) => {
                existing.version = entry.version;
                existing.granted = entry.granted;
            }
            None => s.plugins.push(entry),
        }
    })
    .map_err(|e| e.to_string())?;
    println!("Linked. Turn it on with: super-desktop plugin activate {}", manifest.id);
    Ok(())
}

fn confirm(question: &str) -> bool {
    use std::io::{BufRead, Write};
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes")
}

fn list(json_out: bool) -> Result<(), String> {
    let store = store::Store::load();
    let status = daemon_status();
    let rows: Vec<Value> = store
        .plugins
        .iter()
        .map(|p| {
            let live = status.as_ref().and_then(|s| s.get(&p.id)).cloned().unwrap_or(Value::Null);
            json!({"id": p.id, "version": p.version, "active": p.active, "dir": p.dir, "source": p.source, "state": live})
        })
        .collect();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&rows).unwrap_or_default());
    } else if rows.is_empty() {
        println!("No plugins installed. Try: super-desktop plugin link <folder>");
    } else {
        for row in rows {
            let state = row["state"]["state"].as_str().unwrap_or(if row["active"] == true { "on" } else { "off" }).to_string();
            println!("{:<24} {:<10} {:<10} {}", row["id"].as_str().unwrap_or(""), row["version"].as_str().unwrap_or(""), state, row["dir"].as_str().unwrap_or(""));
        }
    }
    Ok(())
}

fn set_active(id: &str, active: bool) -> Result<(), String> {
    let installed = store::Store::load().get(id).cloned().ok_or_else(|| format!("no plugin `{id}` is installed (see plugin list)"))?;
    if active {
        let manifest = installed_manifest(&installed)?;
        check_activatable(&installed, &manifest)?;
    }
    store::update(|s| {
        if let Some(p) = s.get_mut(id) {
            p.active = active;
        }
    })
    .map_err(|e| e.to_string())?;
    match daemon(json!({"op": "sync"})) {
        Ok(_) => println!("{id} is {}.", if active { "on" } else { "off" }),
        Err(_) => {
            if !active {
                rewrite_binds_without_daemon();
            }
            println!("{id} will be {} when SUPER DESKTOP starts.", if active { "on" } else { "off" });
        }
    }
    Ok(())
}

/// Whether a plugin may be turned on: this build is in its range and every
/// permission it asks for was granted.
pub fn check_activatable(installed: &Installed, manifest: &Manifest) -> Result<(), String> {
    let range = super::version_range::Range::parse(&manifest.engines.super_desktop).ok_or("invalid engines.superDesktop")?;
    if !range.contains(crate::updates::running()) {
        let why = format!("{} needs SUPER DESKTOP {}; this is {}", manifest.id, manifest.engines.super_desktop, crate::updates::running());
        // A linked plugin is its author's work in progress, often written for
        // the build being developed next to it: warn instead of refusing.
        if !matches!(installed.source, Source::Linked { .. }) {
            return Err(why);
        }
        eprintln!("warning: {why} (allowed for a linked plugin)");
    }
    let missing: Vec<&String> = manifest.permissions.iter().filter(|p| !installed.granted.contains(p)).collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} now asks for {} that you have not allowed; link or install it again to review",
            manifest.id,
            missing.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(())
}

fn logs(id: &str, follow: bool) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    let path = super::log_file(id);
    let mut file = std::fs::File::open(&path).map_err(|_| format!("no log yet at {}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|e| e.to_string())?;
    print!("{text}");
    if !follow {
        return Ok(());
    }
    let mut at = file.stream_position().map_err(|e| e.to_string())?;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if len < at {
            at = 0;
            file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        }
        if len > at {
            file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
            let mut more = String::new();
            file.read_to_string(&mut more).map_err(|e| e.to_string())?;
            print!("{more}");
            at = len;
        }
    }
}

fn remove(id: &str, purge: bool) -> Result<(), String> {
    let installed = store::Store::load().get(id).cloned().ok_or_else(|| format!("no plugin `{id}` is installed"))?;
    store::update(|s| s.plugins.retain(|p| p.id != id)).map_err(|e| e.to_string())?;
    if daemon(json!({"op": "sync"})).is_err() {
        rewrite_binds_without_daemon();
    }
    if let Source::Git { .. } = installed.source {
        let _ = std::fs::remove_dir_all(super::code_dir(id));
    }
    if purge {
        let _ = std::fs::remove_dir_all(super::settings_dir(id));
        let _ = std::fs::remove_dir_all(super::data_dir(id));
    }
    println!("Removed {id}{}.", if purge { " and its settings and data" } else { " (settings and data kept; --purge deletes them)" });
    Ok(())
}

fn disable_all() -> Result<(), String> {
    let count = store::update(|s| {
        let n = s.plugins.iter().filter(|p| p.active).count();
        s.plugins.iter_mut().for_each(|p| p.active = false);
        n
    })
    .map_err(|e| e.to_string())?;
    if daemon(json!({"op": "sync"})).is_err() {
        // No daemon to do it: take every plugin shortcut out of bindings.lua.
        if let Err(error) = crate::shortcut::apply_plugin_binds(&[]) {
            eprintln!("warning: plugin shortcuts not removed: {error}");
        }
    }
    println!("Turned off {count} plugin(s).");
    Ok(())
}

/// Ask the running daemon. `Err` when it is not running or refused.
fn daemon(request: Value) -> Result<Value, String> {
    match crate::ipc_request(&format!("plugin {request}")) {
        crate::Ipc::Reply(text) => {
            let value: Value = serde_json::from_str(&text).map_err(|_| text.clone())?;
            if value["ok"] == true {
                Ok(value)
            } else {
                Err(value["error"].as_str().unwrap_or("refused").to_string())
            }
        }
        crate::Ipc::NoDaemon => Err("SUPER DESKTOP is not running".into()),
        crate::Ipc::Stalled => Err("SUPER DESKTOP did not answer in time".into()),
    }
}

fn daemon_status() -> Option<serde_json::Map<String, Value>> {
    daemon(json!({"op": "status"})).ok().and_then(|v| v["plugins"].as_object().cloned())
}

/// With no daemon running, `bindings.lua` still must not keep shortcuts of
/// plugins that are off or removed.
fn rewrite_binds_without_daemon() {
    let desired = super::binds::desired_from_store(&store::Store::load());
    if let Err(error) = crate::shortcut::apply_plugin_binds(&desired.global) {
        eprintln!("warning: plugin shortcuts not updated: {error}");
    }
}
