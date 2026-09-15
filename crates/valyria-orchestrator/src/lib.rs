//! `valyria-orchestrator` — layer 4 (Model).
//!
//! Everything between "the agent wants a model to do something" and a
//! concrete [`ModelRuntime`](valyria_model::ModelRuntime):
//!
//! - [`Orchestrator`] — one-model-per-role binding, hot-swappable
//!   (`rebind`) without a daemon restart. `Orchestrator::generate_action`
//!   is what the live agent loop actually calls
//!   (`valyria-agent/src/driver.rs`): it already runs every turn through
//!   [`structured::resolve_action`] (the **tool-call transport ladder**,
//!   D5 — native `tool_calls` first, then a tolerant recovery parser over
//!   fenced/tagged model text, then a bounded reformat-retry that feeds
//!   the parse error back to the model as evidence).
//! - [`router::RoleRouter`] — role bindings with ordered **fallback
//!   chains** and health-aware escalation (§38). Built and tested; **not
//!   yet wired into the live loop** — `Orchestrator` is still one binding
//!   per role, so a ladder failure has nowhere to fall back to. See
//!   `docs/COMPLETION-PLAN.md` milestone M1.
//! - [`pool::ModelPool`] — memory-aware **admission control**: LRU-within-
//!   role-priority eviction and `ResourcePressure` events (§4.22, §41).
//!   Built and tested; **not yet wired into the live loop** — multiple
//!   roles loaded at once (coder + embedder) are not yet arbitrated by it.
//!   See `docs/COMPLETION-PLAN.md` milestone M1/M6.

#![forbid(unsafe_code)]

pub mod error;
pub mod orchestrator;
pub mod placeholder;
pub mod pool;
pub mod role;
pub mod router;
pub mod structured;

pub use error::{OrchestratorError, Result};
pub use orchestrator::Orchestrator;
pub use placeholder::NoModelRuntime;
pub use pool::{EvictReason, ModelPool, PoolError, PoolEvent};
pub use role::Role;
pub use router::{RoleBinding, RoleRouter, RoutedCompletion};
pub use structured::{
    extract, recover_from_text, resolve_action, resolve_tool_calls, ExtractError, Extraction,
    ResolvedAction,
};
