//! Phase 8: model-authored planning and plan-driven execution (§4.25).
//!
//! `Planning` (in [`PlanningMode::ModelAuthored`]) asks the model for a
//! `submit_plan`, validates it with `valyria-plan`, and hands structured
//! errors back under a bounded repair budget until it accepts one — which
//! it persists to `plan_revision` and then enters `Implementing` for.
//!
//! Plan-driven `Implementing` walks the schedule one step at a time.
//! Everything a resume needs is durable: the plan lives in `plan_revision`,
//! and "which steps are done / checkpointed / started" is rebuilt from the
//! task journal (`plan_step_completed` / `plan_checkpoint` /
//! `plan_step_started` entries) — never from process memory. So a `kill -9`
//! mid-plan plus `valyria task resume` picks up at the next incomplete
//! step without re-running the finished ones.
//!
//! Checkpoints at `rollback_boundary` steps make [`AgentDriver::
//! rollback_to_checkpoint`] able to restore the tree to a step boundary,
//! refusing (via the ledger) on any file a human has touched since.
//!
//! Deliberate scope for this phase: verification *interleaved between*
//! plan steps is not run here — the mandatory full `Verifying` suite after
//! the last step (the Phase 7 machinery) is the backstop, and per-step
//! `verification` is still enforced *structurally* by the validator.

use std::collections::BTreeSet;
use std::path::PathBuf;

use valyria_model::{GenerateRequest, Message};
use valyria_orchestrator::Role;
use valyria_plan::{
    schedule, validate, Plan, PlanContext, PlanError, PlanErrorCode, PlanRepairDecision,
    PlanRepairLedger, PlanRevision, PlanStepId, RollbackError, RollbackReport,
};
use valyria_task::{kinds, JournalEntryKind, JournalSeq};
use valyria_types::{AgentState, CheckpointId, EffectId, StepId, TaskId};
use valyria_util::{CancellationToken, ContentHash};

use crate::action::ActionRequest;
use crate::driver::{AgentDriver, Flow, MAX_PLAN_REPAIR_ATTEMPTS, MAX_STEP_TURNS};
use crate::error::{AgentError, Result};

impl AgentDriver {
    // --- Planning -----------------------------------------------------

