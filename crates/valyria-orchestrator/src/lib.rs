//! `valyria-orchestrator` — layer 4 (Model).
//!
//! Everything between "the agent wants a model to do something" and a
//! concrete [`ModelRuntime`](valyria_model::ModelRuntime):
//!
//! - [`router::RoleRouter`] — role bindings with ordered **fallback
//!   chains** (§38), hot-swappable (`bind`/`bind_single`) without a daemon
//!   restart. `RoleRouter::generate_action` is what the live agent loop
//!   actually calls (`valyria-agent/src/driver.rs` and `plan_exec.rs`, and
//!   `valyria-app/src/runtime.rs`'s real-inference wiring — M1): for each
//!   candidate model in the chain, in order, it runs the full **tool-call
//!   transport ladder** (D5 — native `tool_calls` first, then a tolerant
//!   recovery parser over fenced/tagged model text, then a bounded
//!   reformat-retry that feeds the parse error back to the model as
//!   evidence), falling back to the next candidate on a retryable model
//!   error or a model that never produces a parseable turn. `AgentDriver::
//!   model_role` additionally chooses `FastCoder` over `PrimaryCoder` for
//!   the main loop and each repair attempt, escalating for the rest of a
//!   task's run on a `SwitchRole` repair decision.
//! - [`Orchestrator`] — the simpler predecessor: one model per role, no
//!   fallback chain, otherwise the same `generate`/`generate_action`
//!   shape. Kept as a smaller building block and its own test surface for
//!   the ladder's "fast path costs exactly one call" contract
//!   (`tests/generate_action.rs`); no longer what the live loop or
//!   `valyria-app` construct.
//! - [`pool::ModelPool`] — memory-aware **admission control**: LRU-within-
//!   role-priority eviction and `ResourcePressure` events (§4.22, §41).
//!   Built and tested; **not yet wired into `valyria-app`'s real-model
//!   boot path** (`spawn_model_boot`/`model_activate` start a
//!   `llama-server` unconditionally, with no admission check against
//!   measured available memory) — deliberately left to
//!   `docs/COMPLETION-PLAN.md` milestone M6, which also wires in the real
//!   *measured* footprint (a probe result) rather than a size-on-disk
//!   proxy, and projects pool events onto the protocol.

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
