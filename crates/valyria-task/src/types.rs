//! Task and journal domain types (§4.23, D1).
//!
//! `JournalEntryKind`'s effect payloads are deliberately raw JSON, not
//! richer types — the effects an agent issues (model calls, tool calls)
//! are defined above this crate's layer, and `valyria-task` must stay a
//! layer-5 leaf that `valyria-agent` (also layer 5) can depend on without a
//! cycle. The [`kinds`] module is the single source of truth for the
//! `effect_kind`/`outcome_kind` string tags both crates use, so they never
//! drift out of sync.

use serde::{Deserialize, Serialize};
use valyria_types::{AgentState, EffectId, Generation, StepId, TaskId, Timestamp, WorkspaceId};

/// String tags used in [`JournalEntryKind::EffectIssued::effect_kind`] and
/// [`JournalEntryKind::EffectCompleted::outcome_kind`]. Kept as constants
/// (rather than a closed enum) because the journal itself is meant to stay
/// agent-agnostic — `valyria-task` records these strings without knowing
/// what they mean — but a single shared source avoids typos at the two
/// crates (`valyria-task`, `valyria-agent`) that do care.
pub mod kinds {
    pub const MODEL_CALL: &str = "model_call";
    pub const MODEL_COMPLETION: &str = "model_completion";
    pub const TOOL: &str = "tool";
    pub const TOOL_RESULT: &str = "tool_result";
    pub const TOOL_DENIED: &str = "tool_denied";
    pub const PERMISSION_ASK: &str = "permission_ask";
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: TaskId,
    pub workspace_id: WorkspaceId,
    pub parent_task: Option<TaskId>,
    pub objective: String,
    pub state: AgentState,
    /// The state this task was in when it was last paused, so resuming can
    /// be checked against it (`state.rs`'s module docs: the abstract
    /// transition table alone cannot enforce "resume returns to where it
    /// was paused from" — that's this field's job).
    pub paused_from: Option<AgentState>,
    pub plan_scope: Vec<String>,
    pub budget: Budget,
    pub index_generation_at_start: Option<Generation>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
    pub recovery_note: Option<String>,
    /// A durable pause/cancel request, checked by `AgentDriver::run` at the
    /// top of every loop iteration (which already re-fetches the task every
    /// time). Durable rather than an in-memory channel *on purpose*: a
    /// `valyria task pause <id>` CLI invocation is a separate process from
    /// whichever `valyria run` invocation is actually driving the task
    /// (D11's embedded model, Phase 3 — there is no daemon yet to hold a
    /// live in-memory handle across processes), so the only thing two
    /// separate processes share is this row. Cleared automatically by
    /// `TaskManager::transition` on every transition, since a signal that
    /// produced one has been consumed, and a stale one from an unrelated
    /// old request shouldn't linger past any state change.
    pub pending_signal: Option<ControlSignal>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Budget {
    pub max_steps: Option<u32>,
    pub max_wall_ms: Option<u64>,
    pub max_tokens: Option<u64>,
}

/// A durable position in one task's journal. Unlike `valyria_events::Seq`
/// (global across all tasks), this is scoped per-task, matching the
/// `task_journal` table's `(task_id, seq)` index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JournalSeq(pub u64);

impl JournalSeq {
    pub const ZERO: JournalSeq = JournalSeq(0);
}

impl std::fmt::Display for JournalSeq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEntryKind {
    TaskCreated,
    StateChanged {
        from: AgentState,
        to: AgentState,
    },
    EffectIssued {
        effect_id: EffectId,
        step_id: StepId,
        effect_kind: String,
        payload: serde_json::Value,
    },
    EffectCompleted {
        effect_id: EffectId,
        step_id: StepId,
        outcome_kind: String,
        payload: serde_json::Value,
    },
    RecoveryNote {
        note: String,
    },
}

impl JournalEntryKind {
    pub fn tag(&self) -> &'static str {
        match self {
            JournalEntryKind::TaskCreated => "task_created",
            JournalEntryKind::StateChanged { .. } => "state_changed",
            JournalEntryKind::EffectIssued { .. } => "effect_issued",
            JournalEntryKind::EffectCompleted { .. } => "effect_completed",
            JournalEntryKind::RecoveryNote { .. } => "recovery_note",
        }
    }

    pub fn effect_id(&self) -> Option<EffectId> {
        match self {
            JournalEntryKind::EffectIssued { effect_id, .. }
            | JournalEntryKind::EffectCompleted { effect_id, .. } => Some(*effect_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    pub seq: JournalSeq,
    pub task_id: TaskId,
    pub kind: JournalEntryKind,
    pub created_at: Timestamp,
}

/// What a running driver has been asked to do, checked between steps. See
/// [`Task::pending_signal`] for why this is durable rather than an
/// in-memory channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlSignal {
    PauseRequested,
    CancelRequested,
}

/// A tool call reconstructed from the journal that still needs a driver
/// follow-up before the task can proceed — either because it's awaiting an
/// explicit permission decision, or because a crash interrupted it before
/// any completion was recorded. See `TaskManager::pending_tool_call` and
/// `TaskManager::interrupted_tool_call`.
///
/// `effect_id` is the *original* `EffectIssued`'s id. A permission-ask
/// resolution must complete that same id (a fresh `EffectCompleted` for it
/// supersedes the earlier `permission_ask` one) rather than mint an
/// unrelated one, or `pending_tool_call` would never see it as resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingToolCall {
    pub effect_id: EffectId,
    pub step_id: StepId,
    pub tool: String,
    pub input: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_entry_kind_tag_matches_variant() {
        assert_eq!(JournalEntryKind::TaskCreated.tag(), "task_created");
        assert_eq!(
            JournalEntryKind::StateChanged {
                from: AgentState::Idle,
                to: AgentState::Understanding
            }
            .tag(),
            "state_changed"
        );
    }

    #[test]
    fn effect_id_present_only_on_effect_variants() {
        assert_eq!(JournalEntryKind::TaskCreated.effect_id(), None);
        let id = EffectId::new();
        let issued = JournalEntryKind::EffectIssued {
            effect_id: id,
            step_id: StepId::new(),
            effect_kind: kinds::TOOL.into(),
            payload: serde_json::json!({}),
        };
        assert_eq!(issued.effect_id(), Some(id));
    }

    #[test]
    fn journal_entry_kind_serde_round_trip() {
        let kind = JournalEntryKind::EffectCompleted {
            effect_id: EffectId::new(),
            step_id: StepId::new(),
            outcome_kind: kinds::TOOL_RESULT.into(),
            payload: serde_json::json!({"success": true}),
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: JournalEntryKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }
}
