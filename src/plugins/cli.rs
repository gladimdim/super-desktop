//! `super-desktop plugin …`: the commands an agent uses to make a plugin and
//! check its own work. None of them needs a running overlay.
use super::{manifest, paths, skill};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const USAGE: &str = "usage: super-desktop plugin <command>

  list [--json]                       every plugin, valid or not, and its state
  new <id> [--slot dock.left|dock.right|toolbar.item|panel|settings.page|card|fullscreen]
      [--name NAME] [--request TEXT]
                                      create a working plugin to start from
  check <id|dir> [--json]             manifest and QML errors (exit 0 when clean)
  snapshot <id|dir> [--surface ID] [--width W] [--height H] [--scale S]
           [--out FILE.png] [--json]  render a surface to a PNG off screen
  logs <id> [--lines N]               the end of the plugin's log
  doctor [--json]                     what works and what is missing for plugins
  path                                print the plugins folder

Plugins are folders in the plugins folder; SUPER DESKTOP reloads one on
screen whenever a file in it is saved. Agents: read the skill that
`plugin new` puts in .claude/skills/super-desktop-plugin/SKILL.md.";

struct Flags {
    positional: Vec<String>,
    json: bool,
    options: Vec<(String, String)>,
}

impl Flags {
    fn parse(args: &[String], valued: &[&str]) -> Result<Self, String> {
        let mut flags = Flags { positional: Vec::new(), json: false, options: Vec::new() };
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            if arg == "--json" {
                flags.json = true;
            } else if let Some(name) = arg.strip_prefix("--") {
                if !valued.contains(&name) {
                    return Err(format!("unknown option --{name}"));
                }
                let value = iter.next().ok_or_else(|| format!("--{name} needs a value"))?;
                flags.options.push((name.to_owned(), value.clone()));
            } else {
                flags.positional.push(arg.clone());
            }
        }
        Ok(flags)
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.options.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn number<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>, String> {
        self.get(name)
            .map(|v| v.parse().map_err(|_| format!("--{name} needs a number, got {v:?}")))
            .transpose()
    }
}

/// Runs `plugin <args>`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let (command, rest) = match args.split_first() {
        Some((command, rest)) => (command.as_str(), rest),
        None => ("help", &[][..]),
    };
    let result = match command {
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(0)
        }
        "path" => {
            println!("{}", paths::plugins_dir().display());
            Ok(0)
        }
        "list" => Flags::parse(rest, &[]).map(|f| list(f.json)),
        "new" => Flags::parse(rest, &["slot", "name", "request"]).and_then(|f| new(&f)),
        "check" => Flags::parse(rest, &[]).and_then(|f| check(&f)),
        "snapshot" => Flags::parse(rest, &["surface", "width", "height", "scale", "out", "wait"]).and_then(|f| snapshot(&f)),
        "logs" => Flags::parse(rest, &["lines"]).and_then(|f| logs(&f)),
        "doctor" => Flags::parse(rest, &[]).map(|f| doctor(f.json)),
        other => Err(format!("unknown plugin command {other:?}")),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("super-desktop plugin: {message}\n\n{USAGE}");
            2
        }
    }
}

fn print_json(value: &Value) {
    println!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
}

