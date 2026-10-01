//! View models from plugins (`skills/super-desktop-plugin/schemas/ui.schema.json`):
//! checked here before the GTK side builds anything, so a bad model is an
//! error answer to the plugin, never a broken panel.
use super::rpc::RpcError;
use serde_json::Value;
use std::collections::BTreeSet;

pub const MAX_NODES: usize = 2000;
pub const MAX_CHILDREN: usize = 500;
pub const MAX_TEXT: usize = 64 * 1024;
pub const MAX_OPS: usize = 500;
const DOCS: &str = "references/ui.md#nodes";

pub const TYPES: [&str; 18] = [
    "column", "row", "scroll", "list", "label", "markdown", "code", "badge", "icon", "button", "toggle", "checkbox", "entry",
    "textArea", "select", "progress", "spinner", "separator",
];

pub fn is_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

fn err(path: &str, why: impl std::fmt::Display) -> RpcError {
    RpcError::invalid_params(format!("{path}: {why}"), DOCS)
}

/// Check a whole tree; returns the ids it contains.
pub fn check_tree(node: &Value) -> Result<BTreeSet<String>, RpcError> {
    let mut ids = BTreeSet::new();
    let mut count = 0;
    check_node(node, "model", &mut ids, &mut count)?;
    Ok(ids)
}

fn check_node(node: &Value, path: &str, ids: &mut BTreeSet<String>, count: &mut usize) -> Result<(), RpcError> {
    *count += 1;
    if *count > MAX_NODES {
        return Err(err(path, format!("more than {MAX_NODES} nodes")));
    }
    let object = node.as_object().ok_or_else(|| err(path, "a node is an object"))?;
    let kind = object.get("type").and_then(Value::as_str).ok_or_else(|| err(path, "missing \"type\""))?;
    if !TYPES.contains(&kind) {
        return Err(err(path, format!("unknown type `{kind}`; use one of {}", TYPES.join(", "))));
    }
    let id = object.get("id").and_then(Value::as_str).ok_or_else(|| err(path, "missing \"id\""))?;
    if !is_id(id) {
        return Err(err(path, format!("id `{id}` must be 1–64 of letters, digits, _ . : -")));
    }
    if !ids.insert(id.to_string()) {
        return Err(err(path, format!("id `{id}` is used twice in the view")));
    }
    let path = format!("{path}[{id}]");
    check_props(kind, object, &path, false)?;
    if let Some(children) = object.get("children") {
        if !matches!(kind, "column" | "row" | "list" | "scroll") {
            return Err(err(&path, format!("`{kind}` cannot have children")));
        }
        let children = children.as_array().ok_or_else(|| err(&path, "children is an array"))?;
        if children.len() > MAX_CHILDREN {
            return Err(err(&path, format!("more than {MAX_CHILDREN} children")));
        }
        for child in children {
            check_node(child, &path, ids, count)?;
        }
    }
    Ok(())
}

/// Props checked against the node's type. `partial` is a `set`: it changes
/// only the props it names, so none is required.
pub fn check_props(kind: &str, props: &serde_json::Map<String, Value>, path: &str, partial: bool) -> Result<(), RpcError> {
    let text = |key: &str, max: usize, required: bool| -> Result<(), RpcError> {
        let required = required && !partial;
        match props.get(key) {
            None if required => Err(err(path, format!("`{kind}` needs \"{key}\""))),
            None => Ok(()),
            Some(Value::String(s)) if s.len() <= max => Ok(()),
            Some(_) => Err(err(path, format!("\"{key}\" must be a string of at most {max} bytes"))),
        }
    };
    let boolean = |key: &str, required: bool| -> Result<(), RpcError> {
        let required = required && !partial;
        match props.get(key) {
            None if required => Err(err(path, format!("`{kind}` needs \"{key}\""))),
            None | Some(Value::Bool(_)) => Ok(()),
            Some(_) => Err(err(path, format!("\"{key}\" must be true or false"))),
        }
    };
    let one_of = |key: &str, allowed: &[&str]| -> Result<(), RpcError> {
        match props.get(key) {
            None => Ok(()),
            Some(Value::String(s)) if allowed.contains(&s.as_str()) => Ok(()),
            Some(_) => Err(err(path, format!("\"{key}\" must be one of {}", allowed.join(", ")))),
        }
    };
    let int = |key: &str, min: i64, max: i64| -> Result<(), RpcError> {
        match props.get(key) {
            None => Ok(()),
            Some(v) if v.as_i64().is_some_and(|n| (min..=max).contains(&n)) => Ok(()),
            Some(_) => Err(err(path, format!("\"{key}\" must be an integer from {min} to {max}"))),
        }
    };
    boolean("visible", false)?;
    match kind {
        "column" | "row" | "list" => int("gap", 0, 32),
        "scroll" => int("maxHeight", 40, 1200),
        "label" => {
            text("text", MAX_TEXT, true)?;
            boolean("wrap", false)?;
            one_of("style", &["title", "body", "muted", "error", "success", "mono"])
        }
        "markdown" | "code" => text("text", MAX_TEXT, true),
        "badge" => {
            text("text", 24, true)?;
            one_of("tone", &["neutral", "accent", "success", "warning", "error"])
        }
        "icon" => {
            text("name", 256, true)?;
            int("size", 8, 128)
        }
        "button" => {
            text("label", 48, true)?;
            text("icon", 256, false)?;
            boolean("enabled", false)?;
            one_of("tone", &["default", "primary", "danger"])
        }
        "toggle" | "checkbox" => {
            text("label", 120, false)?;
            boolean("value", true)?;
            boolean("enabled", false)
        }
        "entry" => {
            text("value", 4096, false)?;
            text("placeholder", 120, false)?;
            boolean("enabled", false)
        }
        "textArea" => {
            text("value", MAX_TEXT, false)?;
            int("rows", 1, 40)?;
            boolean("enabled", false)
        }
        "select" => {
            text("value", 4096, false)?;
            if partial && !props.contains_key("options") {
                return Ok(());
            }
            let options = props.get("options").and_then(Value::as_array).ok_or_else(|| err(path, "`select` needs \"options\""))?;
            if options.is_empty() || options.len() > 100 {
                return Err(err(path, "options must have 1–100 entries"));
            }
            for option in options {
                if !(option["value"].is_string() && option["label"].as_str().is_some_and(|l| l.len() <= 80)) {
                    return Err(err(path, "each option is {\"value\": string, \"label\": string}"));
                }
            }
            Ok(())
        }
        "progress" => match props.get("value") {
            None | Some(Value::Null) => Ok(()),
            Some(v) if v.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n)) => Ok(()),
            Some(_) => Err(err(path, "\"value\" is a number from 0 to 1, or null")),
        },
        _ => Ok(()),
    }
}

