//! Additive PC-to-PC protocol, independent of GTK and persisted AppState.
//!
//! Wire types are deliberately explicit: exporting AppState would leak local
//! preferences and eventually outgoing peer credentials. Unimplemented features
//! must not be advertised by the bridge.
#![allow(dead_code)] // Contracts consumed by the following implementation steps.

use serde::{Deserialize, Serialize};

pub const DESKTOP_API_VERSION: u32 = 1;
pub const WORKSPACE_SNAPSHOT: &str = "workspace-snapshot-v1";
pub const WORKSPACE_LAYOUT: &str = "workspace-layout-v1";
pub const TERMINAL_PTY: &str = "terminal-pty-v1";
/// Live workspace events: `GET /api/v1/desktop/events` pushes a snapshot on
/// connect and then one per published change, with sequenced heartbeats and an
/// explicit `resync`. A viewer polls only hosts that do not advertise it.
pub const WORKSPACE_EVENTS: &str = "workspace-events-v1";

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

/// Workspace event subscriptions one credential may hold at once, and the
/// bridge-wide total. A viewer holds one per selected host; the slack covers a
/// reconnect racing the host noticing the previous socket is gone.
pub const MAX_EVENT_SUBSCRIPTIONS: usize = 4;
pub const MAX_EVENT_SUBSCRIPTIONS_TOTAL: usize = 16;
/// Events a host queues for one subscriber before it stops queueing and asks
/// that viewer to fetch a snapshot instead (`resync`).
pub const EVENT_QUEUE: usize = 4;
/// A heartbeat is sent after this much silence, and a viewer that hears nothing
/// for three of them treats the subscription as lost.
pub const EVENT_HEARTBEAT_SECS: u64 = 5;
/// Longest event message: a 1 MiB owner snapshot plus the bridge's envelope.
pub const EVENT_MAX_MESSAGE: usize = 2 * 1024 * 1024;

/// Largest coordinate or size a viewer may name in a layout command. The owner
/// clamps to its own screen afterwards; these bounds only stop absurd or
/// hostile numbers before any daemon work happens.
pub const LAYOUT_MAX_COORD: i32 = 32768;
pub const LAYOUT_MAX_SIZE: u32 = 32768;
/// Mirrors `tag::TAG_COUNT`: the host normalizes anything else to "no tag".
pub const LAYOUT_MAX_TAG: u8 = 8;
/// Longest accepted card identity, request identity and daemon epoch.
pub const MAX_CARD_ID: usize = 128;
pub const MAX_REQUEST_ID: usize = 64;
pub const MAX_EPOCH: usize = 64;
/// Longest workspace string a viewer may name in a command.
pub const MAX_WORKSPACE: usize = 4096;
/// How many folders a host offers a viewer to start a harness in. The effective
/// folder comes first, then the folders that host has used before.
pub const MAX_FOLDERS: usize = 16;