    /// Ask the model for a plan, validate it, repair invalid ones under a
    /// bounded budget, persist the accepted revision and move to
    /// `Implementing`.
    ///
    /// Crash-safe: the raw plan submission is stored *inside* the planning
    /// `model_completion` journal entry, so a resume that lands after the
    /// completion but before acceptance re-processes that stored
    /// submission rather than calling the model again — which is what keeps
    /// the shared model-turn counter (and therefore the scripted fake
    /// model) in sync across a restart. An already-accepted plan just
    /// re-enters `Implementing`.
    pub(crate) async fn step_planning(
        &self,
        task_id: TaskId,
        cancel: &CancellationToken,
    ) -> Result<Flow> {
        if self.has_accepted_plan(task_id).await? {
            self.tasks
                .transition(task_id, AgentState::Implementing)
                .await?;
            return Ok(Flow::Continue);
        }

        let prior_rejections = self.plan_rejection_count(task_id).await?;
        if prior_rejections >= MAX_PLAN_REPAIR_ATTEMPTS {
            self.tasks
                .append_journal(
                    task_id,
                    JournalEntryKind::RecoveryNote {
                        note: format!(
                            "planning gave up: plan still invalid after {prior_rejections} attempts"
                        ),
                    },
                )
                .await?;
            self.tasks.transition(task_id, AgentState::Failed).await?;
            return Ok(Flow::Return);
        }

        let mut repair = PlanRepairLedger::resumed(MAX_PLAN_REPAIR_ATTEMPTS, prior_rejections);
        let mut feedback: Option<String> = None;

        loop {
            if cancel.is_cancelled() {
                self.tasks
                    .transition(task_id, AgentState::Cancelled)
                    .await?;
                return Ok(Flow::Return);
            }

            // A completed-but-unprocessed planning turn from before a crash?
            let submission = match self.unprocessed_plan_submission(task_id).await? {
                Some(v) => v,
                None => {
                    self.request_plan_from_model(task_id, cancel, &feedback)
                        .await?
                }
            };

            let errors: Vec<PlanError> = match submission_to_plan(submission) {
                Ok(plan) => match validate(&plan, &self.plan_context(task_id)) {
                    Ok(validated) => {
                        let hash = validated.plan().content_hash().to_hex();
                        let step_count = validated.step_count();
                        let rev = PlanRevision::first(
                            validated.into_plan(),
                            "model-authored plan",
                            self.clock.now(),
                        );
                        self.plan_store
                            .save_revision(task_id, &rev)
                            .await
                            .map_err(plan_err)?;
                        self.tasks
                            .append_journal(
                                task_id,
                                JournalEntryKind::EffectIssued {
                                    effect_id: EffectId::new(),
                                    step_id: StepId::new(),
                                    effect_kind: kinds::PLAN_ACCEPTED.into(),
                                    payload: serde_json::json!({
                                        "revision": rev.revision,
                                        "hash": hash,
                                        "step_count": step_count,
                                    }),
                                },
                            )
                            .await?;
                        self.tasks
                            .transition(task_id, AgentState::Implementing)
                            .await?;
                        return Ok(Flow::Continue);
                    }
                    Err(errs) => errs,
                },
                Err(errs) => errs,
            };

            let codes: Vec<&str> = errors.iter().map(|e| e.code.as_str()).collect();
            self.tasks
                .append_journal(
                    task_id,
                    JournalEntryKind::EffectCompleted {
                        effect_id: EffectId::new(),
                        step_id: StepId::new(),
                        outcome_kind: kinds::PLAN_REJECTED.into(),
                        payload: serde_json::json!({"error_codes": codes}),
                    },
                )
                .await?;

            match repair.record_and_decide(&errors) {
                PlanRepairDecision::Retry { feedback: fb } => {
                    feedback = Some(fb);
                    continue;
                }
                PlanRepairDecision::GiveUp { reason } => {
                    self.tasks
                        .append_journal(
                            task_id,
                            JournalEntryKind::RecoveryNote {
                                note: format!("planning gave up: {reason}"),
                            },
                        )
                        .await?;
                    self.tasks.transition(task_id, AgentState::Failed).await?;
                    return Ok(Flow::Return);
                }
            }
        }
    }

    /// One planning model call. Journals the call, then journals its
    /// completion **with the raw plan submission embedded** (or `null` if
    /// the model didn't submit one) so a crash after this point never
    /// needs to re-call the model. Returns the submission value.
    async fn request_plan_from_model(
        &self,
        task_id: TaskId,
        cancel: &CancellationToken,
        feedback: &Option<String>,
    ) -> Result<serde_json::Value> {
        let objective = self.tasks.get(task_id).await?.objective;
        let turn_index = self.tasks.count_model_calls(task_id).await?;
        let step_id = StepId::new();
        let effect_id = EffectId::new();
        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectIssued {
                    effect_id,
                    step_id,
                    effect_kind: kinds::MODEL_CALL.into(),
                    payload: serde_json::json!({"turn_index": turn_index, "phase": "plan"}),
                },
            )
            .await?;

        let prompt = match feedback {
            None => format!(
                "{objective}\n\nProduce a plan for this task. Respond with a single \
                 `{action}` tool call whose arguments are the plan JSON.",
                action = crate::driver::SUBMIT_PLAN_ACTION,
            ),
            Some(fb) => format!("{objective}\n\n{fb}\n\nResubmit the corrected plan."),
        };
        let request = GenerateRequest::new(vec![Message::user(prompt)]).with_turn_hint(turn_index);
        let completion = self
            .orchestrator
            .generate(Role::PrimaryCoder, request, cancel.child())
            .await?;

