//! `valyria-app` — layer 6 (Interface).
//!
//! The application/wiring layer (§4.1): `Runtime::open` composes every
//! already-built subsystem (store, events, permissions, tools, ledger,
//! orchestrator + fake model, context, task manager, agent driver) into one
//! embedded runtime for a workspace, and `EmbeddedClient` exposes it
//! through `valyria-protocol`'s `Client` trait — the only thing
//! `valyria-cli` is allowed to depend on from this crate's neighborhood
//! (D11).

#![forbid(unsafe_code)]

pub mod client;
pub mod error;
pub mod migrations;
pub mod runtime;

pub use client::EmbeddedClient;
pub use error::{AppError, Result};
pub use runtime::{load_scenario, Runtime, RuntimeConfig};