/// Card ids come from the host's own snapshot, never from a tmux target the
/// caller supplied, so this is the only shape any route accepts.
pub fn valid_card_id(card_id: &str) -> bool {
    !card_id.is_empty()
        && card_id.len() <= MAX_CARD_ID
        && card_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Request ids are opaque to the host but must survive logging and the owner's
/// bounded deduplication cache.
pub fn valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty()
        && request_id.len() <= MAX_REQUEST_ID
        && request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

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
            // `workspace-layout-v1` is advertised because `POST
            // /api/v1/desktop/commands` has a handler for every command
            // variant: layout, expand, close, create and default folder.
            capabilities: vec![
                WORKSPACE_SNAPSHOT.into(),
                WORKSPACE_LAYOUT.into(),
                TERMINAL_PTY.into(),
                WORKSPACE_EVENTS.into(),
            ],
        }
    }

    /// Live workspace events. Without it a viewer keeps the two-second
    /// snapshot poll, which every host with `workspace-snapshot-v1` serves.
    pub fn supports_workspace_events(&self) -> bool {
        self.desktop_api_version == DESKTOP_API_VERSION
            && self.capabilities.iter().any(|c| c == WORKSPACE_EVENTS)
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
pub const ATTACH_REASONS: [&str; 12] = [
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
    // The event route's budget refusal; it shares this list so a viewer maps
    // it without trusting free-form text.
    "subscription_limit",
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
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
    /// The folder this host would start a new harness in.
    pub workspace: String,
    /// Every folder this host is willing to start a harness in: its own
    /// effective folder first, then the ones it has used before. A viewer picks
    /// from this list; it can never name a path of its own. A host that
    /// publishes none offers exactly `workspace` (see [`offers_folder`]), which
    /// is what a viewer could launch into before this list existed.
    #[serde(default)]
    pub folders: Vec<String>,
    pub home_directory: String,
    pub visible_harnesses: Vec<String>,
    pub harness_types: Vec<HarnessType>,
    pub cards: Vec<DesktopCard>,
}

/// One message on the workspace event stream.
///
/// Every message carries `sequence`, which starts at 1 on each connection and
/// grows by exactly one per message, so a viewer can tell a lost message from
/// a quiet host. Snapshots are complete: there is no delta to apply, and
/// recovery from a gap, an epoch change or `resync` is one snapshot fetch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum WorkspaceEvent {
    /// The complete workspace, on connect and after every published change.
    Snapshot {
        #[serde(default)]
        sequence: u64,
        workspace: WorkspaceSnapshot,
    },
    /// The owner is not answering; snapshots resume when it does.
    Unavailable {
        #[serde(default)]
        sequence: u64,
        error: String,
    },
    /// Nothing changed. Names the host's current epoch and revision, so a
    /// viewer that missed a change learns it without waiting for the next one.
    Heartbeat {
        sequence: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epoch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<u64>,
    },
    /// The host dropped this subscriber's queued events (it was not reading
    /// fast enough). Fetch a snapshot; later events follow on this stream.
    Resync { sequence: u64, reason: String },
}

impl WorkspaceEvent {
    pub fn sequence(&self) -> u64 {
        match self {
            Self::Snapshot { sequence, .. }
            | Self::Unavailable { sequence, .. }
            | Self::Heartbeat { sequence, .. }
            | Self::Resync { sequence, .. } => *sequence,
        }
    }
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
    /// Expand or collapse one card. This is presentation, not saved geometry:
    /// the owner exports `expanded` with the card, so a viewer mirrors whatever
    /// it decides.
    SetExpanded {
        card_id: String,
        expected_revision: u64,
        expanded: bool,
    },
    SetWorkspace {
        workspace: String,
        expected_revision: u64,
    },
}

impl WorkspaceCommand {
    /// The card this command addresses, if it addresses one at all. Commands
    /// without a card are checked against the workspace revision instead.
    pub fn card_id(&self) -> Option<&str> {
        match self {
            Self::CreateTerminal { .. } | Self::SetWorkspace { .. } => None,
            Self::CloseTerminal { card_id, .. }
            | Self::SetLayout { card_id, .. }
            | Self::SetExpanded { card_id, .. } => Some(card_id),
        }
    }

    /// The revision the viewer believed when it built this command. Zero means
    /// it sent no expectation, which the owner refuses: a mutation must never
    /// be applied on top of an unknown state.
    pub fn expected_revision(&self) -> u64 {
        match self {
            Self::CreateTerminal { .. } => 0,
            Self::CloseTerminal {
                expected_revision, ..
            }
            | Self::SetLayout {
                expected_revision, ..
            }
            | Self::SetExpanded {
                expected_revision, ..
            }
            | Self::SetWorkspace {
                expected_revision, ..
            } => *expected_revision,
        }
    }

