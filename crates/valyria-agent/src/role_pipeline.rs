//! M5: the Researcher → Planner → Implementer → Tester → Reviewer role
//! pipeline (§4.25) — [`AgentDriver::run_role_pipeline`] runs it as a
//! sequence of child tasks under one coordinator task, each producing a
//! typed `Artifact` persisted to the `PlanStore`, the only channel roles
//! communicate through.
//!
//! Distinct from [`AgentDriver::run`] (the single-task state machine): a
//! role pipeline coordinates *whole child tasks*, reusing as much of the
//! existing single-task machinery as each role's job actually calls for
//! rather than reinventing it:
//! - Planner reuses `step_planning` verbatim — it already does exactly
//!   "ask the model for `submit_plan`, validate, repair under a bounded
//!   budget" against whatever task id it's given.
//! - Implementer reuses the entire existing `run` state machine on its own
//!   child, pre-loaded with the Planner's accepted revision, so it gets
//!   the full edit/verify/diagnose/repair loop for free.
//! - Researcher and Reviewer — both read-only, with no existing
//!   single-task analogue — get a small bounded Reason/Select/Execute loop
//!   ([`AgentDriver::run_role_turn_loop`]) restricted to their tool
//!   allowlist plus one role-specific "submit" tool that ends it.
//! - Tester makes no model call of its own at all: its `VerificationReport`
//!   is read straight from the `VerificationLog` rows the Implementer's
//!   own mandatory verification already produced.
//!
//! The coordinator's own `AgentState` walks the same mainline path a
//! single task's does (`Understanding` → `Discovery` → `Planning` →
//! `Implementing` → `Verifying` → `Completed`/`Failed`/`WaitingForUser`),
//! reusing each state's existing meaning for the role whose work fills it,
//! rather than inventing new states just for the coordinator.
//!
//! **Not yet crash-recoverable as its own unit (M5, deferred):** a
//! coordinator killed mid-pipeline restarts `run_role_pipeline` from
//! scratch on the next call, which would mint duplicate child tasks
//! rather than resuming the ones already in flight. Each *child* task
//! itself is fully crash-safe (it's an ordinary task, recovered exactly
//! like any other), so no work is silently lost — only the coordinator's
//! own "which role am I on" bookkeeping needs a follow-up (reading
//! `children_of(task_id)` back before spawning a new one) to make
//! resuming the pipeline itself idempotent.
//!
//! **Also deferred:** a Reviewer finding or a failing Tester report hands
//! the coordinator to `WaitingForUser` rather than looping back into a new
//! Implementer revision — the auto-repair-revision loop this could drive
//! is real follow-up work, not a corner cut silently.

use valyria_model::{GenerateRequest, ToolSpec};
use valyria_orchestrator::Role;
use valyria_plan::{AgentRole, Artifact, StoredArtifact};
use valyria_task::{kinds, Budget, JournalEntryKind};
use valyria_types::{AgentState, EffectId, StepId, TaskId};
use valyria_util::CancellationToken;

use crate::action::ActionRequest;
use crate::driver::{AgentDriver, MAX_REFORMAT_RETRIES};
use crate::error::{AgentError, Result};
use crate::plan_exec::plan_err;

/// Cap on read-only exploration turns before a Researcher/Reviewer role is
/// forced to stop (with or without having submitted) — mirrors
/// `MAX_STEP_TURNS`'s "don't let a role think forever" bound, just a
/// little more generous since these roles are meant to explore.
const MAX_ROLE_TURNS: usize = 6;

