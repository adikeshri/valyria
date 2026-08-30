//! `AgentDriver`: the step machine's driver (§4.24, D1). Executes one
//! `AgentState` at a time, journaling every effect before it runs and
//! every completion once it's done, so a `kill -9` between any two lines
//! of this file leaves the task in a state `recover_incomplete_tasks` and
//! this same driver can pick back up from — nothing here is cached only in
//! memory that the journal doesn't also durably record.
//!
//! Phase 3 simplification of §4.24's Reason -> Select -> Authorize ->
//! Execute -> Observe -> Update -> Retrieve sketch: Permission is a side
//! effect of the Tool call (via `ToolRuntime::invoke`'s internal
//! preflight+evaluate), not its own journaled effect kind; Context is
//! folded into the `Discovery` state's handling, not a separately issued
//! effect. Planning is a pass-through unless the driver is built with
//! [`PlanningMode::ModelAuthored`] (Phase 8), in which case `Planning`
//! asks the model for a plan, validates it, and repairs invalid plans
//! under a bounded budget — see [`crate::plan_exec`].
//!
//! Phase 7 adds the verify → diagnose → repair loop. Verification runs
//! are driver-initiated (not model tool calls): `Verifying` discovers the
//! repo's real commands, plans an escalation, runs the next check, and
//! persists it as `Evidence` (D4). A failure routes to `Diagnosing`
//! (distil the failure, feed the loop detector) and then `Repairing` (one
//! model-authored edit) before looping back to `Verifying`. The loop
//! detector and repair ledger are process-local per task — the durable
//! record is the journal plus the `verification_run` table, from which
//! the completion report is rebuilt.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use valyria_context::{AssembledContext, ContextAssembler, ContextQuery};
use valyria_ledger::Ledger;
use valyria_model::{GenerateRequest, Message};
use valyria_orchestrator::{Orchestrator, Role};
use valyria_permissions::{GrantScope, PermissionEngine};
use valyria_plan::PlanStore;
use valyria_sandbox::{ProcessLauncher, SandboxProfile};
use valyria_task::{kinds, ControlSignal, JournalEntryKind, TaskManager};
use valyria_tools::{InvocationResult, ToolCtx, ToolOutcome, ToolRuntime};
use valyria_types::{AgentState, EffectId, ProvenanceSource, StepId, TaskId, Trust};
use valyria_util::{CancellationToken, Clock, ContentHash};
use valyria_verify::{
    changeset_hash, diagnose, Diagnosis, EscalationStrategy, ProcessProbeRunner, VerificationLog,
    VerificationPlan, VerificationRun, VerificationRunner, Verifier,
};
use valyria_vfs::{HashCache, WorkspaceRoot};

use crate::action::ActionRequest;
use crate::error::{AgentError, Result};
use crate::loop_detect::{LoopDetector, LoopFinding, ProgressMetric, StepSignature};
use crate::repair::{RepairAttempt, RepairDecision, RepairLedger, RepairOutcome};

/// Token budget for the Phase 3 explicit-file context stage. Arbitrary but
/// generous — the real, configurable budget model is Phase 6's job.
const DEFAULT_CONTEXT_BUDGET_TOKENS: usize = 50_000;

/// Cap on repair cycles before the loop gives up to the user (§8).
const MAX_REPAIR_ATTEMPTS: u32 = 4;

/// Cap on plan-repair rounds before `Planning` fails to the user (§4.25:
/// "bounded repair attempts").
pub(crate) const MAX_PLAN_REPAIR_ATTEMPTS: u32 = 3;

/// Model turns granted to one plan step before the driver force-completes
/// it and lets the mandatory full `Verifying` run be the backstop. Bounds
/// the plan loop the same way `MAX_REPAIR_ATTEMPTS` bounds repair.
pub(crate) const MAX_STEP_TURNS: usize = 3;

/// The driver-level "action" a model uses to hand back a plan: a tool call
/// by this name whose arguments are the plan JSON. Not a real registered
/// tool — `step_planning` intercepts it before `ToolRuntime` ever sees it.
pub const SUBMIT_PLAN_ACTION: &str = "submit_plan";

/// Whether the `Planning` state asks the model for a plan or is the
/// Phase 3 pass-through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanningMode {
    /// `Planning` transitions straight to `Implementing` with no model
    /// call — every pre-Phase-8 scenario and test relies on this.
    #[default]
    Passthrough,
    /// `Planning` asks the model for a `submit_plan`, validates it, and
    /// repairs invalid plans under a bounded budget before `Implementing`.
    ModelAuthored,
}

