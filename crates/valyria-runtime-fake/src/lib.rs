//! `valyria-runtime-fake` — layer 4 (Model).
//!
//! `FakeModelRuntime` (D12): a deterministic, scripted `ModelRuntime`. Ships
//! in the workspace as first-class infrastructure, not test scaffolding —
//! nearly all agent-loop tests, and Phase 3's walking-skeleton demo, run
//! against it instead of a real model.

#![forbid(unsafe_code)]

pub mod error;
pub mod runtime;
pub mod scenario;

pub use error::{FakeRuntimeError, Result};
pub use runtime::FakeModelRuntime;
pub use scenario::{Scenario, ScriptedTurn};
