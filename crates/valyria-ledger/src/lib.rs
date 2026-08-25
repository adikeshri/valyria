//! `valyria-ledger` — layer 3 (Execution).
//!
//! The change ledger (§26) and user-change protection (§25): every
//! agent-owned modification maps to the task/step/tool-invocation that
//! made it, and the ledger can classify a newly observed file state as
//! agent-authored, pre-existing, or a concurrent user modification —
//! which is what lets rollback refuse to blindly overwrite work that
//! happened after the point it's reverting to.

#![forbid(unsafe_code)]

pub mod error;
pub mod ledger;
pub mod types;

pub use error::{LedgerError, Result};
pub use ledger::Ledger;
pub use types::{AgentFileState, ChangeClassification, FileBaseline, LedgerEntry};
