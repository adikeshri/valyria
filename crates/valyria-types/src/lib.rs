//! `valyria-types` — layer 0 (Foundation).
//!
//! IDs, domain enums, the trust lattice, evidence, and the shared error
//! taxonomy. This crate performs no I/O and depends on nothing else in the
//! workspace: everything above it can assume these types are always
//! available and never pull in a cycle.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod error;
pub mod evidence;
pub mod id;
pub mod policy;
pub mod state;
pub mod time;
pub mod trust;

pub use error::{CodedError, ErrorCode};
pub use evidence::{Evidence, EvidenceBody, EvidenceSource};
pub use id::{
    ApprovalId, CheckpointId, ContextSnapshotId, EffectId, EventId, Generation, IdParseError,
    LedgerEntryId, MemoryId, ModelInstanceId, PlanId, SessionId, StepId, TaskId, ToolInvocationId,
    VerificationRunId, WorkspaceId,
};
pub use policy::{Access, NetworkPolicy, PermissionCategory, PermissionMode};
pub use state::AgentState;
pub use time::Timestamp;
pub use trust::{Provenance, ProvenanceSource, Trust};
