//! `super-desktop plugin …`: the commands an agent uses to make a plugin and
//! check its own work. None of them needs a running overlay.
use super::{install, lint, manifest, paths, registry, skill};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const USAGE: &str = "usage: super-desktop plugin <command>

  list [--json]                       every plugin, valid or not, and its state
  new <id> [--slot dock.left|dock.right|toolbar.item|panel|settings.page|card|fullscreen|popup]
      [--name NAME] [--request TEXT]
                                      create a working plugin to start from
  check <id|dir> [--json]             manifest and QML errors (exit 0 when clean)
  snapshot <id|dir> [--surface ID] [--width W] [--height H] [--scale S]
           [--wait MS] [--values FILE.json] [--out FILE.png] [--json]
           [--do 'click \"Add\"; type hi; key Return; expect \"hi\"; shot'] [--steps FILE]
                                      render a surface to a PNG off screen, after
                                      clicking and typing like a user (--do)
  preview <id|dir> [--surface ID]... [--values FILE.json] [--out FILE.png] [--json]
                                      draw preview.png (1280×640) from the plugin's surfaces
  publish <id|dir> [--json]           check it is ready for the public list; print its entry
  logs <id> [--lines N]               the end of the plugin's log
  doctor [--json]                     what works and what is missing for plugins
  search [TEXT] [--category C] [--json]
                                      the public plugin list, by name or by category
  install <id|git-url|folder> [--json]
                                      install a plugin (an id from search); it runs once approved
  approve <id> [--json]               let an installed plugin run
  update <id> [--json]                fast-forward an installed plugin from its git source
  remove <id> [--json]                move a plugin's folder to the trash
  path                                print the plugins folder

Plugins are folders in the plugins folder; SUPER DESKTOP reloads one on
screen whenever a file in it is saved. Agents: read the skill that
`plugin new` puts in .claude/skills/super-desktop-plugin/SKILL.md.";

const SNAPSHOT_OPTIONS: &[&str] = &["surface", "width", "height", "scale", "out", "wait", "values", "do", "steps"];

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

    fn all(&self, name: &str) -> Vec<&str> {
        self.options.iter().filter(|(n, _)| n == name).map(|(_, v)| v.as_str()).collect()
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

/// The lines of USAGE about one command, or `None` for an unknown one.
fn command_help(command: &str) -> Option<String> {
    let mut lines = USAGE.lines().skip_while(|l| !(l.starts_with(&format!("  {command} ")) || *l == format!("  {command}")));
    let first = lines.next()?;
    // Its continuation lines are indented further than the next command.
    let rest: Vec<&str> = lines.take_while(|l| l.starts_with("     ")).collect();
    Some(format!("usage: super-desktop plugin {}\n{}", first.trim_start(), rest.join("\n")).trim_end().to_owned())
}

/// Runs `plugin <args>`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let (command, rest) = match args.split_first() {
        Some((command, rest)) => (command.as_str(), rest),
        None => ("help", &[][..]),
    };
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        match command_help(command) {
            Some(help) => println!("{help}\n\nAll commands: super-desktop plugin help"),
            None => println!("{USAGE}"),
        }
        return 0;
    }
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
        "snapshot" => Flags::parse(rest, SNAPSHOT_OPTIONS).and_then(|f| snapshot(&f)),
        "preview" => Flags::parse(rest, &["surface", "values", "out"]).and_then(|f| preview(&f)),
        "publish" => Flags::parse(rest, &[]).and_then(|f| publish(&f)),
        "logs" => Flags::parse(rest, &["lines"]).and_then(|f| logs(&f)),
        "doctor" => Flags::parse(rest, &[]).map(|f| doctor(f.json)),
        "search" => Flags::parse(rest, &["category"]).and_then(|f| search(&f)),
        "install" => Flags::parse(rest, &[]).and_then(|f| install_command(&f)),
        "approve" => Flags::parse(rest, &[]).and_then(|f| one_id(&f).and_then(|id| {
            install::approve(&install::Places::current(), &id)?;
            report(f.json, json!({"ok": true, "id": id}), &format!("Approved {id}: it runs in SUPER DESKTOP now."));
            Ok(0)
        })),
        "update" => Flags::parse(rest, &[]).and_then(|f| one_id(&f).and_then(|id| {
            let updated = install::update(&install::Places::current(), &id)?;
            let text = if updated.from == updated.to {
                format!("{id} is up to date ({}).", short(&updated.to))
            } else {
                format!("Updated {id} {}..{}:\n{}", short(&updated.from), short(&updated.to), updated.changes)
            };
            report(f.json, json!({"ok": true, "id": id, "from": updated.from, "to": updated.to, "changes": updated.changes}), &text);
            Ok(0)
        })),
        "remove" => Flags::parse(rest, &[]).and_then(|f| one_id(&f).and_then(|id| {
            let trashed = install::remove(&install::Places::current(), &id, &install::user_trash())?;
            report(f.json, json!({"ok": true, "id": id, "trash": trashed}), &format!("Moved {id} to the trash ({}).", trashed.display()));
            Ok(0)
        })),
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
                    "id": m.id, "name": m.name, "version": m.version, "valid": true, "enabled": m.enabled,
                    "approved": install::approved(&install::Places::current(), &m.id),
                    "source": install::source(&install::Places::current(), &m.id).map(|s| s["source"].clone()),
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
                let enabled = if row["approved"] != true { " (waiting for approval)" } else if row["enabled"] == true { "" } else { " (disabled)" };
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
    let report = check_report(&dir);
    let ok = report["ok"] == true;
    if flags.json {
        print_json(&report);
    } else {
        print_check(&report);
    }
    Ok(if ok { 0 } else { 1 })
}