/// Process-local verify/diagnose/repair bookkeeping for one task. Cloned
/// out, mutated, and written back by each state handler (only one driver
/// loop runs per task at a time). Not persisted: a cross-process resume
/// rebuilds the plan from scratch and the completion report from the
/// durable `verification_run` rows.
#[derive(Clone, Default)]
struct VerifyState {
    detector: LoopDetector,
    repair: RepairLedger,
    plan: Option<VerificationPlan>,
    executed: usize,
    last_passed: bool,
    last_run: Option<VerificationRun>,
    pending_diagnosis: Option<Diagnosis>,
    /// Set by an `EscalateStrategy` decision — the next plan drops style
    /// gates and keeps the full suite mandatory.
    broaden: bool,
    /// Role the repair Reason step uses; bumped by `SwitchRole`.
    repair_role_primary: bool,
    files_touched: BTreeSet<PathBuf>,
}

pub struct AgentDriver {
    pub(crate) tasks: Arc<TaskManager>,
    pub(crate) tools: Arc<ToolRuntime>,
    pub(crate) orchestrator: Arc<Orchestrator>,
    pub(crate) context: Arc<ContextAssembler>,
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) permissions: Arc<PermissionEngine>,
    pub(crate) verification_log: Arc<VerificationLog>,
    pub(crate) plan_store: Arc<PlanStore>,
    pub(crate) planning_mode: PlanningMode,
    pub(crate) workspace_root: WorkspaceRoot,
    pub(crate) hash_cache: Arc<HashCache>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) launcher: Arc<dyn ProcessLauncher>,
    pub(crate) sandbox_profile: SandboxProfile,
    verify_states: Mutex<HashMap<TaskId, VerifyState>>,
}

