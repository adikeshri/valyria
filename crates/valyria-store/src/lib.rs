//! `valyria-store` — layer 0 (Foundation).
//!
//! The state substrate (D7): a single-writer SQLite actor for structured,
//! transactional state (tasks, journal, ledger, evidence, index metadata),
//! plus a content-addressed blob store for everything too large to want in
//! a database row. Forward-only migrations only, tracked in
//! `schema_migrations` — see the build plan's storage section (§48) for the
//! on-disk layout this crate implements.

#![forbid(unsafe_code)]

pub mod actor;
pub mod blob;
pub mod error;
pub mod migrations;

pub use actor::Store;
pub use blob::{BlobStore, BlobStoreReport};
pub use error::{Result, StoreError};
pub use migrations::{applied_versions, run_migrations, Migration};