/// Manifest problems, each surface's QML errors and warnings, the lints.
fn check_report(dir: &Path) -> Value {
    match manifest::load(dir) {
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
                // name, a binding loop), so it fails the check too, like a
                // lint error; lint advice does not.
                let findings = lint::lint(&m);
                let ok = surfaces.iter().all(|s| {
                    s["ok"] == true && s["warnings"].as_array().is_none_or(Vec::is_empty)
                }) && findings.iter().all(|f| f.level != lint::Level::Error);
                json!({"ok": ok, "dir": dir, "id": m.id, "manifest": [], "surfaces": surfaces, "lint": findings})
            }
        },
    }
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
    for f in report["lint"].as_array().into_iter().flatten() {
        println!("{}:{}: {}: {} [{}]\n  hint: {}", f["file"].as_str().unwrap_or(""), f["line"], f["level"].as_str().unwrap_or(""),
            f["message"].as_str().unwrap_or(""), f["rule"].as_str().unwrap_or(""), f["hint"].as_str().unwrap_or(""));
    }
    if report["ok"] == true {
        println!("OK: {} surface(s) load cleanly.", report["surfaces"].as_array().map_or(0, Vec::len));
    } else {
        println!("Not clean: fix the lines above, then check again.");
    }
}

fn snapshot(flags: &Flags) -> Result<i32, String> {
    let (report, out) = snapshot_report(flags, None)?;
    let ok = report["ok"] == true;
    if flags.json {
        print_json(&report);
    } else if ok {
        println!("Wrote {} ({}x{} px, {:.0}% painted)", out.display(), report["width"], report["height"],
            report["paintedFraction"].as_f64().unwrap_or(0.0) * 100.0);
        print_steps(&report);
        for w in report["warnings"].as_array().into_iter().flatten() {
            println!("warning: {}:{}: {}", w["file"].as_str().unwrap_or(""), w["line"], w["message"].as_str().unwrap_or(""));
        }
    } else if report.get("steps").is_some() {
        print_steps(&report);
        println!("A step failed; the picture shows the surface at that moment: {}", out.display());
    } else {
        print_check(&json!({"ok": false, "manifest": [], "surfaces": [{"surface": report["surface"], "errors": report["errors"]}]}));
    }
    Ok(if ok { 0 } else { 1 })
}

/// What `--do` did: each step, the calls and programs the plugin asked
/// for, its log and the values it saved.
fn print_steps(report: &Value) {
    for step in report["steps"].as_array().into_iter().flatten() {
        let mark = if step["ok"] == true { "✓" } else { "✗" };
        let mut line = format!("{mark} {}", step["step"].as_str().unwrap_or(""));
        if let Some(at) = step["at"].as_array() {
            line += &format!("  (at {},{})", at[0], at[1]);
        }
        if let Some(out) = step["out"].as_str() {
            line += &format!("  → {out}");
        }
        println!("{line}");
        if let Some(error) = step["error"].as_str() {
            println!("  {error}");
        }
    }
    for call in report["calls"].as_array().into_iter().flatten() {
        println!("sd.call {} {}", call["method"].as_str().unwrap_or(""), call["params"]);
    }
    for run in report["runs"].as_array().into_iter().flatten() {
        println!("sd.run {run}");
    }
    for log in report["logs"].as_array().into_iter().flatten() {
        println!("sd.log {}", log.as_str().unwrap_or(""));
    }
    if report.get("steps").is_some() {
        println!("values: {}", report["values"]);
    }
}