impl AgentDriver {
    /// Runs the full role pipeline against `task_id`'s objective. `task_id`
    /// is the coordinator; every role's real work happens in a child task
    /// created under it via `TaskManager::create_child`.
    pub async fn run_role_pipeline(
        &self,
        task_id: TaskId,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let coordinator = self.tasks.get(task_id).await?;
        if coordinator.state == AgentState::Idle {
            self.tasks
                .transition(task_id, AgentState::Understanding)
                .await?;
        }
        let objective = coordinator.objective.clone();

        // --- Researcher ------------------------------------------------
        self.tasks
            .transition(task_id, AgentState::Discovery)
            .await?;
        let researcher = self
            .spawn_role_child(
                task_id,
                format!(
                    "Research task: {objective}\n\nExplore the repository with your available \
                     tools, then call `submit_research_brief` with a summary of what's \
                     relevant, the files involved, and any open questions. Call it once, when \
                     you're done exploring."
                ),
            )
            .await?;
        let brief_value = self
            .run_role_turn_loop(
                researcher.id,
                cancel,
                AgentRole::Researcher,
                submit_research_brief_tool_spec(),
                MAX_ROLE_TURNS,
            )
            .await?;
        self.save_role_artifact(
            task_id,
            AgentRole::Researcher,
            parse_research_brief(brief_value),
        )
        .await?;
        self.finish_role_child(researcher.id).await?;

        // --- Planner -----------------------------------------------------
        self.tasks.transition(task_id, AgentState::Planning).await?;
        let planner = self.spawn_role_child(task_id, objective.clone()).await?;
        for state in [AgentState::Discovery, AgentState::Planning] {
            self.tasks.transition(planner.id, state).await?;
        }
        let planner_role = self.preferred_model_role(AgentRole::Planner);
        self.step_planning(planner.id, cancel, Some(planner_role))
            .await?;
        let planner_final = self.tasks.get(planner.id).await?;
        if planner_final.state != AgentState::Implementing {
            self.give_up(task_id, "the Planner role could not produce a valid plan")
                .await?;
            return Ok(());
        }
        let revision = self
            .plan_store
            .latest_revision(planner.id)
            .await
            .map_err(plan_err)?
            .ok_or_else(|| AgentError::Plan("planner accepted a plan but none is stored".into()))?;
        self.save_role_artifact(
            task_id,
            AgentRole::Planner,
            Artifact::Plan {
                revision: revision.clone(),
            },
        )
        .await?;
        self.finish_role_child(planner.id).await?;

        // --- Implementer ---------------------------------------------------
        self.tasks
            .transition(task_id, AgentState::Implementing)
            .await?;
        let implementer = self.spawn_role_child(task_id, objective.clone()).await?;
        self.plan_store
            .save_revision(implementer.id, &revision)
            .await
            .map_err(plan_err)?;
        for state in [
            AgentState::Discovery,
            AgentState::Planning,
            AgentState::Implementing,
        ] {
            self.tasks.transition(implementer.id, state).await?;
        }
        let implementer_role = self.preferred_model_role(AgentRole::Implementer);
        self.run_with_role(implementer.id, cancel.child(), Some(implementer_role))
            .await?;
        let implementer_final = self.tasks.get(implementer.id).await?;
        let changed = self.task_changed_files(implementer.id);
        self.save_role_artifact(
            task_id,
            AgentRole::Implementer,
            Artifact::ChangeSet {
                summary: format!("{} file(s) changed", changed.len()),
                files_changed: changed.iter().map(|p| p.display().to_string()).collect(),
                ledger_entries: changed.len(),
            },
        )
        .await?;
        if implementer_final.state != AgentState::Completed {
            self.give_up(
                task_id,
                &format!(
                    "the Implementer role ended in {:?}, not Completed",
                    implementer_final.state
                ),
            )
            .await?;
            return Ok(());
        }

        // --- Tester --------------------------------------------------------
        self.tasks
            .transition(task_id, AgentState::Verifying)
            .await?;
        let runs = self
            .verification_log
            .list_for_task(implementer.id)
            .await
            .map_err(|e| AgentError::MalformedCompletion {
                detail: format!("verification log: {e}"),
            })?;
        let tester_passed = runs.iter().all(|r| r.passed());
        self.save_role_artifact(
            task_id,
            AgentRole::Tester,
            Artifact::VerificationReport {
                passed: tester_passed,
                commands_run: runs.iter().map(|r| r.command_display.clone()).collect(),
                failures: runs
                    .iter()
                    .flat_map(|r| r.failures.iter().map(|f| f.message.clone()))
                    .collect(),
            },
        )
        .await?;

        // --- Reviewer -------------------------------------------------------
        let reviewer = self
            .spawn_role_child(
                task_id,
                format!(
                    "Review task: {objective}\n\n{} file(s) were changed to implement this. \
                     Verification {}. Use your read-only tools to inspect the current state of \
                     the repository, then call `submit_review_findings` with your verdict. Call \
                     it once, when you're done reviewing.",
                    changed.len(),
                    if tester_passed { "passed" } else { "failed" },
                ),
            )
            .await?;
        let findings_value = self
            .run_role_turn_loop(
                reviewer.id,
                cancel,
                AgentRole::Reviewer,
                submit_review_findings_tool_spec(),
                MAX_ROLE_TURNS,
            )
            .await?;
        let findings = parse_review_findings(findings_value);
        let reviewer_approved =
            matches!(&findings, Artifact::ReviewFindings { approved, .. } if *approved);
        self.save_role_artifact(task_id, AgentRole::Reviewer, findings)
            .await?;
        self.finish_role_child(reviewer.id).await?;

        if tester_passed && reviewer_approved {
            self.tasks
                .transition(task_id, AgentState::Completed)
                .await?;
        } else {
            self.tasks
                .transition(task_id, AgentState::Diagnosing)
                .await?;
            self.tasks
                .append_journal(
                    task_id,
                    JournalEntryKind::RecoveryNote {
                        note: format!(
                            "role pipeline: tester_passed={tester_passed} \
                             reviewer_approved={reviewer_approved} — the auto-repair-revision \
                             loop back to the Implementer is not yet wired (M5, deferred); \
                             handing off to a human"
                        ),
                    },
                )
                .await?;
            self.tasks
                .transition(task_id, AgentState::WaitingForUser)
                .await?;
        }
        Ok(())
    }

