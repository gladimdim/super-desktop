//! Explicit MCP projections of owner-control commands. The discriminators are
//! server-owned so a caller cannot switch to another method or workspace action.
use crate::cli_extended::valid_id;
use crate::control::{Command, Reply};
use crate::mcp_settings::Config;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

pub(crate) struct Operation {
    pub name: &'static str,
    method: &'static str,
    description: &'static str,
    // For selected workspace variants, expose their fields at the top level.
    selector: Option<(&'static str, &'static str)>,
    pub permission: Permission,
    pub mutation: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum Permission {
    Layout,
    Notes,
    Files,
    Admin,
    Plugins,
    History,
    Editor,
    Replace,
    Controls,
    Metadata,
}

macro_rules! op {
    ($name:literal, $method:literal, $desc:literal, $perm:ident, $write:literal) => {
        Operation {
            name: $name,
            method: $method,
            description: $desc,
            permission: Permission::$perm,
            mutation: $write,
            selector: None,
        }
    };
    ($name:literal, $method:literal, $desc:literal, $perm:ident, $write:literal, $key:literal, $variant:literal) => {
        Operation {
            selector: Some(($key, $variant)),
            ..op!($name, $method, $desc, $perm, $write)
        }
    };
}

pub(crate) const OPS: &[Operation] = &[
    op!("run_slash_command","terminal.input","Send a catalog slash command with the harness-specific Enter behavior.",Controls,true,"input","slash"),
    op!("read_editor_buffer","workspace.read","Read a UTF-8 page and revision of an open document.",Editor,false,"query","buffer"),
    op!("edit_editor_buffer","workspace.edit","Create, modify, save or close a live editor document using the inspected revision.",Editor,true,"edit","editor"),
    op!("export_desktop_layout","workspace.read","Export all four card types as a version 2 layout for validate_layout and apply_layout.",Layout,false,"query","desktopLayout"),
    op!("inspect_telegram","telegram.read","Read Telegram configuration and pending requests. Set invitation=true to read the active private invite.",Admin,false),
    op!("manage_telegram","telegram.edit","Manage Telegram pairing, forum topics, permissions, Mini App settings and bot configuration.",Admin,true),
    op!(
        "prompt_history",
        "terminal.history",
        "Read up to ten dated prompts from a card; use nextOffset for the next page.",
        History,
        false
    ),
    op!(
        "list_slash_commands",
        "harness.slash",
        "List native slash commands, descriptions and whether to press Enter for a harness ID.",
        Metadata,
        false
    ),
    op!(
        "list_editors",
        "workspace.read",
        "List open editor cards, folders and tabs.",
        Editor,
        false,
        "query",
        "editors"
    ),
    op!(
        "open_editor",
        "workspace.edit",
        "Open or focus an editor at an absolute directory and optionally open a file.",
        Editor,
        true,
        "edit",
        "openEditor"
    ),
    op!(
        "open_plugin_surface",
        "workspace.edit",
        "Open an approved plugin panel, card or fullscreen surface.",
        Plugins,
        true,
        "edit",
        "pluginSurface"
    ),
    op!(
        "arrange_desktop",
        "workspace.edit",
        "Use the desktop Arrange action for all card types.",
        Layout,
        true,
        "edit",
        "arrangeDesktop"
    ),
    op!(
        "navigate_cards",
        "workspace.edit",
        "Focus the next, previous or attention-needing terminal.",
        Layout,
        true,
        "edit",
        "navigate"
    ),
    op!(
        "forget_recent_folder",
        "workspace.edit",
        "Remove a folder from the recent dropdown.",
        Layout,
        true,
        "edit",
        "forgetFolder"
    ),
    op!(
        "move_terminal",
        "terminal.move",
        "Move a terminal card in logical pixels.",
        Layout,
        true
    ),
    op!(
        "resize_terminal",
        "terminal.resize",
        "Resize a terminal card in logical pixels.",
        Layout,
        true
    ),
    op!(
        "set_terminal_mode",
        "terminal.mode",
        "Minimize, restore, expand or collapse a terminal.",
        Layout,
        true
    ),
    op!(
        "change_terminal_card",
        "terminal.card",
        "Focus, raise or tag a terminal card.",
        Layout,
        true
    ),
    op!(
        "list_viewports",
        "terminal.viewport.list",
        "Inspect temporary terminal grid leases.",
        Layout,
        false
    ),
    op!(
        "change_viewport",
        "terminal.viewport",
        "Acquire, change or release a temporary grid lease.",
        Layout,
        true
    ),
    op!(
        "inspect_workspace",
        "workspace.read",
        "Read workspace dimensions, card counts and revision guards.",
        Layout,
        false,
        "query",
        "inspect"
    ),
    op!(
        "list_workspace_folders",
        "workspace.read",
        "List selected and remembered workspace folders.",
        Layout,
        false,
        "query",
        "folders"
    ),
    op!(
        "export_layout",
        "workspace.read",
        "Export the terminal and sticky note layout.",
        Layout,
        false,
        "query",
        "layout"
    ),
    op!(
        "validate_layout",
        "workspace.read",
        "Validate an exported layout against the current workspace.",
        Layout,
        false,
        "query",
        "validateLayout"
    ),
    op!(
        "apply_layout",
        "workspace.edit",
        "Apply a validated version 1 or mixed-card version 2 layout.",
        Layout,
        true,
        "edit",
        "layout"
    ),
    op!(
        "arrange_workspace",
        "workspace.edit",
        "Arrange terminal and note cards.",
        Layout,
        true,
        "edit",
        "arrange"
    ),
    op!(
        "set_workspace_folder",
        "workspace.edit",
        "Select the absolute folder for future launches.",
        Layout,
        true,
        "edit",
        "folder"
    ),
    op!(
        "move_note",
        "workspace.edit",
        "Move a sticky note.",
        Notes,
        true,
        "edit",
        "noteMove"
    ),
    op!(
        "resize_note",
        "workspace.edit",
        "Resize a sticky note.",
        Notes,
        true,
        "edit",
        "noteResize"
    ),
    op!(
        "tag_note",
        "workspace.edit",
        "Set a sticky note color tag (0–8).",
        Notes,
        true,
        "edit",
        "noteTag"
    ),
    op!(
        "terminal_files",
        "terminal.files.read",
        "List checked file references or read a bounded chunk.",
        Files,
        false
    ),
    op!(
        "edit_terminal_files",
        "terminal.files.edit",
        "Add, save or remove a checked workspace file reference.",
        Files,
        true
    ),
    op!(
        "forget_terminal",
        "terminal.forget",
        "Remove the card while preserving its terminal session.",
        Replace,
        true
    ),
    op!(
        "relaunch_terminal",
        "terminal.relaunch",
        "Replace a terminal with a fresh or explicitly selected native session.",
        Replace,
        true
    ),
    op!(
        "read_preferences",
        "settings.read",
        "Read typed desktop settings, launcher configuration, theme or usage.",
        Admin,
        false
    ),
    op!(
        "edit_preferences",
        "settings.edit",
        "Change a typed setting or launcher configuration.",
        Admin,
        true
    ),
    op!(
        "inspect_access",
        "access.list",
        "Inspect local owner control access.",
        Admin,
        false
    ),
    op!(
        "list_audit_records",
        "audit.list",
        "List a bounded page of mutation receipt metadata.",
        Admin,
        false
    ),
    op!(
        "list_connections",
        "connection.read",
        "List authorized devices or pending requests.",
        Admin,
        false
    ),
    op!(
        "invite_connection",
        "connection.invite",
        "Create a single-use invitation in a new private file.",
        Admin,
        true
    ),
    op!(
        "decide_connection",
        "connection.decide",
        "Approve or reject the exact pending request with its comparison code.",
        Admin,
        true
    ),
    op!(
        "revoke_connection",
        "connection.revoke",
        "Revoke one device and disconnect its streams.",
        Admin,
        true
    ),
    op!(
        "add_peer",
        "peer.add",
        "Request pairing with a pinned peer invitation.",
        Admin,
        true
    ),
    op!(
        "inspect_peer_pairing",
        "peer.pairing",
        "Read one outgoing pairing job.",
        Admin,
        false
    ),
    op!(
        "read_peers",
        "peer.read",
        "List, inspect or read a peer workspace (query list, inspect or workspace).",
        Admin,
        false
    ),
    op!(
        "send_peer_command",
        "peer.command",
        "Send an explicitly authorized typed remote workspace command.",
        Admin,
        true
    ),
    op!(
        "forget_peer",
        "peer.forget",
        "Remove a saved outgoing peer.",
        Admin,
        true
    ),
    op!(
        "check_updates",
        "updates.check",
        "Queue a source-install update check.",
        Admin,
        true
    ),
    op!(
        "inspect_update",
        "updates.status",
        "Read an update job.",
        Admin,
        false
    ),
    op!(
        "install_update",
        "updates.install",
        "Install a reviewed version and commit.",
        Admin,
        true
    ),
];

pub(crate) fn lookup(name: &str) -> Option<&'static Operation> {
    OPS.iter().find(|op| op.name == name)
}

