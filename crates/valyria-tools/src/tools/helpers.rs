//! Shared plumbing every tool implementation uses: input-field extraction
//! with consistent error shapes, and the authorization self-check every
//! `execute` performs before doing anything side-effecting (D2 defense in
//! depth).

use serde_json::Value;
use valyria_permissions::Authorization;
use valyria_types::{StepId, TaskId};
use valyria_util::ContentHash;

use crate::canonical::canonical_input_hash;
use crate::error::{Result, ToolError};

pub fn require_str<'a>(
    input: &'a Value,
    field: &'static str,
    tool: &'static str,
) -> Result<&'a str> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput {
            tool,
            reason: format!("missing or non-string field `{field}`"),
        })
}

pub fn optional_str<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input.get(field).and_then(|v| v.as_str())
}

pub fn optional_bool(input: &Value, field: &str, default: bool) -> bool {
    input
        .get(field)
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

pub fn optional_u64(input: &Value, field: &str) -> Option<u64> {
    input.get(field).and_then(|v| v.as_u64())
}

/// Verifies `auth` actually covers `(task_id, step_id, tool, input)` —
/// every tool's `execute` calls this before doing anything
/// side-effecting, rather than trusting the caller passed a matching
/// authorization.
pub fn verify_authorization(
    auth: &Authorization,
    task_id: TaskId,
    step_id: StepId,
    tool: &'static str,
    input: &Value,
) -> Result<()> {
    let input_hash: ContentHash = canonical_input_hash(input);
    if auth.matches(task_id, step_id, tool, input_hash) {
        Ok(())
    } else {
        Err(ToolError::AuthorizationMismatch)
    }
}

/// A `Clock` that reads real wall-clock time. Tool execution needs *a*
/// clock to timestamp ledger entries; a fully injected clock (for
/// deterministic tests of tool *sequencing*) is future work once
/// `ToolCtx` carries one — today each call reads real time, which is fine
/// since ledger tests exercise the timestamp-sensitive logic directly
/// against `valyria-ledger`, not through this indirection.
pub struct SystemClockRef;
impl valyria_util::Clock for SystemClockRef {
    fn now(&self) -> valyria_types::Timestamp {
        valyria_types::Timestamp::now()
    }
}

/// Minimal JSON-Schema object builder — enough structure for a tool
/// descriptor without pulling in a schema-generation crate for a handful
/// of hand-written shapes.
pub fn object_schema(properties: Value, required: &[&str]) -> Value {
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}