    async fn give_up(&self, task_id: TaskId, reason: &str) -> Result<()> {
        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::RecoveryNote {
                    note: format!("role pipeline gave up: {reason}"),
                },
            )
            .await?;
        self.tasks.transition(task_id, AgentState::Failed).await?;
        Ok(())
    }

    async fn spawn_role_child(
        &self,
        parent_id: TaskId,
        objective: impl Into<String>,
    ) -> Result<valyria_task::Task> {
        let child = self
            .tasks
            .create_child(parent_id, objective.into(), Budget::default())
            .await?;
        self.tasks
            .transition(child.id, AgentState::Understanding)
            .await?;
        Ok(child)
    }

    /// Walks a role child that never enters the normal state machine
    /// (Researcher, Planner-on-success, Reviewer) through *whatever
    /// remains* of the mainline path to `Completed` — pure bookkeeping
    /// (`transition` has no side effects beyond journaling), needed so
    /// `recover_incomplete_tasks` never mistakes a role child whose real
    /// work is already done for a crashed one still needing a driver.
    ///
    /// Not a fixed transition list: a Researcher/Reviewer child (driven
    /// only by `run_role_turn_loop`) is still sitting at `Understanding`
    /// when this runs, but a Planner child that `step_planning` already
    /// accepted a plan for is sitting at `Implementing` — replaying the
    /// full mainline from `Discovery` onward would try (and fail) an
    /// illegal `Implementing -> Discovery` transition.
    async fn finish_role_child(&self, child_id: TaskId) -> Result<()> {
        const MAINLINE: [AgentState; 6] = [
            AgentState::Understanding,
            AgentState::Discovery,
            AgentState::Planning,
            AgentState::Implementing,
            AgentState::Verifying,
            AgentState::Completed,
        ];
        let current = self.tasks.get(child_id).await?.state;
        if let Some(pos) = MAINLINE.iter().position(|s| *s == current) {
            for state in &MAINLINE[pos + 1..] {
                self.tasks.transition(child_id, *state).await?;
            }
        }
        Ok(())
    }

    /// Which `valyria_orchestrator::Role` (model binding) an `AgentRole`
    /// should use — falling back to the ordinary single-task `model_role`
    /// selection when nothing is bound specifically for it, so a driver
    /// that only ever bound `PrimaryCoder` (every pre-M5 setup) still runs
    /// a role pipeline end to end on that one model. Implementer always
    /// prefers `PrimaryCoder` outright — every driver binds it (it's the
    /// mandatory baseline `Role` the rest of the loop already requires) —
    /// rather than going through `model_role`'s FastCoder-if-bound
    /// preference, which would otherwise make the Implementer's model
    /// choice depend on whatever's bound for a completely unrelated role
    /// (Researcher's `FastCoder`), a coincidence of binding rather than a
    /// deliberate choice.
    fn preferred_model_role(&self, role: AgentRole) -> Role {
        let preferred = match role {
            AgentRole::Researcher => Role::FastCoder,
            AgentRole::Planner => Role::Planner,
            AgentRole::Implementer | AgentRole::Tester => Role::PrimaryCoder,
            AgentRole::Reviewer => Role::Reviewer,
        };
        if role == AgentRole::Implementer || self.orchestrator.is_bound(preferred) {
            preferred
        } else {
            self.model_role(false)
        }
    }

    async fn save_role_artifact(
        &self,
        task_id: TaskId,
        role: AgentRole,
        artifact: Artifact,
    ) -> Result<()> {
        self.plan_store
            .save_artifact(&StoredArtifact {
                task_id,
                produced_by: role,
                artifact,
                created_at: self.clock.now(),
            })
            .await
            .map_err(plan_err)
    }

    /// A bounded Reason/Select/Execute loop for a read-only role
    /// (Researcher, Reviewer): each turn offers exactly `role`'s tool
    /// allowlist plus `submit_tool`, executes any other tool call through
    /// the same `issue_and_execute_tool_call` the single-task loop uses,
    /// and returns the submission's arguments the moment the model calls
    /// `submit_tool` — or `serde_json::Value::Null` if `max_turns` is
    /// exhausted (or the model finishes/asks instead of submitting)
    /// without ever calling it.
    async fn run_role_turn_loop(
        &self,
        task_id: TaskId,
        cancel: &CancellationToken,
        role: AgentRole,
        submit_tool: ToolSpec,
        max_turns: usize,
    ) -> Result<serde_json::Value> {
        let submit_name = submit_tool.name.clone();
        let mut tools: Vec<ToolSpec> = self
            .tool_specs
            .iter()
            .filter(|t| role.tool_allowlist().contains(&t.name.as_str()))
            .cloned()
            .collect();
        tools.push(submit_tool);

        for _ in 0..max_turns {
            if let Some(pending) = self.tasks.interrupted_tool_call(task_id).await? {
                self.issue_and_execute_tool_call(
                    task_id,
                    cancel,
                    pending.step_id,
                    &pending.tool,
                    pending.input,
                )
                .await?;
                continue;
            }

            let turn_index = self.tasks.count_model_calls(task_id).await?;
            let step_id = StepId::new();
            let model_effect_id = EffectId::new();
            self.tasks
                .append_journal(
                    task_id,
                    JournalEntryKind::EffectIssued {
                        effect_id: model_effect_id,
                        step_id,
                        effect_kind: kinds::MODEL_CALL.into(),
                        payload: serde_json::json!({
                            "turn_index": turn_index,
                            "role": role.as_str(),
                        }),
                    },
                )
                .await?;

            let model_role = self.preferred_model_role(role);
            let messages = self.build_conversation(task_id).await?;
            let request = GenerateRequest::new(messages)
                .with_tools(tools.clone())
                .with_turn_hint(turn_index);
            let routed = self
                .orchestrator
                .generate_action(model_role, request, cancel.child(), MAX_REFORMAT_RETRIES)
                .await?;
            let completion = routed.completion;

            self.tasks
                .append_journal(
                    task_id,
                    JournalEntryKind::EffectCompleted {
                        effect_id: model_effect_id,
                        step_id,
                        outcome_kind: kinds::MODEL_COMPLETION.into(),
                        payload: serde_json::json!({
                            "finish_reason": format!("{:?}", completion.finish_reason),
                            "text": completion.text,
                            "role": model_role.as_str(),
                            "model_id": routed.model_id,
                        }),
                    },
                )
                .await?;

            match ActionRequest::from_completion(&completion)? {
                ActionRequest::ToolCall { tool, input } if tool == submit_name => {
                    return Ok(input);
                }
                ActionRequest::ToolCall { tool, input } => {
                    self.issue_and_execute_tool_call(task_id, cancel, step_id, &tool, input)
                        .await?;
                }
                ActionRequest::Finish { .. } | ActionRequest::Ask { .. } => {
                    return Ok(serde_json::Value::Null);
                }
            }
        }
        Ok(serde_json::Value::Null)
    }
}

