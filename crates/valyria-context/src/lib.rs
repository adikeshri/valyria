//! `valyria-context` — layer 5 (Agent).
//!
//! Phase 3 ships only the explicit-file subset of the context pipeline
//! (§4.17): wrap named files, read through the real permissioned tool
//! runtime, into trust-tagged [`ContextItem`]s, budget-checked with a
//! heuristic token counter. The full query -> retrieval -> rank ->
//! structural expansion -> compress -> budget -> assemble pipeline (with
//! D3's trust-ordered, nonce-fenced prompt assembly) lands in Phase 6.

#![forbid(unsafe_code)]

pub mod assembler;
pub mod error;
pub mod item;
pub mod query;

pub use assembler::ContextAssembler;
pub use error::{ContextError, Result};
pub use item::{ContextBody, ContextItem};
pub use query::{AssembledContext, ContextQuery};
