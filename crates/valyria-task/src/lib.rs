//! `valyria-task` — layer 5 (Agent).
//!
//! The task manager (§4.23): task lifecycle, the durable per-task journal
//! (D1), its projection into `valyria-events` (§4.2), crash recovery on
//! startup, and pause/cancel signaling into a running `AgentDriver`. This
//! crate owns the only write path to `tasks.state` and `task_journal` —
//! `valyria-agent` drives a task's *behavior* but never touches its
//! persisted state directly.

#![forbid(unsafe_code)]

mod codec;
pub mod error;
pub mod manager;
pub mod migrations;
pub mod types;

pub use error::{Result, TaskError};
pub use manager::TaskManager;
pub use migrations::MIGRATIONS;
pub use types::{
    kinds, Budget, ControlSignal, JournalEntry, JournalEntryKind, JournalSeq, PendingToolCall, Task,
};
