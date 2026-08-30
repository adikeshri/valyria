//! Wire message shapes (§4.27). IDs cross the wire as their `Display`
//! strings (`task_01H...`), never bare ULIDs — the same convention
//! `valyria_types::id` uses internally, kept consistent at the protocol
//! boundary so a client never has to know about the prefix scheme to
//! round-trip an id it was handed.
//!
//! Every type here derives [`schemars::JsonSchema`] so `xtask schema` can
//! export the wire contract and the CI compat gate can fail a breaking
//! change that did not also bump [`crate::PROTOCOL_VERSION`] (§4.27, D11).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HelloRequest {
    pub client_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HelloResponse {
    pub protocol_version: String,
    pub runtime_version: String,
    /// What this runtime build can do — a client negotiates against this
    /// rather than the version string. Names are stable identifiers
    /// (`plan`, `daemon`, `doctor`, …); unknown names are ignored.
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskCreateRequest {
    pub objective: String,
    /// Optional per-task autonomy override (§25). One of `manual` |
    /// `assisted` | `autonomous`. When absent, the task inherits the
    /// daemon's start-time mode. Additive as of protocol 1.1.0 — an older
    /// client omits it and gets the daemon default, unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskCreateResponse {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskIdRequest {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskStatusRequest {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskStatusResponse {
    pub task_id: String,
    pub objective: String,
    pub state: String,
    pub paused_from: Option<String>,
    pub recovery_note: Option<String>,
}

/// An empty request payload. Kept as a named unit struct (rather than
/// `()`) so it has a stable schema name and a place to grow fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Empty {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskSummary {
    pub task_id: String,
    pub objective: String,
    pub state: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskListResponse {
    pub tasks: Vec<TaskSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VerifiedClaimWire {
    pub kind: String,
    pub command: String,
    pub outcome: String,
    pub run_id: String,
}

/// Mirrors `valyria_verify::CompletionReport` (§15, D4) — assembled only
/// from persisted verification runs, so an unbacked "tests pass" shows up
/// in `unverified`, never `verified`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskReportResponse {
    pub task_id: String,
    pub status: String,
    pub verified: Vec<VerifiedClaimWire>,
    pub unverified: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskRollbackRequest {
    pub task_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskRollbackResponse {
    pub reverted_entries: u64,
    pub restored_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanStepSummary {
    pub id: String,
    pub intent: String,
    pub targets: Vec<String>,
    pub depends_on: Vec<String>,
    pub rollback_boundary: bool,
    pub checkpoint: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanGetResponse {
    /// `None` when the task ran as the pass-through (no model-authored
    /// plan) — not an error.
    pub revision: Option<u32>,
    pub content_hash: Option<String>,
    pub steps: Vec<PlanStepSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionResolveRequest {
    pub task_id: String,
    pub approve: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventsSubscribeRequest {
    pub since: u64,
}

// --- doctor (§4.28) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorCheckWire {
    pub name: String,
    /// `pass` | `warn` | `fail`.
    pub status: String,
    pub detail: String,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorRunResponse {
    pub checks: Vec<DoctorCheckWire>,
    /// The worst status across all checks: `pass` | `warn` | `fail`.
    pub summary: String,
}

// --- storage inspection / clean (§4.1, §48) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StorageEntryWire {
    /// e.g. `workspace.db`, `blobs`, `index`, `tasks`, `logs`, `models`.
    pub name: String,
    pub bytes: u64,
    pub detail: Option<String>,
    /// Whether `storage.purge` can reclaim this entry.
    pub purgeable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StorageInspectResponse {
    pub entries: Vec<StorageEntryWire>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StoragePurgeRequest {
    /// `memory` | `cache` | `tasks` | `logs`.
    pub scope: String,
    /// When true, report what *would* be freed without deleting anything.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PurgeResponse {
    pub freed_bytes: u64,
    pub items_removed: u64,
    pub dry_run: bool,
}

// --- config (§4.3) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigEntryWire {
    pub key: String,
    pub value: String,
    /// Where the effective value came from: `default` | `global` |
    /// `workspace` | `env` | `task`.
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigShowResponse {
    pub entries: Vec<ConfigEntryWire>,
}

/// Write one dotted config leaf to a Core-owned config file, then report
/// the re-resolved effective view (§24). Additive as of protocol 1.1.0.
///
/// The write is validated against the policy floor
/// (`valyria_config::validate_floor`) *before* it touches disk: a value
/// that would loosen access past the compiled-in ceiling is rejected with
/// `config.policy_floor_violation` and nothing is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigSetRequest {
    /// Dotted key, e.g. `permission.mode`, `log.format`,
    /// `network.internet`. Must be a key `config_show` already reports.
    pub key: String,
    /// The new value, as the string form `config_show` would display.
    pub value: String,
    /// `workspace` writes `<repo>/.valyria/config.toml`; `user` writes
    /// `~/.valyria/config.toml`. Anything else is `config.invalid_scope`.
    pub scope: String,
}

// --- memory (§4.19, §32) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryListRequest {
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryEntryWire {
    pub id: String,
    pub kind: String,
    pub scope: String,
    pub author: String,
    pub text: String,
    pub effective_confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryListResponse {
    pub entries: Vec<MemoryEntryWire>,
}

// --- models (§4.21) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelSummaryWire {
    pub id: String,
    pub family: String,
    pub quantization: String,
    pub size_bytes: u64,
    pub installed: bool,
    pub license: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelListResponse {
    pub models: Vec<ModelSummaryWire>,
}

// --- workspace ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceStatusResponse {
    pub workspace_id: String,
    pub root: String,
    pub data_dir: String,
    pub index_generation: Option<u64>,
    pub active_tasks: u32,
    pub total_tasks: u32,
}

/// Mirrors `valyria_types::CodedError` at the wire boundary — every
/// `ErrorCode`-implementing error in the runtime reduces to this shape
/// before crossing the protocol, matching §3's "errors that reach the
/// model [and the client] are converted through a redaction pass first"
/// convention (redaction itself is a later phase; the shape is stable now).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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

    #[test]
    fn storage_purge_request_dry_run_defaults_false() {
        let req: StoragePurgeRequest = serde_json::from_str(r#"{"scope":"cache"}"#).unwrap();
        assert!(!req.dry_run);
    }

    #[test]
    fn task_create_request_permission_mode_defaults_none_and_is_omitted() {
        // An older client that sends only `objective` still parses.
        let req: TaskCreateRequest =
            serde_json::from_str(r#"{"objective":"add a function"}"#).unwrap();
        assert_eq!(req.permission_mode, None);
        // And a None mode does not serialize a null key back onto the wire.
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"objective":"add a function"}"#);
    }

    #[test]
    fn task_create_request_carries_permission_mode_when_set() {
        let req = TaskCreateRequest {
            objective: "x".into(),
            permission_mode: Some("manual".into()),
        };
        let back: TaskCreateRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn config_set_request_round_trips() {
        let req = ConfigSetRequest {
            key: "log.format".into(),
            value: "json".into(),
            scope: "workspace".into(),
        };
        let back: ConfigSetRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back, req);
    }
}
