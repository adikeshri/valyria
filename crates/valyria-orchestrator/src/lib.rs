//! `valyria-orchestrator` — layer 4 (Model).
//!
//! Everything between "the agent wants a model to do something" and a
//! concrete [`ModelRuntime`](valyria_model::ModelRuntime):
//!
//! - [`Orchestrator`] — the Phase 3 minimal role→model binding, unchanged.
//! - [`router::RoleRouter`] — role bindings with ordered **fallback
//!   chains** and health-aware escalation (§38).
//! - [`structured`] — the **tool-call transport ladder** (D5): native
//!   `tool_calls` first, then a tolerant recovery parser over fenced /
//!   tagged model text, then a bounded reformat-retry that feeds the parse
//!   error back to the model.
//! - [`pool::ModelPool`] — memory-aware **admission control**: LRU-within-
//!   role-priority eviction and `ResourcePressure` events (§4.22, §41).
//!
//! Wiring the ladder, router and pool into the live agent loop (which still
//! drives through [`Orchestrator`]) is a deliberate follow-up — see
//! `docs/ROADMAP.md`.

#![forbid(unsafe_code)]

pub mod error;
pub mod orchestrator;
pub mod pool;
pub mod role;
pub mod router;
pub mod structured;

pub use error::{OrchestratorError, Result};
pub use orchestrator::Orchestrator;
pub use pool::{EvictReason, ModelPool, PoolError, PoolEvent};
pub use role::Role;
pub use router::{RoleBinding, RoleRouter, RoutedCompletion};
pub use structured::{extract, recover_from_text, resolve_tool_calls, ExtractError, Extraction};
