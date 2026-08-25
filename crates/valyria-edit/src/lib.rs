//! `valyria-edit` — layer 3 (Execution).
//!
//! The editing engine (§19): the strategy ladder from exact replacement
//! through whole-file replacement (symbol-aware and AST transformation are
//! placeholders pending the layer-2 index/parser crates), wrapped in a
//! transaction that enforces the optimistic-concurrency precondition (D6)
//! and produces a uniform diff for every strategy so a caller can verify
//! the expected change actually occurred (§19).

#![forbid(unsafe_code)]

pub mod error;
pub mod strategy;
pub mod transaction;

pub use error::{EditError, Result};
pub use strategy::{apply_strategy, EditStrategy};
pub use transaction::{EditOutcome, EditRequest, EditTransaction, Precondition};