    /// Identity shape and bounds, checked independently of the owner's current
    /// state. The owner clamps geometry to its own screen afterwards.
    pub fn bounds_ok(&self) -> bool {
        match self {
            Self::CreateTerminal {
                agent_type,
                workspace,
            } => {
                !agent_type.is_empty()
                    && agent_type.len() <= 64
                    && agent_type.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !workspace.is_empty()
                    && workspace.len() <= MAX_WORKSPACE
                    && !workspace.contains('\0')
            }
            Self::CloseTerminal { card_id, .. } | Self::SetExpanded { card_id, .. } => {
                valid_card_id(card_id)
            }
            Self::SetLayout { card_id, layout, .. } => {
                let bounded = |value: i32| (-LAYOUT_MAX_COORD..=LAYOUT_MAX_COORD).contains(&value);
                valid_card_id(card_id)
                    && bounded(layout.x)
                    && bounded(layout.y)
                    && layout.width.clamp(1, LAYOUT_MAX_SIZE) == layout.width
                    && layout.height.clamp(1, LAYOUT_MAX_SIZE) == layout.height
                    && layout.restored_width <= LAYOUT_MAX_SIZE
                    && layout.restored_height <= LAYOUT_MAX_SIZE
                    && layout.icon_x.is_none_or(bounded)
                    && layout.icon_y.is_none_or(bounded)
                    && layout.tag <= LAYOUT_MAX_TAG
            }
            Self::SetWorkspace {
                workspace,
                expected_revision,
            } => {
                !workspace.is_empty()
                    && workspace.len() <= MAX_WORKSPACE
                    && !workspace.contains('\0')
                    && *expected_revision > 0
            }
        }
    }

    /// The stable code a malformed instance of this variant deserves.
    fn invalid_code(&self) -> &'static str {
        match self {
            Self::SetLayout { .. } => "invalid_layout",
            _ => "invalid_command",
        }
    }

    /// The refusal code for a command that can never be applied, or `None` when
    /// its shape and bounds are acceptable. Both the bridge and the owner check
    /// this, so a malformed command is refused with the same code before any
    /// work happens on either side.
    pub fn shape_error(&self) -> Option<&'static str> {
        // A card mutation never applies on top of an unknown state: the viewer
        // has to name the revision it based the command on.
        if self.card_id().is_some() && self.expected_revision() == 0 {
            return Some("invalid_command");
        }
        if !self.bounds_ok() {
            return Some(self.invalid_code());
        }
        None
    }
}

/// Stable refusal codes for the command route, shared with viewers the same way
/// attach reasons are: a peer never puts free-form text in front of the user.
/// The list covers what the owner refuses and what the bridge answers when the
/// owner could not be reached at all.
pub const COMMAND_ERRORS: [&str; 16] = [
    "unknown_card",
    "terminal_expanded",
    "epoch_changed",
    "conflict",
    "invalid_layout",
    "invalid_command",
    "unsupported_command",
    "unsupported_harness",
    "invalid_workspace",
    "wrong_machine",
    "unknown_outcome",
    "desktop_unavailable",
    "desktop_not_ready",
    "desktop_timeout",
    "invalid_desktop_response",
    "too_many_desktop_cards",
];

/// Returns the code when it is one of ours, so callers can safely branch on it.
pub fn known_command_error(value: &str) -> Option<&'static str> {
    COMMAND_ERRORS.iter().copied().find(|code| *code == value)
}

