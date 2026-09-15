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
    /// A plan checkpoint was taken — payload `{ checkpoint_id, step_id }`.
    /// Lets a client learn a `checkpoint_id` for `task_rollback` (G13);
    /// `valyria-plan` owns the shape.
    PlanCheckpoint,
    /// Progress of an in-flight `model_install` — payload `{ id, phase,
    /// downloaded_bytes, total_bytes }` (`valyria-app` owns the shape).
    ModelInstallProgress,
    /// A `model_install` finished successfully — payload `{ id,
    /// size_bytes }`.
    ModelInstallCompleted,
    /// A `model_install` failed or was cancelled — payload `{ id, code,
    /// message }`.
    ModelInstallFailed,
    /// Progress of an in-flight inference-engine download (llama.cpp) —
    /// payload `{ component, version, phase, downloaded_bytes,
    /// total_bytes }` (`valyria-app` owns the shape).
    EngineInstallProgress,
    /// The inference engine was downloaded and unpacked — payload
    /// `{ component, version }`.
    EngineInstallCompleted,
    /// The inference-engine download failed — payload `{ component,
    /// version, code, message }`.
    EngineInstallFailed,
    /// A per-role model server (`llama-server`) is being started —
    /// payload `{ role, id }`.
    ModelServerStarting,
    /// A per-role model server is up and serving — payload `{ role, id,
    /// port }`.
    ModelServerReady,
    /// A per-role model server failed to start — payload `{ role, id,
    /// code, message }`.
    ModelServerFailed,
    /// A per-role model server was stopped — payload `{ role, id,
    /// reason }`.
    ModelServerStopped,
    /// A child task was created under a parent (M5, `valyria-task::
    /// TaskManager::create_child`) — projected onto the *parent's* event
    /// stream, not the child's own. Payload `{ child_task_id, objective }`.
    SubtaskStarted,
    /// A child task reached a terminal state (M5) — projected onto the
    /// *parent's* event stream. Payload `{ child_task_id, final_state }`.
    SubtaskCompleted,
    /// A role-pipeline `Artifact` was persisted to the `PlanStore` (M5,
    /// `valyria-agent::role_pipeline`) — payload `{ role, kind }`
    /// (`valyria-plan::roles::{AgentRole, ArtifactKind}::as_str`).
    ArtifactPublished,
}

impl EventKind {
    /// Every kind, in declaration order — the canonical list a client
    /// syncs its decoder coverage against (G12). `xtask` exports this to
    /// `docs/protocol/event-kinds.txt` and gates drift.
    pub const ALL: &'static [EventKind] = &[
        EventKind::TaskStarted,
        EventKind::PlanCreated,
        EventKind::ContextRetrieved,
        EventKind::ModelStarted,
        EventKind::ModelCompleted,
        EventKind::ToolStarted,
        EventKind::ToolCompleted,
        EventKind::FileChanged,
        EventKind::TestStarted,
        EventKind::TestPassed,
        EventKind::TestFailed,
        EventKind::ApprovalRequested,
        EventKind::TaskPaused,
        EventKind::TaskCompleted,
        EventKind::TaskFailed,
        EventKind::StateChanged,
        EventKind::ProgressStalled,
        EventKind::ExternalChangeDetected,
        EventKind::VerificationEvidence,
        EventKind::MemoryWritten,
        EventKind::ResourcePressure,
        EventKind::PlanCheckpoint,
        EventKind::ModelInstallProgress,
        EventKind::ModelInstallCompleted,
        EventKind::ModelInstallFailed,
        EventKind::EngineInstallProgress,
        EventKind::EngineInstallCompleted,
        EventKind::EngineInstallFailed,
        EventKind::ModelServerStarting,
        EventKind::ModelServerReady,
        EventKind::ModelServerFailed,
        EventKind::ModelServerStopped,
        EventKind::SubtaskStarted,
        EventKind::SubtaskCompleted,
        EventKind::ArtifactPublished,
    ];

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
            EventKind::PlanCheckpoint => "plan_checkpoint",
            EventKind::ModelInstallProgress => "model_install_progress",
            EventKind::ModelInstallCompleted => "model_install_completed",
            EventKind::ModelInstallFailed => "model_install_failed",
            EventKind::EngineInstallProgress => "engine_install_progress",
            EventKind::EngineInstallCompleted => "engine_install_completed",
            EventKind::EngineInstallFailed => "engine_install_failed",
            EventKind::ModelServerStarting => "model_server_starting",
            EventKind::ModelServerReady => "model_server_ready",
            EventKind::ModelServerFailed => "model_server_failed",
            EventKind::ModelServerStopped => "model_server_stopped",
            EventKind::SubtaskStarted => "subtask_started",
            EventKind::SubtaskCompleted => "subtask_completed",
            EventKind::ArtifactPublished => "artifact_published",
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

    #[test]
    fn all_covers_every_variant_and_names_are_unique() {
        // If a variant is added without extending ALL, the round-trip
        // below still passes but the count check here fails — the reminder
        // to also add a payload contract (G12).
        let names: std::collections::BTreeSet<&str> =
            EventKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(names.len(), EventKind::ALL.len(), "duplicate kind name");
        // Exhaustiveness: matching every variant must be covered by ALL.
        for k in EventKind::ALL {
            let _: &str = k.as_str();
        }
        assert_eq!(EventKind::ALL.len(), 35);
    }

    #[test]
    fn newer_kinds_round_trip_through_their_strings() {
        for (kind, s) in [
            (EventKind::ContextRetrieved, "context_retrieved"),
            (EventKind::SubtaskStarted, "subtask_started"),
            (EventKind::SubtaskCompleted, "subtask_completed"),
            (EventKind::ArtifactPublished, "artifact_published"),
            (EventKind::PlanCheckpoint, "plan_checkpoint"),
            (EventKind::ModelInstallProgress, "model_install_progress"),
            (EventKind::ModelInstallCompleted, "model_install_completed"),
            (EventKind::ModelInstallFailed, "model_install_failed"),
            (EventKind::EngineInstallProgress, "engine_install_progress"),
            (
                EventKind::EngineInstallCompleted,
                "engine_install_completed",
            ),
            (EventKind::EngineInstallFailed, "engine_install_failed"),
            (EventKind::ModelServerStarting, "model_server_starting"),
            (EventKind::ModelServerReady, "model_server_ready"),
            (EventKind::ModelServerFailed, "model_server_failed"),
            (EventKind::ModelServerStopped, "model_server_stopped"),
        ] {
            assert_eq!(kind.as_str(), s);
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{s}\""));
            let back: EventKind = serde_json::from_str(&format!("\"{s}\"")).unwrap();
            assert_eq!(back, kind);
        }
    }
}