fn submit_research_brief_tool_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_research_brief".to_string(),
        description: "Submit your research findings before planning begins.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["summary"],
            "properties": {
                "summary": {"type": "string", "description": "What you found, in prose."},
                "relevant_files": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Paths most relevant to the task."
                },
                "open_questions": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Anything left unclear that planning should account for."
                }
            }
        }),
    }
}

fn submit_review_findings_tool_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_review_findings".to_string(),
        description: "Submit your review verdict on the implementation.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["approved"],
            "properties": {
                "approved": {
                    "type": "boolean",
                    "description": "Whether the change looks correct and safe to ship."
                },
                "findings": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Problems found, if any. Empty when approved with nothing to flag."
                }
            }
        }),
    }
}

fn parse_research_brief(value: serde_json::Value) -> Artifact {
    Artifact::ResearchBrief {
        summary: value
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(the researcher did not submit a brief)")
            .to_string(),
        relevant_files: string_array(&value, "relevant_files"),
        open_questions: string_array(&value, "open_questions"),
    }
}

fn parse_review_findings(value: serde_json::Value) -> Artifact {
    Artifact::ReviewFindings {
        // A reviewer that never submitted has approved nothing — the
        // absence of a verdict must never read as a silent pass.
        approved: value
            .get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        findings: {
            let mut f = string_array(&value, "findings");
            if value.get("approved").is_none() {
                f.push("the reviewer did not submit a verdict".to_string());
            }
            f
        },
    }
}

fn string_array(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