static COMMAND_SCHEMA: LazyLock<Value> = LazyLock::new(|| json!(schemars::schema_for!(Command)));

fn variant<'a>(schema: &'a Value, key: &str, value: &str) -> &'a Value {
    schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["properties"][key]["const"] == value)
        .expect("registered control variant")
}

// Inline referenced definitions so each advertised tool has a standalone schema.
fn resolve(value: &Value) -> Value {
    if let Some(reference) = value["$ref"].as_str() {
        return resolve(
            COMMAND_SCHEMA
                .pointer(reference.strip_prefix('#').unwrap())
                .unwrap(),
        );
    }
    match value {
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), resolve(v))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(resolve).collect()),
        other => other.clone(),
    }
}

impl Operation {
    fn shape(&self) -> (&Value, Option<&Value>) {
        let outer = variant(&COMMAND_SCHEMA, "method", self.method);
        let inner = self.selector.map(|(key, tag)| {
            let path = outer["properties"][key]["$ref"].as_str().unwrap();
            variant(COMMAND_SCHEMA.pointer(&path[1..]).unwrap(), "kind", tag)
        });
        (outer, inner)
    }

    pub fn tool(&self) -> Value {
        let (outer, inner) = self.shape();
        let mut properties = Map::new();
        let mut required = Vec::new();
        for schema in [Some(outer), inner].into_iter().flatten() {
            for (key, spec) in schema["properties"].as_object().unwrap() {
                if key == "method" || key == "kind" || self.selector.is_some_and(|(k, _)| k == key)
                {
                    continue;
                }
                properties.insert(key.clone(), resolve(spec));
                if schema["required"]
                    .as_array()
                    .is_some_and(|r| r.contains(&json!(key)))
                {
                    required.push(key.clone());
                }
            }
        }
        if matches!(self.permission, Permission::Replace) {
            properties.insert("confirm".into(), json!({"type":"boolean","const":true}));
            required.push("confirm".into());
        }
        if self.mutation {
            properties.insert(
                "requestId".into(),
                json!({"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"}),
            );
            required.push("requestId".into());
        }
        json!({"name":self.name,"description":self.description,
            "inputSchema":{"type":"object","additionalProperties":false,"properties":properties,"required":required},
            "outputSchema":schemars::schema_for!(Reply),
            "annotations":{"readOnlyHint":!self.mutation,"destructiveHint":self.mutation,"idempotentHint":!self.mutation,"openWorldHint":true}})
    }

