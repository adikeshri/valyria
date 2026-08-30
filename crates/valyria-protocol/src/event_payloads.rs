//! Per-`kind` event payload contracts (§4.27, G12).
//!
//! `WireEvent.payload` is deliberately `serde_json::Value` so a new event
//! shape never forces a protocol break. That freedom is also the project's
//! highest-frequency *silent* breakage risk (a renamed field yields an
//! empty UI cell, not an error). These mirror structs pin the shape of the
//! payloads a client renders specifically; [`payload_schemas`] exports one
//! JSON Schema per kind into `docs/protocol/events/`, and
//! `xtask check-protocol` gates drift the same way it gates the request /
//! response schemas.
//!
//! Kinds without a struct here have an intentionally open payload — a
//! client decodes them tolerantly and renders a generic row.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The canonical list of event `kind` strings, in `valyria_events`'
/// declaration order (G12). A client syncs its decoder coverage against
/// this; `xtask check-protocol` exports it to
/// `docs/protocol/event-kinds.txt` and gates drift. Kept in lockstep with
/// `valyria_events::EventKind::ALL` by a cross-check test in that crate.
pub const EVENT_KINDS: &[&str] = &[
    "task_started",
    "plan_created",
    "context_retrieved",
    "model_started",
    "model_completed",
    "tool_started",
    "tool_completed",
    "file_changed",
    "test_started",
    "test_passed",
    "test_failed",
    "approval_requested",
    "task_paused",
    "task_completed",
    "task_failed",
    "state_changed",
    "progress_stalled",
    "external_change_detected",
    "verification_evidence",
    "memory_written",
    "resource_pressure",
    "plan_checkpoint",
    "model_install_progress",
    "model_install_completed",
    "model_install_failed",
];

/// `state_changed` — an `AgentState` transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StateChangedPayload {
    pub from: String,
    pub to: String,
}

/// `tool_started` — a tool invocation began. `tool_invocation_id` pairs
/// this with the matching `tool_completed` (G14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolStartedPayload {
    pub tool: String,
    pub tool_invocation_id: String,
    /// The tool's canonical input JSON — shape is per-tool, left open.
    pub input: serde_json::Value,
}

/// `tool_completed` — a tool invocation finished (G14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCompletedPayload {
    pub success: bool,
    pub tool_invocation_id: String,
    pub tool_record_id: String,
    /// Shell tools only; `null` otherwise.
    pub exit_code: Option<i64>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub duration_ms: u64,
    /// The pre-formatted human blob (`exit=… \n--- stdout ---\n…`).
    pub rendered: String,
}

/// `approval_requested` — the agent is blocked on a permission decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalRequestedPayload {
    pub prompt: String,
    pub tool: String,
    pub category: String,
    pub target: String,
    pub risk: String,
}

/// One retrieved context item (§34, G7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContextItemPayload {
    pub path: String,
    /// The ordered retrieval path, joined with ` -> `, or `explicit`.
    pub reason: String,
    /// `policy` | `instruction` | `evidence` | `repo_data` | `model_output`.
    pub trust_level: String,
    pub tokens: u64,
    pub score: Option<f64>,
}

/// `context_retrieved` — the working set the assembler produced for a step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContextRetrievedPayload {
    pub items: Vec<ContextItemPayload>,
    pub budget_used: u64,
    pub budget_total: u64,
}

/// `plan_checkpoint` — a rollback point was taken (§16, G13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanCheckpointPayload {
    pub checkpoint_id: String,
    pub step_id: String,
}

/// `model_install_progress` — an in-flight `model_install` (G5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelInstallProgressPayload {
    pub id: String,
    /// `downloading` | `verifying` | `probing`.
    pub phase: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

/// `model_install_completed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelInstallCompletedPayload {
    pub id: String,
    pub size_bytes: u64,
}

/// `model_install_failed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelInstallFailedPayload {
    pub id: String,
    pub code: String,
    pub message: String,
}

/// A parsed verification failure location (§19, §35, G15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FailureLocationPayload {
    pub path: String,
    pub line: Option<u32>,
}

/// One parsed verification failure (G15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FailurePayload {
    /// `compile_error` | `test_failure` | `test_panic` | `lint_error` |
    /// `type_error` | `format_violation` | `timeout` | `unknown`.
    pub kind: String,
    pub message: String,
    pub failing_test: Option<String>,
    pub location: Vec<FailureLocationPayload>,
}

/// `verification_evidence` / `test_failed` — a verification run result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VerificationEvidencePayload {
    pub command: String,
    pub passed: bool,
    pub outcome: String,
    pub exit_code: Option<i64>,
    pub failure_count: u64,
    pub run_id: String,
    pub digest: String,
    pub failures: Vec<FailurePayload>,
}

/// `(kind, pretty-printed JSON Schema)` for every kind with a pinned
/// payload contract. Exported to `docs/protocol/events/<kind>.schema.json`.
pub fn payload_schemas() -> Vec<(&'static str, String)> {
    fn s<T: JsonSchema>() -> String {
        let mut out = serde_json::to_string_pretty(&schemars::schema_for!(T))
            .expect("payload schema serializes");
        out.push('\n');
        out
    }
    vec![
        ("state_changed", s::<StateChangedPayload>()),
        ("tool_started", s::<ToolStartedPayload>()),
        ("tool_completed", s::<ToolCompletedPayload>()),
        ("approval_requested", s::<ApprovalRequestedPayload>()),
        ("context_retrieved", s::<ContextRetrievedPayload>()),
        ("plan_checkpoint", s::<PlanCheckpointPayload>()),
        ("model_install_progress", s::<ModelInstallProgressPayload>()),
        (
            "model_install_completed",
            s::<ModelInstallCompletedPayload>(),
        ),
        ("model_install_failed", s::<ModelInstallFailedPayload>()),
        ("verification_evidence", s::<VerificationEvidencePayload>()),
        ("test_failed", s::<VerificationEvidencePayload>()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_payload_schema_is_valid_json_and_named() {
        for (kind, json) in payload_schemas() {
            assert!(!kind.is_empty());
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(
                v.get("properties").is_some(),
                "{kind} schema has no properties"
            );
        }
    }

    #[test]
    fn a_real_context_retrieved_payload_deserializes() {
        let p: ContextRetrievedPayload = serde_json::from_value(serde_json::json!({
            "items": [{"path": "src/x.rs", "reason": "explicit", "trust_level": "repo_data", "tokens": 12, "score": null}],
            "budget_used": 12,
            "budget_total": 50000
        }))
        .unwrap();
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.budget_total, 50_000);
    }
}
