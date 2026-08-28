//! `valyria-index` — layer 2 (Repository intelligence).
//!
//! The repository index: what files exist, what they define, what they
//! import, what they call, and what tests them — persisted, queryable, and
//! **generational**.
//!
//! Generations are the load-bearing idea (D8). Every row records the
//! generation range it was true for, so a read at generation `g` sees
//! exactly the repository as it was when `g` was published, however much
//! the index has moved on since. A long agent step therefore never has the
//! index shift underneath it, and "was this action planned against stale
//! context?" (§8) becomes a comparison of two integers rather than a
//! guess.
//!
//! The write path has one entry point ([`IndexStore::write_generation`])
//! and three callers: a full [`bootstrap`](IndexPipeline::bootstrap), an
//! incremental [`apply_paths`](IndexPipeline::apply_paths), and a
//! [`resync`](IndexPipeline::resync) for bulk changes like a branch
//! switch. [`verify_index`] rebuilds independently and diffs, because
//! index drift has no symptom of its own.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod error;
pub mod migrations;
pub mod pipeline;
pub mod record;
pub mod scan;
pub mod store;
pub mod verify;

pub use error::{IndexError, Result};
pub use migrations::MIGRATIONS;
pub use pipeline::{IndexPipeline, IndexProgress};
pub use record::{
    CallRecord, FileRecord, GenerationInfo, GenerationStage, ImportRecord, IndexDelta, IndexStats,
    RelPath, SymbolRecord, TestRecord,
};
pub use scan::{scan_paths, scan_workspace, ScanOptions, ScanProgress, ScannedFile};
pub use store::{IndexStore, PublishOptions};
pub use verify::{verify_index, IndexDrift, SymbolMismatch};
