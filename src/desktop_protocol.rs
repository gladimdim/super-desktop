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

/// How many remote terminals one viewer may keep attached at once. The host
/// enforces this per credential, and the viewer uses the same number to decide
/// which visible cards receive a live stream.
pub const MAX_REMOTE_VIEWERS: usize = 8;
/// Largest binary frame either side sends on an attach stream.
pub const ATTACH_MAX_CHUNK: usize = 16 * 1024;
/// Bounded pending output per attachment. A viewer that falls this far behind
/// is disconnected rather than silently dropping terminal bytes.
pub const ATTACH_MAX_BACKLOG: usize = 1024 * 1024;
/// Longest an attach stream lives before the viewer is asked to reconnect.
pub const ATTACH_MAX_SECS: u64 = 1800;

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
            // `workspace-layout-v1` stays unadvertised: the host does not accept
            // remote layout/lifecycle mutations yet.
            capabilities: vec![WORKSPACE_SNAPSHOT.into(), TERMINAL_PTY.into()],
        }
    }

    pub fn supports_remote_desktop(&self) -> bool {
        self.desktop_api_version == DESKTOP_API_VERSION
            && [WORKSPACE_LAYOUT, TERMINAL_PTY]
                .iter()
                .all(|required| self.capabilities.iter().any(|c| c == required))
    }

    /// Read-only live terminal output over WSS. Separate from
    /// `supports_remote_desktop`, which additionally requires host-writable
    /// layout support.
    pub fn supports_terminal_stream(&self) -> bool {
        self.desktop_api_version == DESKTOP_API_VERSION
            && self.capabilities.iter().any(|c| c == TERMINAL_PTY)
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

/// Host→viewer text frames on a `terminals/<card-id>/attach` stream. Binary
/// frames carry raw terminal bytes and are never text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum AttachEvent {
    /// The host attached its tmux client. `columns`/`rows` are the host-owned
    /// grid the viewer's emulator must match; it is never the viewer's size.
    Attached {
        card_id: String,
        columns: u16,
        rows: u16,
    },
    /// The host grid differs from what the viewer applied, or changed since the
    /// last message. The viewer resizes its emulator to this authoritative grid.
    Grid { columns: u16, rows: u16 },
}

/// Viewer→host control frames (WebSocket text). Keystrokes travel as binary
/// frames instead: up to `ATTACH_MAX_CHUNK` raw terminal bytes per frame,
/// written into the host PTY behind the same input guard that arbitrates
/// local and phone input. Binary frames are never buffered for replay: a
/// busy guard or a slow PTY drops them. Unknown text fields and unknown frame
/// types are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum AttachCommand {
    /// The grid the viewer observed in its workspace snapshot. The host verifies
    /// it against its live grid and only applies a size that is already its own.
    Grid { columns: u16, rows: u16 },
}

impl AttachCommand {
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > 512 {
            return None;
        }
        let command: Self = serde_json::from_str(text).ok()?;
        match command {
            Self::Grid { columns, rows } => TerminalSize { columns, rows }.validate().ok()?,
        };
        Some(command)
    }
}

/// Stable failure codes shared by the attach route, its close frames and its
/// HTTP errors. Both sides map a received code through this list, so a peer can
/// never put free-form text (or a credential) in front of the user.
pub const ATTACH_REASONS: [&str; 11] = [
    "unknown_card",
    "terminal_exited",
    "terminal_grid_unknown",
    "terminal_unavailable",
    "invalid_card",
    "invalid_terminal_size",
    "attachment_limit",
    "desktop_unavailable",
    "desktop_not_ready",
    "update_remote_super_desktop",
    "reconnect",
];

/// Returns the code when it is one of ours, so callers can safely branch on it.
pub fn known_reason(value: &str) -> Option<&'static str> {
    ATTACH_REASONS.iter().copied().find(|code| *code == value)
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
        let mut caps = Capabilities {
            machine_id: "machine-b".into(),
            desktop_api_version: DESKTOP_API_VERSION,
            capabilities: vec![
                WORKSPACE_SNAPSHOT.into(),
                WORKSPACE_LAYOUT.into(),
                TERMINAL_PTY.into(),
                "future-optional-feature".into(),
            ],
        };
        assert!(caps.supports_remote_desktop());
        caps.capabilities.retain(|c| c != WORKSPACE_LAYOUT);
        assert!(!caps.supports_remote_desktop());
        // Live output needs the PTY transport, not host-writable layout.
        assert!(caps.supports_terminal_stream());
        caps.capabilities.retain(|c| c != TERMINAL_PTY);
        assert!(!caps.supports_terminal_stream());
        caps.desktop_api_version += 1;
        assert!(!caps.supports_terminal_stream());
    }

    #[test]
    fn this_host_advertises_live_output_but_not_remote_layout_writes() {
        let caps = Capabilities::current("machine-b".into());
        assert!(caps.supports_terminal_stream());
        assert!(!caps.supports_remote_desktop());
        assert!(caps.capabilities.contains(&WORKSPACE_SNAPSHOT.to_string()));
    }

    #[test]
    fn attach_frames_are_typed_and_bounded() {
        assert_eq!(
            serde_json::to_value(AttachEvent::Attached {
                card_id: "card-one".into(),
                columns: 120,
                rows: 40
            })
            .unwrap(),
            json!({"type":"attached","cardId":"card-one","columns":120,"rows":40})
        );
        let command = AttachCommand::parse(r#"{"type":"grid","columns":80,"rows":24}"#).unwrap();
        assert_eq!(
            command,
            AttachCommand::Grid {
                columns: 80,
                rows: 24
            }
        );
        // No text input frame exists (keystrokes travel as binary frames),
        // and malformed/oversized controls are refused.
        for bad in [
            r#"{"type":"write","data":"rm -rf /"}"#,
            r#"{"type":"input","data":"aGk="}"#,
            r#"{"type":"grid","columns":0,"rows":24}"#,
            r#"{"type":"grid","columns":80,"rows":24,"extra":1}"#,
            r#"{"type":"grid","columns":80}"#,
            "not json",
            &format!(r#"{{"type":"grid","columns":80,"rows":24,"pad":"{}"}}"#, "x".repeat(600)),
        ] {
            assert!(AttachCommand::parse(bad).is_none(), "{bad}");
        }
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
