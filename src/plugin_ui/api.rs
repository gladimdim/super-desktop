//! `sd.call(method, params)` from plugin QML. Every method does what a click
//! of the user would do; none of them changes the bridge or the PC-to-PC
//! protocol.
use super::surface::SurfaceSpec;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;

/// What the overlay lets plugins do, wired in `window.rs`.
pub struct Hooks {
    pub hide: Rc<dyn Fn()>,
    /// `[{id, name, icon}]` offered in the top bar.
    pub harnesses: Rc<dyn Fn() -> Value>,
    /// Opens a card for a harness id, in `folder` or the top bar's folder.
    pub launch: Rc<dyn Fn(&str, Option<&str>) -> Result<(), String>>,
    /// `[{id, kind, title, harness, folder}]`.
    pub cards: Rc<dyn Fn() -> Value>,
    /// Opens a card for a harness in `folder` and returns its tmux session.
    pub open_card: Rc<dyn Fn(&str, &str) -> Result<String, String>>,
    /// Brings a card (by its id from `cards`) to the front with the keyboard.
    pub focus_card: Rc<dyn Fn(&str) -> Result<(), String>>,
    /// A terminal card's tmux session and harness, by card id.
    pub card_session: Rc<dyn Fn(&str) -> Option<(String, String)>>,
}

/// The hooks, if the overlay has set them.
pub fn hooks() -> Option<Hooks> {
    HOOKS.with(|cell| {
        cell.borrow().as_ref().map(|h| Hooks {
            hide: Rc::clone(&h.hide),
            harnesses: Rc::clone(&h.harnesses),
            launch: Rc::clone(&h.launch),
            cards: Rc::clone(&h.cards),
            open_card: Rc::clone(&h.open_card),
            focus_card: Rc::clone(&h.focus_card),
            card_session: Rc::clone(&h.card_session),
        })
    })
}

thread_local! {
    static HOOKS: RefCell<Option<Hooks>> = const { RefCell::new(None) };
}

pub fn set_hooks(hooks: Hooks) {
    HOOKS.with(|cell| *cell.borrow_mut() = Some(hooks));
}

fn error(code: &str, message: impl Into<String>, hint: &str) -> Value {
    json!({"code": code, "message": message.into(), "hint": hint})
}

/// Most text sent into a card at once.
const MAX_SEND: usize = 64 * 1024;
/// Most lines `cards.text` returns.
const MAX_LINES: usize = 300;

