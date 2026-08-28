//! `valyria-edit` — layer 3 (Execution).
//!
//! The editing engine (§19): the full strategy ladder, tried in order of
//! precision — exact replacement, unified diff, symbol-aware replacement,
//! AST transformation, whole-file replacement — wrapped in a transaction
//! that enforces the optimistic-concurrency precondition (D6), refuses an
//! edit that introduces syntax errors into a file that parsed cleanly
//! (§4.11), and produces a uniform diff for every strategy so a caller can
//! verify the expected change actually occurred.
//!
//! The upper rungs of the ladder need a parser, which arrives as a
//! [`LanguageRegistry`](valyria_lang::LanguageRegistry) via
//! [`EditTransaction::with_languages`]. Without one the lower rungs still
//! work and the upper ones report
//! [`EditError::LanguageUnavailable`] rather than silently degrading to a
//! text-based approximation.

#![forbid(unsafe_code)]

pub mod ast;
pub mod error;
pub mod strategy;
pub mod transaction;

pub use ast::AstTransform;
pub use error::{EditError, Result};
pub use strategy::{apply_strategy, EditContext, EditStrategy};
pub use transaction::{EditOutcome, EditRequest, EditTransaction, Precondition};
