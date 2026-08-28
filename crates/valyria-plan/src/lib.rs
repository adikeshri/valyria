//! `valyria-plan` — layer 5 (Agent).
//!
//! The planner and its supporting machinery (§4.25): a [`Plan`] is a DAG
//! of [`PlanStep`]s the model proposes and the runtime **validates**
//! ([`validate`]), schedules into dependency waves ([`schedule`]), executes
//! with **checkpoints at rollback boundaries** ([`checkpoint`]), and keeps
//! as a living, revisable, diffable document. Invalid plans are handed back
//! to the model as structured errors under a bounded [`repair`] budget.
//!
//! Multi-agent Researcher / Planner / Implementer / Tester / Reviewer are
//! [`roles`] over this same machinery, communicating only through typed
//! [`Artifact`](roles::Artifact)s. All of it persists through [`PlanStore`]
//! (migration block 800-899) so a resumed task rebuilds its plan and
//! checkpoints from durable rows, not memory.

#![forbid(unsafe_code)]

pub mod checkpoint;
pub mod error;
pub mod migrations;
pub mod model;
pub mod repair;
pub mod roles;
pub mod schedule;
pub mod store;
pub mod validate;

pub use checkpoint::{capture, rollback, Checkpoint, RollbackError, RollbackReport};
pub use error::{PlanError as PlanCrateError, PlanFormatError, Result};
pub use migrations::MIGRATIONS;
pub use model::{
    EstimatedScope, Plan, PlanDiff, PlanRevision, PlanStep, PlanStepId, Risk,
    VerificationRequirement,
};
pub use repair::{render_feedback, PlanRepairDecision, PlanRepairLedger};
pub use roles::{AgentRole, Artifact, ArtifactKind, StoredArtifact};
pub use schedule::{schedule, Schedule};
pub use store::PlanStore;
pub use validate::{validate, PlanContext, PlanError, PlanErrorCode, ValidatedPlan};

/// Marks this crate as present in the workspace topology for the given
/// phase (kept from the scaffold; the layering/CI checks read it).
pub const PHASE: u8 = 8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_is_recorded() {
        assert_eq!(PHASE, 8);
    }
}
