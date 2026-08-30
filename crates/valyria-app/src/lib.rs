//! `valyria-app` — layer 6 (Interface).
//!
//! The application/wiring layer (§4.1): `Runtime::open` composes every
//! already-built subsystem (store, events, permissions, tools, ledger,
//! orchestrator + fake model, context, task manager, agent driver, plus
//! the global `~/.valyria` store) into one embedded runtime for a
//! workspace, and `EmbeddedClient` exposes it through `valyria-protocol`'s
//! `Client` trait — the only thing `valyria-cli` is allowed to depend on
//! from this crate's neighborhood (D11).
//!
//! Phase 10 adds the rest of that surface: [`doctor`] (environment
//! checks), [`storage`] (inspect / clean), [`global`] (`global.db`
//! assembly), and [`daemon::serve`] (the Unix-socket transport — a pure
//! backend swap behind the same `Client` trait).

#![forbid(unsafe_code)]

pub mod client;
pub mod daemon;
pub mod doctor;
pub mod error;
pub mod global;
pub mod migrations;
pub mod runtime;
pub mod storage;

pub use client::EmbeddedClient;
pub use daemon::serve;
pub use doctor::{CheckStatus, Doctor, DoctorCheck, DoctorReport};
pub use error::{AppError, Result};
pub use global::{global_migrations, GlobalStore, WorkspaceRegistration};
pub use runtime::{
    load_scenario, ConfigWriteScope, GitStatusView, LedgerChangeView, ModelInspectView, Runtime,
    RuntimeConfig,
};
pub use storage::{PurgeOutcome, PurgeScope, StorageEntry, StorageInspector, StorageReport};

pub use valyria_agent::{ApprovalDecision, PlanningMode};
pub use valyria_plan::{RollbackError, RollbackReport};
