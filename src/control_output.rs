//! Discoverable output contracts for read-only terminal observations.
//! These describe local CLI output, independently of phone and peer protocols.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Envelope<T> {
    pub schema_version: u32,
    pub request_id: String,
    pub target: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::control::Error>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProcessStatus {
    Running,
    Exited,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    pub id: String,
    pub session_name: String,
    pub pane_id: String,
    /// Opaque identity: compare it, never derive or interpret it.
    pub pane_identity: String,
    pub pane_pid: u32,
    pub status: ProcessStatus,
    pub columns: u32,
    pub rows: u32,
    /// Terminal dimensions are cell counts, not card pixels.
    pub units: String,
    pub alternate_screen: bool,
    pub retained_history_lines: u64,
    pub observed_at_unix_ms: u64,
    /// A running process alone does not establish harness readiness.
    pub readiness: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Composer {
    #[serde(flatten)]
    pub runtime: Runtime,
    /// True only when a supported foreground harness has a recognized empty composer.
    pub ready: bool,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Truncation {
    pub bytes: bool,
    pub history: bool,
    pub retained: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub id: String,
    pub session_name: String,
    /// Plain screen/history text. In follow snapshots, read /data/text.
    /// Each snapshot replaces the previous screen; it is not a delta or append-only log.
    pub text: String,
    pub format: String,
    pub mode: String,
    pub runtime: Runtime,
    pub observed_at_unix_ms: u64,
    pub consistency: String,
    pub requested_history_lines: u32,
    pub returned_lines: usize,
    pub max_capture_bytes: usize,
    pub truncated: bool,
    pub truncation: Truncation,
    pub encoding_lossy: bool,
    pub history_scope: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EndReason {
    Duration,
    Limit,
}

/// One successful line of terminal follow's JSONL stream.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum FollowEvent {
    Snapshot {
        schema_version: u32,
        ok: bool,
        target: String,
        stream_id: String,
        /// Starts at one; unchanged snapshots are omitted.
        sequence: u64,
        /// Polling can miss output between observations.
        may_have_gaps: bool,
        data: Capture,
    },
    End {
        schema_version: u32,
        ok: bool,
        stream_id: String,
        sequence: u64,
        reason: EndReason,
    },
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub schema_version: u32,
    pub request_id: String,
    pub target: String,
    pub ok: bool,
    pub error: crate::control::Error,
}

/// Parse each JSONL line separately. An error line ends the stream unsuccessfully.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FollowLine {
    Event(FollowEvent),
    Error(Failure),
}

pub const COVERED: &[&str] = &[
    "terminal runtime",
    "terminal capture",
    "terminal composer",
    "terminal follow",
];

pub fn schema(command: &str) -> Option<Value> {
    let schema = match command {
        "terminal runtime" => schemars::schema_for!(Envelope<Runtime>),
        "terminal capture" => schemars::schema_for!(Envelope<Capture>),
        "terminal composer" => schemars::schema_for!(Envelope<Composer>),
        "terminal follow" => schemars::schema_for!(FollowLine),
        _ => return None,
    };
    let mut value = serde_json::to_value(schema).expect("output schema serializes");
    // Envelope success and error branches are mutually exclusive, unlike
    // composed launch's deliberately partial replies (not covered here).
    if command != "terminal follow" {
        value["oneOf"] = json!([
            {"properties":{"ok":{"const":true},"data":{"not":{"type":"null"}}},"required":["data"],"not":{"required":["error"]}},
            {"properties":{"ok":{"const":false},"error":{"not":{"type":"null"}}},"required":["error"],"not":{"required":["data"]}}
        ]);
    } else {
        // Each follow line is either a successful snapshot/end or a failed
        // standard envelope. Keep unknown future fields permitted.
        value["oneOf"] = json!([
            {"properties":{"ok":{"const":true}},"required":["type"],"not":{"required":["error"]}},
            {"properties":{"ok":{"const":false}},"required":["error"],"not":{"required":["type"]}}
        ]);
    }
    Some(value)
}

pub fn automation(command: &str) -> Option<Value> {
    (command == "terminal follow").then(|| {
        json!({
            "format":"jsonl", "discriminator":"/type", "snapshotType":"snapshot",
            "textPointer":"/data/text", "snapshotSemantics":"replace-screen",
            "endType":"end", "endReasonPointer":"/reason", "errorPointer":"/error",
            "duplicateScreensOmitted":true, "mayMissInterveningOutput":true,
            "completionSignal":false
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observation_schemas_expose_text_path_and_distinguish_stream_end_from_failure() {
        for command in COVERED {
            let schema = schema(command).unwrap();
            assert!(schema.get("$schema").is_some());
            assert!(schema["oneOf"].is_array());
        }
        let schema = schema("terminal follow").unwrap();
        assert!(schema["$defs"]["Capture"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("text")));
        assert_eq!(
            schema["$defs"]["Capture"]["properties"]["text"]["type"],
            "string"
        );
        assert_eq!(
            automation("terminal follow").unwrap()["textPointer"],
            "/data/text"
        );
        assert!(schema["$defs"]["EndReason"]
            .to_string()
            .contains("duration"));
        assert!(serde_json::from_value::<FollowLine>(json!({"type":"end","schemaVersion":1,"ok":true,"streamId":"example","sequence":2,"reason":"duration"})).is_ok());
        assert!(serde_json::from_value::<FollowLine>(json!({"type":"end","schemaVersion":1,"ok":true,"streamId":"example","sequence":2,"reason":"unexpected"})).is_err());
        assert!(serde_json::from_value::<FollowLine>(
            serde_json::to_value(crate::control::Reply::failure(
                "read",
                "not_found",
                "No card"
            ))
            .unwrap()
        )
        .is_ok());
    }
}