/// A patch op's shape (`patchOp` in the schema). Whether its id exists is up
/// to the view that applies it.
pub fn check_op(op: &Value) -> Result<(), RpcError> {
    let kind = op["op"].as_str().ok_or_else(|| err("op", "missing \"op\""))?;
    let id = op["id"].as_str().filter(|id| is_id(id)).ok_or_else(|| err("op", "missing or invalid \"id\""))?;
    match kind {
        "set" => {
            op["props"].as_object().ok_or_else(|| err(id, "set needs \"props\""))?;
            if op["props"].get("type").is_some() || op["props"].get("id").is_some() || op["props"].get("children").is_some() {
                return Err(err(id, "set cannot change type, id or children; use replace"));
            }
            Ok(())
        }
        "replace" | "append" => check_tree(&op["node"]).map(|_| ()),
        "remove" => Ok(()),
        other => Err(err(id, format!("unknown op `{other}`; use set, replace, append or remove"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plugin_ui_model_checks() {
        let good = json!({"type": "column", "id": "root", "gap": 8, "children": [
            {"type": "label", "id": "l", "text": "hi", "style": "muted"},
            {"type": "button", "id": "b", "label": "Go", "tone": "primary"},
            {"type": "select", "id": "s", "options": [{"value": "a", "label": "A"}]},
            {"type": "progress", "id": "p", "value": null}
        ]});
        assert_eq!(check_tree(&good).unwrap().len(), 5);
        for bad in [
            json!({"type": "div", "id": "x"}),
            json!({"type": "label", "id": "x"}),
            json!({"type": "label", "id": "bad id", "text": ""}),
            json!({"type": "row", "id": "r", "children": [{"type": "spinner", "id": "a"}, {"type": "spinner", "id": "a"}]}),
            json!({"type": "label", "id": "x", "text": "t", "children": []}),
            json!({"type": "button", "id": "x", "label": "Go", "tone": "loud"}),
            json!({"type": "progress", "id": "x", "value": 2}),
        ] {
            assert!(check_tree(&bad).is_err(), "{bad}");
        }
        assert!(check_op(&json!({"op": "set", "id": "l", "props": {"text": "x"}})).is_ok());
        // A set changes only what it names: nothing is required.
        let partial = json!({"visible": true}).as_object().unwrap().clone();
        for kind in ["button", "label", "checkbox", "select", "badge", "icon"] {
            assert!(check_props(kind, &partial, "x", true).is_ok(), "{kind}");
            assert!(check_props(kind, &partial, "x", false).is_err(), "{kind} needs its props when built");
        }
        assert!(check_op(&json!({"op": "set", "id": "l", "props": {"type": "row"}})).is_err());
        assert!(check_op(&json!({"op": "move", "id": "l"})).is_err());
    }

    /// The node types here and in ui.schema.json are the same list.
    #[test]
    fn plugin_spec_ui_types_match_the_schema() {
        let schema: Value = serde_json::from_str(include_str!("../../skills/super-desktop-plugin/schemas/ui.schema.json")).unwrap();
        let listed: BTreeSet<&str> = schema["$defs"]["node"]["properties"]["type"]["enum"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(listed, TYPES.into_iter().collect());
    }
}