fn list(as_json: bool) -> i32 {
    let root = paths::plugins_dir();
    let found = manifest::discover(&root);
    let rows: Vec<Value> = found
        .iter()
        .map(|f| {
            let status = paths::read_status(&f.folder);
            match &f.manifest {
                Ok(m) => json!({
                    "id": m.id, "name": m.name, "valid": true, "enabled": m.enabled,
                    "slots": m.surfaces.iter().map(|s| s.slot.name()).collect::<Vec<_>>(),
                    "state": status.as_ref().and_then(|s| s.get("state").cloned()).unwrap_or(Value::Null),
                    "dir": f.dir,
                }),
                Err(problems) => json!({
                    "id": f.folder, "valid": false, "problems": problems,
                    "state": status.as_ref().and_then(|s| s.get("state").cloned()).unwrap_or(Value::Null),
                    "dir": f.dir,
                }),
            }
        })
        .collect();
    if as_json {
        print_json(&json!({"pluginsDir": root, "plugins": rows}));
    } else if rows.is_empty() {
        println!("No plugins in {}. Create one: super-desktop plugin new <id>", root.display());
    } else {
        for row in &rows {
            let id = row["id"].as_str().unwrap_or("");
            let state = row["state"].as_str().unwrap_or("not loaded");
            if row["valid"] == true {
                let slots: Vec<&str> = row["slots"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                let enabled = if row["enabled"] == true { "" } else { " (disabled)" };
                println!("{id:<24} {:<22} {state}{enabled}", slots.join(","));
            } else {
                println!("{id:<24} invalid: {}", row["problems"][0]["message"].as_str().unwrap_or(""));
            }
        }
    }
    0
}

fn new(flags: &Flags) -> Result<i32, String> {
    let [id] = flags.positional.as_slice() else {
        return Err("new needs exactly one plugin id".into());
    };
    let slot_text = flags.get("slot").unwrap_or("dock.right");
    let template = skill::Template::parse(slot_text)
        .ok_or_else(|| format!("unknown slot {slot_text:?}; use one of {}", skill::Template::NAMES.join(", ")))?;
    let root = paths::plugins_dir();
    let dir = skill::new_plugin(&root, id, flags.get("name").unwrap_or(""), template, flags.get("request").unwrap_or(""))?;
    if flags.json {
        print_json(&json!({"ok": true, "id": id, "dir": dir}));
    } else {
        println!("Created {}", dir.display());
        println!("It is live in SUPER DESKTOP now. Edit its QML; check with: super-desktop plugin check {id}");
    }
    Ok(0)
}

fn plugin_dir(flags: &Flags) -> Result<PathBuf, String> {
    match flags.positional.as_slice() {
        [arg] => Ok(paths::resolve_plugin_dir(arg)),
        _ => Err("give one plugin id or folder".into()),
    }
}

fn host_or_error() -> Result<PathBuf, Value> {
    paths::qml_host_binary().ok_or_else(|| json!({
        "message": "sd-qml-host is not installed, so QML cannot be checked or drawn",
        "hint": "Install qt6-declarative, then update SUPER DESKTOP (or run scripts/build-qml-host.sh in its folder).",
    }))
}

/// `sd-qml-host --check` for one QML file.
fn check_qml(host: &Path, qml: &Path, dir: &Path) -> Value {
    match Command::new(host).arg("--check").arg(qml).arg("--plugin-dir").arg(dir).output() {
        Ok(output) => serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|_| json!({
            "ok": false,
            "errors": [{"file": qml, "line": 0, "column": 0,
                        "message": format!("the QML host failed: {}", String::from_utf8_lossy(&output.stderr).trim())}],
        })),
        Err(e) => json!({"ok": false, "errors": [{"file": qml, "line": 0, "column": 0, "message": format!("cannot run {}: {e}", host.display())}]}),
    }
}

fn check(flags: &Flags) -> Result<i32, String> {
    let dir = plugin_dir(flags)?;
    let report = match manifest::load(&dir) {
        Err(problems) => json!({"ok": false, "dir": dir, "manifest": problems, "surfaces": []}),
        Ok(m) => match host_or_error() {
            Err(error) => json!({"ok": false, "dir": dir, "manifest": [], "surfaces": [], "host": error}),
            Ok(host) => {
                let surfaces: Vec<Value> = m.surfaces.iter().map(|s| {
                    let mut result = check_qml(&host, &m.qml_path(s), &dir);
                    result["surface"] = json!(s.id);
                    result["slot"] = json!(s.slot.name());
                    result
                }).collect();
                // A warning at load is almost always a real bug (an undefined
                // name, a binding loop), so it fails the check too.
                let ok = surfaces.iter().all(|s| {
                    s["ok"] == true && s["warnings"].as_array().is_none_or(Vec::is_empty)
                });
                json!({"ok": ok, "dir": dir, "id": m.id, "manifest": [], "surfaces": surfaces})
            }
        },
    };
    let ok = report["ok"] == true;
    if flags.json {
        print_json(&report);
    } else {
        print_check(&report);
    }
    Ok(if ok { 0 } else { 1 })
}