/// Like `call`, for every method; the ones that run tmux do it off the GTK
/// thread and answer later through `done`.
pub fn call_async(spec: &SurfaceSpec, method: &str, params: &Value, done: Box<dyn FnOnce(Result<Value, Value>)>) {
    if method == "llm.complete" {
        let request = match super::llm::Request::from_params(params) {
            Ok(request) => request,
            Err(e) => return done(Err(e)),
        };
        super_desktop::plugins::paths::append_log(&spec.plugin_id, &format!("[{}] call llm.complete ({} bytes)", spec.surface_id, request.prompt.len()));
        let handle = gtk4::gio::spawn_blocking(move || super::llm::complete(&request));
        gtk4::glib::spawn_future_local(async move {
            done(handle.await.unwrap_or_else(|_| Err(error("unavailable", "the request failed", "Try again."))));
        });
        return;
    }
    if method == "notify" && params.get("actions").is_some() {
        let command = match notify_command(spec, params) {
            Ok(command) => command,
            Err(e) => return done(Err(e)),
        };
        super_desktop::plugins::paths::append_log(&spec.plugin_id, &format!("[{}] call notify (with actions)", spec.surface_id));
        // notify-send waits for a click on an action, or for the notification to close.
        let handle = gtk4::gio::spawn_blocking(move || {
            let mut command = command;
            let output = command.stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output()
                .map_err(|e| error("unavailable", format!("notify-send failed: {e}"), "Install libnotify."))?;
            let chosen = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            Ok(json!({"action": if chosen.is_empty() { Value::Null } else { Value::String(chosen) }}))
        });
        gtk4::glib::spawn_future_local(async move {
            done(handle.await.unwrap_or_else(|_| Err(error("unavailable", "the notification failed", "Try again."))));
        });
        return;
    }
    if method == "overlay.show" {
        super_desktop::plugins::paths::append_log(&spec.plugin_id, &format!("[{}] call overlay.show", spec.surface_id));
        // Through the overlay's own command socket, the way `super-desktop
        // show` does it; the answer comes once it is on screen.
        let handle = gtk4::gio::spawn_blocking(|| {
            let socket = super_desktop::platform::runtime::socket_path();
            let mut stream = std::os::unix::net::UnixStream::connect(&socket)
                .map_err(|e| error("unavailable", format!("cannot reach SUPER DESKTOP: {e}"), "Try again in a moment."))?;
            let _ = stream.set_read_timeout(Some(super_desktop::cli_legacy::TIMEOUT));
            super_desktop::cli_legacy::send(&mut stream, "show")
                .and_then(|()| super_desktop::cli_legacy::receive(&mut stream, 64 * 1024))
                .map(|_| json!({}))
                .map_err(|e| error("unavailable", format!("SUPER DESKTOP did not answer: {e}"), "Try again in a moment."))
        });
        gtk4::glib::spawn_future_local(async move {
            done(handle.await.unwrap_or_else(|_| Err(error("unavailable", "showing failed", "Try again."))));
        });
        return;
    }
    if !matches!(method, "cards.text" | "cards.send") {
        done(call(spec, method, params));
        return;
    }
    let Some(hooks) = hooks() else {
        done(Err(error("unavailable", "SUPER DESKTOP is still starting", "Try again in a moment.")));
        return;
    };
    let Some(id) = params.get("id").and_then(Value::as_str) else {
        done(Err(error("bad_params", format!("{method} needs {{id}}"), "Pass the id of a terminal card from cards.list.")));
        return;
    };
    let Some((session, harness)) = (hooks.card_session)(id) else {
        done(Err(error("bad_params", format!("no terminal card {id:?}"), "Pass the id of a terminal card from cards.list (notes have no terminal).")));
        return;
    };
    super_desktop::plugins::paths::append_log(&spec.plugin_id, &format!("[{}] call {method} {id}", spec.surface_id));
    let work: Box<dyn FnOnce() -> Result<Value, Value> + Send> = if method == "cards.text" {
        let lines = params.get("lines").and_then(Value::as_u64).map_or(60, |n| n as usize).clamp(1, MAX_LINES);
        Box::new(move || {
            let text = crate::tmux::capture_pane_history(&session)
                .ok_or_else(|| error("unavailable", "the card's terminal could not be read", "The card may have just closed."))?;
            let all: Vec<&str> = text.trim_end().lines().collect();
            Ok(json!({"text": all[all.len().saturating_sub(lines)..].join("\n")}))
        })
    } else {
        let Some(text) = params.get("text").and_then(Value::as_str).map(str::to_owned) else {
            done(Err(error("bad_params", "cards.send needs {text}", "Pass the text to type; add enter: true to press Enter after it.")));
            return;
        };
        if text.len() > MAX_SEND {
            done(Err(error("bad_params", "text is longer than 64 KiB", "Send less at once.")));
            return;
        }
        let enter = params.get("enter").and_then(Value::as_bool).unwrap_or(false);
        Box::new(move || {
            crate::tmux::send_keys(&session, &harness, &text, enter)
                .map(|()| json!({}))
                .map_err(|e| error("unavailable", format!("could not type into the card: {e}"), "The card may have just closed."))
        })
    };
    let handle = gtk4::gio::spawn_blocking(work);
    gtk4::glib::spawn_future_local(async move {
        let result = handle.await.unwrap_or_else(|_| Err(error("unavailable", "the request failed", "Try again.")));
        done(result);
    });
}

