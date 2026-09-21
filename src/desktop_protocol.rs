//! Additive PC-to-PC protocol, independent of GTK and persisted AppState.
//!
//! Wire types are deliberately explicit: exporting AppState would leak local
//! preferences and eventually outgoing peer credentials. Unimplemented features
//! must not be advertised by the bridge. See docs/REMOTE_DESKTOP_PROTOCOL.md.
#![allow(dead_code)] // Contracts consumed by the following implementation steps.

use serde::{Deserialize, Serialize};

pub const DESKTOP_API_VERSION: u32 = 1;
pub const WORKSPACE_SNAPSHOT: &str = "workspace-snapshot-v1";
pub const WORKSPACE_LAYOUT: &str = "workspace-layout-v1";
pub const TERMINAL_PTY: &str = "terminal-pty-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub machine_id: String,
    pub desktop_api_version: u32,
    pub capabilities: Vec<String>,
}

impl Capabilities {
    pub fn current(machine_id: String) -> Self {
        Self {
            machine_id,
            desktop_api_version: DESKTOP_API_VERSION,
            // Enable only once the complete endpoint behavior is available.
            capabilities: vec![WORKSPACE_SNAPSHOT.into()],
        }
    }

    pub fn supports_remote_desktop(&self) -> bool {
        self.desktop_api_version == DESKTOP_API_VERSION
            && [WORKSPACE_LAYOUT, TERMINAL_PTY]
                .iter()
                .all(|required| self.capabilities.iter().any(|c| c == required))
    }
}

/// Remote object keys must include machine identity even when card IDs happen
/// to match. Local selection never requires a running bridge or a peer token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MachineSelection {
    Local,
    Remote(String),
}

impl Default for MachineSelection {
    fn default() -> Self {
        Self::Local
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardKey {
    pub machine_id: String,
    pub card_id: String,
}

/// Logical pixels. Insets describe the dock within the canvas, not a second
/// independently scaled coordinate space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Canvas {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub top_inset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CardLayout {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub restored_width: u32,
    pub restored_height: u32,
    pub iconified: bool,
    pub icon_x: Option<i32>,
    pub icon_y: Option<i32>,
    pub tag: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalSize {
    pub columns: u16,
    pub rows: u16,
}

impl TerminalSize {
    /// Bound both dimensions and total cells before PTY allocation or resize.
    pub fn validate(self) -> Result<Self, &'static str> {
        if self.columns == 0
            || self.rows == 0
            || self.columns > 1000
            || self.rows > 1000
            || u32::from(self.columns) * u32::from(self.rows) > 250_000
        {
            return Err("invalid_terminal_size");
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopCard {
    pub card_id: String,
    pub session_name: String,
    pub agent_type: String,
    pub title: String,
    pub status: String,
    pub session_alive: Option<bool>,
    pub workspace: String,
    pub revision: u64,
    pub layout: CardLayout,
    pub stacking_order: u32,
    pub expanded: bool,
    pub terminal_size: Option<TerminalSize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessType {
    pub id: String,
    pub name: String,
    pub available: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshot {
    pub machine_id: String,
    #[serde(flatten)]
    pub local: LocalWorkspaceSnapshot,
}

/// Daemon-owned payload. The bridge binds this to its persistent identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalWorkspaceSnapshot {
    pub epoch: String,
    pub revision: u64,
    pub canvas: Canvas,
    pub workspace: String,
    pub home_directory: String,
    pub visible_harnesses: Vec<String>,
    pub harness_types: Vec<HarnessType>,
    pub cards: Vec<DesktopCard>,
}

/// Initial subscriptions and recovery use complete snapshots. No delta schema
/// is promised until there is a retained event log and gap recovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkspaceEvent {
    Snapshot { workspace: WorkspaceSnapshot },
    Unavailable { error: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRequest {
    pub request_id: String,
    pub machine_id: String,
    pub expected_epoch: String,
    pub command: WorkspaceCommand,
}

/// Fields accepted from viewers, never arbitrary commands or tmux arguments.
/// The host must additionally check ownership, revisions and geometry bounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WorkspaceCommand {
    CreateTerminal {
        agent_type: String,
        workspace: String,
    },
    CloseTerminal {
        card_id: String,
        expected_revision: u64,
    },
    SetLayout {
        card_id: String,
        expected_revision: u64,
        layout: CardLayout,
    },
    SetWorkspace {
        workspace: String,
        expected_revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReply {
    pub request_id: String,
    pub machine_id: String,
    pub epoch: String,
    pub revision: u64,
    pub result: CommandResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CommandResult {
    Applied { card_id: Option<String> },
    Rejected { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn negotiation_requires_both_features_and_known_version() {
        let mut caps = Capabilities::current("machine-b".into());
        assert!(!caps.supports_remote_desktop());
        caps.capabilities.push(WORKSPACE_LAYOUT.into());
        assert!(!caps.supports_remote_desktop());
        caps.capabilities.push(TERMINAL_PTY.into());
        caps.capabilities.push("future-optional-feature".into());
        assert!(caps.supports_remote_desktop());
        caps.desktop_api_version += 1;
        assert!(!caps.supports_remote_desktop());
    }

    #[test]
    fn identity_does_not_alias_equal_card_ids_on_different_hosts() {
        let a = CardKey {
            machine_id: "a".into(),
            card_id: "sd_term_1".into(),
        };
        let b = CardKey {
            machine_id: "b".into(),
            card_id: "sd_term_1".into(),
        };
        assert_ne!(a, b);
        assert_eq!(MachineSelection::default(), MachineSelection::Local);
    }

    #[test]
    fn command_wire_format_is_typed_and_rejects_unknown_arguments() {
        let mut request = json!({
            "requestId": "r1", "machineId": "b", "expectedEpoch": "boot1",
            "command": {"type": "createTerminal", "agentType": "shell", "workspace": "/work/project with spaces"}
        });
        let parsed: CommandRequest = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), request);
        request["command"]["shellCommand"] = json!("arbitrary command");
        assert!(serde_json::from_value::<CommandRequest>(request).is_err());
        assert!(serde_json::from_value::<WorkspaceCommand>(json!({
            "type": "closeTerminal", "cardId": "card-without-revision"
        }))
        .is_err());
    }

    #[test]
    fn pty_sizes_are_nonzero_and_bounded() {
        assert!(TerminalSize {
            columns: 120,
            rows: 40
        }
        .validate()
        .is_ok());
        for (columns, rows) in [(0, 24), (80, 0), (1001, 1), (1000, 1000)] {
            assert!(TerminalSize { columns, rows }.validate().is_err());
        }
    }
}
