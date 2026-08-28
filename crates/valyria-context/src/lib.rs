//! `valyria-context` — layer 5 (Agent).
//!
//! The context pipeline (§4.17, §11-12): a query becomes a set of
//! trust-tagged, provenance-carrying candidates; the candidates are
//! ranked, structurally expanded, compressed to fit a per-section token
//! budget, and assembled into a prompt where the trust lattice (D3) is
//! enforced *structurally* —
//!
//! * only [`Trust::Policy`](valyria_types::Trust) / `Trust::Instruction`
//!   content occupies a system position;
//! * everything at `Trust::Evidence` or below is wrapped in a per-assembly
//!   nonce fence and framed as data, with any instruction-shaped content
//!   annotated (never stripped) by the [`inject`] detector;
//! * the budget allocator fails loudly rather than truncate silently, and
//!   compression drops whole lines or whole symbols — never a fragment of
//!   one;
//! * the result is a [`ContextSnapshot`] and the messages are
//!   `snapshot.render()`, so any prompt can be rebuilt from stored
//!   provenance byte-for-byte.
//!
//! ## Two entry points
//!
//! [`ContextAssembler`] is the original Phase 3 path: wrap an explicit list
//! of files, read through the real permissioned tool runtime, into a flat
//! budget. It is still what the embedded runtime drives with.
//!
//! [`ContextEngine`] is the full pipeline: it converts a discovered
//! [`InstructionSet`](valyria_instructions::InstructionSet), a
//! [`RetrievedMemory`](valyria_memory::RetrievedMemory), and the output of
//! a [`Retriever`] into candidates and runs [`PromptAssembler`] over them.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod assemble;
pub mod assembler;
pub mod budget;
pub mod candidate;
pub mod compress;
pub mod engine;
pub mod error;
pub mod inject;
pub mod item;
pub mod query;
pub mod retrieve;
pub mod snapshot;

pub use assemble::{AssembledPrompt, AssemblyRequest, DroppedItem, PromptAssembler};
pub use assembler::ContextAssembler;
pub use budget::{allocate, Allocation, ContextBudget, SectionKind, SectionSpec};
pub use candidate::{CandidateContent, CompressionLevel, RetrievalCandidate, SymbolSpan};
pub use engine::{ContextEngine, EngineInput};
pub use error::{ContextError, Result};
pub use inject::{InjectionKind, InjectionSignal};
pub use item::{ContextBody, ContextItem};
pub use query::{AssembledContext, ContextQuery};
pub use retrieve::{RetrievalQuery, Retriever, StaticRetriever};
pub use snapshot::{AssembledItem, ContextSnapshot, DEFAULT_RUNTIME_POLICY, STANDING_DATA_FRAME};

#[cfg(feature = "intelligence")]
pub use retrieve::SearchRetriever;

/// The build phase this crate's full implementation belongs to
/// ([docs/PLAN.md §5](../docs/PLAN.md)).
pub const PHASE: u8 = 6;