pub fn call(spec: &SurfaceSpec, method: &str, params: &Value) -> Result<Value, Value> {
    let Some(hooks) = hooks() else {
        return Err(error("unavailable", "SUPER DESKTOP is still starting", "Try again in a moment."));
    };
    super_desktop::plugins::paths::append_log(&spec.plugin_id, &format!("[{}] call {method}", spec.surface_id));
    match method {
        "overlay.hide" => {
            // After this call returns: hiding unmaps the caller's own surface.
            let hide = Rc::clone(&hooks.hide);
            gtk4::glib::idle_add_local_once(move || hide());
            Ok(json!({}))
        }
        "harness.list" => Ok((hooks.harnesses)()),
        "harness.launch" => {
            let Some(harness) = params.get("harness").and_then(Value::as_str) else {
                return Err(error("bad_params", "harness.launch needs {harness}", "Pass an id from harness.list, e.g. {harness: \"claude\"}."));
            };
            let folder = params.get("folder").and_then(Value::as_str);
            if let Some(folder) = folder {
                if !std::path::Path::new(folder).is_dir() {
                    return Err(error("bad_params", format!("{folder} is not a folder"), "Give an absolute path to an existing folder, or leave folder out."));
                }
            }
            (hooks.launch)(harness, folder)
                .map(|()| json!({}))
                .map_err(|message| error("bad_params", message, "Use an id from harness.list."))
        }
        "cards.list" => Ok((hooks.cards)()),
        "cards.focus" => {
            let Some(id) = params.get("id").and_then(Value::as_str) else {
                return Err(error("bad_params", "cards.focus needs {id}", "Pass an id from cards.list."));
            };
            // After this call returns: focusing moves the keyboard away from the caller.
            let (focus, id) = (Rc::clone(&hooks.focus_card), id.to_owned());
            let known = (hooks.cards)().as_array().is_some_and(|cards| cards.iter().any(|c| c["id"] == id.as_str()));
            if !known {
                return Err(error("bad_params", format!("no card {id:?}"), "Pass an id from cards.list; cards come and go."));
            }
            gtk4::glib::idle_add_local_once(move || {
                let _ = focus(&id);
            });
            Ok(json!({}))
        }
        "card.new" => {
            let Some(surface) = params.get("surface").and_then(Value::as_str) else {
                return Err(error("bad_params", "card.new needs {surface}", "Pass the id of one of your plugin's \"card\" surfaces that has \"multiple\": true."));
            };
            let Some(slots) = super::slots::get() else {
                return Err(error("unavailable", "the overlay is not ready", "Try again in a moment."));
            };
            slots.card_check(&spec.plugin_id, surface)
                .map_err(|message| error("bad_params", message, "Declare the surface in plugin.json with \"slot\": \"card\"."))?;
            if !slots.card_is_multiple(&spec.plugin_id, surface) {
                return Err(error("bad_params", format!("{surface:?} is not \"multiple\""), "Add \"multiple\": true to the card surface, or use card.open."));
            }
            let (plugin, surface) = (spec.plugin_id.clone(), surface.to_owned());
            gtk4::glib::idle_add_local_once(move || {
                let _ = slots.new_card(&plugin, &surface);
            });
            Ok(json!({}))
        }
        "panel.open" | "panel.close" | "panel.toggle" | "card.open" | "card.close" | "card.toggle"
        | "fullscreen.open" | "fullscreen.close" | "fullscreen.toggle" => {
            let (kind, action) = method.split_once('.').unwrap_or((method, ""));
            let Some(surface) = params.get("surface").and_then(Value::as_str) else {
                return Err(error("bad_params", format!("{method} needs {{surface}}"), &format!("Pass the id of one of your plugin's \"{kind}\" surfaces, e.g. {{surface: \"main\"}}.")));
            };
            let Some(slots) = super::slots::get() else {
                return Err(error("unavailable", "the overlay is not ready", "Try again in a moment."));
            };
            let plugin = spec.plugin_id.clone();
            let surface = surface.to_owned();
            // A card's copy: given, else the caller's own when it is that card.
            let instance = params.get("instance").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| {
                if spec.surface_id == surface { spec.instance.clone() } else { String::new() }
            });
            let open = match kind {
                "panel" => slots.is_panel_open(&plugin, &surface),
                "card" => slots.is_card_open(&plugin, &surface, &instance),
                _ => slots.is_fullscreen_open(&plugin, &surface),
            };
            let want = match action {
                "open" => true,
                "close" => false,
                _ => !open,
            };
            if want {
                let checked = match kind {
                    "panel" => slots.panel_check(&plugin, &surface),
                    "card" => slots.card_check(&plugin, &surface),
                    _ => slots.fullscreen_check(&plugin, &surface),
                };
                let slot_name = if kind == "fullscreen" { "fullscreen" } else { kind };
                checked.map_err(|message| error("bad_params", message, &format!("Declare the surface in plugin.json with \"slot\": \"{slot_name}\".")))?;
            }
            // After this call returns: opening and closing change the widgets
            // the caller may be drawn in.
            let action = format!("{kind}.{}", if want { "open" } else { "close" });
            gtk4::glib::idle_add_local_once(move || {
                let _ = slots.act(&plugin, &action, &surface, &instance);
            });
            Ok(json!({"open": want}))
        }
        "popup.open" | "popup.close" | "popup.toggle" => {
            let Some(surface) = params.get("surface").and_then(Value::as_str) else {
                return Err(error("bad_params", format!("{method} needs {{surface}}"), "Pass the id of one of your plugin's \"popup\" surfaces, e.g. {surface: \"menu\"}."));
            };
            let Some(slots) = super::slots::get() else {
                return Err(error("unavailable", "the overlay is not ready", "Try again in a moment."));
            };
            let (plugin, surface) = (spec.plugin_id.clone(), surface.to_owned());
            let open = slots.is_popup_open(&plugin, &surface);
            // A press on the caller already closed it: its toggle leaves it closed.
            let want = match method {
                "popup.open" => true,
                "popup.close" => false,
                _ => !open && !slots.popup_just_closed(&plugin, &surface),
            };
            if want {
                slots.popup_check(&plugin, &surface)
                    .map_err(|message| error("bad_params", message, "Declare the surface in plugin.json with \"slot\": \"popup\"."))?;
                // Where in the caller it opens: x, y (and width, height) in
                // the caller's own coordinates; else beside the whole caller.
                let number = |name: &str| params.get(name).and_then(Value::as_f64).filter(|n| n.is_finite());
                let rect = number("x").zip(number("y")).map(|(x, y)| {
                    (x, y, number("width").unwrap_or(0.0).max(0.0), number("height").unwrap_or(0.0).max(0.0))
                });
                let anchor = super::slots::PopupAnchor { caller: spec.surface_id.clone(), instance: spec.instance.clone(), rect };
                // After this call returns: opening moves the keyboard.
                gtk4::glib::idle_add_local_once(move || {
                    let _ = slots.open_popup(&plugin, &surface, anchor);
                });
            } else if open {
                gtk4::glib::idle_add_local_once(move || {
                    slots.close_popup();
                });
            }
            Ok(json!({"open": want}))
        }
        "popup.resize" => {
            let Some(surface) = params.get("surface").and_then(Value::as_str) else {
                return Err(error("bad_params", "popup.resize needs {surface}", "Pass the open popup's id, e.g. {surface: sd.surfaceId, height: 200}."));
            };
            let Some(slots) = super::slots::get() else {
                return Err(error("unavailable", "the overlay is not ready", "Try again in a moment."));
            };
            let number = |name: &str| params.get(name).and_then(Value::as_f64).filter(|n| n.is_finite()).map(|n| n.round() as i32);
            let (width, height) = (number("width"), number("height"));
            if width.is_none() && height.is_none() {
                return Err(error("bad_params", "popup.resize needs width or height", "Pass a size in logical pixels, e.g. {surface: sd.surfaceId, height: column.implicitHeight + 24}."));
            }
            slots.resize_popup(&spec.plugin_id, surface, width, height)
                .map(|(width, height)| json!({"width": width, "height": height}))
                .map_err(|message| error("bad_params", message, "Resize a popup while it is open, usually from the popup itself."))
        }
        "notify" => {
            let mut command = notify_command(spec, params)?;
            let spawned = command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            match spawned {
                Ok(mut child) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                    Ok(json!({}))
                }
                Err(e) => Err(error("unavailable", format!("notify-send failed: {e}"), "Install libnotify.")),
            }
        }
        "sound.play" => {
            let path = sound_path(spec, params)?;
            play(&path).map(|()| json!({}))
        }
        _ => Err(error(
            "unknown_method",
            format!("unknown method {method:?}"),
            "Methods: overlay.show, overlay.hide, harness.list, harness.launch, cards.list, cards.focus, cards.text, cards.send, llm.complete, notify, sound.play, card.new, panel.open/close/toggle, card.open/close/toggle, fullscreen.open/close/toggle, popup.open/close/toggle, popup.resize.",
        )),
    }
}