/// Renders the surface; the host's report (with `surface`) and the PNG path.
/// `saved` is where the plugin's values are (default: its data file).
fn snapshot_report(flags: &Flags, saved: Option<&Path>) -> Result<(Value, PathBuf), String> {
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
    // The plugin's saved values, or sample ones from --values, in a copy:
    // what the QML saves during a snapshot is thrown away.
    let values: Value = match flags.get("values") {
        Some(file) => {
            let text = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
            let value: Value = serde_json::from_str(&text).map_err(|e| format!("{file} is not valid JSON: {e}"))?;
            if !value.is_object() {
                return Err(format!("{file} must hold one JSON object, like sd.values: {{\"key\": value}}"));
            }
            value
        }
        None => std::fs::read_to_string(saved.map_or_else(|| paths::data_path(&m.id), Path::to_path_buf)).ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({})),
    };
    let data = std::env::temp_dir().join(format!("sd-snapshot-{}-{}.json", m.id, std::process::id()));
    std::fs::write(&data, values.to_string()).map_err(|e| format!("cannot write {}: {e}", data.display()))?;
    let mut steps: Vec<String> = flags.all("do").into_iter().map(str::to_owned).collect();
    if let Some(file) = flags.get("steps") {
        steps.push(std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?);
    }
    let theme = std::fs::read_to_string(paths::theme_path()).unwrap_or_default();
    let mut command = Command::new(&host);
    command
        .arg("--snapshot").arg(m.qml_path(surface))
        .arg("--out").arg(&out)
        .arg("--plugin-dir").arg(&dir)
        .args(["--plugin-id", &m.id, "--surface-id", &surface.id])
        .args(["--width", &width.to_string(), "--height", &height.to_string()])
        .args(["--scale", &scale.to_string(), "--wait", &wait.to_string()])
        .arg("--data").arg(&data);
    if !steps.is_empty() {
        command.arg("--do").arg(steps.join("\n"));
    }
    if !theme.trim().is_empty() {
        command.args(["--theme", theme.trim()]);
    }
    let output = command.output();
    let _ = std::fs::remove_file(&data);
    let output = output.map_err(|e| format!("cannot run {}: {e}", host.display()))?;
    let mut report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| json!({
        "ok": false, "errors": [{"message": String::from_utf8_lossy(&output.stderr).trim().to_owned()}],
    }));
    report["surface"] = json!(surface.id);
    Ok((report, out))
}

/// One picture placed on the preview.
struct Placed {
    png: PathBuf,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    framed: bool,
}

