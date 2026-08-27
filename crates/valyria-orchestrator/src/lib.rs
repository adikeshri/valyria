//! `valyria-orchestrator` — layer 4 (Model).
//!
//! Minimal role routing for Phase 3: bind a [`Role`] to a
//! `valyria_model::ModelRuntime` and delegate `generate` calls to it. The
//! full model pool, admission control, and tool-call transport ladder (D5)
//! land in Phase 9 once multiple, less-reliable real adapters exist.

#![forbid(unsafe_code)]

pub mod error;
pub mod orchestrator;
pub mod role;

pub use error::{OrchestratorError, Result};
pub use orchestrator::Orchestrator;
pub use role::Role;