/// `notify-send` for `{title, body, urgency?, actions?: [{id, label}]}`.
fn notify_command(spec: &SurfaceSpec, params: &Value) -> Result<std::process::Command, Value> {
    let title = params.get("title").and_then(Value::as_str).unwrap_or(&spec.plugin_name).to_owned();
    let body = params.get("body").and_then(Value::as_str).unwrap_or("").to_owned();
    let mut command = std::process::Command::new("notify-send");
    command.args(["--app-name", "SUPER DESKTOP"]);
    match params.get("urgency").and_then(Value::as_str) {
        None => {}
        Some(level @ ("low" | "normal" | "critical")) => {
            command.args(["--urgency", level]);
        }
        Some(other) => return Err(error("bad_params", format!("unknown urgency {other:?}"), "Use \"low\", \"normal\" or \"critical\" (stays until dismissed).")),
    }
    if let Some(actions) = params.get("actions") {
        let hint = "Pass up to 3 actions like [{id: \"default\", label: \"Show\"}, {id: \"snooze\", label: \"Snooze\"}]; \"default\" also runs on a click on the notification.";
        let list = actions.as_array().filter(|a| !a.is_empty() && a.len() <= 3).ok_or_else(|| error("bad_params", "actions must be a list of 1 to 3", hint))?;
        for action in list {
            let id = action.get("id").and_then(Value::as_str).unwrap_or("");
            let label = action.get("label").and_then(Value::as_str).unwrap_or("");
            let id_ok = !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
            if !id_ok || label.trim().is_empty() || label.chars().count() > 40 {
                return Err(error("bad_params", format!("bad action {action}"), hint));
            }
            command.arg(format!("--action={id}={}", label.trim()));
        }
    }
    command.args(["--", &title, &body]);
    Ok(command)
}