fn print_check(report: &Value) {
    for problem in report["manifest"].as_array().into_iter().flatten() {
        println!("plugin.json: {}: {}\n  hint: {}", problem["field"].as_str().unwrap_or(""),
            problem["message"].as_str().unwrap_or(""), problem["hint"].as_str().unwrap_or(""));
    }
    if let Some(host) = report.get("host") {
        println!("{}\n  hint: {}", host["message"].as_str().unwrap_or(""), host["hint"].as_str().unwrap_or(""));
    }
    for surface in report["surfaces"].as_array().into_iter().flatten() {
        let id = surface["surface"].as_str().unwrap_or("");
        for (kind, key) in [("error", "errors"), ("warning", "warnings")] {
            for e in surface[key].as_array().into_iter().flatten() {
                println!("{}:{}:{}: {kind}: {} (surface {id})", e["file"].as_str().unwrap_or(""),
                    e["line"], e["column"], e["message"].as_str().unwrap_or(""));
            }
        }
    }
    if report["ok"] == true {
        println!("OK: {} surface(s) load cleanly.", report["surfaces"].as_array().map_or(0, Vec::len));
    } else {
        println!("Not clean: fix the lines above, then check again.");
    }
}

fn snapshot(flags: &Flags) -> Result<i32, String> {
    let dir = plugin_dir(flags)?;
    let m = manifest::load(&dir).map_err(|problems| {
        format!("plugin.json has problems; run `super-desktop plugin check`: {}", problems[0].message)
    })?;
    let surface = match flags.get("surface") {
        Some(id) => m.surfaces.iter().find(|s| s.id == id).ok_or_else(|| format!("no surface {id:?}"))?,
        None => &m.surfaces[0],
    };
    let host = host_or_error().map_err(|e| e["message"].as_str().unwrap_or("").to_owned())?;
    let (default_w, default_h) = surface.slot.default_snapshot_size(surface.size, surface.height);
    let width: i32 = flags.number("width")?.unwrap_or(default_w);
    let height: i32 = flags.number("height")?.unwrap_or(default_h);
    let scale: f64 = flags.number("scale")?.unwrap_or(1.0);
    let wait: u32 = flags.number("wait")?.unwrap_or(300);
    let out = flags.get("out").map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("{}-{}.png", m.id, surface.id))
    });
    let theme = std::fs::read_to_string(paths::theme_path()).unwrap_or_default();
    let mut command = Command::new(&host);
    command
        .arg("--snapshot").arg(m.qml_path(surface))
        .arg("--out").arg(&out)
        .arg("--plugin-dir").arg(&dir)
        .args(["--plugin-id", &m.id, "--surface-id", &surface.id])
        .args(["--width", &width.to_string(), "--height", &height.to_string()])
        .args(["--scale", &scale.to_string(), "--wait", &wait.to_string()])
        .arg("--data").arg(paths::data_path(&m.id));
    if !theme.trim().is_empty() {
        command.args(["--theme", theme.trim()]);
    }
    let output = command.output().map_err(|e| format!("cannot run {}: {e}", host.display()))?;
    let mut report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| json!({
        "ok": false, "errors": [{"message": String::from_utf8_lossy(&output.stderr).trim().to_owned()}],
    }));
    report["surface"] = json!(surface.id);
    let ok = report["ok"] == true;
    if flags.json {
        print_json(&report);
    } else if ok {
        println!("Wrote {} ({}x{} px, {:.0}% painted)", out.display(), report["width"], report["height"],
            report["paintedFraction"].as_f64().unwrap_or(0.0) * 100.0);
        for w in report["warnings"].as_array().into_iter().flatten() {
            println!("warning: {}:{}: {}", w["file"].as_str().unwrap_or(""), w["line"], w["message"].as_str().unwrap_or(""));
        }
    } else {
        print_check(&json!({"ok": false, "manifest": [], "surfaces": [{"surface": surface.id, "errors": report["errors"]}]}));
    }
    Ok(if ok { 0 } else { 1 })
}

fn logs(flags: &Flags) -> Result<i32, String> {
    let [id] = flags.positional.as_slice() else {
        return Err("logs needs one plugin id".into());
    };
    let lines: usize = flags.number("lines")?.unwrap_or(80);
    match std::fs::read_to_string(paths::log_path(id)) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            for line in &all[all.len().saturating_sub(lines)..] {
                println!("{line}");
            }
            Ok(0)
        }
        Err(_) => {
            println!("No log yet for {id} ({}).", paths::log_path(id).display());
            Ok(0)
        }
    }
}

fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

const DOCTOR_QML: &str = "import QtQuick\nimport QtQuick.Controls\nimport SuperDesktop.Terminal\nRectangle { color: \"#ff0000\"; Button { text: \"ok\" } }\n";

/// Checks everything plugins depend on and says how to fix what is missing.
fn doctor(as_json: bool) -> i32 {
    let mut checks: Vec<Value> = Vec::new();
    let mut add = |name: &str, ok: bool, detail: String, hint: &str| {
        checks.push(json!({"check": name, "ok": ok, "detail": detail, "hint": if ok { "" } else { hint }}));
    };

    let host = paths::qml_host_binary();
    add("qml-host", host.is_some(),
        host.as_ref().map_or("sd-qml-host not found".into(), |p| p.display().to_string()),
        "Install qt6-declarative, then update SUPER DESKTOP (or run scripts/build-qml-host.sh in its folder).");
    if let Some(host) = &host {
        // Draw something real: Qt Quick, Controls and the built-in terminal module.
        let dir = std::env::temp_dir().join(format!("sd-plugin-doctor-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let qml = dir.join("Doctor.qml");
        let png = dir.join("doctor.png");
        let rendered = std::fs::write(&qml, DOCTOR_QML).ok().and_then(|()| {
            Command::new(host).arg("--snapshot").arg(&qml).arg("--out").arg(&png)
                .args(["--width", "64", "--height", "32", "--wait", "50"]).output().ok()
        });
        let report: Value = rendered.as_ref().and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or(Value::Null);
        let ok = report["ok"] == true && report["paintedFraction"].as_f64().unwrap_or(0.0) > 0.5;
        let detail = if ok { "draws QML off screen".to_owned() } else {
            report["errors"][0]["message"].as_str().map(str::to_owned).unwrap_or_else(|| {
                rendered.map(|o| String::from_utf8_lossy(&o.stderr).trim().to_owned()).unwrap_or_else(|| "did not run".into())
            })
        };
        add("qml-render", ok, detail, "Qt Quick could not draw: check that qt6-declarative is installed and matches qt6-base.");
        let _ = std::fs::remove_dir_all(&dir);
    }

    let root = paths::plugins_dir();
    let found = manifest::discover(&root);
    let invalid: Vec<&str> = found.iter().filter(|f| f.manifest.is_err()).map(|f| f.folder.as_str()).collect();
    add("plugins-folder", root.is_dir(),
        format!("{} ({} plugins{})", root.display(), found.len(),
            if invalid.is_empty() { String::new() } else { format!(", invalid: {}", invalid.join(", ")) }),
        "It is created when SUPER DESKTOP starts, or by `super-desktop plugin new <id>`.");

    let socket = crate::platform::runtime::socket_path();
    let running = std::os::unix::net::UnixStream::connect(&socket).is_ok();
    add("overlay", running,
        if running { "SUPER DESKTOP is running".into() } else { "SUPER DESKTOP is not running".into() },
        "Start it (super-desktop show); plugins appear in the overlay, and check/snapshot work without it.");
    let theme = paths::theme_path().is_file();
    add("theme", theme,
        if theme { "the overlay has shared its theme with plugins".into() } else { "no theme saved for plugins yet".into() },
        "Open SUPER DESKTOP once with this version; snapshots use a default theme until then.");

    for (tool, why) in [("wl-paste", "pasting into TerminalView"), ("notify-send", "the notify call"), ("git", "plugin install and update")] {
        add(tool, command_exists(tool), why.into(), "Install it with pacman (wl-clipboard, libnotify, git).");
    }

    let ok = checks.iter().take(2).all(|c| c["ok"] == true);
    if as_json {
        print_json(&json!({"ok": ok, "checks": checks}));
    } else {
        for c in &checks {
            let mark = if c["ok"] == true { "✓" } else { "✗" };
            println!("{mark} {:<15} {}", c["check"].as_str().unwrap_or(""), c["detail"].as_str().unwrap_or(""));
            if c["ok"] != true {
                println!("  {:<15} {}", "", c["hint"].as_str().unwrap_or(""));
            }
        }
    }
    if ok { 0 } else { 1 }
}
