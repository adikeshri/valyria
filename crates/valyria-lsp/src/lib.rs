//! `valyria-lsp` — layer 2 (Repository intelligence).
//!
//! A Language Server Protocol client, its lifecycle, and a capped pool of
//! them (§4.13).
//!
//! **LSP is enrichment, never a dependency.** A language server gives
//! type-aware answers the index cannot — the real definition behind an
//! overloaded name, every reference including the ones reached through a
//! trait — but no part of the runtime may require one. Most machines have
//! none installed for most languages, and the ones that are installed
//! crash, hang, and take a minute to warm up. So every entry point in
//! [`LspPool`] returns an empty answer rather than an error, and the
//! caller merges what it gets with index-derived results, each marked with
//! its [`ResultSource`] so ranking can prefer the higher-fidelity one.
//!
//! The client is generic over its streams rather than tied to a child
//! process, which is what makes lifecycle, timeouts, crash handling and
//! malformed input testable without any server being installed.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod client;
pub mod error;
pub mod framing;
pub mod model;
pub mod pool;
pub mod server;
pub mod uri;

pub use client::{LspClient, DEFAULT_INITIALIZE_TIMEOUT, DEFAULT_REQUEST_TIMEOUT};
pub use error::{LspError, Result};
pub use model::{
    Diagnostic, Location, Position, ResultSource, ServerCapabilities, Severity, SymbolInfo,
};
pub use pool::{LspPool, PoolConfig, Unavailable};
pub use server::{spec_for, ServerSpec, DEFAULT_SERVERS};
