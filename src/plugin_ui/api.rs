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
        "panel.open" | "panel.close" | "panel.toggle" => {
            let Some(surface) = params.get("surface").and_then(Value::as_str) else {
                return Err(error("bad_params", format!("{method} needs {{surface}}"), "Pass the id of one of your plugin's \"panel\" surfaces, e.g. {surface: \"main\"}."));
            };
            let Some(slots) = super::slots::get() else {
                return Err(error("unavailable", "panels are not ready", "Try again in a moment."));
            };
            let plugin = spec.plugin_id.clone();
            let surface = surface.to_owned();
            let open = slots.is_panel_open(&plugin, &surface);
            let want = match method {
                "panel.open" => true,
                "panel.close" => false,
                _ => !open,
            };
            if want {
                // Checked now, opened after this call returns.
                slots.panel_check(&plugin, &surface)
                    .map_err(|message| error("bad_params", message, "Declare the surface in plugin.json with \"slot\": \"panel\"."))?;
                gtk4::glib::idle_add_local_once(move || {
                    let _ = slots.open_panel(&plugin, &surface);
                });
            } else if open {
                gtk4::glib::idle_add_local_once(move || slots.close_panel(&plugin, &surface));
            }
            Ok(json!({"open": want}))
        }
        "notify" => {
            let title = params.get("title").and_then(Value::as_str).unwrap_or(&spec.plugin_name).to_owned();
            let body = params.get("body").and_then(Value::as_str).unwrap_or("").to_owned();
            let spawned = std::process::Command::new("notify-send")
                .args(["--app-name", "SUPER DESKTOP", &title, &body])
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
        _ => Err(error(
            "unknown_method",
            format!("unknown method {method:?}"),
            "Methods: overlay.hide, harness.list, harness.launch, cards.list, notify, panel.open, panel.close, panel.toggle.",
        )),
    }
}
