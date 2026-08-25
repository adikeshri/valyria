//! What a tool asks the permission engine for.

use valyria_types::{PermissionCategory, StepId, TaskId};
use valyria_util::ContentHash;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    Read,
    Write,
    Execute,
}

/// How dangerous a specific action looks, independent of permission mode —
/// computed by [`crate::risk`] for shell commands, or assigned directly by
/// a tool for non-shell categories.
///
/// `Safe` and `Controlled` are auto-allowable in Autonomous mode;
/// `Unknown` and `Destructive` always ask, in every mode — "defaults to
/// Ask on unknown" (§4.9) means an unrecognized binary is never silently
/// trusted just because the mode is permissive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RiskLevel {
    Safe,
    Controlled,
    Unknown,
    Destructive,
}

impl RiskLevel {
    pub fn auto_allowable(self) -> bool {
        matches!(self, RiskLevel::Safe | RiskLevel::Controlled)
    }
}

#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub task_id: TaskId,
    pub step_id: StepId,
    /// Static tool name (`"write_file"`, `"run_command"`, ...) — matches
    /// [`crate::authorization::AuthorizationKey::tool`].
    pub tool: &'static str,
    pub category: PermissionCategory,
    pub action: ActionKind,
    pub risk: RiskLevel,
    /// Hash of the tool's canonical input — what the eventual
    /// `Authorization` binds to.
    pub input_hash: ContentHash,
    /// Human-readable description of what this touches, for `Ask` prompts
    /// and audit — a path, a command line, a URL.
    pub target: String,
    /// Whether `target` falls within the task's declared plan scope
    /// (§10) — drives permission auto-allow in Assisted/Autonomous modes.
    pub in_plan_scope: bool,
}
