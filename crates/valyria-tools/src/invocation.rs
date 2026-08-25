//! The tool invocation record (§18): every call, no exceptions.

use serde_json::Value;
use valyria_types::{StepId, TaskId, Timestamp, ToolInvocationId};

#[derive(Debug, Clone)]
pub struct ToolInvocationRecord {
    pub id: ToolInvocationId,
    pub task_id: TaskId,
    pub step_id: StepId,
    pub tool: &'static str,
    pub input: Value,
    /// Whether an `Authorization` actually covered this call — always
    /// `true` for a record that reaches this point, since the runtime
    /// refuses to call `execute` otherwise; kept explicit in the record
    /// rather than implied, so the audit trail doesn't depend on that
    /// invariant holding forever.
    pub authorized: bool,
    pub start_time: Timestamp,
    pub end_time: Timestamp,
    pub success: bool,
    pub exit_status: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub error: Option<String>,
}