/// The HTTP status for one stable command code, for answers that carry no
/// workspace snapshot of their own.
pub fn command_status(error: &str) -> (u16, &'static str) {
    match error {
        "unknown_card" => (404, "Not Found"),
        "conflict" | "terminal_expanded" | "epoch_changed" | "unknown_outcome" | "wrong_machine"
        | "too_many_desktop_cards" => (409, "Conflict"),
        "desktop_unavailable" | "desktop_not_ready" => (503, "Service Unavailable"),
        "desktop_timeout" => (504, "Gateway Timeout"),
        "invalid_layout" | "invalid_command" | "unsupported_command"
        | "unsupported_harness" | "invalid_workspace" => (400, "Bad Request"),
        _ => (502, "Bad Gateway"),
    }
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
    /// Accepted. `card_revision`/`layout` are the owner's published state right
    /// after it, so the viewer can adopt them without another round trip. A
    /// closed card is simply gone, leaving both fields empty.
    Applied {
        card_id: Option<String>,
        card_revision: Option<u64>,
        layout: Option<CardLayout>,
        /// The owner's own `expanded`, which is presentation and so is not part
        /// of the saved layout: a viewer mirrors it from here.
        expanded: Option<bool>,
    },
    /// Refused because the addressed card changed since the viewer's snapshot.
    /// The owner's current geometry comes along, so the viewer redraws the real
    /// state instead of overwriting a concurrent edit.
    Conflict {
        card_id: String,
        card_revision: Option<u64>,
        layout: Option<CardLayout>,
        expanded: Option<bool>,
    },
    /// Refused for any other stable reason; `error` is one of
    /// [`COMMAND_ERRORS`].
    Rejected { error: String },
}

/// The owner's typed answer to one command. The bridge binds it to its own
/// machine id and maps it onto the viewer's [`CommandReply`], so the daemon
/// never has to know the bridge's identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandOutcome {
    pub ok: bool,
    pub epoch: String,
    pub revision: u64,
    pub card_id: Option<String>,
    pub card_revision: Option<u64>,
    pub layout: Option<CardLayout>,
    pub expanded: Option<bool>,
    pub error: Option<String>,
}

impl CommandOutcome {
    /// Accepted: the addressed card's published state after the mutation.
    pub fn applied(snapshot: &LocalWorkspaceSnapshot, card_id: Option<String>) -> Self {
        let card = card_id
            .as_deref()
            .and_then(|id| snapshot.cards.iter().find(|card| card.card_id == id));
        Self {
            ok: true,
            epoch: snapshot.epoch.clone(),
            revision: snapshot.revision,
            card_id,
            card_revision: card.map(|card| card.revision),
            layout: card.map(|card| card.layout.clone()),
            expanded: card.map(|card| card.expanded),
            error: None,
        }
    }

    /// Refused because the card moved on. Carries the owner's own geometry.
    pub fn conflicted(snapshot: &LocalWorkspaceSnapshot, card: &DesktopCard) -> Self {
        Self {
            ok: false,
            epoch: snapshot.epoch.clone(),
            revision: snapshot.revision,
            card_id: Some(card.card_id.clone()),
            card_revision: Some(card.revision),
            layout: Some(card.layout.clone()),
            expanded: Some(card.expanded),
            error: Some("conflict".into()),
        }
    }

    /// Refused before anything was applied.
    pub fn rejected(snapshot: &LocalWorkspaceSnapshot, error: &str) -> Self {
        Self {
            ok: false,
            epoch: snapshot.epoch.clone(),
            revision: snapshot.revision,
            card_id: None,
            card_revision: None,
            layout: None,
            expanded: None,
            error: Some(
                known_command_error(error)
                    .unwrap_or("invalid_command")
                    .to_string(),
            ),
        }
    }

    /// The HTTP status a viewer sees for this answer.
    pub fn status(&self) -> (u16, &'static str) {
        if self.ok {
            return (200, "OK");
        }
        command_status(self.error.as_deref().unwrap_or("invalid_command"))
    }

    /// The viewer-facing reply, carrying the bridge's own machine id.
    pub fn into_reply(self, machine_id: &str, request_id: &str) -> CommandReply {
        let result = if self.ok {
            CommandResult::Applied {
                card_id: self.card_id,
                card_revision: self.card_revision,
                layout: self.layout,
                expanded: self.expanded,
            }
        } else {
            match self.error.as_deref() {
                Some("conflict") => CommandResult::Conflict {
                    card_id: self.card_id.unwrap_or_default(),
                    card_revision: self.card_revision,
                    layout: self.layout,
                    expanded: self.expanded,
                },
                other => CommandResult::Rejected {
                    error: known_command_error(other.unwrap_or("invalid_command"))
                        .unwrap_or("invalid_command")
                        .to_string(),
                },
            }
        };
        CommandReply {
            request_id: request_id.to_string(),
            machine_id: machine_id.to_string(),
            epoch: self.epoch,
            revision: self.revision,
            result,
        }
    }
}

