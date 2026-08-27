//! Wire message shapes (§4.27). IDs cross the wire as their `Display`
//! strings (`task_01H...`), never bare ULIDs — the same convention
//! `valyria_types::id` uses internally, kept consistent at the protocol
//! boundary so a client never has to know about the prefix scheme to
//! round-trip an id it was handed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloRequest {
    pub client_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloResponse {
    pub protocol_version: String,
    pub runtime_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCreateRequest {
    pub objective: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCreateResponse {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskIdRequest {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStatusRequest {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStatusResponse {
    pub task_id: String,
    pub objective: String,
    pub state: String,
    pub paused_from: Option<String>,
    pub recovery_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionResolveRequest {
    pub task_id: String,
    pub approve: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsSubscribeRequest {
    pub since: u64,
}

/// Mirrors `valyria_types::CodedError` at the wire boundary — every
/// `ErrorCode`-implementing error in the runtime reduces to this shape
/// before crossing the protocol, matching §3's "errors that reach the
/// model [and the client] are converted through a redaction pass first"
/// convention (redaction itself is a later phase; the shape is stable now).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// A projected event (§43), independent of transport. `kind`/`payload`
/// mirror `valyria_events::EventEnvelope` — deliberately loose-typed
/// (`kind` as a string, `payload` as raw JSON) since new event kinds and
/// payload shapes are added by many crates over the life of the project,
/// and the wire schema shouldn't need a breaking change every time one is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireEvent {
    pub seq: u64,
    pub task_id: Option<String>,
    pub ts_ms: u128,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_event_serde_round_trip() {
        let event = WireEvent {
            seq: 1,
            task_id: Some("task_01H".into()),
            ts_ms: 1000,
            kind: "state_changed".into(),
            payload: serde_json::json!({"from": "IDLE", "to": "UNDERSTANDING"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: WireEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn permission_resolve_request_round_trip() {
        let req = PermissionResolveRequest {
            task_id: "task_01H".into(),
            approve: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: PermissionResolveRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }
}