/// The sound file of `{file}` (in the plugin folder) or `{name}` (a
/// freedesktop sound such as "complete" or "bell").
fn sound_path(spec: &SurfaceSpec, params: &Value) -> Result<std::path::PathBuf, Value> {
    const SYSTEM: &str = "/usr/share/sounds/freedesktop/stereo";
    if let Some(file) = params.get("file").and_then(Value::as_str) {
        let relative = std::path::Path::new(file);
        let inside = !file.is_empty() && relative.components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir));
        let path = spec.dir.join(relative);
        if !inside || !path.is_file() {
            return Err(error("bad_params", format!("no sound file {file:?} in the plugin folder"), "Pass a .wav, .oga or .ogg file relative to the plugin folder, e.g. {file: \"ding.wav\"}, or a system sound {name: \"complete\"}."));
        }
        return Ok(path);
    }
    if let Some(name) = params.get("name").and_then(Value::as_str) {
        let path = std::path::Path::new(SYSTEM).join(format!("{name}.oga"));
        if name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && path.is_file() {
            return Ok(path);
        }
        let mut names: Vec<String> = std::fs::read_dir(SYSTEM).into_iter().flatten().flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".oga").map(str::to_owned))
            .collect();
        names.sort();
        return Err(error("bad_params", format!("no system sound {name:?}"), &format!("Use one of: {}.", names.join(", "))));
    }
    Err(error("bad_params", "sound.play needs {file} or {name}", "Pass {file: \"ding.wav\"} (in the plugin folder) or {name: \"complete\"}."))
}

