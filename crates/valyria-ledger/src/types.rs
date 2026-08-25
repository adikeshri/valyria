//! The ledger's core data shapes (§25, §26).

use std::path::PathBuf;

use valyria_types::{LedgerEntryId, StepId, TaskId, Timestamp, ToolInvocationId};
use valyria_util::ContentHash;

/// One agent-owned modification, mapping to the task/step/tool-invocation
/// that made it — the audit trail §26 requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    pub id: LedgerEntryId,
    pub task_id: TaskId,
    pub step_id: StepId,
    pub tool_invocation_id: Option<ToolInvocationId>,
    pub path: PathBuf,
    /// `None` means the path did not exist before this entry.
    pub before_hash: Option<ContentHash>,
    /// `None` means this entry deleted the path — distinct from "the file
    /// now contains zero bytes", which is `Some(hash_of_empty_content)`.
    pub after_hash: Option<ContentHash>,
    pub timestamp: Timestamp,
    /// Whether this entry's before-content was retained in the blob store
    /// (so it can actually be rolled back to, not just described).
    pub content_retained: bool,
    /// Set when this entry is itself a rollback — the entry it reverted.
    pub reverts: Option<LedgerEntryId>,
}

/// What the ledger knows about one file's history within the current task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBaseline {
    pub path: PathBuf,
    /// The file's hash when the task began touching it, or `None` if the
    /// file didn't exist yet — captures whatever "pre-existing user
    /// change" state already existed at that point; the ledger doesn't
    /// need to separately detect that, since it's baked into this value.
    pub hash_at_task_start: Option<ContentHash>,
    /// What the agent believes this path looks like right now, from its
    /// own actions alone. `Untouched` until the agent's first write or
    /// delete of this path.
    pub agent_state: AgentFileState,
}

/// The agent's own view of a path's current content, distinct from a
/// bare `Option<ContentHash>` so "the agent deleted this file" and "the
/// agent has never touched this file" aren't conflated into the same
/// `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFileState {
    Untouched,
    Written(ContentHash),
    Deleted,
}

/// Who is responsible for the difference between a baseline and a newly
/// observed hash (§25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeClassification {
    /// Matches the agent's own last write — no surprise.
    AgentAuthored,
    /// The agent never touched this path; whatever it looks like is
    /// whatever it looked like at task start.
    PreExisting,
    /// The file changed since the agent's last write (or since task
    /// start, if the agent never wrote it) without a corresponding ledger
    /// entry — someone or something other than the agent touched it.
    ConcurrentUserModification,
    /// No baseline was ever recorded for this path, so nothing can be
    /// said about it — the caller should record one before trusting any
    /// classification here.
    Unknown,
}