impl AgentDriver {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tasks: Arc<TaskManager>,
        tools: Arc<ToolRuntime>,
        orchestrator: Arc<Orchestrator>,
        context: Arc<ContextAssembler>,
        ledger: Arc<Ledger>,
        permissions: Arc<PermissionEngine>,
        verification_log: Arc<VerificationLog>,
        plan_store: Arc<PlanStore>,
        workspace_root: WorkspaceRoot,
        hash_cache: Arc<HashCache>,
        clock: Arc<dyn Clock>,
        launcher: Arc<dyn ProcessLauncher>,
        sandbox_profile: SandboxProfile,
    ) -> Self {
        Self {
            tasks,
            tools,
            orchestrator,
            context,
            ledger,
            permissions,
            verification_log,
            plan_store,
            planning_mode: PlanningMode::Passthrough,
            workspace_root,
            hash_cache,
            clock,
            launcher,
            sandbox_profile,
            verify_states: Mutex::new(HashMap::new()),
        }
    }

    /// Opt this driver into model-authored planning (§4.25). Off by
    /// default so every pre-Phase-8 scenario keeps its exact behaviour.
    pub fn with_planning_mode(mut self, mode: PlanningMode) -> Self {
        self.planning_mode = mode;
        self
    }

    /// Runs `task_id` until it reaches a terminal state, is paused, or asks
    /// to wait on the user/a permission decision. Pause/cancel are checked
    /// only between steps (never mid-effect) so a pause always lands
    /// cleanly on a step boundary.
    pub async fn run(&self, task_id: TaskId, cancel: CancellationToken) -> Result<()> {
        loop {
            if cancel.is_cancelled() {
                self.tasks
                    .transition(task_id, AgentState::Cancelled)
                    .await?;
                return Ok(());
            }

            let task = self.tasks.get(task_id).await?;
            if let Some(signal) = task.pending_signal {
                match signal {
                    ControlSignal::CancelRequested => {
                        self.tasks
                            .transition(task_id, AgentState::Cancelled)
                            .await?;
                        return Ok(());
                    }
                    ControlSignal::PauseRequested => {
                        self.tasks.transition(task_id, AgentState::Paused).await?;
                        return Ok(());
                    }
                }
            }

            match task.state {
                AgentState::Idle => {
                    self.tasks
                        .transition(task_id, AgentState::Understanding)
                        .await?;
                }
                AgentState::Understanding => {
                    self.tasks
                        .transition(task_id, AgentState::Discovery)
                        .await?;
                }
                AgentState::Discovery => {
                    let ctx = self.build_ctx(task_id, StepId::new(), cancel.child());
                    let assembled = self
                        .context
                        .assemble(&ctx, ContextQuery::new(DEFAULT_CONTEXT_BUDGET_TOKENS))
                        .await?;
                    self.journal_context_retrieved(
                        task_id,
                        &assembled,
                        DEFAULT_CONTEXT_BUDGET_TOKENS,
                    )
                    .await?;
                    self.tasks.transition(task_id, AgentState::Planning).await?;
                }
                AgentState::Planning => match self.planning_mode {
                    PlanningMode::Passthrough => {
                        self.tasks
                            .transition(task_id, AgentState::Implementing)
                            .await?;
                    }
                    PlanningMode::ModelAuthored => {
                        if self.step_planning(task_id, &cancel).await? == Flow::Return {
                            return Ok(());
                        }
                    }
                },
                AgentState::Implementing => {
                    if self.task_has_plan(task_id).await? {
                        if self.step_implementing_plan(task_id, &cancel).await? == Flow::Return {
                            return Ok(());
                        }
                    } else {
                        self.step_implementing(task_id, &cancel).await?;
                    }
                }
                AgentState::Verifying => {
                    if self.step_verifying(task_id, &cancel).await? == Flow::Return {
                        return Ok(());
                    }
                }
                AgentState::Diagnosing => {
                    self.step_diagnosing(task_id, &cancel).await?;
                }
                AgentState::Repairing => {
                    if self.step_repairing(task_id, &cancel).await? == Flow::Return {
                        return Ok(());
                    }
                }
                AgentState::WaitingForPermission | AgentState::WaitingForUser => {
                    return Ok(());
                }
                AgentState::Completed | AgentState::Failed | AgentState::Cancelled => {
                    self.verify_states.lock().unwrap().remove(&task_id);
                    return Ok(());
                }
                AgentState::Paused => {
                    return Ok(());
                }
            }
        }
    }

    async fn step_implementing(&self, task_id: TaskId, cancel: &CancellationToken) -> Result<()> {
        // D1: re-issue any effect that was issued but never completed.
        if let Some(pending) = self.tasks.interrupted_tool_call(task_id).await? {
            self.issue_and_execute_tool_call(
                task_id,
                cancel,
                pending.step_id,
                &pending.tool,
                pending.input,
            )
            .await?;
            return Ok(());
        }

        // Reason
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
                    payload: serde_json::json!({"turn_index": turn_index}),
                },
            )
            .await?;

        let messages = vec![Message::user(self.tasks.get(task_id).await?.objective)];
        let request = GenerateRequest::new(messages).with_turn_hint(turn_index);
        let completion = self
            .orchestrator
            .generate(Role::PrimaryCoder, request, cancel.child())
            .await?;

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
                    }),
                },
            )
            .await?;

        // Select
        let action = ActionRequest::from_completion(&completion)?;
        match action {
            ActionRequest::Finish { .. } => {
                self.tasks
                    .transition(task_id, AgentState::Verifying)
                    .await?;
            }
            ActionRequest::Ask { .. } => {
                self.tasks
                    .transition(task_id, AgentState::WaitingForUser)
                    .await?;
            }
            ActionRequest::ToolCall { tool, input } => {
                self.issue_and_execute_tool_call(task_id, cancel, step_id, &tool, input)
                    .await?;
            }
        }
        Ok(())
    }

    // --- Phase 7: verify -> diagnose -> repair ---------------------------

    /// Run the next check in the escalation plan. Returns `Flow::Return`
    /// when the task reached a terminal state.
    async fn step_verifying(&self, task_id: TaskId, cancel: &CancellationToken) -> Result<Flow> {
        let mut vs = self.take_verify_state(task_id);
        let changed = self.task_changed_files(task_id);

        // (Re)build the plan on first entry or after a repair widened it.
        if vs.plan.is_none() {
            let report = valyria_verify::scan(self.workspace_root.as_path());
            if report.is_empty() {
                // Nothing to verify — honest completion, exactly as Phase 3.
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::Completed)
                    .await?;
                return Ok(Flow::Return);
            }
            let probe = ProcessProbeRunner::new(self.workspace_root.as_path().to_path_buf());
            let tooling = valyria_verify::validate(&report, &probe).await;
            if tooling.validated.is_empty() {
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::Completed)
                    .await?;
                return Ok(Flow::Return);
            }
            let opts = valyria_verify::strategy::StrategyOptions {
                include_style: !vs.broaden,
                require_full_suite: true,
            };
            let plan = EscalationStrategy::plan(
                &tooling.validated,
                &valyria_verify::ChangeSet::from_files(changed.clone()),
                &[],
                &opts,
            );
            if plan.is_empty() {
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::Completed)
                    .await?;
                return Ok(Flow::Return);
            }
            vs.plan = Some(plan);
            vs.executed = 0;
            vs.last_passed = true;
        }

        let plan = vs.plan.clone().unwrap();
        let step = match plan.next_after(vs.executed, vs.last_passed) {
            Some(step) => step.clone(),
            None if !vs.last_passed => {
                // The last check failed — go diagnose it.
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::Diagnosing)
                    .await?;
                return Ok(Flow::Continue);
            }
            None => {
                // Plan exhausted, everything passed, broad run satisfied.
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::Completed)
                    .await?;
                return Ok(Flow::Return);
            }
        };

        let step_id = StepId::new();
        let effect_id = EffectId::new();
        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectIssued {
                    effect_id,
                    step_id,
                    effect_kind: kinds::VERIFY.into(),
                    payload: serde_json::json!({
                        "command": step.command.display(),
                        "kind": step.command.kind.as_str(),
                        "tier": format!("{:?}", step.tier),
                        "rationale": step.rationale,
                    }),
                },
            )
            .await?;

        let runner = VerificationRunner::new(
            self.workspace_root.as_path().to_path_buf(),
            self.clock.clone(),
        )
        .with_sandbox(self.launcher.clone(), self.sandbox_profile.clone());

        let cs_hash = changeset_hash(&changed);
        let run = runner
            .run(
                &step.command,
                Some(step.tier),
                Some(cs_hash),
                cancel.child(),
            )
            .await
            .map_err(|e| AgentError::MalformedCompletion {
                detail: format!("verification runner: {e}"),
            })?;

        self.verification_log
            .record(task_id, &run)
            .await
            .map_err(|e| AgentError::MalformedCompletion {
                detail: format!("verification log: {e}"),
            })?;

        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectCompleted {
                    effect_id,
                    step_id,
                    outcome_kind: kinds::VERIFY_RESULT.into(),
                    payload: serde_json::json!({
                        "command": run.command.display(),
                        "passed": run.passed(),
                        "outcome": format!("{:?}", run.outcome),
                        "exit_code": run.exit_code,
                        "failure_count": run.failures.len(),
                        "run_id": run.id.to_string(),
                        "digest": run.digest(3),
                    }),
                },
            )
            .await?;

        vs.executed += 1;
        vs.last_passed = run.passed();
        vs.last_run = Some(run.clone());

        if run.passed() {
            vs.detector.observe_failure(None);
            self.put_verify_state(task_id, vs);
            // Stay in Verifying: the outer loop re-enters and runs the
            // next step (or completes when the plan is exhausted).
            Ok(Flow::Continue)
        } else {
            self.put_verify_state(task_id, vs);
            self.tasks
                .transition(task_id, AgentState::Diagnosing)
                .await?;
            Ok(Flow::Continue)
        }
    }

    async fn step_diagnosing(&self, task_id: TaskId, _cancel: &CancellationToken) -> Result<()> {
        let mut vs = self.take_verify_state(task_id);
        let changed = self.task_changed_files(task_id);

        let failures = vs
            .last_run
            .as_ref()
            .map(|r| r.failures.clone())
            .unwrap_or_default();
        // No graph wiring in the live loop yet (Phase 6 follow-up); an
        // empty neighbour set means suspects come from the failure
        // locations ∩ the change ledger alone.
        let diagnosis = diagnose(&failures, &changed, &[]);
        let fingerprint = diagnosis.fingerprint();

        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectCompleted {
                    effect_id: EffectId::new(),
                    step_id: StepId::new(),
                    outcome_kind: kinds::DIAGNOSIS.into(),
                    payload: serde_json::json!({
                        "summary": diagnosis.summary,
                        "fingerprint": fingerprint,
                        "suspects": diagnosis
                            .suspects
                            .iter()
                            .take(5)
                            .map(|s| s.path.display().to_string())
                            .collect::<Vec<_>>(),
                        "digest": diagnosis.context_digest(3, 3),
                    }),
                },
            )
            .await?;

        // Loop / progress detection.
        for f in changed.iter().cloned() {
            vs.files_touched.insert(f);
        }
        let file_state = self.changed_files_state_hash(&changed);
        let step_sig = StepSignature::default()
            .with_error(&fingerprint)
            .with_file_state(file_state);
        let finding = vs
            .detector
            .observe_step(step_sig)
            .or_else(|| vs.detector.observe_failure(Some(&fingerprint)))
            .or_else(|| {
                vs.detector.observe_progress(ProgressMetric {
                    verification_frontier: vs.executed,
                    failure_count: failures.len(),
                    files_touched: vs.files_touched.clone(),
                })
            });

        if let Some(finding) = &finding {
            self.tasks
                .append_journal(
                    task_id,
                    JournalEntryKind::EffectCompleted {
                        effect_id: EffectId::new(),
                        step_id: StepId::new(),
                        outcome_kind: kinds::LOOP_DETECTED.into(),
                        payload: serde_json::json!({
                            "class": finding.code(),
                            "detail": describe_finding(finding),
                        }),
                    },
                )
                .await?;
        }

        let decision = vs.repair.decide(&fingerprint, finding.as_ref());
        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectCompleted {
                    effect_id: EffectId::new(),
                    step_id: StepId::new(),
                    outcome_kind: kinds::REPAIR_DECISION.into(),
                    payload: serde_json::json!({"decision": describe_decision(&decision)}),
                },
            )
            .await?;

        vs.pending_diagnosis = Some(diagnosis);

        match decision {
            RepairDecision::Continue => {
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::Repairing)
                    .await?;
            }
            RepairDecision::EscalateStrategy => {
                vs.repair.mark_escalated();
                vs.broaden = true;
                vs.plan = None; // rebuilt wider on the way back through Verifying
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::Repairing)
                    .await?;
            }
            RepairDecision::SwitchRole => {
                vs.repair.mark_switched_role();
                vs.repair_role_primary = true;
                vs.plan = None;
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::Repairing)
                    .await?;
            }
            RepairDecision::AskUser { reason } => {
                self.tasks
                    .append_journal(
                        task_id,
                        JournalEntryKind::RecoveryNote {
                            note: format!("repair paused for the user: {reason}"),
                        },
                    )
                    .await?;
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::WaitingForUser)
                    .await?;
            }
            RepairDecision::GiveUp { reason } => {
                self.tasks
                    .append_journal(
                        task_id,
                        JournalEntryKind::RecoveryNote {
                            note: format!("repair gave up: {reason}"),
                        },
                    )
                    .await?;
                self.verify_states.lock().unwrap().remove(&task_id);
                self.tasks.transition(task_id, AgentState::Failed).await?;
            }
        }
        Ok(())
    }

    async fn step_repairing(&self, task_id: TaskId, cancel: &CancellationToken) -> Result<Flow> {
        // D1: an interrupted repair edit is redone before anything new.
        if let Some(pending) = self.tasks.interrupted_tool_call(task_id).await? {
            self.issue_and_execute_tool_call(
                task_id,
                cancel,
                pending.step_id,
                &pending.tool,
                pending.input,
            )
            .await?;
            if self.tasks.get(task_id).await?.state == AgentState::Repairing {
                self.tasks
                    .transition(task_id, AgentState::Verifying)
                    .await?;
            }
            return Ok(Flow::Continue);
        }

        let mut vs = self.take_verify_state(task_id);
        let digest = vs
            .pending_diagnosis
            .as_ref()
            .map(|d| d.context_digest(4, 4))
            .unwrap_or_else(|| "a verification check failed".to_string());
        let fingerprint = vs
            .pending_diagnosis
            .as_ref()
            .map(|d| d.fingerprint())
            .unwrap_or_default();
        // Only `PrimaryCoder` is bound in Phase 7; `SwitchRole` still
        // advances the escalation ladder in `RepairLedger::decide` (next
        // stop: the user) even though the role binding is unchanged until
        // a `FastCoder`/`PrimaryCoder` split lands with real models.
        let role = Role::PrimaryCoder;
        let _ = vs.repair_role_primary;

        // Reason (repair-focused).
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
                    payload: serde_json::json!({"turn_index": turn_index, "phase": "repair"}),
                },
            )
            .await?;

        let objective = self.tasks.get(task_id).await?.objective;
        let prompt = format!(
            "{objective}\n\nA verification check just failed. Make the minimal edit that \
             fixes it, then finish.\n\n{digest}"
        );
        let request = GenerateRequest::new(vec![Message::user(prompt)]).with_turn_hint(turn_index);
        let completion = self
            .orchestrator
            .generate(role, request, cancel.child())
            .await?;

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
                    }),
                },
            )
            .await?;

        let action = ActionRequest::from_completion(&completion)?;
        let (edit_summary, outcome) = match action {
            ActionRequest::ToolCall { tool, input } => {
                let summary = format!("{tool} {input}");
                self.issue_and_execute_tool_call(task_id, cancel, step_id, &tool, input)
                    .await?;
                (summary, RepairOutcome::Improved)
            }
            ActionRequest::Finish { .. } => {
                // The model believes it is fixed without editing — let the
                // re-verification be the judge.
                (
                    "no edit (model finished)".to_string(),
                    RepairOutcome::NoChange,
                )
            }
            ActionRequest::Ask { .. } => {
                vs.repair.record(RepairAttempt {
                    attempt: 0,
                    diagnosis_fingerprint: fingerprint,
                    edit_summary: "model asked a question".into(),
                    outcome: RepairOutcome::NoChange,
                });
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::WaitingForUser)
                    .await?;
                return Ok(Flow::Return);
            }
        };

        vs.repair.record(RepairAttempt {
            attempt: 0,
            diagnosis_fingerprint: fingerprint,
            edit_summary,
            outcome,
        });
        vs.plan = None; // re-verify from the start of the escalation
        vs.executed = 0;
        vs.last_passed = true;
        vs.detector.observe_step(
            StepSignature::default()
                .with_file_state(self.changed_files_state_hash(&self.task_changed_files(task_id))),
        );

        let state_now = self.tasks.get(task_id).await?.state;
        if state_now == AgentState::Repairing {
            self.put_verify_state(task_id, vs);
            self.tasks
                .transition(task_id, AgentState::Verifying)
                .await?;
            Ok(Flow::Continue)
        } else {
            // The edit needed permission (WAITING_FOR_PERMISSION) or was
            // denied (FAILED) — `issue_and_execute_tool_call` already
            // transitioned; just persist the ledger and stop.
            if state_now == AgentState::Failed {
                self.verify_states.lock().unwrap().remove(&task_id);
            } else {
                self.put_verify_state(task_id, vs);
            }
            Ok(Flow::Return)
        }
    }

    // --- shared tool-call plumbing -----------------------------------

    pub(crate) async fn issue_and_execute_tool_call(
        &self,
        task_id: TaskId,
        cancel: &CancellationToken,
        step_id: StepId,
        tool: &str,
        input: serde_json::Value,
    ) -> Result<()> {
        let effect_id = EffectId::new();
        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectIssued {
                    effect_id,
                    step_id,
                    effect_kind: kinds::TOOL.into(),
                    payload: serde_json::json!({"tool": tool, "input": input}),
                },
            )
            .await?;

        let ctx = self.build_ctx(task_id, step_id, cancel.child());
        self.record_baseline_if_path(&ctx, &input);

        let result = self.tools.invoke(&ctx, tool, input).await;
        self.observe_tool_result(task_id, step_id, effect_id, result)
            .await
    }

    async fn observe_tool_result(
        &self,
        task_id: TaskId,
        step_id: StepId,
        effect_id: EffectId,
        result: InvocationResult,
    ) -> Result<()> {
        match result {
            InvocationResult::Executed { outcome, record } => {
                let (success, rendered) = match &outcome {
                    ToolOutcome::Success { rendered, .. } => (true, rendered.clone()),
                    ToolOutcome::Failure { rendered, .. } => (false, rendered.clone()),
                };
                self.tasks
                    .append_journal(
                        task_id,
                        JournalEntryKind::EffectCompleted {
                            effect_id,
                            step_id,
                            outcome_kind: kinds::TOOL_RESULT.into(),
                            payload: serde_json::json!({
                                "success": success,
                                "tool_invocation_id": record.id.to_string(),
                                "rendered": rendered,
                            }),
                        },
                    )
                    .await?;
                Ok(())
            }
            InvocationResult::AskRequired { prompt, request } => {
                self.tasks
                    .append_journal(
                        task_id,
                        JournalEntryKind::EffectCompleted {
                            effect_id,
                            step_id,
                            outcome_kind: kinds::PERMISSION_ASK.into(),
                            payload: serde_json::json!({
                                "prompt": prompt,
                                "tool": request.tool,
                                "category": format!("{:?}", request.category),
                                "target": request.target,
                                "risk": format!("{:?}", request.risk),
                            }),
                        },
                    )
                    .await?;
                self.tasks
                    .transition(task_id, AgentState::WaitingForPermission)
                    .await?;
                Ok(())
            }
            InvocationResult::Denied { reason } => {
                self.tasks
                    .append_journal(
                        task_id,
                        JournalEntryKind::EffectCompleted {
                            effect_id,
                            step_id,
                            outcome_kind: kinds::TOOL_DENIED.into(),
                            payload: serde_json::json!({"reason": reason}),
                        },
                    )
                    .await?;
                self.tasks.transition(task_id, AgentState::Failed).await?;
                Ok(())
            }
            InvocationResult::UnknownTool(name) => Err(AgentError::UnknownTool(name)),
        }
    }

    pub(crate) fn record_baseline_if_path(&self, ctx: &ToolCtx, input: &serde_json::Value) {
        let Some(path) = input.get("path").and_then(|v| v.as_str()) else {
            return;
        };
        let hash = ctx
            .workspace_root
            .resolve(path)
            .ok()
            .and_then(|resolved| self.hash_cache.hash_file(&resolved).ok());
        self.ledger.record_baseline(PathBuf::from(path), hash);
    }

    pub async fn resolve_permission(&self, task_id: TaskId, approve: bool) -> Result<()> {
        let task = self.tasks.get(task_id).await?;
        if task.state != AgentState::WaitingForPermission {
            return Err(AgentError::NotWaitingForPermission(task_id));
        }
        let pending = self
            .tasks
            .pending_tool_call(task_id)
            .await?
            .ok_or(AgentError::NoPendingToolCall(task_id))?;
        let tool = self
            .tools
            .get_tool(&pending.tool)
            .ok_or_else(|| AgentError::UnknownTool(pending.tool.clone()))?;

        let ctx = self.build_ctx(task_id, pending.step_id, CancellationToken::new());
        let effect_id = pending.effect_id;

        if !approve {
            self.tasks
                .append_journal(
                    task_id,
                    JournalEntryKind::EffectCompleted {
                        effect_id,
                        step_id: pending.step_id,
                        outcome_kind: kinds::TOOL_DENIED.into(),
                        payload: serde_json::json!({"reason": "denied by user"}),
                    },
                )
                .await?;
            self.tasks.transition(task_id, AgentState::Failed).await?;
            return Ok(());
        }

        let request =
            tool.preflight(&ctx, &pending.input)
                .map_err(|e| AgentError::MalformedCompletion {
                    detail: e.to_string(),
                })?;
        let auth = self.permissions.approve(request, GrantScope::OneShot, None);
        let result = self
            .tools
            .invoke_with_authorization(&ctx, &pending.tool, pending.input, auth)
            .await;

        match &result {
            InvocationResult::Executed { .. } | InvocationResult::Denied { .. } => {
                self.observe_tool_result(task_id, pending.step_id, effect_id, result)
                    .await?;
                // Resume into whichever loop phase asked for the edit.
                if self.tasks.get(task_id).await?.state == AgentState::WaitingForPermission {
                    let resume_to = if self.verify_states.lock().unwrap().contains_key(&task_id) {
                        AgentState::Verifying
                    } else {
                        AgentState::Implementing
                    };
                    self.tasks.transition(task_id, resume_to).await?;
                }
                Ok(())
            }
            InvocationResult::AskRequired { .. } => {
                unreachable!("invoke_with_authorization executes directly and never re-asks")
            }
            InvocationResult::UnknownTool(name) => Err(AgentError::UnknownTool(name.clone())),
        }
    }

    // --- helpers ---------------------------------------------------

    pub(crate) fn task_changed_files(&self, task_id: TaskId) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self
            .ledger
            .entries_for_task(task_id)
            .into_iter()
            .map(|e| e.path)
            .collect();
        files.sort();
        files.dedup();
        files
    }

    /// A hash over the current on-disk content of every file the task has
    /// touched — the "file state" axis of loop detection.
    fn changed_files_state_hash(&self, files: &[PathBuf]) -> ContentHash {
        let mut buf = Vec::new();
        for f in files {
            buf.extend_from_slice(f.to_string_lossy().as_bytes());
            buf.push(0);
            if let Ok(resolved) = self.workspace_root.resolve(f.to_string_lossy().as_ref()) {
                if let Ok(bytes) = std::fs::read(&resolved) {
                    buf.extend_from_slice(&bytes);
                }
            }
            buf.push(0);
        }
        ContentHash::of_bytes(&buf)
    }

    fn take_verify_state(&self, task_id: TaskId) -> VerifyState {
        self.verify_states
            .lock()
            .unwrap()
            .entry(task_id)
            .or_insert_with(|| VerifyState {
                repair: RepairLedger::new(MAX_REPAIR_ATTEMPTS),
                ..VerifyState::default()
            })
            .clone()
    }

    fn put_verify_state(&self, task_id: TaskId, state: VerifyState) {
        self.verify_states.lock().unwrap().insert(task_id, state);
    }

    /// Record what the context assembler retrieved for a step so it
    /// projects to a `context_retrieved` event (§34, G7). Read-only: this
    /// is a completed-effect journal entry with no matching issue.
    async fn journal_context_retrieved(
        &self,
        task_id: TaskId,
        assembled: &AssembledContext,
        budget_total: usize,
    ) -> Result<()> {
        let items: Vec<serde_json::Value> = assembled
            .items
            .iter()
            .map(|item| {
                let path = match &item.provenance.source {
                    ProvenanceSource::File { path } => path.clone(),
                    ProvenanceSource::Instruction { path } => path.clone(),
                    ProvenanceSource::ToolOutput { invocation } => format!("tool:{invocation}"),
                    ProvenanceSource::Git { commit } => format!("git:{commit}"),
                    ProvenanceSource::Memory { id } => format!("memory:{id}"),
                    ProvenanceSource::ModelTurn => "<model turn>".to_string(),
                };
                let reason = if item.provenance.retrieval_path.is_empty() {
                    "explicit".to_string()
                } else {
                    item.provenance.retrieval_path.join(" -> ")
                };
                serde_json::json!({
                    "path": path,
                    "reason": reason,
                    "trust_level": trust_level_str(item.trust),
                    "tokens": item.tokens,
                    "score": item.provenance.score,
                })
            })
            .collect();

        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectCompleted {
                    effect_id: EffectId::new(),
                    step_id: StepId::new(),
                    outcome_kind: kinds::CONTEXT_RETRIEVED.into(),
                    payload: serde_json::json!({
                        "items": items,
                        "budget_used": assembled.total_tokens,
                        "budget_total": budget_total,
                    }),
                },
            )
            .await?;
        Ok(())
    }

    pub(crate) fn build_ctx(
        &self,
        task_id: TaskId,
        step_id: StepId,
        cancel: CancellationToken,
    ) -> ToolCtx {
        ToolCtx {
            workspace_root: self.workspace_root.clone(),
            hash_cache: self.hash_cache.clone(),
            ledger: self.ledger.clone(),
            task_id,
            step_id,
            cancel,
            launcher: self.launcher.clone(),
            sandbox_profile: self.sandbox_profile.clone(),
        }
    }
}