/// `plugin preview`: the plugin's surfaces with sample values, drawn on the
/// theme's background into a 1280×640 picture, its middle square (what
/// Discover shows) holding them.
fn preview(flags: &Flags) -> Result<i32, String> {
    let dir = plugin_dir(flags)?;
    let m = manifest::load(&dir).map_err(|problems| {
        format!("plugin.json has problems; run `super-desktop plugin check`: {}", problems[0].message)
    })?;
    let host = host_or_error().map_err(|e| e["message"].as_str().unwrap_or("").to_owned())?;
    let wanted = flags.all("surface");
    let chosen: Vec<&manifest::Surface> = if wanted.is_empty() {
        let mut bars: Vec<_> = m.surfaces.iter().filter(|s| is_bar(s.slot)).collect();
        bars.sort_by_key(|s| s.slot != manifest::Slot::ToolbarItem);
        let mut others: Vec<_> = m.surfaces.iter()
            .filter(|s| matches!(s.slot, manifest::Slot::Popup | manifest::Slot::Panel | manifest::Slot::Card))
            .collect();
        others.sort_by_key(|s| [manifest::Slot::Popup, manifest::Slot::Panel, manifest::Slot::Card].iter().position(|k| *k == s.slot));
        let mut chosen: Vec<_> = bars.into_iter().take(2).chain(others.into_iter().take(2)).collect();
        if chosen.is_empty() {
            chosen = m.surfaces.iter().take(1).collect();
        }
        chosen
    } else {
        wanted.iter().map(|id| m.surface(id).ok_or_else(|| format!("no surface {id:?}"))).collect::<Result<_, _>>()?
    };
    // Logical sizes, then one scale that fills the middle square.
    let logical = |s: &manifest::Surface| -> (i32, i32) {
        let (w, h) = s.slot.default_snapshot_size(s.size, s.height);
        if matches!(s.slot, manifest::Slot::DockLeft | manifest::Slot::DockRight) { (w, 400) } else { (w, h) }
    };
    let (bars, others): (Vec<_>, Vec<_>) = chosen.iter().partition(|s| is_bar(s.slot));
    let row = |list: &[&&manifest::Surface], gap: i32| -> (i32, i32) {
        let w = list.iter().map(|s| logical(s).0).sum::<i32>() + gap * (list.len() as i32 - 1).max(0);
        (w, list.iter().map(|s| logical(s).1).max().unwrap_or(0))
    };
    let ((bw, bh), (ow, oh)) = (row(&bars, 16), row(&others, 24));
    let both = !bars.is_empty() && !others.is_empty();
    let (lw, lh) = (bw.max(ow), bh + oh + if both { 24 } else { 0 });
    // A whole scale (fractional ones leave edge pixels unpainted), drawn
    // down to fit when it is too big.
    let scale = (560.0 / f64::from(lw.max(1))).min(560.0 / f64::from(lh.max(1))).ceil().clamp(1.0, 3.0);
    let work = std::env::temp_dir().join(format!("sd-preview-{}-{}", m.id, std::process::id()));
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    let mut shots: Vec<(&manifest::Surface, PathBuf, f64, f64)> = Vec::new();
    for s in &chosen {
        let png = work.join(format!("{}.png", s.id));
        let (w, h) = logical(s);
        let mut args = vec![dir.to_string_lossy().into_owned(), "--surface".into(), s.id.clone(), "--out".into(),
            png.to_string_lossy().into_owned(), "--scale".into(), format!("{scale}"),
            "--width".into(), w.to_string(), "--height".into(), h.to_string()];
        if let Some(values) = flags.get("values") {
            args.extend(["--values".into(), values.into()]);
        }
        let (report, _) = snapshot_report(&Flags::parse(&args, SNAPSHOT_OPTIONS)?, None)?;
        if report["ok"] != true {
            let _ = std::fs::remove_dir_all(&work);
            return Err(format!("surface {} did not draw: {}", s.id, report["errors"]));
        }
        shots.push((s, png, report["width"].as_f64().unwrap_or(0.0), report["height"].as_f64().unwrap_or(0.0)));
    }
    // Place them: top-bar items and docks in a row over a bar-colored strip,
    // what they open below, all centered and fitted in 600×600.
    let (canvas_w, canvas_h, square) = (1280.0, 640.0, 600.0);
    let px_row = |bar: bool, gap: f64| -> (f64, f64) {
        let items: Vec<_> = shots.iter().filter(|(s, ..)| is_bar(s.slot) == bar).collect();
        let w = items.iter().map(|(_, _, w, _)| *w).sum::<f64>() + gap * (items.len().saturating_sub(1)) as f64;
        (w, items.iter().map(|(_, _, _, h)| *h).fold(0.0, f64::max))
    };
    let gap = 24.0 * scale;
    let ((pbw, pbh), (pow, poh)) = (px_row(true, 16.0 * scale), px_row(false, gap));
    let (tw, th) = (pbw.max(pow), pbh + poh + if both { gap } else { 0.0 });
    let fit = (square / tw.max(1.0)).min(square / th.max(1.0)).min(1.0);
    let top = (canvas_h - th * fit) / 2.0;
    let mut placed = Vec::new();
    for (bar, y, row_w, gap) in [(true, top, pbw, 16.0 * scale), (false, top + if both { (pbh + gap) * fit } else { 0.0 }, pow, gap)] {
        let mut x = (canvas_w - row_w * fit) / 2.0;
        for (s, png, w, h) in shots.iter().filter(|(s, ..)| is_bar(s.slot) == bar) {
            placed.push(Placed { png: png.clone(), x, y, width: w * fit, height: h * fit, framed: !is_bar(s.slot) });
            x += (w + gap) * fit;
        }
    }
    let strip = (!bars.is_empty()).then(|| (top - 10.0 * fit * scale, pbh * fit + 20.0 * fit * scale));
    let qml = work.join("Preview.qml");
    std::fs::write(&qml, preview_qml(&placed, strip)).map_err(|e| format!("cannot write {}: {e}", qml.display()))?;
    let out = flags.get("out").map(PathBuf::from).unwrap_or_else(|| dir.join("preview.png"));
    let theme = std::fs::read_to_string(paths::theme_path()).unwrap_or_default();
    let mut command = Command::new(&host);
    command.arg("--snapshot").arg(&qml).arg("--out").arg(&out)
        .args(["--width", "1280", "--height", "640", "--wait", "200"]);
    if !theme.trim().is_empty() {
        command.args(["--theme", theme.trim()]);
    }
    let output = command.output().map_err(|e| format!("cannot run {}: {e}", host.display()));
    let _ = std::fs::remove_dir_all(&work);
    let report: Value = serde_json::from_slice(&output?.stdout).unwrap_or(Value::Null);
    if report["ok"] != true {
        return Err(format!("the preview did not draw: {report}"));
    }
    let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    let too_big = bytes as usize > registry::MAX_PREVIEW_BYTES;
    let surfaces: Vec<&str> = chosen.iter().map(|s| s.id.as_str()).collect();
    if flags.json {
        print_json(&json!({"ok": !too_big, "out": out, "width": 1280, "height": 640, "bytes": bytes, "surfaces": surfaces}));
    } else {
        println!("Wrote {} (1280×640, {} KiB) from {}. Open it and look at it.", out.display(), bytes / 1024, surfaces.join(", "));
        if too_big {
            println!("It is larger than 1.5 MiB: SUPER DESKTOP will not show it. Use fewer surfaces or simpler sample values.");
        }
    }
    Ok(if too_big { 1 } else { 0 })
}