/// Sounds playing now; more at once is refused.
static PLAYING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Plays a sound file with the first player the PC has, without waiting.
fn play(path: &std::path::Path) -> Result<(), Value> {
    use std::sync::atomic::Ordering;
    if PLAYING.fetch_add(1, Ordering::SeqCst) >= 4 {
        PLAYING.fetch_sub(1, Ordering::SeqCst);
        return Err(error("busy", "four sounds are playing already", "Play the next one when one ends."));
    }
    let spawned = std::process::Command::new("sh")
        .args(["-c", "pw-play -- \"$1\" 2>/dev/null || paplay -- \"$1\" 2>/dev/null || aplay -q -- \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
                PLAYING.fetch_sub(1, Ordering::SeqCst);
            });
            Ok(())
        }
        Err(e) => {
            PLAYING.fetch_sub(1, Ordering::SeqCst);
            Err(error("unavailable", format!("no sound player: {e}"), "Install pipewire (pw-play), pulseaudio-utils (paplay) or alsa-utils (aplay)."))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn wait(context: &gtk4::glib::MainContext, result: &Rc<RefCell<Option<Result<Value, Value>>>>) -> Result<Value, Value> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while result.borrow().is_none() {
            assert!(std::time::Instant::now() < deadline, "no answer");
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        result.borrow_mut().take().unwrap()
    }

    /// Runs against the private tmux server of the test run (test_isolation).
    #[test]
    fn plugins_cards_send_types_into_a_card_and_text_reads_it() {
        if std::process::Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("skipped: tmux is not installed");
            return;
        }
        let session = format!("sd_plugin_api_{}", std::process::id());
        // The shared helper: the same server settings as every other tmux test.
        let _session = crate::test_isolation::TmuxSession::start(&session, &["-x", "80", "-y", "20"], &["cat"]);
        let context = gtk4::glib::MainContext::new();
        context.with_thread_default(|| {
            let target = session.clone();
            set_hooks(Hooks {
                hide: Rc::new(|| {}),
                harnesses: Rc::new(|| json!([])),
                launch: Rc::new(|_, _| Ok(())),
                cards: Rc::new(|| json!([])),
                open_card: Rc::new(|_, _| Err("no".into())),
                focus_card: Rc::new(|_| Ok(())),
                card_session: Rc::new(move |id| (id == "card-1").then(|| (target.clone(), "shell".to_owned()))),
            });
            let spec = SurfaceSpec {
                plugin_id: "api-test".into(), plugin_name: "api-test".into(), surface_id: "main".into(), instance: String::new(),
                qml: "/dev/null".into(), dir: std::env::temp_dir(),
            };
            let result: Rc<RefCell<Option<Result<Value, Value>>>> = Rc::new(RefCell::new(None));
            let ask = |method: &str, params: Value| {
                let slot = Rc::clone(&result);
                call_async(&spec, method, &params, Box::new(move |r| *slot.borrow_mut() = Some(r)));
                wait(&context, &result)
            };
            assert_eq!(ask("cards.send", json!({"id": "card-9", "text": "x"})).unwrap_err()["code"], "bad_params");
            assert_eq!(ask("cards.send", json!({"id": "card-1"})).unwrap_err()["code"], "bad_params");
            ask("cards.send", json!({"id": "card-1", "text": "hello from a plugin", "enter": true})).unwrap();
            // cat echoes the line back: it is on the screen twice.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let text = ask("cards.text", json!({"id": "card-1", "lines": 5})).unwrap()["text"].as_str().unwrap().to_owned();
                if text.matches("hello from a plugin").count() >= 2 {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "not on screen: {text:?}");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }).unwrap();
    }
    #[test]
    fn plugins_sound_and_notify_params() {
        let dir = std::env::temp_dir().join(format!("sd-plugins-sound-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sounds")).unwrap();
        std::fs::write(dir.join("sounds/ding.wav"), b"RIFF").unwrap();
        let spec = SurfaceSpec {
            plugin_id: "beeper".into(), plugin_name: "Beeper".into(), surface_id: "main".into(), instance: String::new(),
            qml: dir.join("Main.qml"), dir: dir.clone(),
        };
        assert_eq!(sound_path(&spec, &json!({"file": "sounds/ding.wav"})).unwrap(), dir.join("sounds/ding.wav"));
        for bad in [json!({"file": "../etc/passwd"}), json!({"file": "/usr/share/sounds/freedesktop/stereo/bell.oga"}), json!({"file": "nope.wav"}), json!({})] {
            assert_eq!(sound_path(&spec, &bad).unwrap_err()["code"], "bad_params", "{bad}");
        }
        assert_eq!(sound_path(&spec, &json!({"name": "../x"})).unwrap_err()["code"], "bad_params");
        if std::path::Path::new("/usr/share/sounds/freedesktop/stereo/complete.oga").is_file() {
            assert!(sound_path(&spec, &json!({"name": "complete"})).is_ok());
            assert!(sound_path(&spec, &json!({"name": "nope"})).unwrap_err()["hint"].as_str().unwrap().contains("complete"));
        }
        let args = |params: Value| -> Vec<String> {
            notify_command(&spec, &params).unwrap().get_args().map(|a| a.to_string_lossy().into_owned()).collect()
        };
        assert_eq!(args(json!({"body": "-rf"})), ["--app-name", "SUPER DESKTOP", "--", "Beeper", "-rf"]);
        assert_eq!(args(json!({"title": "Due", "urgency": "critical", "actions": [{"id": "default", "label": "Show"}, {"id": "snooze", "label": " Snooze 5 min "}]})),
            ["--app-name", "SUPER DESKTOP", "--urgency", "critical", "--action=default=Show", "--action=snooze=Snooze 5 min", "--", "Due", ""]);
        for bad in [json!({"urgency": "loud"}), json!({"actions": []}), json!({"actions": [{"id": "a=b", "label": "x"}]}), json!({"actions": [{"id": "a"}]})] {
            assert_eq!(notify_command(&spec, &bad).unwrap_err()["code"], "bad_params", "{bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
