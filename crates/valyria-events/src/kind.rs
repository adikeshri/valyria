//! Event kinds (§43), plus the extras the build plan calls for beyond the
//! PRD's baseline list (`docs/PLAN.md` §4.2): `StateChanged`,
//! `ProgressStalled`, `ExternalChangeDetected`, `VerificationEvidence`,
//! `MemoryWritten`, `ResourcePressure`.
//!
//! This crate is layer 0 and cannot depend on the richer types that live
//! higher up (a `Plan`, a `ToolInvocationRecord`, ...), so each kind's
//! payload is a JSON value rather than a strongly-typed struct. The owning
//! crate for a given kind documents and owns its payload shape; this enum
//! only needs to be extended when a genuinely new *category* of event is
//! introduced, not for every new field.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    TaskStarted,
    PlanCreated,
    ContextRetrieved,
    ModelStarted,
    ModelCompleted,
    ToolStarted,
    ToolCompleted,
    FileChanged,
    TestStarted,
    TestPassed,
    TestFailed,
    ApprovalRequested,
    TaskPaused,
    TaskCompleted,
    TaskFailed,
    StateChanged,
    ProgressStalled,
    ExternalChangeDetected,
    VerificationEvidence,
    MemoryWritten,
    ResourcePressure,
    /// Progress of an in-flight `model_install` — payload `{ id, phase,
    /// downloaded_bytes, total_bytes }` (`valyria-app` owns the shape).
    ModelInstallProgress,
    /// A `model_install` finished successfully — payload `{ id,
    /// size_bytes }`.
    ModelInstallCompleted,
    /// A `model_install` failed or was cancelled — payload `{ id, code,
    /// message }`.
    ModelInstallFailed,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::TaskStarted => "task_started",
            EventKind::PlanCreated => "plan_created",
            EventKind::ContextRetrieved => "context_retrieved",
            EventKind::ModelStarted => "model_started",
            EventKind::ModelCompleted => "model_completed",
            EventKind::ToolStarted => "tool_started",
            EventKind::ToolCompleted => "tool_completed",
            EventKind::FileChanged => "file_changed",
            EventKind::TestStarted => "test_started",
            EventKind::TestPassed => "test_passed",
            EventKind::TestFailed => "test_failed",
            EventKind::ApprovalRequested => "approval_requested",
            EventKind::TaskPaused => "task_paused",
            EventKind::TaskCompleted => "task_completed",
            EventKind::TaskFailed => "task_failed",
            EventKind::StateChanged => "state_changed",
            EventKind::ProgressStalled => "progress_stalled",
            EventKind::ExternalChangeDetected => "external_change_detected",
            EventKind::VerificationEvidence => "verification_evidence",
            EventKind::MemoryWritten => "memory_written",
            EventKind::ResourcePressure => "resource_pressure",
            EventKind::ModelInstallProgress => "model_install_progress",
            EventKind::ModelInstallCompleted => "model_install_completed",
            EventKind::ModelInstallFailed => "model_install_failed",
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_tag_matches_snake_case_display() {
        let json = serde_json::to_string(&EventKind::ExternalChangeDetected).unwrap();
        assert_eq!(json, "\"external_change_detected\"");
        assert_eq!(
            EventKind::ExternalChangeDetected.as_str(),
            "external_change_detected"
        );
    }
}