/// Whether the owner itself offers this folder. Everything a viewer may name
/// comes from the snapshot's own list, so a command can never introduce a path
/// the host did not publish.
pub fn offers_folder(snapshot: &LocalWorkspaceSnapshot, workspace: &str) -> bool {
    if snapshot.folders.is_empty() {
        return workspace == snapshot.workspace;
    }
    snapshot.folders.iter().any(|folder| folder == workspace)
}

/// Validate one command against the owner's published workspace, before any
/// mutation. Refusing here is what keeps a stale viewer from overwriting a
/// concurrent local edit, and what keeps a create/close from being replayed
/// onto a restarted daemon's different epoch.
///
/// The refusal is deliberately a value, not an error code: a conflict has to
/// carry the owner's current revision and geometry to the viewer.
#[allow(clippy::result_large_err)]
pub fn check_command(
    snapshot: &LocalWorkspaceSnapshot,
    request: &CommandRequest,
) -> Result<(), CommandOutcome> {
    if !valid_request_id(&request.request_id)
        || request.machine_id.is_empty()
        || request.machine_id.len() > MAX_CARD_ID
        || request.expected_epoch.is_empty()
        || request.expected_epoch.len() > MAX_EPOCH
    {
        return Err(CommandOutcome::rejected(snapshot, "invalid_command"));
    }
    if request.expected_epoch != snapshot.epoch {
        return Err(CommandOutcome::rejected(snapshot, "epoch_changed"));
    }
    if let Some(error) = request.command.shape_error() {
        return Err(CommandOutcome::rejected(snapshot, error));
    }
    if let WorkspaceCommand::CreateTerminal {
        agent_type,
        workspace,
    } = &request.command
    {
        // A viewer may only launch a harness this host offers, in the folder
        // this host published. It never names a command, a flag or a path of
        // its own: the inventory and the folder come from the snapshot.
        if !snapshot
            .visible_harnesses
            .iter()
            .any(|key| key == agent_type)
        {
            return Err(CommandOutcome::rejected(snapshot, "unsupported_harness"));
        }
        if !offers_folder(snapshot, workspace) {
            return Err(CommandOutcome::rejected(snapshot, "invalid_workspace"));
        }
        return Ok(());
    }
    if let WorkspaceCommand::SetWorkspace {
        workspace,
        expected_revision,
    } = &request.command
    {
        // A folder change is judged against the workspace revision, not a
        // card's: it is one field of the workspace, and two viewers changing it
        // at once must not silently overwrite each other.
        if *expected_revision != snapshot.revision {
            return Err(CommandOutcome::rejected(snapshot, "conflict"));
        }
        if !offers_folder(snapshot, workspace) {
            return Err(CommandOutcome::rejected(snapshot, "invalid_workspace"));
        }
        return Ok(());
    }
    let Some(card_id) = request.command.card_id() else {
        return Err(CommandOutcome::rejected(snapshot, "unsupported_command"));
    };
    let card = snapshot
        .cards
        .iter()
        .find(|card| card.card_id == card_id)
        .ok_or_else(|| CommandOutcome::rejected(snapshot, "unknown_card"))?;
    // An expanded card's rectangle is transient and must not be overwritten, so
    // layout commands are refused. Closing writes no geometry and stays
    // allowed: a close wins over a layout gesture.
    if card.expanded && matches!(&request.command, WorkspaceCommand::SetLayout { .. }) {
        return Err(CommandOutcome::rejected(snapshot, "terminal_expanded"));
    }
    let expected = request.command.expected_revision();
    if expected == 0 || expected != card.revision {
        return Err(CommandOutcome::conflicted(snapshot, card));
    }
    Ok(())
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
    fn this_host_advertises_live_output_and_layout_commands() {
        let caps = Capabilities::current("machine-b".into());
        assert!(caps.supports_terminal_stream());
        // The command route accepts layout and close commands, so the
        // capability is no longer a promise the host cannot keep.
        assert!(caps.supports_remote_desktop());
        assert!(caps.capabilities.contains(&WORKSPACE_SNAPSHOT.to_string()));
        // Live workspace events replace the viewer's poll on this host.
        assert!(caps.supports_workspace_events());
        let mut older = caps.clone();
        older.capabilities.retain(|c| c != WORKSPACE_EVENTS);
        assert!(!older.supports_workspace_events());
    }

    #[test]
    fn workspace_events_are_sequenced_on_the_wire() {
        assert_eq!(
            serde_json::to_value(WorkspaceEvent::Heartbeat {
                sequence: 4,
                epoch: Some("e".into()),
                revision: Some(9)
            })
            .unwrap(),
            json!({"type":"heartbeat","sequence":4,"epoch":"e","revision":9})
        );
        // An unavailable host's heartbeat names no revision.
        assert_eq!(
            serde_json::to_value(WorkspaceEvent::Heartbeat { sequence: 5, epoch: None, revision: None })
                .unwrap(),
            json!({"type":"heartbeat","sequence":5})
        );
        assert_eq!(
            serde_json::to_value(WorkspaceEvent::Resync { sequence: 6, reason: "backpressure".into() })
                .unwrap(),
            json!({"type":"resync","sequence":6,"reason":"backpressure"})
        );
        let unavailable: WorkspaceEvent =
            serde_json::from_value(json!({"type":"unavailable","sequence":2,"error":"desktop_unavailable"}))
                .unwrap();
        assert_eq!(unavailable.sequence(), 2);
        // A subscription-limit refusal is one of the shared stream codes.
        assert_eq!(known_reason("subscription_limit"), Some("subscription_limit"));
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

    /// A move-only drop still carries the whole layout, because the revision it
    /// is checked against guards every field at once.
    fn layout_request(revision: u64, layout: CardLayout) -> CommandRequest {
        CommandRequest {
            request_id: "r1".into(),
            machine_id: "b".into(),
            expected_epoch: "host-one".into(),
            command: WorkspaceCommand::SetLayout {
                card_id: "card-one".into(),
                expected_revision: revision,
                layout,
            },
        }
    }

    fn host_layout() -> CardLayout {
        crate::remote_workspace::fixture().local.cards[0].layout.clone()
    }

    fn set_layout_request(revision: u64) -> CommandRequest {
        layout_request(revision, host_layout())
    }

    #[test]
    fn commands_are_checked_against_epoch_revision_and_owner_state() {
        let snapshot = crate::remote_workspace::fixture().local;
        check_command(&snapshot, &set_layout_request(1)).unwrap();

        // A stale viewer is refused with the owner's own geometry, never
        // silently allowed to overwrite a concurrent edit.
        let stale = check_command(&snapshot, &set_layout_request(9)).err().unwrap();
        assert!(!stale.ok);
        assert_eq!(stale.error.as_deref(), Some("conflict"));
        assert_eq!(stale.card_id.as_deref(), Some("card-one"));
        assert_eq!(stale.card_revision, Some(1));
        assert_eq!(stale.layout.clone().unwrap().x, 100);
        assert_eq!(stale.status(), (409, "Conflict"));

        // A revision of zero is not "no expectation", it is a malformed command.
        assert_eq!(
            check_command(&snapshot, &set_layout_request(0))
                .err()
                .unwrap()
                .error
                .as_deref(),
            Some("invalid_command")
        );

        let mut other_epoch = set_layout_request(1);
        other_epoch.expected_epoch = "host-two".into();
        assert_eq!(
            check_command(&snapshot, &other_epoch).err().unwrap().error.as_deref(),
            Some("epoch_changed")
        );

        let mut unknown = set_layout_request(1);
        unknown.command = WorkspaceCommand::CloseTerminal {
            card_id: "card-two".into(),
            expected_revision: 1,
        };
        let refusal = check_command(&snapshot, &unknown).err().unwrap();
        assert_eq!(refusal.error.as_deref(), Some("unknown_card"));
        assert_eq!(refusal.status(), (404, "Not Found"));

        // A create may only name a harness this host offers, in the folder this
        // host published: the viewer never sends a command, a flag or a path.
        let mut create = set_layout_request(1);
        create.command = WorkspaceCommand::CreateTerminal {
            agent_type: "shell".into(),
            workspace: "/project".into(),
        };
        check_command(&snapshot, &create).unwrap();
        create.command = WorkspaceCommand::CreateTerminal {
            agent_type: "aider".into(),
            workspace: "/project".into(),
        };
        let refusal = check_command(&snapshot, &create).err().unwrap();
        assert_eq!(refusal.error.as_deref(), Some("unsupported_harness"));
        assert_eq!(refusal.status(), (400, "Bad Request"));
        create.command = WorkspaceCommand::CreateTerminal {
            agent_type: "shell".into(),
            workspace: "/etc".into(),
        };
        assert_eq!(
            check_command(&snapshot, &create).err().unwrap().error.as_deref(),
            Some("invalid_workspace")
        );

        // The default-folder command is the one variant still unimplemented, and
        // it is refused with its own code instead of half-applying.
        // A folder change names a folder the host offers and the workspace
        // revision it saw: it is one field of the workspace, so it is judged
        // against the workspace's own revision rather than a card's.
        let mut folder = set_layout_request(1);
        folder.command = WorkspaceCommand::SetWorkspace {
            workspace: "/work/notes".into(),
            expected_revision: snapshot.revision,
        };
        check_command(&snapshot, &folder).unwrap();
        folder.command = WorkspaceCommand::SetWorkspace {
            workspace: "/etc".into(),
            expected_revision: snapshot.revision,
        };
        assert_eq!(
            check_command(&snapshot, &folder).err().unwrap().error.as_deref(),
            Some("invalid_workspace")
        );
        folder.command = WorkspaceCommand::SetWorkspace {
            workspace: "/work/notes".into(),
            expected_revision: snapshot.revision + 5,
        };
        let stale = check_command(&snapshot, &folder).err().unwrap();
        assert_eq!(stale.error.as_deref(), Some("conflict"));
        assert_eq!(stale.status(), (409, "Conflict"));
        // A create may name any folder the host offers, not only the current
        // one, so picking a folder and launching straight away cannot race.
        let mut create_elsewhere = set_layout_request(1);
        create_elsewhere.command = WorkspaceCommand::CreateTerminal {
            agent_type: "shell".into(),
            workspace: "/work/notes".into(),
        };
        check_command(&snapshot, &create_elsewhere).unwrap();

        // A host that publishes no folder list offers exactly the folder it
        // reports, which is what a viewer could launch into before the list
        // existed.
        let mut older = crate::remote_workspace::fixture().local;
        older.folders.clear();
        assert!(offers_folder(&older, "/project"));
        assert!(!offers_folder(&older, "/work/notes"));

        // An expanded card refuses layout commands but still accepts a close:
        // its transient rectangle must not be written, yet a close wins.
        let mut expanded = crate::remote_workspace::fixture().local;
        expanded.cards[0].expanded = true;
        assert_eq!(
            check_command(&expanded, &set_layout_request(1))
                .err()
                .unwrap()
                .error
                .as_deref(),
            Some("terminal_expanded")
        );
        let mut close = set_layout_request(1);
        close.command = WorkspaceCommand::CloseTerminal {
            card_id: "card-one".into(),
            expected_revision: 1,
        };
        check_command(&expanded, &close).unwrap();

        // Expanding and collapsing are allowed on an expanded card (collapsing
        // is how it stops being expanded), unlike a layout write.
        let mut toggle = set_layout_request(1);
        toggle.command = WorkspaceCommand::SetExpanded {
            card_id: "card-one".into(),
            expected_revision: 1,
            expanded: false,
        };
        check_command(&expanded, &toggle).unwrap();
        check_command(&snapshot, &toggle).unwrap();
        toggle.command = WorkspaceCommand::SetExpanded {
            card_id: "card-one".into(),
            expected_revision: 9,
            expanded: true,
        };
        assert_eq!(
            check_command(&snapshot, &toggle).err().unwrap().error.as_deref(),
            Some("conflict")
        );
    }

    #[test]
    fn command_bounds_reject_hostile_geometry_before_the_owner_sees_it() {
        let mut layout = host_layout();
        let bounds = |layout: CardLayout| layout_request(1, layout).command.bounds_ok();
        assert!(bounds(layout.clone()));
        layout.width = 0;
        assert!(!bounds(layout.clone()));
        layout.width = LAYOUT_MAX_SIZE + 1;
        assert!(!bounds(layout.clone()));
        layout.width = 640;
        layout.y = i32::MIN;
        assert!(!bounds(layout.clone()));
        layout.y = 200;
        layout.tag = LAYOUT_MAX_TAG + 1;
        assert!(!bounds(layout.clone()));
        layout.tag = 3;
        layout.icon_x = Some(LAYOUT_MAX_COORD + 1);
        assert!(!bounds(layout.clone()));
        layout.icon_x = None;
        assert!(bounds(layout.clone()));

        // Request ids reach a log and a dedup cache, so their shape is bounded.
        for bad in ["", "with space", "with/slash", &"x".repeat(MAX_REQUEST_ID + 1)] {
            let mut request = set_layout_request(1);
            request.request_id = bad.into();
            assert!(!valid_request_id(bad), "{bad}");
            assert!(
                check_command(&crate::remote_workspace::fixture().local, &request).is_err(),
                "{bad}"
            );
        }
        assert!(valid_request_id("a-b_9"));
        // `tag::TAG_COUNT` is the palette's real size; this constant mirrors it.
        assert_eq!(LAYOUT_MAX_TAG, crate::tag::TAG_COUNT);
    }

    #[test]
    fn an_outcome_is_typed_on_the_wire_and_reports_the_published_revision() {
        let mut snapshot = crate::remote_workspace::fixture().local;
        snapshot.revision = 7;
        snapshot.cards[0].revision = 7;
        snapshot.cards[0].layout.x = 700;
        let applied = CommandOutcome::applied(&snapshot, Some("card-one".into()));
        assert_eq!(applied.status(), (200, "OK"));
        let reply = applied.into_reply("machine-b", "r1");
        assert_eq!(
            serde_json::to_value(&reply).unwrap(),
            json!({
                "requestId": "r1", "machineId": "machine-b",
                "epoch": "host-one", "revision": 7,
                "result": {"type": "applied", "cardId": "card-one", "cardRevision": 7,
                           "expanded": false,
                           "layout": serde_json::to_value(&snapshot.cards[0].layout).unwrap()}
            })
        );

        // A close removes the card, so it has no revision to report.
        let mut closed = snapshot.clone();
        closed.cards.clear();
        closed.revision = 8;
        let applied = CommandOutcome::applied(&closed, Some("card-one".into()));
        assert_eq!(
            applied.into_reply("machine-b", "r2").result,
            CommandResult::Applied {
                card_id: Some("card-one".into()),
                card_revision: None,
                layout: None,
                expanded: None,
            }
        );

        let refusal = CommandOutcome::rejected(&snapshot, "conflict");
        assert_eq!(refusal.status(), (409, "Conflict"));
        // Free-form text from a peer can never reach the user.
        let refusal = CommandOutcome::rejected(&snapshot, "rm -rf /");
        assert_eq!(refusal.error.as_deref(), Some("invalid_command"));
        assert_eq!(refusal.status(), (400, "Bad Request"));
        assert_eq!(
            refusal.into_reply("machine-b", "r3").result,
            CommandResult::Rejected {
                error: "invalid_command".into()
            }
        );
        assert!(known_command_error("unknown_outcome").is_some());
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