fn is_bar(slot: manifest::Slot) -> bool {
    matches!(slot, manifest::Slot::ToolbarItem | manifest::Slot::DockLeft | manifest::Slot::DockRight)
}

fn preview_qml(placed: &[Placed], strip: Option<(f64, f64)>) -> String {
    let mut qml = String::from("import QtQuick\nRectangle {\n    color: sd.theme.background\n");
    if let Some((y, height)) = strip {
        qml += &format!("    Rectangle {{ x: 0; y: {y:.1}; width: parent.width; height: {height:.1}; color: sd.theme.surface }}\n");
    }
    for p in placed {
        let source = serde_json::to_string(&format!("file://{}", p.png.display())).unwrap_or_default();
        if p.framed {
            // A soft shadow and a hairline, like a popup over the desktop.
            qml += &format!("    Rectangle {{ x: {:.1}; y: {:.1}; width: {:.1}; height: {:.1}; radius: 10; color: \"black\"; opacity: 0.3 }}\n",
                p.x + 6.0, p.y + 8.0, p.width, p.height);
        }
        qml += &format!("    Image {{ x: {:.1}; y: {:.1}; width: {:.1}; height: {:.1}; source: {source}; smooth: true }}\n",
            p.x, p.y, p.width, p.height);
        if p.framed {
            qml += &format!("    Rectangle {{ x: {:.1}; y: {:.1}; width: {:.1}; height: {:.1}; color: \"transparent\"; border.color: Qt.alpha(sd.theme.foreground, 0.18) }}\n",
                p.x - 1.0, p.y - 1.0, p.width + 2.0, p.height + 2.0);
        }
    }
    qml + "}\n"
}

/// `plugin publish`: whether the plugin is ready for the public list, and
/// the entry to add to it. Changes nothing.
fn publish(flags: &Flags) -> Result<i32, String> {
    let dir = plugin_dir(flags)?;
    let mut checks: Vec<Value> = Vec::new();
    let mut add = |name: &str, ok: bool, required: bool, detail: String, hint: &str| {
        checks.push(json!({"check": name, "ok": ok, "required": required, "detail": detail, "hint": if ok { "" } else { hint }}));
    };
    let report = check_report(&dir);
    let clean = report["ok"] == true;
    add("check", clean, true, if clean { "plugin check is clean".into() } else { "plugin check has errors".into() },
        "Run `super-desktop plugin check <id>` and fix what it reports.");
    let Ok(m) = manifest::load(&dir) else {
        print_publish(flags.json, &checks, None);
        return Ok(1);
    };
    add("version", !m.version.is_empty(), false, if m.version.is_empty() { "no \"version\"".into() } else { m.version.clone() },
        "Add \"version\": \"1.0.0\" to plugin.json and raise it with each release.");
    add("minHost", !m.min_host.is_empty(), true,
        if m.min_host.is_empty() { "no \"minHost\"".into() } else { format!("needs SUPER DESKTOP {}", m.min_host) },
        &format!("Add \"minHost\": \"{}\" (the version you tested with) to plugin.json.", manifest::HOST_VERSION));
    add("categories", !m.categories.is_empty(), false,
        if m.categories.is_empty() { "no \"categories\"".into() } else { m.categories.join(", ") },
        "Add \"categories\": [\"productivity\"] (up to 5) to plugin.json: Discover groups plugins by them.");
    add("description", m.description.trim().len() >= 20, true, m.description.clone(),
        "Write a \"description\" in plugin.json: one or two sentences on what it does.");
    let preview = dir.join("preview.png");
    let preview_ok = registry::is_preview(&preview);
    add("preview.png", preview_ok, false,
        if preview_ok { "a PNG of at most 1.5 MiB".into() } else if preview.exists() { "not a PNG, or larger than 1.5 MiB".into() } else { "missing".into() },
        "Run `super-desktop plugin preview <id> --values sample.json`, then look at preview.png.");
    let readme = ["README.md", "README"].iter().any(|f| dir.join(f).is_file());
    add("README", readme, true, if readme { "present".into() } else { "missing".into() },
        "Add a README.md: what it does, the programs it runs, the files it writes, anything it sends.");
    let git = |args: &[&str]| Command::new("git").arg("-C").arg(&dir).args(args).output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    let top = git(&["rev-parse", "--show-toplevel"]);
    let own_repo = top.as_deref().map(Path::new).and_then(|t| t.canonicalize().ok()) == dir.canonicalize().ok();
    add("git", own_repo, true, top.clone().unwrap_or_else(|| "not a git repository".into()),
        "Make the plugin folder its own repository: git init, commit, then push it to GitHub.");
    let remote = git(&["remote", "get-url", "origin"]).unwrap_or_default();
    let repo = github_repo(&remote);
    add("github", repo.is_some(), true, if remote.is_empty() { "no origin remote".into() } else { remote.clone() },
        "Push it to GitHub: gh repo create <name> --public --source . --push");
    if own_repo {
        let dirty = git(&["status", "--porcelain"]).unwrap_or_default();
        add("committed", dirty.is_empty(), true, if dirty.is_empty() { "nothing uncommitted".into() } else { format!("uncommitted: {}", dirty.lines().count()) },
            "Commit your changes (git add -A && git commit).");
        let ahead = git(&["rev-list", "--count", "@{u}..HEAD"]);
        add("pushed", ahead.as_deref() == Some("0"), true,
            match ahead.as_deref() { Some("0") => "up to date with its remote".into(), Some(n) => format!("{n} commits not pushed"), None => "no upstream branch".into() },
            "Push: git push -u origin HEAD");
        let skills = git(&["ls-files", ".claude"]).unwrap_or_default();
        add(".claude", skills.is_empty(), true, if skills.is_empty() { "not committed".into() } else { ".claude/ is committed".into() },
            "Remove it from git (git rm -r --cached .claude) and add .claude/ to .gitignore: SUPER DESKTOP adds the current skill itself.");
    }
    let mut slots: Vec<&str> = m.surfaces.iter().map(|s| s.slot.name()).collect();
    slots.sort_unstable();
    slots.dedup();
    let entry = json!({
        "id": m.id, "name": m.name, "description": m.description,
        "repo": repo.clone().unwrap_or_else(|| "OWNER/REPO".into()),
        "tags": m.categories, "author": repo.as_deref().and_then(|r| r.split('/').next()).unwrap_or("OWNER"),
        "api": manifest::API_VERSION, "minHost": m.min_host, "slots": slots,
    });
    let ready = checks.iter().all(|c| c["ok"] == true || c["required"] != true);
    // Only then: it may fetch the list.
    let listed = ready && registry::cached().or_else(|| registry::fetch().ok().map(|(e, _)| (e, 0)))
        .is_some_and(|(entries, _)| entries.iter().any(|e| e.id == m.id));
    print_publish(flags.json, &checks, Some((&entry, listed, ready)));
    Ok(if ready { 0 } else { 1 })
}