/// Whether a state handler wants the outer `run` loop to keep going or to
/// return (the task reached a state no driver should keep spinning on).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    Continue,
    Return,
}

fn trust_level_str(t: Trust) -> &'static str {
    match t {
        Trust::Policy => "policy",
        Trust::Instruction => "instruction",
        Trust::Evidence => "evidence",
        Trust::RepoData => "repo_data",
        Trust::ModelOutput => "model_output",
    }
}

fn describe_finding(f: &LoopFinding) -> String {
    match f {
        LoopFinding::ExactRepeat { count, .. } => format!("identical step ×{count}"),
        LoopFinding::Oscillation { period } => format!("cycle of period {period}"),
        LoopFinding::RepeatedFailure { count, .. } => format!("same failure ×{count}"),
        LoopFinding::NoChangeIteration { iterations } => {
            format!("{iterations} steps, no file change")
        }
        LoopFinding::FrontierStalled { iterations } => {
            format!("{iterations} cycles, no progress")
        }
    }
}

fn describe_decision(d: &RepairDecision) -> String {
    match d {
        RepairDecision::Continue => "continue".into(),
        RepairDecision::EscalateStrategy => "escalate_strategy".into(),
        RepairDecision::SwitchRole => "switch_role".into(),
        RepairDecision::AskUser { reason } => format!("ask_user: {reason}"),
        RepairDecision::GiveUp { reason } => format!("give_up: {reason}"),
    }
}