        let submission = match ActionRequest::from_completion(&completion)? {
            ActionRequest::ToolCall { tool, input }
                if tool == crate::driver::SUBMIT_PLAN_ACTION =>
            {
                input
            }
            ActionRequest::Ask { .. } => serde_json::Value::Null,
            _ => serde_json::Value::Null,
        };

        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectCompleted {
                    effect_id,
                    step_id,
                    outcome_kind: kinds::MODEL_COMPLETION.into(),
                    payload: serde_json::json!({
                        "finish_reason": format!("{:?}", completion.finish_reason),
                        "text": completion.text,
                        "phase": "plan",
                        "plan_submission": submission,
                    }),
                },
            )
            .await?;

        Ok(submission)
    }

    // --- plan-driven Implementing ----------------------------------

    pub(crate) async fn step_implementing_plan(
        &self,
        task_id: TaskId,
        cancel: &CancellationToken,
    ) -> Result<Flow> {
        // D1: redo an interrupted tool call before anything new.
        if let Some(pending) = self.tasks.interrupted_tool_call(task_id).await? {
            self.issue_and_execute_tool_call(
                task_id,
                cancel,
                pending.step_id,
                &pending.tool,
                pending.input,
            )
            .await?;
            return Ok(Flow::Continue);
        }

        let rev = self
            .plan_store
            .latest_revision(task_id)
            .await
            .map_err(plan_err)?
            .ok_or_else(|| AgentError::Plan("no plan revision for task in Implementing".into()))?;

        let validated = match validate(&rev.plan, &self.plan_context(task_id)) {
            Ok(v) => v,
            Err(errs) => {
                let codes: Vec<&str> = errs.iter().map(|e| e.code.as_str()).collect();
                self.tasks
                    .append_journal(
                        task_id,
                        JournalEntryKind::RecoveryNote {
                            note: format!(
                                "stored plan no longer validates ({}) — failing",
                                codes.join(", ")
                            ),
                        },
                    )
                    .await?;
                self.tasks.transition(task_id, AgentState::Failed).await?;
                return Ok(Flow::Return);
            }
        };

        let sched = schedule(&validated);
        let done = self.completed_plan_steps(task_id).await?;

        let step = match sched.next_incomplete(&done) {
            None => {
                // Every step done — hand off to the mandatory full suite.
                self.tasks
                    .transition(task_id, AgentState::Verifying)
                    .await?;
                return Ok(Flow::Continue);
            }
            Some(step) => step.clone(),
        };

        // Checkpoint at a rollback boundary — once per step.
        if step.checkpoint && !self.checkpoint_taken_for(task_id, &step.id).await? {
            self.take_checkpoint(task_id, &step.id).await?;
        }

        // Journal the step's start — once.
        if !self.step_started(task_id, &step.id).await? {
            self.tasks
                .append_journal(
                    task_id,
                    JournalEntryKind::EffectIssued {
                        effect_id: EffectId::new(),
                        step_id: StepId::new(),
                        effect_kind: kinds::PLAN_STEP_STARTED.into(),
                        payload: serde_json::json!({
                            "step_id": step.id.as_str(),
                            "intent": step.intent,
                        }),
                    },
                )
                .await?;
        }

        // Bound the step: after MAX_STEP_TURNS model turns, force-complete
        // and let `Verifying` be the judge.
        if self.model_turns_for_step(task_id, &step.id).await? >= MAX_STEP_TURNS {
            self.complete_step(task_id, &step.id).await?;
            return Ok(Flow::Continue);
        }

        // Reason, scoped to the step. Crash-safe the same way planning is:
        // the parsed action is stored in the step's `model_completion`
        // entry, so a resume after the completion but before the tool call
        // replays that action rather than re-calling the model (which
        // would advance the shared turn counter and desync the script).
        let sid = StepId::new();
        let action = match self.unprocessed_step_action(task_id, &step.id).await? {
            Some(a) => a,
            None => self.request_step_action(task_id, cancel, &step).await?,
        };

        match action {
            StepAction::ToolCall { tool, input } => {
                self.note_scope_expansion_if_any(task_id, &rev.plan, &input)
                    .await?;
                self.issue_and_execute_tool_call(task_id, cancel, sid, &tool, input)
                    .await?;
                let state_now = self.tasks.get(task_id).await?.state;
                if state_now == AgentState::Implementing {
                    Ok(Flow::Continue)
                } else {
                    // WAITING_FOR_PERMISSION or FAILED — the tool plumbing
                    // already transitioned; resume happens via
                    // `resolve_permission`.
                    Ok(Flow::Return)
                }
            }
            StepAction::Finish => {
                self.complete_step(task_id, &step.id).await?;
                Ok(Flow::Continue)
            }
            StepAction::Ask => {
                self.tasks
                    .transition(task_id, AgentState::WaitingForUser)
                    .await?;
                Ok(Flow::Return)
            }
        }
    }

    /// One step model call. Journals the call, then the completion with the
    /// parsed action embedded so a crash never forces a re-call.
    async fn request_step_action(
        &self,
        task_id: TaskId,
        cancel: &CancellationToken,
        step: &valyria_plan::PlanStep,
    ) -> Result<StepAction> {
        let objective = self.tasks.get(task_id).await?.objective;
        let turn_index = self.tasks.count_model_calls(task_id).await?;
        let sid = StepId::new();
        let effect_id = EffectId::new();
        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectIssued {
                    effect_id,
                    step_id: sid,
                    effect_kind: kinds::MODEL_CALL.into(),
                    payload: serde_json::json!({
                        "turn_index": turn_index,
                        "phase": "plan_step",
                        "plan_step": step.id.as_str(),
                    }),
                },
            )
            .await?;

        let targets: Vec<String> = step
            .targets
            .iter()
            .map(|t| t.display().to_string())
            .collect();
        let prompt = format!(
            "{objective}\n\nYou are executing plan step `{}`: {}\nDeclared targets: {}\n\
             Make exactly the edits this step needs, one tool call at a time, then finish \
             the step.",
            step.id,
            step.intent,
            if targets.is_empty() {
                "(none)".to_string()
            } else {
                targets.join(", ")
            },
        );
        let request = GenerateRequest::new(vec![Message::user(prompt)]).with_turn_hint(turn_index);
        let completion = self
            .orchestrator
            .generate(Role::PrimaryCoder, request, cancel.child())
            .await?;

        let action = match ActionRequest::from_completion(&completion)? {
            ActionRequest::ToolCall { tool, input } => StepAction::ToolCall { tool, input },
            ActionRequest::Finish { .. } => StepAction::Finish,
            ActionRequest::Ask { .. } => StepAction::Ask,
        };

        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectCompleted {
                    effect_id,
                    step_id: sid,
                    outcome_kind: kinds::MODEL_COMPLETION.into(),
                    payload: serde_json::json!({
                        "finish_reason": format!("{:?}", completion.finish_reason),
                        "text": completion.text,
                        "phase": "plan_step",
                        "plan_step": step.id.as_str(),
                        "step_action": action.to_json(),
                    }),
                },
            )
            .await?;

        Ok(action)
    }

    /// A step `model_completion` for `step` with nothing acted on it yet
    /// (no tool issued, no step completion since) — replay its stored
    /// action instead of asking the model again.
    async fn unprocessed_step_action(
        &self,
        task_id: TaskId,
        step: &PlanStepId,
    ) -> Result<Option<StepAction>> {
        let entries = self.journal(task_id).await?;
        let idx = entries.iter().rposition(|e| {
            matches!(
                &e.kind,
                JournalEntryKind::EffectCompleted { outcome_kind, payload, .. }
                    if outcome_kind == kinds::MODEL_COMPLETION
                        && payload.get("phase").and_then(|v| v.as_str()) == Some("plan_step")
                        && payload.get("plan_step").and_then(|v| v.as_str())
                            == Some(step.as_str())
            )
        });
        let Some(idx) = idx else { return Ok(None) };
        let acted = entries[idx + 1..].iter().any(|e| match &e.kind {
            JournalEntryKind::EffectIssued { effect_kind, .. } => effect_kind == kinds::TOOL,
            JournalEntryKind::EffectCompleted { outcome_kind, .. } => {
                outcome_kind == kinds::PLAN_STEP_COMPLETED
                    || outcome_kind == kinds::PLAN_SCOPE_EXPANSION
            }
            _ => false,
        });
        if acted {
            return Ok(None);
        }
        let action = match &entries[idx].kind {
            JournalEntryKind::EffectCompleted { payload, .. } => {
                payload.get("step_action").and_then(StepAction::from_json)
            }
            _ => None,
        };
        Ok(action)
    }

    // --- rollback --------------------------------------------------

    /// Roll the workspace back to a checkpoint taken at a plan step
    /// boundary. Restores every checkpointed file exactly; refuses (via the
    /// change ledger) on any file touched since — by anyone.
    pub async fn rollback_to_checkpoint(
        &self,
        task_id: TaskId,
        checkpoint_id: CheckpointId,
    ) -> std::result::Result<RollbackReport, RollbackError> {
        let cp = self
            .plan_store
            .checkpoint(checkpoint_id)
            .await
            .map_err(|e| RollbackError::Ledger(e.to_string()))?
            .ok_or_else(|| RollbackError::NotFound(checkpoint_id.to_string()))?;
        if cp.task_id != task_id {
            return Err(RollbackError::NotFound(checkpoint_id.to_string()));
        }

        let result = valyria_plan::rollback(
            &cp,
            &self.ledger,
            &self.workspace_root,
            StepId::new(),
            self.clock.as_ref(),
        );

        match &result {
            Ok(report) => {
                let reverted: Vec<String> = report
                    .reverted
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                let _ = self
                    .tasks
                    .append_journal(
                        task_id,
                        JournalEntryKind::EffectCompleted {
                            effect_id: EffectId::new(),
                            step_id: StepId::new(),
                            outcome_kind: kinds::PLAN_ROLLBACK.into(),
                            payload: serde_json::json!({
                                "checkpoint_id": checkpoint_id.to_string(),
                                "reverted": reverted,
                            }),
                        },
                    )
                    .await;
            }
            Err(e) => {
                let _ = self
                    .tasks
                    .append_journal(
                        task_id,
                        JournalEntryKind::RecoveryNote {
                            note: format!("rollback to {checkpoint_id} refused: {e}"),
                        },
                    )
                    .await;
            }
        }
        result
    }

    // --- helpers -------------------------------------------------

    pub(crate) async fn task_has_plan(&self, task_id: TaskId) -> Result<bool> {
        Ok(self
            .plan_store
            .latest_revision(task_id)
            .await
            .map_err(plan_err)?
            .is_some())
    }

    fn plan_context(&self, _task_id: TaskId) -> PlanContext<'_> {
        PlanContext {
            workspace_root: &self.workspace_root,
            permission_mode: self.permissions.mode(),
            allowed_write_roots: self.sandbox_profile.allow_write.clone(),
        }
    }

    async fn journal(&self, task_id: TaskId) -> Result<Vec<valyria_task::JournalEntry>> {
        Ok(self.tasks.journal_since(task_id, JournalSeq::ZERO).await?)
    }

    async fn has_accepted_plan(&self, task_id: TaskId) -> Result<bool> {
        Ok(self.journal(task_id).await?.iter().any(|e| {
            matches!(
                &e.kind,
                JournalEntryKind::EffectIssued { effect_kind, .. }
                    if effect_kind == kinds::PLAN_ACCEPTED
            )
        }))
    }

    async fn plan_rejection_count(&self, task_id: TaskId) -> Result<u32> {
        Ok(self
            .journal(task_id)
            .await?
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    JournalEntryKind::EffectCompleted { outcome_kind, .. }
                        if outcome_kind == kinds::PLAN_REJECTED
                )
            })
            .count() as u32)
    }

    /// The plan submission from the most recent planning `model_completion`
    /// that has not yet been followed by an accept or a reject — i.e. a
    /// crash landed between "model answered" and "runtime decided". `None`
    /// when the last planning turn was already processed (or there is
    /// none), meaning a fresh model call is needed.
    async fn unprocessed_plan_submission(
        &self,
        task_id: TaskId,
    ) -> Result<Option<serde_json::Value>> {
        let entries = self.journal(task_id).await?;
        let last_plan_completion = entries.iter().rposition(|e| {
            matches!(
                &e.kind,
                JournalEntryKind::EffectCompleted { outcome_kind, payload, .. }
                    if outcome_kind == kinds::MODEL_COMPLETION
                        && payload.get("phase").and_then(|v| v.as_str()) == Some("plan")
            )
        });
        let Some(idx) = last_plan_completion else {
            return Ok(None);
        };
        let decided_since = entries[idx + 1..].iter().any(|e| match &e.kind {
            JournalEntryKind::EffectIssued { effect_kind, .. } => {
                effect_kind == kinds::PLAN_ACCEPTED
            }
            JournalEntryKind::EffectCompleted { outcome_kind, .. } => {
                outcome_kind == kinds::PLAN_REJECTED
            }
            _ => false,
        });
        if decided_since {
            return Ok(None);
        }
        let submission = match &entries[idx].kind {
            JournalEntryKind::EffectCompleted { payload, .. } => payload
                .get("plan_submission")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            _ => serde_json::Value::Null,
        };
        Ok(Some(submission))
    }

    async fn completed_plan_steps(&self, task_id: TaskId) -> Result<BTreeSet<PlanStepId>> {
        let mut done = BTreeSet::new();
        for e in self.journal(task_id).await? {
            if let JournalEntryKind::EffectCompleted {
                outcome_kind,
                payload,
                ..
            } = &e.kind
            {
                if outcome_kind == kinds::PLAN_STEP_COMPLETED {
                    if let Some(id) = payload.get("step_id").and_then(|v| v.as_str()) {
                        if let Ok(sid) = PlanStepId::new(id) {
                            done.insert(sid);
                        }
                    }
                }
            }
        }
        Ok(done)
    }

    async fn step_started(&self, task_id: TaskId, step: &PlanStepId) -> Result<bool> {
        Ok(self.journal(task_id).await?.iter().any(|e| {
            matches!(
                &e.kind,
                JournalEntryKind::EffectIssued { effect_kind, payload, .. }
                    if effect_kind == kinds::PLAN_STEP_STARTED
                        && payload.get("step_id").and_then(|v| v.as_str()) == Some(step.as_str())
            )
        }))
    }

    async fn checkpoint_taken_for(&self, task_id: TaskId, step: &PlanStepId) -> Result<bool> {
        Ok(self.journal(task_id).await?.iter().any(|e| {
            matches!(
                &e.kind,
                JournalEntryKind::EffectCompleted { outcome_kind, payload, .. }
                    if outcome_kind == kinds::PLAN_CHECKPOINT
                        && payload.get("step_id").and_then(|v| v.as_str()) == Some(step.as_str())
            )
        }))
    }

    /// Model turns spent on `step` since its `plan_step_started` entry.
    async fn model_turns_for_step(&self, task_id: TaskId, step: &PlanStepId) -> Result<usize> {
        let entries = self.journal(task_id).await?;
        let start = entries.iter().position(|e| {
            matches!(
                &e.kind,
                JournalEntryKind::EffectIssued { effect_kind, payload, .. }
                    if effect_kind == kinds::PLAN_STEP_STARTED
                        && payload.get("step_id").and_then(|v| v.as_str()) == Some(step.as_str())
            )
        });
        let Some(start) = start else { return Ok(0) };
        Ok(entries[start + 1..]
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    JournalEntryKind::EffectIssued { effect_kind, payload, .. }
                        if effect_kind == kinds::MODEL_CALL
                            && payload.get("plan_step").and_then(|v| v.as_str())
                                == Some(step.as_str())
                )
            })
            .count())
    }

    async fn complete_step(&self, task_id: TaskId, step: &PlanStepId) -> Result<()> {
        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectCompleted {
                    effect_id: EffectId::new(),
                    step_id: StepId::new(),
                    outcome_kind: kinds::PLAN_STEP_COMPLETED.into(),
                    payload: serde_json::json!({"step_id": step.as_str()}),
                },
            )
            .await?;
        Ok(())
    }

    async fn take_checkpoint(&self, task_id: TaskId, step: &PlanStepId) -> Result<()> {
        let changed = self.task_changed_files(task_id);
        let touched: Vec<(PathBuf, Option<ContentHash>)> = changed
            .iter()
            .map(|p| {
                let h = self
                    .workspace_root
                    .resolve(p)
                    .ok()
                    .and_then(|r| std::fs::read(&r).ok())
                    .map(|b| ContentHash::of_bytes(&b));
                (p.clone(), h)
            })
            .collect();
        let watermark = self.ledger.entries_for_task(task_id).len();
        let cp = valyria_plan::capture(task_id, step, touched, watermark, self.clock.now());
        self.plan_store
            .save_checkpoint(&cp)
            .await
            .map_err(plan_err)?;
        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectCompleted {
                    effect_id: EffectId::new(),
                    step_id: StepId::new(),
                    outcome_kind: kinds::PLAN_CHECKPOINT.into(),
                    payload: serde_json::json!({
                        "checkpoint_id": cp.id.to_string(),
                        "step_id": step.as_str(),
                    }),
                },
            )
            .await?;
        Ok(())
    }

    /// If a write tool's target lands outside the plan's declared
    /// `plan_scope`, record it as the permission event §4.25 calls for.
    /// Detection + journal only — the permission engine still gates the
    /// write itself.
    async fn note_scope_expansion_if_any(
        &self,
        task_id: TaskId,
        plan: &Plan,
        input: &serde_json::Value,
    ) -> Result<()> {
        let Some(path) = input.get("path").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        if plan.plan_scope.is_empty() {
            return Ok(());
        }
        let normalized = path.replace('\\', "/");
        let in_scope = plan.plan_scope.iter().any(|raw| {
            let p = raw.trim_end_matches('/');
            p.is_empty() || normalized == p || normalized.starts_with(&format!("{p}/"))
        });
        if !in_scope {
            self.tasks
                .append_journal(
                    task_id,
                    JournalEntryKind::EffectCompleted {
                        effect_id: EffectId::new(),
                        step_id: StepId::new(),
                        outcome_kind: kinds::PLAN_SCOPE_EXPANSION.into(),
                        payload: serde_json::json!({"path": path}),
                    },
                )
                .await?;
        }
        Ok(())
    }
}