    pub fn parse(&self, args: Option<&Value>) -> Result<(Command, Option<String>), &'static str> {
        let mut fields = match args {
            Some(value) => value
                .as_object()
                .ok_or("Expected an arguments object")?
                .clone(),
            None => Map::new(),
        };
        if fields.contains_key("method") || fields.contains_key("kind") {
            return Err("Unexpected discriminator");
        }
        if matches!(self.permission, Permission::Replace)
            && fields.remove("confirm") != Some(json!(true))
        {
            return Err("Explicit confirm=true required.");
        }
        let id = if self.mutation {
            let value = fields
                .remove("requestId")
                .ok_or("An explicit requestId is required")?;
            Some(
                value
                    .as_str()
                    .filter(|s| valid_id(s, 64))
                    .ok_or("Invalid requestId")?
                    .to_owned(),
            )
        } else {
            None
        };
        if let Some((key, tag)) = self.selector {
            if fields.contains_key(key) {
                return Err("Unexpected nested action");
            }
            let mut inner = Map::new();
            for field in self.shape().1.unwrap()["properties"]
                .as_object()
                .unwrap()
                .keys()
            {
                if let Some(value) = fields.remove(field) {
                    inner.insert(field.clone(), value);
                }
            }
            inner.insert("kind".into(), json!(tag));
            fields.insert(key.into(), Value::Object(inner));
        }
        fields.insert("method".into(), json!(self.method));
        let command = serde_json::from_value(Value::Object(fields))
            .map_err(|_| "Invalid arguments for this operation")?;
        Ok((command, id))
    }

    pub fn allowed(&self, c: &Config) -> bool {
        match self.permission {
            Permission::Layout => c.layout,
            Permission::Files => c.files,
            Permission::Plugins => c.plugins,
            Permission::Admin => c.administration,
            Permission::Editor => c.editor,
            Permission::History => c.history,
            Permission::Notes => c.edit_notes,
            Permission::Controls => c.controls,
            Permission::Metadata => true,
            Permission::Replace => c.administration && c.launch && c.close,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projected_workspace_tools_fix_the_action_and_require_receipt_ids() {
        let op = lookup("move_note").unwrap();
        let args = json!({"id":"note","x":50,"y":160,"expectEpoch":"epoch","expectRevision":"a".repeat(64),"requestId":"edit"});
        let (command, id) = op.parse(Some(&args)).unwrap();
        assert_eq!(id.as_deref(), Some("edit"));
        assert!(matches!(
            command,
            Command::WorkspaceEdit {
                edit: crate::control::WorkspaceEdit::NoteMove { x: 50, y: 160, .. },
                ..
            }
        ));
        for key in ["kind", "method", "edit", "unknown"] {
            let mut forged = args.clone();
            forged[key] = json!("noteDelete");
            assert!(op.parse(Some(&forged)).is_err());
        }
        let mut missing = args;
        missing.as_object_mut().unwrap().remove("requestId");
        assert!(op.parse(Some(&missing)).is_err());
        assert!(!Config::default().allows(op.name));
        assert!(Config {
            edit_notes: true,
            ..Default::default()
        }
        .allows(op.name));
        assert!(!Config {
            edit_notes: true,
            ..Default::default()
        }
        .allows("read_editor_buffer"));
        assert!(!Config::default().allows("future_unregistered_operation"));
    }
}