/// "owner/name" of a GitHub remote URL.
fn github_repo(remote: &str) -> Option<String> {
    let rest = remote.strip_prefix("https://github.com/").or_else(|| remote.strip_prefix("git@github.com:"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (parts.next().is_none() && !owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
}

fn print_publish(as_json: bool, checks: &[Value], entry: Option<(&Value, bool, bool)>) {
    const REGISTRY: &str = "https://github.com/gladimdim/super-desktop-plugins";
    let ready = entry.is_some_and(|(_, _, ready)| ready);
    if as_json {
        print_json(&json!({"ok": ready, "checks": checks, "entry": entry.map(|(e, ..)| e.clone()),
            "listed": entry.map(|(_, listed, _)| listed), "registry": REGISTRY}));
        return;
    }
    for c in checks {
        let mark = if c["ok"] == true { "✓" } else if c["required"] == true { "✗" } else { "!" };
        println!("{mark} {:<12} {}", c["check"].as_str().unwrap_or(""), c["detail"].as_str().unwrap_or(""));
        if c["ok"] != true {
            println!("  {:<12} {}", "", c["hint"].as_str().unwrap_or(""));
        }
    }
    let Some((entry, listed, ready)) = entry else { return };
    if !ready {
        println!("\nNot ready: fix the ✗ lines (! lines are recommended), then run publish again.");
        return;
    }
    println!("\nReady. {} the public list with a pull request to {REGISTRY}:", if listed { "It is listed already; to change its entry, update it in" } else { "Add it to" });
    println!("edit registry.json ({REGISTRY}/edit/main/registry.json makes the fork and the pull request),");
    println!("put this entry in \"plugins\" in id order, and check with python3 validate.py:\n");
    println!("{}", serde_json::to_string_pretty(entry).unwrap_or_default());
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

fn one_id(flags: &Flags) -> Result<String, String> {
    match flags.positional.as_slice() {
        [id] => Ok(id.clone()),
        _ => Err("give one plugin id".into()),
    }
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(8)]
}

fn report(as_json: bool, value: Value, text: &str) {
    if as_json {
        print_json(&value);
    } else {
        println!("{text}");
    }
}

/// The public plugin list, filtered; each with whether it is here already.
fn search(flags: &Flags) -> Result<i32, String> {
    let query = flags.positional.join(" ");
    let (entries, note) = match registry::fetch() {
        Ok((entries, _)) => (entries, None),
        Err(e) => match registry::cached() {
            Some((entries, _)) => (entries, Some(format!("offline ({e}); showing the list from the last fetch"))),
            None => return Err(e),
        },
    };
    let category = flags.get("category");
    let order = if category.is_some() { registry::Order::Name } else { registry::Order::Category };
    let found = registry::select(&entries, &query, category, order);
    let places = install::Places::current();
    let state = |e: &registry::Entry| {
        if places.plugins.join(&e.id).exists() {
            if install::approved(&places, &e.id) { "installed" } else { "waiting for approval" }
        } else {
            "not installed"
        }
    };
    let compat = |e: &registry::Entry| match e.compatibility() {
        registry::Compatibility::Ok => None,
        registry::Compatibility::MadeForNewer(v) => Some(format!("made for SUPER DESKTOP {v} or newer; this is {}", env!("CARGO_PKG_VERSION"))),
        registry::Compatibility::NeedsApi(api) => Some(format!("needs plugin API {api}; update SUPER DESKTOP")),
    };
    if flags.json {
        let rows: Vec<Value> = found.iter().map(|(e, _)| json!({
            "id": e.id, "name": e.name, "description": e.description, "author": e.author, "categories": e.tags,
            "slots": e.slots, "repo": e.web_url(), "minHost": e.min_host, "api": e.api,
            "state": state(e), "warning": compat(e),
        })).collect();
        print_json(&json!({"ok": true, "source": registry::source(), "note": note, "categories": registry::categories(&entries), "plugins": rows}));
        return Ok(0);
    }
    if let Some(note) = &note {
        println!("{note}");
    }
    if found.is_empty() {
        println!("No plugin matches. Categories: {}", registry::categories(&entries).join(", "));
        return Ok(0);
    }
    let mut group: Option<&str> = None;
    for (e, under) in &found {
        if let Some(under) = under.as_deref().filter(|u| Some(*u) != group) {
            println!("\n{under}");
            group = Some(under);
        }
        println!("  {:<16} {} by {} ({})", e.id, e.name, e.author, state(e));
        println!("  {:<16} {}", "", e.description);
        if let Some(warning) = compat(e) {
            println!("  {:<16} ! {warning}", "");
        }
    }
    println!("\nInstall one: super-desktop plugin install <id>. Listing is not a review: read it before you approve it.");
    Ok(0)
}

fn install_command(flags: &Flags) -> Result<i32, String> {
    let [source] = flags.positional.as_slice() else {
        return Err("install needs one plugin id, git URL or folder".into());
    };
    let (source, entry) = registry::resolve(source)?;
    if let Some(registry::Compatibility::NeedsApi(api)) = entry.as_ref().map(registry::Entry::compatibility) {
        return Err(format!("{} needs plugin API {api}; update SUPER DESKTOP first", entry.map(|e| e.id).unwrap_or_default()));
    }
    let installed = install::install_as(&install::Places::current(), &source, entry.as_ref().map(|e| e.id.as_str()))?;
    let surfaces: Vec<Value> = installed.manifest.surfaces.iter()
        .map(|s| json!({"id": s.id, "slot": s.slot.name(), "qml": s.qml}))
        .collect();
    if flags.json {
        print_json(&json!({"ok": true, "id": installed.id, "dir": installed.dir, "commit": installed.commit,
            "approved": false, "surfaces": surfaces}));
    } else {
        println!("Installed {} ({}) in {}", installed.manifest.name, installed.id, installed.dir.display());
        for s in &installed.manifest.surfaces {
            println!("  {:<14} {}", s.slot.name(), s.qml);
        }
        println!("It does not run yet: its QML can run programs as you. Read it (or ask an agent to review it),");
        println!("then turn it on in Settings → Plugins or run: super-desktop plugin approve {}", installed.id);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_cli_help_for_each_command() {
        let snapshot = command_help("snapshot").unwrap();
        assert!(snapshot.starts_with("usage: super-desktop plugin snapshot <id|dir>"), "{snapshot}");
        assert!(snapshot.contains("--values FILE.json"), "{snapshot}");
        assert!(command_help("path").unwrap().contains("print the plugins folder"));
        assert!(command_help("nope").is_none());
        // Every command in the dispatch has its help.
        for command in ["list", "new", "check", "snapshot", "preview", "publish", "logs", "doctor", "search", "install", "approve", "update", "remove", "path"] {
            assert!(command_help(command).is_some(), "{command}");
        }
    }

    /// --values renders with sample values; a snapshot never changes the
    /// plugin's saved values.
    #[test]
    fn plugins_snapshot_uses_sample_values_and_keeps_saved_ones() {
        if paths::qml_host_binary().is_none() {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-snapshot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("sample");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.json"), json!({"id": "sample", "api": 1, "surfaces": [
            {"id": "main", "slot": "panel", "qml": "Main.qml", "size": 200, "height": 120}]}).to_string()).unwrap();
        // Painted only when it sees two items; it also tries to save.
        std::fs::write(dir.join("Main.qml"), "import QtQuick\nRectangle {\n    color: (sd.values.items || []).length === 2 ? \"red\" : \"transparent\"\n    Component.onCompleted: sd.setValue(\"touched\", true)\n}\n").unwrap();
        // Not paths::data_path: library tests run without the private test root.
        let saved = root.join("sample.data.json");
        std::fs::write(&saved, r#"{"items": [1]}"#).unwrap();
        let sample = root.join("sample.json");
        std::fs::write(&sample, r#"{"items": [1, 2]}"#).unwrap();
        let shot = |extra: &[&str]| {
            let mut args: Vec<String> = vec![dir.to_string_lossy().into(), "--out".into(), root.join("out.png").to_string_lossy().into()];
            args.extend(extra.iter().map(|a| a.to_string()));
            let flags = Flags::parse(&args, SNAPSHOT_OPTIONS).unwrap();
            snapshot_report(&flags, Some(&saved)).unwrap().0
        };
        let with_saved = shot(&[]);
        assert_eq!(with_saved["ok"], true, "{with_saved}");
        assert_eq!(with_saved["paintedFraction"], 0.0, "the saved values have one item");
        let with_sample = shot(&["--values", sample.to_str().unwrap()]);
        assert_eq!(with_sample["paintedFraction"], 1.0, "{with_sample}");
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), r#"{"items": [1]}"#, "a snapshot does not save");
        assert_eq!(std::fs::read_to_string(&sample).unwrap(), r#"{"items": [1, 2]}"#);
        std::fs::write(root.join("bad.json"), "[1, 2]").unwrap();
        let args: Vec<String> = vec![dir.to_string_lossy().into(), "--values".into(), root.join("bad.json").to_string_lossy().into()];
        let flags = Flags::parse(&args, &["values"]).unwrap();
        assert!(snapshot_report(&flags, Some(&saved)).unwrap_err().contains("one JSON object"));
        let _ = std::fs::remove_dir_all(&root);
    }
    /// preview composes the surfaces into a 1280×640 PNG; publish says what
    /// is missing before a plugin can be listed.
    #[test]
    fn plugins_preview_and_publish() {
        assert_eq!(github_repo("https://github.com/me/sd-clock.git").as_deref(), Some("me/sd-clock"));
        assert_eq!(github_repo("git@github.com:me/sd-clock").as_deref(), Some("me/sd-clock"));
        assert_eq!(github_repo("https://gitlab.com/me/sd-clock"), None);
        assert_eq!(github_repo("https://github.com/me"), None);
        if paths::qml_host_binary().is_none() {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-plugins-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("clock");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.json"), json!({"id": "clock", "api": 1, "description": "A clock in the top bar and a calendar under it.",
            "surfaces": [{"id": "main", "slot": "toolbar.item", "qml": "Bar.qml", "size": 60},
                         {"id": "cal", "slot": "popup", "qml": "Cal.qml", "size": 300, "height": 200},
                         {"id": "prefs", "slot": "settings.page", "qml": "Cal.qml"}]}).to_string()).unwrap();
        std::fs::write(dir.join("Bar.qml"), "import QtQuick\nRectangle { color: sd.theme.accent; radius: height / 2 }\n").unwrap();
        std::fs::write(dir.join("Cal.qml"), "import QtQuick\nRectangle { color: sd.theme.surface; Text { text: sd.values.day || \"?\"; color: sd.theme.foreground } }\n").unwrap();
        let sample = root.join("sample.json");
        std::fs::write(&sample, r#"{"day": "Mon"}"#).unwrap();
        let out = root.join("preview.png");
        let args: Vec<String> = vec![dir.to_string_lossy().into(), "--values".into(), sample.to_string_lossy().into(), "--out".into(), out.to_string_lossy().into()];
        assert_eq!(preview(&Flags::parse(&args, &["surface", "values", "out"]).unwrap()), Ok(0));
        let png = std::fs::read(&out).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        // The IHDR size: 1280×640.
        assert_eq!((u32::from_be_bytes(png[16..20].try_into().unwrap()), u32::from_be_bytes(png[20..24].try_into().unwrap())), (1280, 640));
        let args: Vec<String> = vec![dir.to_string_lossy().into(), "--surface".into(), "nope".into()];
        assert!(preview(&Flags::parse(&args, &["surface"]).unwrap()).unwrap_err().contains("no surface"));
        // Not its own repository, no minHost, no README: not ready.
        let args: Vec<String> = vec![dir.to_string_lossy().into(), "--json".into()];
        assert_eq!(publish(&Flags::parse(&args, &[]).unwrap()), Ok(1));
        let _ = std::fs::remove_dir_all(&root);
    }
}
