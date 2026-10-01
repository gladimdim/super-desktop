//! `super-desktop plugin test`: a headless host for a plugin's scenarios
//! (`skills/super-desktop-plugin/references/testing.md#scenarios`).
//!
//! The plugin's real process runs against a recording UI with no GTK and no
//! daemon: views are JSON node trees patched like the desktop patches its
//! widgets, `llm.complete` answers from the scenario, notifications are only
//! recorded. Host files (settings, data, log) live in a temporary directory.
//! After the steps the plugin is turned off and its process group must be
//! gone.
use super::api::{self, Session, SessionEvent, Ui};
use super::manifest::Manifest;
use super::rpc::{self, RpcError};
use super::{llm, ui_model};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const EXPECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Recorded {
    calls: Vec<(String, Value)>,
    views: BTreeMap<String, (String, Value)>,
    contrib: BTreeMap<String, Map<String, Value>>,
    notifications: Vec<Value>,
    exited: bool,
    next_handle: u64,
}

/// The fake desktop.
struct Headless {
    state: Mutex<Recorded>,
}

impl Ui for Headless {
    fn call(&self, _plugin: &str, method: &str, params: Value) -> Result<Value, RpcError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.calls.push((method.to_string(), params.clone()));
        match method {
            "contrib.update" => {
                let id = params["id"].as_str().unwrap_or_default().to_string();
                let entry = state.contrib.entry(id).or_default();
                for (key, value) in params.as_object().into_iter().flatten().filter(|(k, _)| *k != "id") {
                    entry.insert(key.clone(), value.clone());
                }
                Ok(json!({}))
            }
            "ui.open" => {
                let view = params["view"].as_str().unwrap_or_default().to_string();
                let existing = state.views.iter().find(|(_, (v, _))| *v == view).map(|(h, _)| h.clone());
                let handle = existing.unwrap_or_else(|| {
                    state.next_handle += 1;
                    format!("v{}", state.next_handle)
                });
                state.views.insert(handle.clone(), (view, params["model"].clone()));
                Ok(json!({"handle": handle}))
            }
            "ui.patch" => {
                let handle = params["handle"].as_str().unwrap_or_default();
                let (_, tree) = state.views.get_mut(handle).ok_or_else(|| RpcError::new(rpc::NOT_FOUND, format!("no open view `{handle}`"), "The view was closed; open it again with ui.open.", "references/ui.md#patching"))?;
                let mut patched = tree.clone();
                for (index, op) in params["ops"].as_array().into_iter().flatten().enumerate() {
                    apply(&mut patched, op).map_err(|why| RpcError::invalid_params(format!("ops[{index}]: {why}"), "references/ui.md#patching"))?;
                }
                *tree = patched;
                Ok(json!({}))
            }
            "ui.close" => {
                let handle = params["handle"].as_str().unwrap_or_default();
                state.views.remove(handle).map(|_| json!({})).ok_or_else(|| RpcError::new(rpc::NOT_FOUND, format!("no open view `{handle}`"), "It is already closed.", "references/ui.md#declaring-and-opening"))
            }
            // Recorded, never started: expect it with {"call": "harness.launch", "params": …}.
            "harness.launch" => {
                state.next_handle += 1;
                Ok(json!({"card": format!("test-card-{}", state.next_handle)}))
            }
            "workspace.cards" => Ok(json!({"screen": {"w": 1920, "h": 1080, "top": 46}, "cards": []})),
            _ => Err(RpcError::new(rpc::UNAVAILABLE, format!("{method} is not simulated by plugin test"), "Test it on a real desktop with plugin link.", "references/testing.md")),
        }
    }

    fn event(&self, _plugin: &str, event: SessionEvent) {
        if event == SessionEvent::Exited {
            self.state.lock().unwrap_or_else(|e| e.into_inner()).exited = true;
        }
    }

    fn notify(&self, _app: &str, urgency: &str, title: &str, body: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.notifications.push(json!({"title": title, "body": body, "urgency": urgency}));
    }
}

fn find<'a>(node: &'a Value, id: &str) -> Option<&'a Value> {
    if node["id"] == id {
        return Some(node);
    }
    node["children"].as_array()?.iter().find_map(|child| find(child, id))
}

fn find_mut<'a>(node: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    if node["id"] == id {
        return Some(node);
    }
    node.get_mut("children")?.as_array_mut()?.iter_mut().find_map(|child| find_mut(child, id))
}