fn plan_err(e: valyria_plan::PlanCrateError) -> AgentError {
    AgentError::Plan(e.to_string())
}

/// A plan step's decided action, stored in the journal so a resume can
/// replay it without a fresh model call.
#[derive(Debug, Clone, PartialEq)]
enum StepAction {
    ToolCall {
        tool: String,
        input: serde_json::Value,
    },
    Finish,
    Ask,
}

impl StepAction {
    fn to_json(&self) -> serde_json::Value {
        match self {
            StepAction::ToolCall { tool, input } => {
                serde_json::json!({"kind": "tool_call", "tool": tool, "input": input})
            }
            StepAction::Finish => serde_json::json!({"kind": "finish"}),
            StepAction::Ask => serde_json::json!({"kind": "ask"}),
        }
    }

    fn from_json(v: &serde_json::Value) -> Option<StepAction> {
        match v.get("kind")?.as_str()? {
            "tool_call" => Some(StepAction::ToolCall {
                tool: v.get("tool")?.as_str()?.to_string(),
                input: v.get("input").cloned().unwrap_or(serde_json::Value::Null),
            }),
            "finish" => Some(StepAction::Finish),
            "ask" => Some(StepAction::Ask),
            _ => None,
        }
    }
}

/// Turn a raw plan submission value into a `Plan`, or the structured
/// error the repair loop feeds back to the model.
fn submission_to_plan(value: serde_json::Value) -> std::result::Result<Plan, Vec<PlanError>> {
    if value.is_null() {
        return Err(vec![PlanError {
            code: PlanErrorCode::NotSubmitted,
            step: None,
            message: "expected a `submit_plan` tool call carrying the plan JSON".into(),
            hint: format!(
                "respond with one `{}` tool call, nothing else",
                crate::driver::SUBMIT_PLAN_ACTION
            ),
        }]);
    }
    serde_json::from_value::<Plan>(value).map_err(|e| {
        vec![PlanError {
            code: PlanErrorCode::Malformed,
            step: None,
            message: format!("plan JSON did not parse: {e}"),
            hint: "emit valid plan JSON: an object with `plan_scope` and `steps`".into(),
        }]
    })
}
