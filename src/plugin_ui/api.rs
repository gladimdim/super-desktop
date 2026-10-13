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
            let open = match kind {
                "panel" => slots.is_panel_open(&plugin, &surface),
                "card" => slots.is_card_open(&plugin, &surface),
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
            let kind = kind.to_owned();
            gtk4::glib::idle_add_local_once(move || {
                let _ = match (kind.as_str(), want) {
                    ("panel", true) => slots.open_panel(&plugin, &surface),
                    ("panel", false) => { slots.close_panel(&plugin, &surface); Ok(()) }
                    ("card", true) => slots.open_card(&plugin, &surface),
                    ("card", false) => { slots.close_card(&plugin, &surface, true); Ok(()) }
                    (_, true) => slots.open_fullscreen(&plugin, &surface),
                    (_, false) => {
                        if slots.is_fullscreen_open(&plugin, &surface) {
                            slots.close_fullscreen();
                        }
                        Ok(())
                    }
                };
            });
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
            "Methods: overlay.hide, harness.list, harness.launch, cards.list, cards.focus, notify, panel.open/close/toggle, card.open/close/toggle, fullscreen.open/close/toggle.",
        )),
    }
}