/// The parent of `id` and its position there.
fn parent_of<'a>(node: &'a mut Value, id: &str) -> Option<(&'a mut Vec<Value>, usize)> {
    let children = node.get_mut("children")?.as_array_mut()?;
    if let Some(index) = children.iter().position(|c| c["id"] == id) {
        return Some((children, index));
    }
    children.iter_mut().find_map(|child| parent_of(child, id))
}

fn ids(node: &Value, out: &mut Vec<String>) {
    if let Some(id) = node["id"].as_str() {
        out.push(id.to_string());
    }
    for child in node["children"].as_array().into_iter().flatten() {
        ids(child, out);
    }
}

/// One `ui.patch` op on a JSON tree, with the desktop's rules.
fn apply(tree: &mut Value, op: &Value) -> Result<(), String> {
    let id = op["id"].as_str().unwrap_or_default();
    let kind = find(tree, id).ok_or_else(|| format!("no node `{id}` in this view"))?["type"].as_str().unwrap_or_default().to_string();
    let new_ids = |tree: &Value, node: &Value, replacing: Option<&str>| -> Result<(), String> {
        let mut incoming = Vec::new();
        ids(node, &mut incoming);
        let mut kept = Vec::new();
        match replacing {
            Some(old) => {
                let mut without = tree.clone();
                if let Some((children, index)) = parent_of(&mut without, old) {
                    children.remove(index);
                    ids(&without, &mut kept);
                }
            }
            None => ids(tree, &mut kept),
        }
        match incoming.iter().find(|i| kept.contains(i)) {
            Some(dup) => Err(format!("id `{dup}` already exists in the view")),
            None => Ok(()),
        }
    };
    match op["op"].as_str().unwrap_or_default() {
        "set" => {
            let props = op["props"].as_object().cloned().unwrap_or_default();
            ui_model::check_props(&kind, &props, id, true).map_err(|e| e.reason())?;
            let node = find_mut(tree, id).expect("found above");
            for (key, value) in props {
                node[key] = value;
            }
        }
        "remove" => match parent_of(tree, id) {
            Some((children, index)) => {
                children.remove(index);
            }
            None => return Err("the root cannot be removed; close the view".into()),
        },
        "append" => {
            if !matches!(kind.as_str(), "column" | "row" | "list" | "scroll") {
                return Err(format!("`{id}` ({kind}) cannot have children"));
            }
            new_ids(tree, &op["node"], None)?;
            let node = find_mut(tree, id).expect("found above");
            match node.get_mut("children").and_then(Value::as_array_mut) {
                Some(children) => children.push(op["node"].clone()),
                None => node["children"] = json!([op["node"].clone()]),
            }
        }
        "replace" => {
            new_ids(tree, &op["node"], Some(id))?;
            match parent_of(tree, id) {
                Some((children, index)) => children[index] = op["node"].clone(),
                None => *tree = op["node"].clone(),
            }
        }
        other => return Err(format!("unknown op `{other}`")),
    }
    Ok(())
}

/// Whether a person could reach `id`: it and every parent visible, and it enabled.
fn reachable(tree: &Value, id: &str) -> Result<(), String> {
    fn walk(node: &Value, id: &str) -> Option<Result<(), String>> {
        let hidden = node["visible"] == false;
        if node["id"] == id {
            return Some(if hidden {
                Err(format!("`{id}` is hidden"))
            } else if node["enabled"] == false {
                Err(format!("`{id}` is disabled"))
            } else {
                Ok(())
            });
        }
        let found = node["children"].as_array()?.iter().find_map(|c| walk(c, id))?;
        Some(if hidden { Err(format!("`{id}` is hidden")) } else { found })
    }
    walk(tree, id).unwrap_or_else(|| Err(format!("no node `{id}` in this view")))
}

/// `actual` has every field of `wanted` with the same value (objects nest).
fn contains(actual: &Value, wanted: &Value) -> bool {
    match (actual, wanted) {
        (Value::Object(a), Value::Object(w)) => w.iter().all(|(k, v)| a.get(k).is_some_and(|x| contains(x, v))),
        _ => actual == wanted,
    }
}

pub struct Outcome {
    pub name: String,
    pub ok: bool,
    pub lines: Vec<String>,
}

