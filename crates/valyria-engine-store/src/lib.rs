//! `valyria-engine-store` — layer 4 (Model).
//!
//! Download, verify, and unpack the local inference engine (llama.cpp's
//! `llama-server`) so a user never runs `brew install` or hunts for a
//! binary. Mirrors `valyria-model-store`'s discipline for weights: a
//! pinned, offline-describable catalog; a resumable chunked download;
//! whole-archive blake3 verification (mismatch deletes the bytes); and a
//! small on-disk manifest so a later `resolve` is free.
//!
//! ```text
//! Catalog::embedded() ──▶ EngineTarget for (os, arch)
//!      │
//!      ▼
//! EngineStore::install_with_progress ──▶ resumable chunked download (.part)
//!      │  ──▶ whole-archive blake3 check  (mismatch ⇒ delete, hard error)
//!      │  ──▶ unpack (tar.gz / zip)
//!      │  ──▶ engine-manifest.json
//!      ▼
//! PathBuf to the `llama-server` binary
//! ```
//!
//! HTTP is behind the [`Fetcher`] trait, same seam as `valyria-model-store`
//! (deliberately not shared — this crate owns its own error type so an
//! engine-download failure is never mistaken for a weights-download
//! failure). The real `reqwest` + `rustls` implementation ([`HttpFetcher`])
//! is compiled by the default `http` feature.

#![forbid(unsafe_code)]

pub mod archive;
pub mod catalog;
pub mod error;
pub mod fetch;
pub mod store;

pub use catalog::{ArchiveKind, Catalog, EngineEntry, EngineTarget};
pub use error::{EngineStoreError, Result};
#[cfg(feature = "http")]
pub use fetch::HttpFetcher;
pub use fetch::{Fetcher, InMemoryFetcher};
pub use store::{EngineStore, InstallPhase, InstallProgress};
