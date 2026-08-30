//! `valyria-model-store` — layer 4 (Model).
//!
//! Download, resume, integrity verification, license surfacing, probe, and
//! disk reclamation for model weights (§4.21, §37, §40). The install flow
//! is never silent and never partial-on-success:
//!
//! ```text
//! plan_install ──▶ InstallPlan { size, license, hardware fit }
//!      │            (caller acknowledges → .confirm())
//!      ▼
//! install ──▶ resumable chunked download (.part)
//!      │  ──▶ whole-file blake3 check  (mismatch ⇒ delete, hard error)
//!      │  ──▶ probe (load + generate)   (via the Prober seam)
//!      │  ──▶ manifest.json
//!      ▼
//! Manifest
//! ```
//!
//! HTTP itself is behind the [`Fetcher`] trait so every path above is
//! exercised offline against [`InMemoryFetcher`]. The real `reqwest` +
//! `rustls` implementation ([`HttpFetcher`]) is compiled by the default
//! `http` feature; turn it off (`--no-default-features`) for a build with
//! no TLS stack at all.

#![forbid(unsafe_code)]

pub mod db;
pub mod error;
pub mod fetch;
#[cfg(feature = "http")]
pub mod http;
pub mod manifest;
pub mod probe;
pub mod store;

pub use db::{InstalledModelRow, InstalledModelStore, MIGRATIONS};
pub use error::{ModelStoreError, Result};
pub use fetch::{Fetcher, InMemoryFetcher, RemoteObject};
#[cfg(feature = "http")]
pub use http::HttpFetcher;
pub use manifest::{Manifest, MANIFEST_FILENAME};
pub use probe::{NullProber, ProbeResult, Prober};
pub use store::{GcReport, InstallPhase, InstallPlan, InstallProgress, ModelStore, StorageReport};

/// Kept for backwards compatibility with the scaffold; the crate is now
/// implemented.
pub const PHASE: u8 = 9;