/// Run one scenario file against the plugin in `dir`.
pub fn run_scenario(dir: &Path, manifest: &Arc<Manifest>, path: &Path) -> Outcome {
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("scenario").to_string();
    let mut lines = Vec::new();
    let result = run_inner(dir, manifest, path, &mut lines);
    let ok = result.is_ok();
    if let Err(why) = result {
        lines.push(format!("FAIL {why}"));
    }
    Outcome { name, ok, lines }
}

fn run_inner(dir: &Path, manifest: &Arc<Manifest>, path: &Path, lines: &mut Vec<String>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw: Value = serde_json::from_str(&text).map_err(|e| format!("{}: not JSON: {e}", path.display()))?;
    // `${fixture}` is a fresh copy of tests/fixtures/ for this scenario.
    let scratch = std::env::temp_dir().join(format!("sd-plugin-test-{}-{}", std::process::id(), nanos()));
    let fixtures = scratch.join("fixtures");
    std::fs::create_dir_all(&fixtures).map_err(|e| e.to_string())?;
    let source = dir.join("tests/fixtures");
    if source.is_dir() {
        copy_dir(&source, &fixtures).map_err(|e| format!("copying tests/fixtures: {e}"))?;
    }
    let scenario: Value = serde_json::from_str(&text.replace("${fixture}", &fixtures.display().to_string())).unwrap_or(raw);
    // Host files (set once by `run_all`) start empty for every scenario.
    if let Some(home) = std::env::var_os("SUPER_DESKTOP_PLUGIN_HOME") {
        let _ = std::fs::remove_dir_all(&home);
    }
    // `setup`: a shell command run in the fixtures copy first (e.g. to create
    // git repositories), with $FIXTURE pointing at it.
    if let Some(setup) = scenario.get("setup").and_then(Value::as_str) {
        let out = std::process::Command::new("sh")
            .args(["-c", setup])
            .current_dir(&fixtures)
            .env("FIXTURE", &fixtures)
            .output()
            .map_err(|e| format!("setup: {e}"))?;
        if !out.status.success() {
            return Err(format!("setup failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        lines.push("ok   setup".into());
    }
    for (key, value) in scenario["settings"].as_object().into_iter().flatten() {
        super::store::set_setting(manifest, key, value.clone()).map_err(|why| format!("settings.{key}: {why}"))?;
    }
    let replies: Vec<Result<String, ()>> = match &scenario["llm"] {
        Value::Object(llm) if llm.get("unavailable") == Some(&Value::Bool(true)) => vec![Err(())],
        Value::Object(llm) => llm.get("replies").and_then(Value::as_array).cloned().or_else(|| llm.get("reply").map(|r| vec![r.clone()])).unwrap_or_default().into_iter().map(|r| Ok(r.as_str().map(str::to_string).unwrap_or_else(|| r.to_string()))).collect(),
        _ => Vec::new(),
    };
    let llm_calls = Arc::new(Mutex::new(0usize));
    let answer: api::LlmOverride = {
        let llm_calls = Arc::clone(&llm_calls);
        Arc::new(move |_request: &llm::Request| {
            let mut n = llm_calls.lock().unwrap_or_else(|e| e.into_inner());
            let reply = replies.get(*n).or_else(|| replies.last()).cloned();
            *n += 1;
            match reply {
                Some(Ok(text)) => Ok(llm::Reply { text, provider: "scenario".into(), model: "scenario".into() }),
                Some(Err(())) => Err(RpcError::new(rpc::UNAVAILABLE, "no AI provider is installed", "Install and sign in to Claude Code or Codex, then choose it in Settings → Plugins → AI provider.", "references/host-api.md#llmcomplete")),
                None => Err(RpcError::new(rpc::UNAVAILABLE, "the scenario has no llm reply", "Add \"llm\": {\"reply\": …} to the scenario.", "references/testing.md#scenarios")),
            }
        })
    };
    let ui = Arc::new(Headless { state: Mutex::default() });
    let session = Session::start_with(Arc::clone(manifest), dir, ui.clone(), Some(answer)).map_err(|why| format!("start: {why}"))?;
    lines.push(format!("ok   activated (pid {})", session.process.pid));
    let result = play(&scenario, &session, &ui, &llm_calls, lines);
    let group = session.process.pid as i32;
    session.stop();
    // SAFETY: probing whether the plugin's process group still exists.
    let left = unsafe { libc::kill(-group, 0) } == 0;
    let _ = std::fs::remove_dir_all(&scratch);
    result?;
    if left {
        return Err("processes of the plugin are still running after deactivate".into());
    }
    lines.push("ok   deactivated; no process left".into());
    Ok(())
}

fn play(scenario: &Value, session: &Session, ui: &Arc<Headless>, llm_calls: &Arc<Mutex<usize>>, lines: &mut Vec<String>) -> Result<(), String> {
    let views = |ui: &Headless| ui.state.lock().unwrap_or_else(|e| e.into_inner()).views.clone();
    let handle_of = |ui: &Headless, view: &str| views(ui).into_iter().find(|(_, (v, _))| v == view).map(|(h, (_, tree))| (h, tree));
    for (index, step) in scenario["steps"].as_array().into_iter().flatten().enumerate() {
        let label = format!("steps[{index}] {}", step);
        if ui.state.lock().unwrap_or_else(|e| e.into_inner()).exited {
            return Err(format!("{label}: the plugin process exited"));
        }
        if let Some(command) = step.get("run") {
            let mut context = json!({"source": "cli"});
            if let Some(args) = step.get("args") {
                context["args"] = args.clone();
            }
            session.notify("command", json!({"command": command, "context": context}));
        } else if let Some(wait) = step.get("wait").and_then(Value::as_u64) {
            std::thread::sleep(Duration::from_millis(wait.min(5000)));
        } else if let Some(method) = step.get("event").and_then(Value::as_str) {
            session.notify(method, step.get("params").cloned().unwrap_or_else(|| json!({})));
        } else if let Some((event, target)) = ["click", "change", "submit"].iter().find_map(|e| step.get(*e).map(|t| (*e, t))) {
            let view = target["view"].as_str().unwrap_or_default();
            let node = target["node"].as_str().unwrap_or_default();
            let (handle, tree) = wait_until(|| handle_of(ui, view)).ok_or_else(|| format!("{label}: view `{view}` is not open"))?;
            reachable(&tree, node).map_err(|why| format!("{label}: {why}"))?;
            let value = target.get("value").cloned().unwrap_or(Value::Null);
            if event == "change" || (event == "submit" && !value.is_null()) {
                // The field shows what the person typed.
                if let Some((_, (_, tree))) = ui.state.lock().unwrap_or_else(|e| e.into_inner()).views.iter_mut().find(|(h, _)| **h == handle) {
                    if let Some(n) = find_mut(tree, node) {
                        n["value"] = value.clone();
                    }
                }
            }
            session.notify("view.event", json!({"handle": handle, "view": view, "node": node, "event": event, "value": value}));
        } else if let Some(expect) = step.get("expect") {
            let check = || -> Result<(), String> { expectation(expect, ui, llm_calls) };
            let deadline = Instant::now() + EXPECT_TIMEOUT;
            loop {
                match check() {
                    Ok(()) => break,
                    Err(why) if Instant::now() > deadline => return Err(format!("{label}: {why}")),
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        } else {
            return Err(format!("{label}: unknown step (use run, click, change, submit, event, wait or expect)"));
        }
        lines.push(format!("ok   {}", step));
    }
    Ok(())
}

fn wait_until<T>(mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + EXPECT_TIMEOUT;
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn expectation(expect: &Value, ui: &Headless, llm_calls: &Arc<Mutex<usize>>) -> Result<(), String> {
    let state = ui.state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(view) = expect.get("view").and_then(Value::as_str) {
        let (_, tree) = state.views.values().find(|(v, _)| v == view).ok_or_else(|| format!("view `{view}` is not open"))?;
        if let Some(node) = expect.get("node").and_then(Value::as_str) {
            let actual = find(tree, node).ok_or_else(|| format!("no node `{node}` in `{view}`"))?;
            let wanted = expect.get("props").cloned().unwrap_or_else(|| json!({}));
            if !contains(actual, &wanted) {
                return Err(format!("`{node}` is {actual}, expected {wanted}"));
            }
        }
        return Ok(());
    }
    if let Some(method) = expect.get("call").and_then(Value::as_str) {
        let count = if method == "llm.complete" {
            *llm_calls.lock().unwrap_or_else(|e| e.into_inner())
        } else {
            let wanted = expect.get("params").cloned().unwrap_or_else(|| json!({}));
            state.calls.iter().filter(|(m, p)| m == method && contains(p, &wanted)).count()
        };
        return match expect.get("count").and_then(Value::as_u64) {
            Some(n) if count as u64 != n => Err(format!("{method} was called {count} time(s), expected {n}")),
            None if count == 0 => Err(format!("{method} was not called")),
            _ => Ok(()),
        };
    }
    if let Some(id) = expect.get("contrib").and_then(Value::as_str) {
        let actual = Value::Object(state.contrib.get(id).cloned().unwrap_or_default());
        let mut wanted = expect.clone();
        wanted.as_object_mut().map(|m| m.remove("contrib"));
        return if contains(&actual, &wanted) { Ok(()) } else { Err(format!("`{id}` is {actual}, expected {wanted}")) };
    }
    if let Some(notify) = expect.get("notify") {
        return if state.notifications.iter().any(|n| contains(n, notify)) {
            Ok(())
        } else {
            Err(format!("no notification like {notify}; got {:?}", state.notifications))
        };
    }
    Err(format!("unknown expectation {expect} (use view/node/props, call/count/params, contrib, notify)"))
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
}

/// Every `tests/*.json` scenario of the plugin in `dir` (or one by name).
///
/// Points `SUPER_DESKTOP_PLUGIN_HOME` at a temporary directory: call it before
/// this process starts any thread (the CLI does), since it sets a variable.
pub fn run_all(dir: &Path, only: Option<&str>) -> Result<Vec<Outcome>, String> {
    let home = std::env::temp_dir().join(format!("sd-plugin-test-home-{}", std::process::id()));
    // SAFETY: called by `plugin test` before it starts any thread.
    unsafe { std::env::set_var("SUPER_DESKTOP_PLUGIN_HOME", &home) };
    let outcomes = run_all_inner(dir, only);
    let _ = std::fs::remove_dir_all(&home);
    outcomes
}

fn run_all_inner(dir: &Path, only: Option<&str>) -> Result<Vec<Outcome>, String> {
    let loaded = super::manifest::load_dir(dir);
    let manifest = match loaded.manifest {
        Some(m) if loaded.report.ok => Arc::new(m),
        _ => return Err("the manifest is not valid (run plugin validate)".into()),
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("tests"))
        .map_err(|_| format!("no tests/ folder in {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| only.is_none_or(|name| p.file_stem().is_some_and(|s| s == name)))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err("no scenarios: add tests/<name>.json (see references/testing.md#scenarios)".into());
    }
    Ok(files.iter().map(|f| run_scenario(dir, &manifest, f)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_test_tree_patches_follow_the_desktop_rules() {
        let mut tree = json!({"type": "column", "id": "root", "children": [
            {"type": "button", "id": "go", "label": "Go"},
            {"type": "list", "id": "rows", "children": []}
        ]});
        apply(&mut tree, &json!({"op": "set", "id": "go", "props": {"visible": false}})).unwrap();
        assert!(reachable(&tree, "go").unwrap_err().contains("hidden"));
        apply(&mut tree, &json!({"op": "append", "id": "rows", "node": {"type": "label", "id": "r1", "text": "one"}})).unwrap();
        assert!(apply(&mut tree, &json!({"op": "append", "id": "rows", "node": {"type": "label", "id": "go", "text": "dup"}})).unwrap_err().contains("already exists"));
        apply(&mut tree, &json!({"op": "replace", "id": "go", "node": {"type": "button", "id": "go", "label": "Again"}})).unwrap();
        assert_eq!(find(&tree, "go").unwrap()["label"], "Again");
        assert!(apply(&mut tree, &json!({"op": "append", "id": "go", "node": {"type": "spinner", "id": "s"}})).unwrap_err().contains("cannot have children"));
        apply(&mut tree, &json!({"op": "set", "id": "rows", "props": {"visible": false}})).unwrap();
        assert!(reachable(&tree, "r1").unwrap_err().contains("hidden"), "a hidden parent hides its children");
        apply(&mut tree, &json!({"op": "remove", "id": "rows"})).unwrap();
        assert!(find(&tree, "r1").is_none());
        assert!(contains(&json!({"a": 1, "b": {"c": 2, "d": 3}}), &json!({"b": {"c": 2}})));
    }
}
