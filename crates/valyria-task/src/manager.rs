//! `TaskManager` (§4.23): the only write path to task state, the journal,
//! and their projection into the event bus (§4.2 — events are projections
//! of the journal, never a parallel mechanism).

use std::sync::Arc;

use rusqlite::OptionalExtension;
use valyria_events::{EventBus, EventKind, NewEvent};
use valyria_store::Store;
use valyria_types::{AgentState, Generation, TaskId, WorkspaceId};
use valyria_util::Clock;

use crate::codec::{signal_from_text, signal_to_text, state_from_text, state_to_text};
use crate::error::{Result, TaskError};
use crate::types::{
    kinds, Budget, ControlSignal, JournalEntry, JournalEntryKind, JournalSeq, PendingToolCall, Task,
};

/// States a task can sit in indefinitely with no driver running, by
/// design: terminal states, `Paused` itself, and the two "waiting on
/// external input" states. `recover_incomplete_tasks` must never treat
/// finding a task in one of these as evidence of a crash — that's exactly
/// what a task looks like the moment `AgentDriver::run` returns normally
/// after reaching one of them.
fn is_stable_without_a_driver(state: AgentState) -> bool {
    state.is_terminal()
        || matches!(
            state,
            AgentState::Paused | AgentState::WaitingForPermission | AgentState::WaitingForUser
        )
}

pub struct TaskManager {
    store: Arc<Store>,
    events: Arc<EventBus>,
    clock: Arc<dyn Clock>,
}

