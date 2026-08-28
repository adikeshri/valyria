//! `valyria-memory` — layer 5 (Agent).
//!
//! Four kinds of memory, one store (§4.19, §32):
//!
//! | Type | Scope | Written by | Retrieval |
//! |---|---|---|---|
//! | Session | one client session | the runtime | always in the context header |
//! | Task | one task | agent observations + a summarizer | task-scoped, recency + relevance |
//! | Repository | the workspace, persistent | extraction after verified work, or explicitly | relevance + trigger |
//! | User | global | explicitly only | matched on relevance |
//!
//! Two rules run through all of it:
//!
//! **Confidence decays.** Every entry is written with a confidence in
//! `[0, 1]`; [`MemoryEntry::effective_confidence`] halves it every
//! half-life of *silence*. Reinforcing an entry (it was retrieved and
//! still held true) resets the clock; contradicting it retires it.
//!
//! **Provenance fixes trust.** A user-authored entry is
//! [`Trust::Instruction`](valyria_types::Trust) — the operator said so. An
//! agent-*extracted* entry is [`Trust::Evidence`]: it can inform the next
//! step, it cannot command it. Prompt assembly relies on that distinction.
//!
//! Everything is local, in the workspace's SQLite database, and every
//! entry is inspectable and deletable ([`MemoryStore::purge`], the hook
//! `valyria clean --memory` is built on).

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod entry;
pub mod error;
pub mod extract;
pub mod migrations;
pub mod store;

pub use entry::{MemoryAuthor, MemoryEntry, MemoryKind, MemoryScope, DEFAULT_HALF_LIFE_MS};
pub use error::{MemoryError, Result};
pub use extract::{extract, Observation, ObservationKind};
pub use migrations::MIGRATIONS;
pub use store::{
    MemoryStats, MemoryStore, PurgeScope, RetrievalRequest, RetrievedMemory, ScoredMemory,
};

/// The build phase this crate's implementation belongs to
/// ([docs/PLAN.md §5](../docs/PLAN.md)).
pub const PHASE: u8 = 6;

#[cfg(test)]
mod tests {
    #[test]
    fn phase_is_recorded() {
        assert_eq!(super::PHASE, 6);
    }
}