impl TaskManager {
    pub fn new(store: Arc<Store>, events: Arc<EventBus>, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            events,
            clock,
        }
    }

    pub async fn create(
        &self,
        workspace_id: WorkspaceId,
        objective: String,
        budget: Budget,
    ) -> Result<Task> {
        let id = TaskId::new();
        let now = self.clock.now();
        let task = Task {
            id,
            workspace_id,
            parent_task: None,
            objective,
            state: AgentState::Idle,
            paused_from: None,
            plan_scope: Vec::new(),
            budget,
            index_generation_at_start: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
            recovery_note: None,
            pending_signal: None,
        };

        let id_str = task.id.to_string();
        let ws_str = task.workspace_id.to_string();
        let objective = task.objective.clone();
        let state_text = state_to_text(task.state);
        let plan_scope_json = serde_json::to_string(&task.plan_scope)?;
        let created_ms = task.created_at.as_millis() as i64;
        let updated_ms = task.updated_at.as_millis() as i64;
        let bms = task.budget.max_steps;
        let bmw = task.budget.max_wall_ms.map(|v| v as i64);
        let bmt = task.budget.max_tokens.map(|v| v as i64);

        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO tasks (id, workspace_id, parent_task, objective, state, \
                     paused_from, plan_scope, budget_max_steps, budget_max_wall_ms, \
                     budget_max_tokens, index_generation_at_start, created_at_ms, \
                     updated_at_ms, completed_at_ms, recovery_note) \
                     VALUES (?1,?2,NULL,?3,?4,NULL,?5,?6,?7,?8,NULL,?9,?10,NULL,NULL)",
                    rusqlite::params![
                        id_str,
                        ws_str,
                        objective,
                        state_text,
                        plan_scope_json,
                        bms,
                        bmw,
                        bmt,
                        created_ms,
                        updated_ms,
                    ],
                )?;
                Ok(())
            })
            .await?;

        self.append_journal(id, JournalEntryKind::TaskCreated)
            .await?;
        Ok(task)
    }

    pub async fn get(&self, id: TaskId) -> Result<Task> {
        let id_str = id.to_string();
        let row = self
            .store
            .call(move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT workspace_id, parent_task, objective, state, paused_from, \
                         plan_scope, budget_max_steps, budget_max_wall_ms, budget_max_tokens, \
                         index_generation_at_start, created_at_ms, updated_at_ms, \
                         completed_at_ms, recovery_note, pending_signal FROM tasks WHERE id = ?1",
                        [&id_str],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Option<String>>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, Option<String>>(4)?,
                                row.get::<_, String>(5)?,
                                row.get::<_, Option<i64>>(6)?,
                                row.get::<_, Option<i64>>(7)?,
                                row.get::<_, Option<i64>>(8)?,
                                row.get::<_, Option<i64>>(9)?,
                                row.get::<_, i64>(10)?,
                                row.get::<_, i64>(11)?,
                                row.get::<_, Option<i64>>(12)?,
                                row.get::<_, Option<String>>(13)?,
                                row.get::<_, Option<String>>(14)?,
                            ))
                        },
                    )
                    .optional()?)
            })
            .await?;

        let Some((
            workspace_id,
            parent_task,
            objective,
            state,
            paused_from,
            plan_scope,
            bms,
            bmw,
            bmt,
            generation,
            created_ms,
            updated_ms,
            completed_ms,
            recovery_note,
            pending_signal,
        )) = row
        else {
            return Err(TaskError::NotFound(id));
        };

        Ok(Task {
            id,
            workspace_id: workspace_id
                .parse()
                .map_err(|_| TaskError::CorruptId(workspace_id))?,
            parent_task: parent_task.and_then(|s| s.parse().ok()),
            objective,
            state: state_from_text(&state).ok_or_else(|| TaskError::CorruptState {
                task: id,
                raw: state.clone(),
            })?,
            paused_from: paused_from.as_deref().and_then(state_from_text),
            plan_scope: serde_json::from_str(&plan_scope)?,
            budget: Budget {
                max_steps: bms.map(|v| v as u32),
                max_wall_ms: bmw.map(|v| v as u64),
                max_tokens: bmt.map(|v| v as u64),
            },
            index_generation_at_start: generation.map(|v| Generation(v as u64)),
            created_at: valyria_types::Timestamp::from_millis(created_ms as u128),
            updated_at: valyria_types::Timestamp::from_millis(updated_ms as u128),
            completed_at: completed_ms.map(|v| valyria_types::Timestamp::from_millis(v as u128)),
            recovery_note,
            pending_signal: pending_signal.as_deref().and_then(signal_from_text),
        })
    }

    pub async fn list(&self, workspace_id: WorkspaceId) -> Result<Vec<Task>> {
        let ws_str = workspace_id.to_string();
        let ids = self
            .store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id FROM tasks WHERE workspace_id = ?1 ORDER BY created_at_ms ASC",
                )?;
                let ids = stmt
                    .query_map([&ws_str], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(ids)
            })
            .await?;

        let mut tasks = Vec::with_capacity(ids.len());
        for id_str in ids {
            let id: TaskId = id_str.parse().map_err(|_| TaskError::CorruptId(id_str))?;
            tasks.push(self.get(id).await?);
        }
        Ok(tasks)
    }

    /// The only write path to `tasks.state`. Checks
    /// `from.can_transition_to(to)` (the abstract graph) plus, for a
    /// `Paused -> X` resume, that `X` is exactly the state this task was
    /// paused from — `AgentState`'s own transition table cannot enforce
    /// that half (see `state.rs`'s module docs).
    pub async fn transition(&self, id: TaskId, to: AgentState) -> Result<()> {
        let task = self.get(id).await?;
        let from = task.state;

        if from == AgentState::Paused {
            match task.paused_from {
                Some(expected) if expected == to => {}
                Some(expected) => {
                    return Err(TaskError::WrongResumeTarget {
                        task: id,
                        expected,
                        actual: to,
                    })
                }
                None => return Err(TaskError::IllegalTransition { task: id, from, to }),
            }
        }
        if !from.can_transition_to(to) {
            return Err(TaskError::IllegalTransition { task: id, from, to });
        }

        let now = self.clock.now();
        let new_paused_from = (to == AgentState::Paused).then_some(from);
        let completed_at = if to.is_terminal() {
            Some(now)
        } else {
            task.completed_at
        };

        let id_str = id.to_string();
        let to_text = state_to_text(to);
        let paused_from_text = new_paused_from.map(state_to_text);
        let now_ms = now.as_millis() as i64;
        let completed_ms = completed_at.map(|t| t.as_millis() as i64);

        self.store
            .call(move |conn| {
                // Clearing `pending_signal` here, unconditionally, is
                // deliberate: whatever signal led to (or was pending during)
                // this transition has been consumed, and a stale signal from
                // an unrelated earlier request shouldn't linger past any
                // state change.
                conn.execute(
                    "UPDATE tasks SET state = ?1, paused_from = ?2, updated_at_ms = ?3, \
                     completed_at_ms = ?4, pending_signal = NULL WHERE id = ?5",
                    rusqlite::params![to_text, paused_from_text, now_ms, completed_ms, id_str],
                )?;
                Ok(())
            })
            .await?;

        self.append_journal(id, JournalEntryKind::StateChanged { from, to })
            .await?;
        Ok(())
    }

    pub async fn append_journal(&self, id: TaskId, kind: JournalEntryKind) -> Result<JournalEntry> {
        let now = self.clock.now();
        let tag = kind.tag().to_string();
        let effect_id = kind.effect_id().map(|e| e.to_string());
        let payload_json = serde_json::to_string(&kind)?;
        let id_str = id.to_string();
        let now_ms = now.as_millis() as i64;

        let seq = self
            .store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO task_journal (task_id, kind, effect_id, payload, created_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id_str, tag, effect_id, payload_json, now_ms],
                )?;
                Ok(JournalSeq(conn.last_insert_rowid() as u64))
            })
            .await?;

        let entry = JournalEntry {
            seq,
            task_id: id,
            kind,
            created_at: now,
        };
        self.project_events(id, &entry.kind).await?;
        Ok(entry)
    }

    pub async fn journal_since(&self, id: TaskId, since: JournalSeq) -> Result<Vec<JournalEntry>> {
        let id_str = id.to_string();
        let since_val = since.0 as i64;
        let rows = self
            .store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT seq, payload, created_at_ms FROM task_journal \
                     WHERE task_id = ?1 AND seq > ?2 ORDER BY seq ASC",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![id_str, since_val], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;

        rows.into_iter()
            .map(|(seq, payload, created_at_ms)| -> Result<JournalEntry> {
                let kind: JournalEntryKind = serde_json::from_str(&payload)?;
                Ok(JournalEntry {
                    seq: JournalSeq(seq as u64),
                    task_id: id,
                    kind,
                    created_at: valyria_types::Timestamp::from_millis(created_at_ms as u128),
                })
            })
            .collect()
    }

    /// How many `Reason` steps this task has completed — the turn index the
    /// step driver hands to the model on the next call. Derived from the
    /// durable journal, not cached, so it's correct immediately after a
    /// crash-recovery resume with no special-casing.
    pub async fn count_model_calls(&self, id: TaskId) -> Result<usize> {
        let entries = self.journal_since(id, JournalSeq::ZERO).await?;
        Ok(entries
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    JournalEntryKind::EffectCompleted { outcome_kind, .. }
                        if outcome_kind == kinds::MODEL_COMPLETION
                )
            })
            .count())
    }

    /// The most recent tool call whose `EffectIssued` is followed by an
    /// `EffectCompleted{outcome_kind: "permission_ask"}` with nothing since
    /// superseding it — i.e. a task sitting in `WAITING_FOR_PERMISSION`
    /// waiting on an explicit `permission.resolve`. `None` once that call
    /// has been resolved (a later `EffectCompleted` for the same
    /// `effect_id` exists).
    pub async fn pending_tool_call(&self, id: TaskId) -> Result<Option<PendingToolCall>> {
        self.last_unresolved_tool_call(id, Some(kinds::PERMISSION_ASK))
            .await
    }

    /// The most recent tool call whose `EffectIssued` has **no** matching
    /// `EffectCompleted` at all — i.e. a crash landed strictly between
    /// issuing the effect and recording its outcome (D1: "re-issue any
    /// effect that was issued but never completed"). The driver redoes
    /// this call automatically on resume, before asking the model anything
    /// new; unlike `pending_tool_call`, this never requires user input.
    pub async fn interrupted_tool_call(&self, id: TaskId) -> Result<Option<PendingToolCall>> {
        self.last_unresolved_tool_call(id, None).await
    }

    async fn last_unresolved_tool_call(
        &self,
        id: TaskId,
        required_outcome: Option<&str>,
    ) -> Result<Option<PendingToolCall>> {
        let entries = self.journal_since(id, JournalSeq::ZERO).await?;

        let Some((issued_idx, effect_id, step_id, tool, input)) = entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, e)| match &e.kind {
                JournalEntryKind::EffectIssued {
                    effect_id,
                    step_id,
                    effect_kind,
                    payload,
                } if effect_kind == kinds::TOOL => {
                    let tool = payload.get("tool")?.as_str()?.to_string();
                    let input = payload.get("input")?.clone();
                    Some((i, *effect_id, *step_id, tool, input))
                }
                _ => None,
            })
        else {
            return Ok(None);
        };

        // The *last* completion for this effect_id, not the first — a
        // resolved permission ask appends a fresh `tool_result`/
        // `tool_denied` EffectCompleted that supersedes the earlier
        // `permission_ask` one rather than overwriting it (the journal is
        // append-only), so "what's the current status" means "most recent".
        let completion_outcome =
            entries[issued_idx + 1..]
                .iter()
                .rev()
                .find_map(|e| match &e.kind {
                    JournalEntryKind::EffectCompleted {
                        effect_id: eid,
                        outcome_kind,
                        ..
                    } if *eid == effect_id => Some(outcome_kind.clone()),
                    _ => None,
                });

        let matches = match (&completion_outcome, required_outcome) {
            (None, None) => true,
            (Some(outcome), Some(required)) => outcome == required,
            _ => false,
        };

        Ok(matches.then_some(PendingToolCall {
            effect_id,
            step_id,
            tool,
            input,
        }))
    }

    /// §4.23 crash recovery, scoped to *one* task: if it's not currently in
    /// a state where a driver should have been actively working it — i.e.
    /// not terminal, and not one of the stable "no driver is expected to
    /// be running right now" states (`Paused` itself;
    /// `WaitingForPermission`/`WaitingForUser`, which are deliberate
    /// at-rest states pending external input, not crash artifacts) — it's
    /// journaled with a recovery note and moved to `Paused`. Returns
    /// whether it actually needed recovering.
    ///
    /// Deliberately scoped to one task rather than scanning the whole
    /// workspace: in the embedded, no-daemon model (Phase 3), *every* CLI
    /// invocation opens its own `Runtime` against the same shared
    /// `workspace.db`, including ones with no intention of driving
    /// anything (`task status`, `task pause`) and ones actively driving a
    /// *different* task. A workspace-wide scan has no way to tell "this
    /// task's driver crashed" apart from "this task's driver is alive and
    /// well, right now, in a different OS process" — there is no liveness
    /// tracking (no PID, no heartbeat) to distinguish them — so it would
    /// spuriously force-pause a task another live process is mid-step on.
    /// Scoping to the one task id the caller (`resume_task`) explicitly
    /// names sidesteps the ambiguity entirely: resuming a specific task is
    /// already an explicit, deliberate signal from the user that *this*
    /// task in particular is believed stuck.
    pub async fn recover_task_if_active(&self, id: TaskId) -> Result<bool> {
        let task = self.get(id).await?;
        if is_stable_without_a_driver(task.state) {
            return Ok(false);
        }
        let note = format!(
            "recovered on resume: task was in {} when last observed",
            task.state
        );
        self.recover_task(id, note).await?;
        Ok(true)
    }

    /// Workspace-wide crash recovery: every task left in an active,
    /// non-stable state is recovered the same way
    /// [`TaskManager::recover_task_if_active`] recovers one. Useful only
    /// where the caller can guarantee it is the *sole* process touching
    /// this workspace's tasks right now (e.g. a future daemon's startup) —
    /// safe for that case, actively harmful otherwise (see
    /// `recover_task_if_active`'s docs). Phase 3's `valyria-app::Runtime`
    /// deliberately does **not** call this on every `open()`; only
    /// `resume_task` recovers, and only the one task it was asked to
    /// resume.
    pub async fn recover_incomplete_tasks(&self) -> Result<Vec<TaskId>> {
        let rows = self
            .store
            .call(|conn| {
                let mut stmt = conn.prepare("SELECT id, state FROM tasks")?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;

        let mut recovered = Vec::new();
        for (id_str, state_str) in rows {
            let Ok(id) = id_str.parse::<TaskId>() else {
                continue;
            };
            let Some(state) = state_from_text(&state_str) else {
                continue;
            };
            if is_stable_without_a_driver(state) {
                continue;
            }
            let note =
                format!("recovered on startup: task was in {state} when the runtime last stopped");
            self.recover_task(id, note).await?;
            recovered.push(id);
        }
        Ok(recovered)
    }

    async fn recover_task(&self, id: TaskId, note: String) -> Result<()> {
        let id_str = id.to_string();
        let note_for_row = note.clone();
        self.store
            .call(move |conn| {
                conn.execute(
                    "UPDATE tasks SET recovery_note = ?1 WHERE id = ?2",
                    rusqlite::params![note_for_row, id_str],
                )?;
                Ok(())
            })
            .await?;
        self.append_journal(id, JournalEntryKind::RecoveryNote { note })
            .await?;
        self.transition(id, AgentState::Paused).await?;
        Ok(())
    }

    /// Durably requests a pause (see [`crate::types::Task::pending_signal`]
    /// for why this goes through the row rather than an in-memory channel).
    /// Succeeds even if no driver is currently running this task in any
    /// process — the signal simply waits in the row until one is (or
    /// until `open()`'s crash recovery pauses it directly, superseding the
    /// request).
    pub async fn request_pause(&self, id: TaskId) -> Result<()> {
        self.set_pending_signal(id, ControlSignal::PauseRequested)
            .await
    }

    pub async fn request_cancel(&self, id: TaskId) -> Result<()> {
        self.set_pending_signal(id, ControlSignal::CancelRequested)
            .await
    }

    async fn set_pending_signal(&self, id: TaskId, signal: ControlSignal) -> Result<()> {
        // Confirms the task exists before writing (a bare UPDATE on a
        // missing id would silently affect zero rows).
        self.get(id).await?;
        let id_str = id.to_string();
        let signal_text = signal_to_text(signal);
        self.store
            .call(move |conn| {
                conn.execute(
                    "UPDATE tasks SET pending_signal = ?1 WHERE id = ?2",
                    rusqlite::params![signal_text, id_str],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn project_events(&self, task_id: TaskId, kind: &JournalEntryKind) -> Result<()> {
        match kind {
            JournalEntryKind::TaskCreated => {
                self.emit(task_id, EventKind::TaskStarted, serde_json::json!({}))
                    .await?;
            }
            JournalEntryKind::StateChanged { from, to } => {
                self.emit(
                    task_id,
                    EventKind::StateChanged,
                    serde_json::json!({"from": from.to_string(), "to": to.to_string()}),
                )
                .await?;
                let terminal = match to {
                    AgentState::Completed => Some(EventKind::TaskCompleted),
                    AgentState::Failed => Some(EventKind::TaskFailed),
                    AgentState::Paused => Some(EventKind::TaskPaused),
                    _ => None,
                };
                if let Some(k) = terminal {
                    self.emit(task_id, k, serde_json::json!({})).await?;
                }
            }
            JournalEntryKind::EffectIssued {
                effect_kind,
                payload,
                ..
            } => match effect_kind.as_str() {
                kinds::MODEL_CALL => {
                    self.emit(task_id, EventKind::ModelStarted, payload.clone())
                        .await?;
                }
                kinds::TOOL => {
                    self.emit(task_id, EventKind::ToolStarted, payload.clone())
                        .await?;
                }
                kinds::VERIFY => {
                    self.emit(task_id, EventKind::TestStarted, payload.clone())
                        .await?;
                }
                kinds::PLAN_ACCEPTED => {
                    self.emit(task_id, EventKind::PlanCreated, payload.clone())
                        .await?;
                }
                _ => {}
            },
            JournalEntryKind::EffectCompleted {
                outcome_kind,
                payload,
                ..
            } => match outcome_kind.as_str() {
                kinds::MODEL_COMPLETION => {
                    self.emit(task_id, EventKind::ModelCompleted, payload.clone())
                        .await?;
                }
                kinds::TOOL_RESULT | kinds::TOOL_DENIED => {
                    self.emit(task_id, EventKind::ToolCompleted, payload.clone())
                        .await?;
                }
                kinds::PERMISSION_ASK => {
                    self.emit(task_id, EventKind::ApprovalRequested, payload.clone())
                        .await?;
                }
                kinds::VERIFY_RESULT => {
                    self.emit(task_id, EventKind::VerificationEvidence, payload.clone())
                        .await?;
                    let passed = payload
                        .get("passed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let k = if passed {
                        EventKind::TestPassed
                    } else {
                        EventKind::TestFailed
                    };
                    self.emit(task_id, k, payload.clone()).await?;
                }
                kinds::LOOP_DETECTED => {
                    self.emit(task_id, EventKind::ProgressStalled, payload.clone())
                        .await?;
                }
                kinds::PLAN_SCOPE_EXPANSION => {
                    self.emit(task_id, EventKind::ApprovalRequested, payload.clone())
                        .await?;
                }
                kinds::PLAN_ROLLBACK => {
                    self.emit(task_id, EventKind::FileChanged, payload.clone())
                        .await?;
                }
                _ => {}
            },
            JournalEntryKind::RecoveryNote { .. } => {
                // Durably recorded in the journal for audit purposes, but
                // not projected as its own live event — the accompanying
                // `StateChanged`/`TaskPaused` pair (always issued right
                // after, by `recover_task`) is what a subscribed client
                // sees.
            }
        }
        Ok(())
    }

    async fn emit(
        &self,
        task_id: TaskId,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> Result<()> {
        self.events
            .append(NewEvent::new(kind, payload).for_task(task_id))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_types::{EffectId, StepId, WorkspaceId};
    use valyria_util::FixedClock;

    fn manager() -> TaskManager {
        let mut migrations: Vec<valyria_store::Migration> = valyria_events::MIGRATIONS.to_vec();
        migrations.extend(crate::migrations::MIGRATIONS.iter().copied());
        let store = Arc::new(Store::open_in_memory(&migrations).unwrap());
        let events = Arc::new(EventBus::new(store.clone()));
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::at_millis(1_000_000));
        TaskManager::new(store, events, clock)
    }

    async fn new_task(mgr: &TaskManager) -> Task {
        mgr.create(
            WorkspaceId::new(),
            "add a function".into(),
            Budget::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn create_and_get_round_trip() {
        let mgr = manager();
        let created = new_task(&mgr).await;
        let fetched = mgr.get(created.id).await.unwrap();
        assert_eq!(fetched, created);
        assert_eq!(fetched.state, AgentState::Idle);
    }

    #[tokio::test]
    async fn get_missing_task_errors() {
        let mgr = manager();
        let err = mgr.get(TaskId::new()).await.unwrap_err();
        assert!(matches!(err, TaskError::NotFound(_)));
    }

    #[tokio::test]
    async fn illegal_transition_is_rejected() {
        let mgr = manager();
        let task = new_task(&mgr).await;
        // Idle has no direct edge to Verifying.
        let err = mgr
            .transition(task.id, AgentState::Verifying)
            .await
            .unwrap_err();
        assert!(matches!(err, TaskError::IllegalTransition { .. }));
        assert_eq!(mgr.get(task.id).await.unwrap().state, AgentState::Idle);
    }

    #[tokio::test]
    async fn legal_transition_updates_state_and_paused_from() {
        let mgr = manager();
        let task = new_task(&mgr).await;
        mgr.transition(task.id, AgentState::Understanding)
            .await
            .unwrap();
        let after = mgr.get(task.id).await.unwrap();
        assert_eq!(after.state, AgentState::Understanding);
        assert_eq!(after.paused_from, None);
    }

    #[tokio::test]
    async fn pause_then_resume_enforces_paused_from() {
        let mgr = manager();
        let task = new_task(&mgr).await;
        mgr.transition(task.id, AgentState::Understanding)
            .await
            .unwrap();
        mgr.transition(task.id, AgentState::Paused).await.unwrap();

        let paused = mgr.get(task.id).await.unwrap();
        assert_eq!(paused.state, AgentState::Paused);
        assert_eq!(paused.paused_from, Some(AgentState::Understanding));

        // Resuming to any state other than paused_from is rejected...
        let err = mgr
            .transition(task.id, AgentState::Implementing)
            .await
            .unwrap_err();
        assert!(matches!(err, TaskError::WrongResumeTarget { .. }));

        // ...but resuming to exactly paused_from succeeds.
        mgr.transition(task.id, AgentState::Understanding)
            .await
            .unwrap();
        assert_eq!(
            mgr.get(task.id).await.unwrap().state,
            AgentState::Understanding
        );
    }

    #[tokio::test]
    async fn journal_since_is_ordered_and_gap_free() {
        let mgr = manager();
        let task = new_task(&mgr).await;
        mgr.transition(task.id, AgentState::Understanding)
            .await
            .unwrap();
        mgr.transition(task.id, AgentState::Discovery)
            .await
            .unwrap();
        mgr.transition(task.id, AgentState::Planning).await.unwrap();

        let entries = mgr.journal_since(task.id, JournalSeq::ZERO).await.unwrap();
        assert!(entries.len() >= 4); // TaskCreated + 3 StateChanged
        for pair in entries.windows(2) {
            assert!(pair[0].seq.0 < pair[1].seq.0);
        }

        // A cursor mid-stream returns only what's strictly newer.
        let first_seq = entries[0].seq;
        let rest = mgr.journal_since(task.id, first_seq).await.unwrap();
        assert_eq!(rest.len(), entries.len() - 1);
    }

    #[tokio::test]
    async fn recover_incomplete_tasks_pauses_active_tasks_and_is_idempotent() {
        let mgr = manager();

        let active = new_task(&mgr).await;
        mgr.transition(active.id, AgentState::Understanding)
            .await
            .unwrap();

        let finished = new_task(&mgr).await;
        for state in [
            AgentState::Understanding,
            AgentState::Discovery,
            AgentState::Planning,
            AgentState::Implementing,
            AgentState::Verifying,
            AgentState::Completed,
        ] {
            mgr.transition(finished.id, state).await.unwrap();
        }

        let recovered = mgr.recover_incomplete_tasks().await.unwrap();
        assert_eq!(recovered, vec![active.id]);

        let active_after = mgr.get(active.id).await.unwrap();
        assert_eq!(active_after.state, AgentState::Paused);
        assert_eq!(active_after.paused_from, Some(AgentState::Understanding));
        assert!(active_after.recovery_note.is_some());

        let finished_after = mgr.get(finished.id).await.unwrap();
        assert_eq!(finished_after.state, AgentState::Completed);

        // Idempotent: the now-Paused task is not "incomplete" anymore.
        let recovered_again = mgr.recover_incomplete_tasks().await.unwrap();
        assert!(recovered_again.is_empty());
    }

    #[tokio::test]
    async fn recover_incomplete_tasks_leaves_waiting_states_alone() {
        // Regression test: a task legitimately waiting on external input
        // (WAITING_FOR_PERMISSION/WAITING_FOR_USER) is not a crash
        // artifact — recovery must not disturb it. Caught via a real CLI
        // repro: `valyria run` returns once a task reaches
        // WAITING_FOR_PERMISSION (by design, per docs/PLAN.md's driver
        // loop), and the *next* CLI invocation's `Runtime::open` used to
        // reclassify that legitimate wait as an interrupted task and pause
        // it out from under the pending permission decision.
        let mgr = manager();

        let waiting_for_permission = new_task(&mgr).await;
        mgr.transition(waiting_for_permission.id, AgentState::Understanding)
            .await
            .unwrap();
        mgr.transition(waiting_for_permission.id, AgentState::Discovery)
            .await
            .unwrap();
        mgr.transition(waiting_for_permission.id, AgentState::Planning)
            .await
            .unwrap();
        mgr.transition(waiting_for_permission.id, AgentState::Implementing)
            .await
            .unwrap();
        mgr.transition(waiting_for_permission.id, AgentState::WaitingForPermission)
            .await
            .unwrap();

        let waiting_for_user = new_task(&mgr).await;
        mgr.transition(waiting_for_user.id, AgentState::Understanding)
            .await
            .unwrap();
        mgr.transition(waiting_for_user.id, AgentState::WaitingForUser)
            .await
            .unwrap();

        let recovered = mgr.recover_incomplete_tasks().await.unwrap();
        assert!(recovered.is_empty(), "{recovered:?}");

        assert_eq!(
            mgr.get(waiting_for_permission.id).await.unwrap().state,
            AgentState::WaitingForPermission
        );
        assert_eq!(
            mgr.get(waiting_for_user.id).await.unwrap().state,
            AgentState::WaitingForUser
        );
    }

    #[tokio::test]
    async fn count_model_calls_counts_only_completed_model_effects() {
        let mgr = manager();
        let task = new_task(&mgr).await;
        assert_eq!(mgr.count_model_calls(task.id).await.unwrap(), 0);

        let effect_id = EffectId::new();
        let step_id = StepId::new();
        mgr.append_journal(
            task.id,
            JournalEntryKind::EffectIssued {
                effect_id,
                step_id,
                effect_kind: kinds::MODEL_CALL.into(),
                payload: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
        // Issued but not yet completed: does not count yet.
        assert_eq!(mgr.count_model_calls(task.id).await.unwrap(), 0);

        mgr.append_journal(
            task.id,
            JournalEntryKind::EffectCompleted {
                effect_id,
                step_id,
                outcome_kind: kinds::MODEL_COMPLETION.into(),
                payload: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
        assert_eq!(mgr.count_model_calls(task.id).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn interrupted_tool_call_detects_a_crash_between_issue_and_completion() {
        let mgr = manager();
        let task = new_task(&mgr).await;
        let effect_id = EffectId::new();
        let step_id = StepId::new();

        assert!(mgr.interrupted_tool_call(task.id).await.unwrap().is_none());

        mgr.append_journal(
            task.id,
            JournalEntryKind::EffectIssued {
                effect_id,
                step_id,
                effect_kind: kinds::TOOL.into(),
                payload: serde_json::json!({"tool": "run_command", "input": {"program": "cat"}}),
            },
        )
        .await
        .unwrap();

        let interrupted = mgr.interrupted_tool_call(task.id).await.unwrap().unwrap();
        assert_eq!(interrupted.tool, "run_command");
        assert_eq!(interrupted.step_id, step_id);
        // Not a permission ask, so pending_tool_call (the explicit-resolve
        // path) does not see it.
        assert!(mgr.pending_tool_call(task.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn pending_tool_call_tracks_an_outstanding_permission_ask_until_superseded() {
        let mgr = manager();
        let task = new_task(&mgr).await;
        let effect_id = EffectId::new();
        let step_id = StepId::new();

        mgr.append_journal(
            task.id,
            JournalEntryKind::EffectIssued {
                effect_id,
                step_id,
                effect_kind: kinds::TOOL.into(),
                payload: serde_json::json!({"tool": "run_command", "input": {"program": "rm"}}),
            },
        )
        .await
        .unwrap();
        mgr.append_journal(
            task.id,
            JournalEntryKind::EffectCompleted {
                effect_id,
                step_id,
                outcome_kind: kinds::PERMISSION_ASK.into(),
                payload: serde_json::json!({"prompt": "allow rm?"}),
            },
        )
        .await
        .unwrap();

        let pending = mgr.pending_tool_call(task.id).await.unwrap().unwrap();
        assert_eq!(pending.tool, "run_command");
        // No longer "interrupted" — it has a completion, just a non-final one.
        assert!(mgr.interrupted_tool_call(task.id).await.unwrap().is_none());

        // Once resolved, a fresh completion supersedes the permission_ask
        // one and pending_tool_call clears.
        mgr.append_journal(
            task.id,
            JournalEntryKind::EffectCompleted {
                effect_id,
                step_id,
                outcome_kind: kinds::TOOL_RESULT.into(),
                payload: serde_json::json!({"success": true}),
            },
        )
        .await
        .unwrap();
        assert!(mgr.pending_tool_call(task.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn pending_signal_is_durable_and_survives_across_manager_instances() {
        let mgr = manager();
        let task = new_task(&mgr).await;
        assert_eq!(mgr.get(task.id).await.unwrap().pending_signal, None);

        mgr.request_pause(task.id).await.unwrap();
        assert_eq!(
            mgr.get(task.id).await.unwrap().pending_signal,
            Some(ControlSignal::PauseRequested)
        );

        // A later request overwrites the earlier one rather than queuing.
        mgr.request_cancel(task.id).await.unwrap();
        assert_eq!(
            mgr.get(task.id).await.unwrap().pending_signal,
            Some(ControlSignal::CancelRequested)
        );
    }

    #[tokio::test]
    async fn transition_clears_any_pending_signal() {
        let mgr = manager();
        let task = new_task(&mgr).await;
        mgr.request_pause(task.id).await.unwrap();
        mgr.transition(task.id, AgentState::Understanding)
            .await
            .unwrap();
        assert_eq!(mgr.get(task.id).await.unwrap().pending_signal, None);
    }

    #[tokio::test]
    async fn requesting_a_signal_for_an_unknown_task_errors() {
        let mgr = manager();
        let err = mgr.request_pause(TaskId::new()).await.unwrap_err();
        assert!(matches!(err, TaskError::NotFound(_)));
    }
}
