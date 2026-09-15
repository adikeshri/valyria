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

use valyria_context::{
    AssembledContext, AssembledPrompt, ContextAssembler, ContextEngine, ContextQuery, EngineInput,
    LiveRetriever,
};
use valyria_instructions::Discovery;
use valyria_ledger::Ledger;
use valyria_model::{GenerateRequest, Message, ToolSpec};
use valyria_orchestrator::{Role, RoleRouter};
use valyria_permissions::{GrantScope, PermissionEngine};
use valyria_plan::PlanStore;
use valyria_sandbox::{ProcessLauncher, SandboxProfile};
use valyria_task::{kinds, ControlSignal, JournalEntryKind, JournalSeq, TaskManager};
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
use crate::tool_specs::bound_tool_specs;

/// Token budget for the Phase 3 explicit-file context stage. Arbitrary but
/// generous — the real, configurable budget model is Phase 6's job.
const DEFAULT_CONTEXT_BUDGET_TOKENS: usize = 50_000;

/// Cap on repair cycles before the loop gives up to the user (§8).
const MAX_REPAIR_ATTEMPTS: u32 = 4;

/// Bounded reformat-retries `generate_action` gets before giving up on a
/// model that won't produce a parseable action — matches the value every
/// existing `generate_action` test in `valyria-orchestrator` already uses.
pub(crate) const MAX_REFORMAT_RETRIES: u32 = 2;

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

/// How a client resolved an outstanding approval (§13, G2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Approve just this one tool call.
    Once,
    /// Approve, and grant the same class of request for the rest of the
    /// task (`GrantScope::Task`).
    Task,
    /// Deny — the task fails.
    Deny,
}

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
/// loop runs per task at a time), and cached in `verify_states` for the
/// rest of this process's lifetime once populated.
///
/// `plan`/`executed`/`last_run`/`pending_diagnosis` are *not* persisted: a
/// cross-process resume rebuilds the escalation plan from scratch (the
/// next `Verifying` entry runs `valyria_verify::scan` again) and the
/// completion report from the durable `verification_run` rows — exactly
/// as before M5.
///
/// `detector` and `repair`, however, **are** reconstructed on first
/// access in a fresh process (M5, C8) — see [`reconstruct_verify_state`].
/// Losing loop-detector history or the repair attempt budget across a
/// crash would mean the exact failure mode these two exist to catch
/// (infinite retries) becomes reachable again just by restarting the
/// process mid-repair.
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
    pub(crate) orchestrator: Arc<RoleRouter>,
    pub(crate) context: Arc<ContextAssembler>,
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) permissions: Arc<PermissionEngine>,
    pub(crate) verification_log: Arc<VerificationLog>,
    pub(crate) plan_store: Arc<PlanStore>,
    pub(crate) planning_mode: PlanningMode,
    /// Real repository retrieval for every implementing/repairing turn's
    /// context (M2). Defaults to `LiveRetriever::Static(empty)` — every
    /// pre-M2 scenario's exact behaviour; set via `with_retriever`.
    pub(crate) retriever: LiveRetriever,
    /// The workspace database, handed to every `ToolCtx` this driver
    /// builds (M2) so `search` / `symbol_search` can open the index/graph
    /// tables. `None` — every pre-M2 scenario's exact behaviour (those
    /// tools report plainly that no index is available) — unless
    /// `with_store` was called.
    pub(crate) store: Option<Arc<valyria_store::Store>>,
    /// Repository memory (M3): written to after a task's mandatory broad
    /// verification run actually passes — commands observed to work,
    /// repeatedly-failing commands as pitfalls (`valyria_memory::extract`).
    /// `None` — nothing is extracted, every pre-M3 scenario's exact
    /// behaviour — unless `with_memory` was called.
    pub(crate) memory: Option<Arc<valyria_memory::MemoryStore>>,
    pub(crate) workspace_root: WorkspaceRoot,
    pub(crate) hash_cache: Arc<HashCache>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) launcher: Arc<dyn ProcessLauncher>,
    pub(crate) sandbox_profile: SandboxProfile,
    verify_states: Mutex<HashMap<TaskId, VerifyState>>,
    /// Every tool the model may call this turn — computed once from the
    /// registry `tools` was built with; the registry is static after
    /// construction so there's nothing to keep in sync.
    pub(crate) tool_specs: Vec<ToolSpec>,
}

impl AgentDriver {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tasks: Arc<TaskManager>,
        tools: Arc<ToolRuntime>,
        orchestrator: Arc<RoleRouter>,
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
        let tool_specs = bound_tool_specs(&tools);
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
            retriever: LiveRetriever::empty(),
            store: None,
            memory: None,
            workspace_root,
            hash_cache,
            clock,
            launcher,
            sandbox_profile,
            verify_states: Mutex::new(HashMap::new()),
            tool_specs,
        }
    }

    /// Opt this driver into model-authored planning (§4.25). Off by
    /// default so every pre-Phase-8 scenario keeps its exact behaviour.
    pub fn with_planning_mode(mut self, mode: PlanningMode) -> Self {
        self.planning_mode = mode;
        self
    }

    /// Wire real repository retrieval into every implementing/repairing
    /// turn's context (§4.24/M2). `LiveRetriever::Static(empty)` — every
    /// pre-M2 scenario's exact behaviour — is the default; call this with
    /// `LiveRetriever::Search(..)` once an index generation exists for the
    /// workspace.
    pub fn with_retriever(mut self, retriever: LiveRetriever) -> Self {
        self.retriever = retriever;
        self
    }

    /// Give every tool call this driver issues access to the workspace
    /// database (M2), so `search` / `symbol_search` can open the index.
    pub fn with_store(mut self, store: Arc<valyria_store::Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// Extract and persist repository memory after a task's mandatory
    /// broad verification run passes (M3).
    pub fn with_memory(mut self, memory: Arc<valyria_memory::MemoryStore>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Runs `task_id` until it reaches a terminal state, is paused, or asks
    /// to wait on the user/a permission decision. Pause/cancel are checked
    /// only between steps (never mid-effect) so a pause always lands
    /// cleanly on a step boundary.
    pub async fn run(&self, task_id: TaskId, cancel: CancellationToken) -> Result<()> {
        self.run_with_role(task_id, cancel, None).await
    }

    /// M5: identical to [`AgentDriver::run`], except every `Planning`/
    /// plan-driven `Implementing` model call uses `role_override` (when
    /// `Some`) instead of the ordinary FastCoder/PrimaryCoder escalation
    /// `model_role` picks. `run` is a thin `None`-forwarding wrapper around
    /// this, so every pre-M5 call site's behavior is unchanged bit-for-bit.
    ///
    /// The role pipeline ([`crate::role_pipeline`]) uses this for its
    /// Implementer child: without it, that child's `Planning`/`Implementing`
    /// calls would resolve through the exact same global FastCoder-bound
    /// check every ordinary task's does, which is indistinguishable from —
    /// and would collide with — whatever role-specific model the pipeline's
    /// other roles (Researcher, Reviewer) are bound to.
    pub(crate) async fn run_with_role(
        &self,
        task_id: TaskId,
        cancel: CancellationToken,
        role_override: Option<Role>,
    ) -> Result<()> {
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
                        if self.step_planning(task_id, &cancel, role_override).await?
                            == Flow::Return
                        {
                            return Ok(());
                        }
                    }
                },
                AgentState::Implementing => {
                    if self.task_has_plan(task_id).await? {
                        if self
                            .step_implementing_plan(task_id, &cancel, role_override)
                            .await?
                            == Flow::Return
                        {
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

    /// The system + task-intent messages every implementing/repairing turn
    /// opens with: the runtime policy, any repo instructions
    /// (`VALYRIA.md`/`AGENTS.md`/`CLAUDE.md`), retrieved repository context
    /// (M2 — real when `self.retriever` is `LiveRetriever::Search`, empty
    /// otherwise), then the objective. Rebuilt fresh every call rather than
    /// cached — `Discovery::discover` reads a handful of size-capped files
    /// and a retrieval is one ranked-search call, both negligible next to
    /// a model call, and it means an instruction file edited (or the index
    /// advancing a generation) mid-task takes effect on the very next
    /// turn.
    async fn system_and_task_messages(&self, task_id: TaskId) -> Result<Vec<Message>> {
        let objective = self.tasks.get(task_id).await?.objective;
        let instructions = Discovery::new(self.workspace_root.as_path()).discover()?;
        let engine = ContextEngine::new(self.retriever.clone());
        let input = EngineInput::new(objective, DEFAULT_CONTEXT_BUDGET_TOKENS)
            .with_instructions(instructions);
        let assembled = engine.build(input).await?;
        self.journal_prompt_context_retrieved(task_id, &assembled, DEFAULT_CONTEXT_BUDGET_TOKENS)
            .await?;
        Ok(assembled.messages)
    }

    /// Reconstruct this task's tool-call history as alternating
    /// assistant/tool-result turns from the journal — the same durable
    /// source of truth D1 crash-recovery already treats as authoritative,
    /// rather than new in-memory-only state. Only *resolved* tool
    /// effects are replayed (`TOOL_RESULT`/`TOOL_DENIED`); an
    /// issued-but-unresolved effect can't reach this call — it's either
    /// mid-crash-recovery (handled by the `interrupted_tool_call` check
    /// at the top of `step_implementing`/`step_repairing`, which returns
    /// before this runs) or mid-permission-ask (the task sits in
    /// `WaitingForPermission` and never re-enters these steps).
    ///
    /// The assistant's own tool-call turn is synthesized as plain text
    /// (`Message::assistant("Calling ...")`) rather than a structured
    /// wire `tool_calls` array — the OpenAI-compat wire layer doesn't
    /// serialize one for outgoing messages today, and extending it is out
    /// of scope here. Worth revisiting once this is exercised against a
    /// real model rather than the fake runtime.
    ///
    /// M4 extends the replay to a model-asked question and the user's
    /// answer (`ActionRequest::Ask` / `TaskManager::respond_to_user`):
    /// there is no effect id to correlate a question against its answer
    /// (the model asked; nothing issued a request), so those two turns
    /// are paired by adjacency instead — the most recent unanswered
    /// `MODEL_COMPLETION{finish_reason: "Ask"}` is the question a
    /// following `USER_RESPONSE` entry answers. Both turn kinds are then
    /// merged into one journal-order sequence by the seq of whichever
    /// entry completed the turn (a tool's `TOOL_RESULT`/`TOOL_DENIED`, or
    /// the response's own `USER_RESPONSE`), since that's the only ordering
    /// that's meaningful when the two kinds interleave across a task with
    /// more than one question in it.
    pub(crate) async fn build_conversation(&self, task_id: TaskId) -> Result<Vec<Message>> {
        let mut messages = self.system_and_task_messages(task_id).await?;

        let entries = self.tasks.journal_since(task_id, JournalSeq::ZERO).await?;
        let mut issued: HashMap<EffectId, (String, serde_json::Value)> = HashMap::new();
        let mut pending_question: Option<String> = None;
        let mut turns: Vec<(JournalSeq, Vec<Message>)> = Vec::new();

        for entry in &entries {
            match &entry.kind {
                JournalEntryKind::EffectIssued {
                    effect_id,
                    effect_kind,
                    payload,
                    ..
                } if effect_kind == kinds::TOOL => {
                    let tool = payload
                        .get("tool")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    issued.insert(
                        *effect_id,
                        (tool, payload.get("input").cloned().unwrap_or_default()),
                    );
                }
                JournalEntryKind::EffectCompleted {
                    effect_id,
                    outcome_kind,
                    payload,
                    ..
                } if outcome_kind == kinds::TOOL_RESULT => {
                    if let Some((tool, input)) = issued.get(effect_id) {
                        let rendered = payload
                            .get("rendered")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        turns.push((
                            entry.seq,
                            vec![
                                Message::assistant(format!("Calling `{tool}` with {input}")),
                                Message::tool_result(effect_id.to_string(), rendered),
                            ],
                        ));
                    }
                }
                JournalEntryKind::EffectCompleted {
                    effect_id,
                    outcome_kind,
                    payload,
                    ..
                } if outcome_kind == kinds::TOOL_DENIED => {
                    if let Some((tool, input)) = issued.get(effect_id) {
                        let reason = payload
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        turns.push((
                            entry.seq,
                            vec![
                                Message::assistant(format!("Calling `{tool}` with {input}")),
                                Message::tool_result(
                                    effect_id.to_string(),
                                    format!("Denied: {reason}"),
                                ),
                            ],
                        ));
                    }
                }
                JournalEntryKind::EffectCompleted {
                    outcome_kind,
                    payload,
                    ..
                } if outcome_kind == kinds::MODEL_COMPLETION => {
                    if payload.get("finish_reason").and_then(|v| v.as_str()) == Some("Ask") {
                        pending_question = payload
                            .get("text")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                    }
                }
                JournalEntryKind::EffectCompleted {
                    outcome_kind,
                    payload,
                    ..
                } if outcome_kind == kinds::USER_RESPONSE => {
                    let answer = payload
                        .get("answer")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let mut pair = Vec::new();
                    if let Some(question) = pending_question.take() {
                        pair.push(Message::assistant(question));
                    }
                    pair.push(Message::user(answer));
                    turns.push((entry.seq, pair));
                }
                _ => {}
            }
        }

        turns.sort_by_key(|(seq, _)| *seq);
        for (_, turn_messages) in turns {
            messages.extend(turn_messages);
        }

        Ok(messages)
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

        let role = self.model_role(self.is_role_escalated(task_id));
        let messages = self.build_conversation(task_id).await?;
        let request = GenerateRequest::new(messages)
            .with_tools(self.tool_specs.clone())
            .with_turn_hint(turn_index);
        let routed = self
            .orchestrator
            .generate_action(role, request, cancel.child(), MAX_REFORMAT_RETRIES)
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
                        "role": role.as_str(),
                        "model_id": routed.model_id,
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
        let mut vs = self.take_verify_state(task_id).await?;
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
                // Plan exhausted, everything passed, broad run satisfied —
                // the one Completed path that is an actual *verified*
                // success, so it's the one that earns repository memory
                // (M3): commands observed to work here, and any that took
                // repeated failures to get right, feed the next task's
                // Verifying discovery and repair strategy.
                self.extract_and_write_memory(task_id).await;
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
                        // Parsed failure locations, when a parser matched (G15).
                        "failures": run.failures.iter().map(failure_payload).collect::<Vec<_>>(),
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
        let mut vs = self.take_verify_state(task_id).await?;
        let changed = self.task_changed_files(task_id);

        let failures = vs
            .last_run
            .as_ref()
            .map(|r| r.failures.clone())
            .unwrap_or_default();
        // M2: graph neighbours of each changed file, so a failure whose
        // location is a *caller* of something this task touched — not the
        // touched file itself — still implicates the right suspect. Empty
        // (exactly pre-M2 behaviour: suspects from failure locations ∩ the
        // change ledger alone) when there's no store, no index generation
        // yet, or the graph errors for any reason — this is an
        // enrichment, never a hard dependency.
        let neighbors = self.graph_neighbors_for(&changed).await;
        let diagnosis = diagnose(&failures, &changed, &neighbors);
        let fingerprint = diagnosis.fingerprint();

        // Loop / progress detection bookkeeping, computed *before* the
        // DIAGNOSIS journal entry below (M5, C8) — its payload carries
        // `file_state_hash`/`verification_frontier`/`failure_count`/
        // `files_touched` precisely so `reconstruct_verify_state` can
        // replay this exact `observe_*` call after a crash, rather than
        // losing the loop detector's history on every cross-process resume.
        for f in changed.iter().cloned() {
            vs.files_touched.insert(f);
        }
        let file_state = self.changed_files_state_hash(&changed);

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
                        "file_state_hash": file_state,
                        "verification_frontier": vs.executed,
                        "failure_count": failures.len(),
                        "files_touched": vs.files_touched
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>(),
                    }),
                },
            )
            .await?;

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

        let mut vs = self.take_verify_state(task_id).await?;
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
        // `SwitchRole` (M1) escalates a repair turn to `PrimaryCoder` the
        // same way the main loop does — `model_role` degrades to the
        // unconditional `Role::PrimaryCoder` this used to hardcode whenever
        // `FastCoder` isn't bound (every install today; multi-role catalog
        // selection is COMPLETION-PLAN.md M6), so a single-model setup sees
        // no behaviour change.
        let role = self.model_role(vs.repair_role_primary);

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

        let mut messages = self.build_conversation(task_id).await?;
        messages.push(Message::user(format!(
            "A verification check just failed. Make the minimal edit that fixes it, \
             then finish.\n\n{digest}"
        )));
        let request = GenerateRequest::new(messages)
            .with_tools(self.tool_specs.clone())
            .with_turn_hint(turn_index);
        let routed = self
            .orchestrator
            .generate_action(role, request, cancel.child(), MAX_REFORMAT_RETRIES)
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
                        "role": role.as_str(),
                        "model_id": routed.model_id,
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
                let attempt = RepairAttempt {
                    attempt: 0,
                    diagnosis_fingerprint: fingerprint,
                    edit_summary: "model asked a question".into(),
                    outcome: RepairOutcome::NoChange,
                };
                self.journal_repair_attempt(task_id, step_id, &attempt)
                    .await?;
                vs.repair.record(attempt);
                self.put_verify_state(task_id, vs);
                self.tasks
                    .transition(task_id, AgentState::WaitingForUser)
                    .await?;
                return Ok(Flow::Return);
            }
        };

        let attempt = RepairAttempt {
            attempt: 0,
            diagnosis_fingerprint: fingerprint,
            edit_summary,
            outcome,
        };
        self.journal_repair_attempt(task_id, step_id, &attempt)
            .await?;
        vs.repair.record(attempt);
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
                    payload: serde_json::json!({
                        "tool": tool,
                        "input": input,
                        // Stable id pairing this start with its completion (G14).
                        "tool_invocation_id": effect_id.to_string(),
                    }),
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
                let duration_ms = record
                    .end_time
                    .as_millis()
                    .saturating_sub(record.start_time.as_millis());
                self.tasks
                    .append_journal(
                        task_id,
                        JournalEntryKind::EffectCompleted {
                            effect_id,
                            step_id,
                            outcome_kind: kinds::TOOL_RESULT.into(),
                            payload: serde_json::json!({
                                "success": success,
                                // Matches `tool_started`'s `tool_invocation_id` (G14).
                                "tool_invocation_id": effect_id.to_string(),
                                "tool_record_id": record.id.to_string(),
                                // Structured completion fields alongside `rendered` (G14).
                                "exit_code": record.exit_status,
                                "stdout": record.stdout,
                                "stderr": record.stderr,
                                "duration_ms": duration_ms,
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
                                // Stable request identity for permission_resolve (G2).
                                "request_id": effect_id.to_string(),
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
        let decision = if approve {
            ApprovalDecision::Once
        } else {
            ApprovalDecision::Deny
        };
        self.resolve_permission_scoped(task_id, None, decision)
            .await
    }

    /// Resolve an outstanding approval, optionally asserting `request_id`
    /// (the pending call's effect id) so a client cannot resolve a prompt
    /// a newer request has superseded (G2). `decision` is `Once` (approve
    /// this call), `Task` (approve and grant for the rest of the task), or
    /// `Deny`.
    pub async fn resolve_permission_scoped(
        &self,
        task_id: TaskId,
        request_id: Option<String>,
        decision: ApprovalDecision,
    ) -> Result<()> {
        let task = self.tasks.get(task_id).await?;
        if task.state != AgentState::WaitingForPermission {
            return Err(AgentError::NotWaitingForPermission(task_id));
        }
        let pending = self
            .tasks
            .pending_tool_call(task_id)
            .await?
            .ok_or(AgentError::NoPendingToolCall(task_id))?;

        if let Some(want) = &request_id {
            let current = pending.effect_id.to_string();
            if *want != current {
                return Err(AgentError::ApprovalSuperseded {
                    got: want.clone(),
                    current,
                });
            }
        }

        let tool = self
            .tools
            .get_tool(&pending.tool)
            .ok_or_else(|| AgentError::UnknownTool(pending.tool.clone()))?;

        let ctx = self.build_ctx(task_id, pending.step_id, CancellationToken::new());
        let effect_id = pending.effect_id;

        if decision == ApprovalDecision::Deny {
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
        let scope = match decision {
            ApprovalDecision::Task => GrantScope::Task(task_id),
            _ => GrantScope::OneShot,
        };
        let auth = self.permissions.approve(request, scope, None);
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

    /// Answer a task parked in `WAITING_FOR_USER` (M4) — the other half of
    /// `ActionRequest::Ask`, which could get a task *into* that state but
    /// had no way back out short of cancelling it. Journals the answer
    /// (`USER_RESPONSE`, replayed by `build_conversation` as the user turn
    /// following the question — `Trust::Instruction`, since this is the
    /// user speaking, the same authority level a repo-owned instruction
    /// file carries) and transitions back to `Implementing` so the next
    /// model call sees both the question and the answer. Mirrors
    /// `resolve_permission_scoped`'s shape: one resolution step, not a
    /// loop — the caller (`valyria_app::Runtime::respond_to_user`) spawns
    /// a fresh driver run afterward if that leaves the task live.
    pub async fn respond_to_user(&self, task_id: TaskId, answer: String) -> Result<()> {
        let task = self.tasks.get(task_id).await?;
        if task.state != AgentState::WaitingForUser {
            return Err(AgentError::NotWaitingForUser(task_id));
        }
        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectCompleted {
                    effect_id: EffectId::new(),
                    step_id: StepId::new(),
                    outcome_kind: kinds::USER_RESPONSE.into(),
                    payload: serde_json::json!({ "answer": answer }),
                },
            )
            .await?;
        self.tasks
            .transition(task_id, AgentState::Implementing)
            .await?;
        Ok(())
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

    /// M5, C8: the in-memory cache is process-local, so the *first* time a
    /// fresh process touches a task's verify state — whether it's genuinely
    /// new or this is a resume after a crash — this map has nothing for it.
    /// Rather than assume "nothing in the map" means "nothing has happened
    /// yet" (true before M5, false after a crash mid-repair), replay the
    /// task's own journal to rebuild `detector`/`repair` exactly as they
    /// would stand had the process never died. A task with no history at
    /// all replays to the same empty detector/ledger this always produced,
    /// so the change is invisible to every pre-M5 scenario.
    async fn take_verify_state(&self, task_id: TaskId) -> Result<VerifyState> {
        if let Some(vs) = self.verify_states.lock().unwrap().get(&task_id).cloned() {
            return Ok(vs);
        }
        let entries = self.tasks.journal_since(task_id, JournalSeq::ZERO).await?;
        let (detector, repair) = reconstruct_verify_state(&entries, MAX_REPAIR_ATTEMPTS);
        let vs = VerifyState {
            detector,
            repair,
            ..VerifyState::default()
        };
        self.verify_states
            .lock()
            .unwrap()
            .insert(task_id, vs.clone());
        Ok(vs)
    }

    fn put_verify_state(&self, task_id: TaskId, state: VerifyState) {
        self.verify_states.lock().unwrap().insert(task_id, state);
    }

    /// Durably records one [`RepairAttempt`] before it's folded into the
    /// (process-local) ledger, so [`reconstruct_verify_state`] can replay
    /// it — and thus the ledger's attempt budget and escalation history —
    /// after a crash (M5, C8).
    async fn journal_repair_attempt(
        &self,
        task_id: TaskId,
        step_id: StepId,
        attempt: &RepairAttempt,
    ) -> Result<()> {
        self.tasks
            .append_journal(
                task_id,
                JournalEntryKind::EffectCompleted {
                    effect_id: EffectId::new(),
                    step_id,
                    outcome_kind: kinds::REPAIR_ATTEMPT.into(),
                    payload: serde_json::json!({
                        "diagnosis_fingerprint": attempt.diagnosis_fingerprint,
                        "edit_summary": attempt.edit_summary,
                        "outcome": repair_outcome_tag(attempt.outcome),
                    }),
                },
            )
            .await?;
        Ok(())
    }

    /// Graph neighbours of `changed`'s files — every file whose graph
    /// edges depend on one of them, within `GraphStore::impact_of`'s
    /// standard depth — as `(changed_file, neighbor)` pairs, the shape
    /// `valyria_verify::diagnose` matches a failure location against
    /// (M2). Empty (not an error) when this driver has no `store`, no
    /// index generation has been published yet, or the graph query fails
    /// for any reason: this only ever enriches suspects beyond "failure
    /// locations ∩ the change ledger", never gates diagnosis on the graph
    /// existing.
    async fn graph_neighbors_for(&self, changed: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
        let Some(store) = self.store.clone() else {
            return Vec::new();
        };
        graph_neighbors(&store, changed).await
    }

    /// This task's verification runs, turned into repository memory (M3):
    /// each run becomes one `Observation` (kind from its `Tier`, success
    /// from its outcome), and `valyria_memory::extract`'s existing
    /// heuristics do the rest — a command seen to pass is worth
    /// remembering as "this works here"; one that took several failures
    /// to get right becomes a pitfall. Best-effort: no `store`/`memory`
    /// wired, no runs recorded, or a write error all just mean nothing is
    /// extracted this time — never a reason to fail an otherwise-completed
    /// task.
    async fn extract_and_write_memory(&self, task_id: TaskId) {
        let Some(memory) = self.memory.clone() else {
            return;
        };
        let runs = match self.verification_log.list_for_task(task_id).await {
            Ok(runs) => runs,
            Err(e) => {
                tracing::warn!(error = %e, "could not list verification runs for memory extraction");
                return;
            }
        };
        if runs.is_empty() {
            return;
        }

        // A command can appear multiple times across the task (a targeted
        // run, then the mandatory full run); fold to its last-seen outcome
        // and total failure count, matching what a human would actually
        // conclude about it.
        use std::collections::BTreeMap;
        let mut folded: BTreeMap<String, valyria_memory::Observation> = BTreeMap::new();
        for run in &runs {
            // `tier` is `Tier`'s Debug format (`evidence.rs`:
            // `run.tier.map(|t| format!("{t:?}"))`) — PascalCase variant
            // names, not a serde rename.
            let kind = match run.tier.as_deref() {
                Some("Syntax") => valyria_memory::ObservationKind::BuildRun,
                Some("TargetedTest") | Some("RelatedTests") | Some("Full") => {
                    valyria_memory::ObservationKind::TestRun
                }
                Some("Style") => valyria_memory::ObservationKind::LintRun,
                _ => valyria_memory::ObservationKind::CommandRun,
            };
            let entry = folded
                .entry(run.command_display.clone())
                .or_insert_with(|| {
                    valyria_memory::Observation::new(kind, run.command_display.clone(), true)
                        .with_provenance(task_id.to_string())
                });
            entry.success = run.passed();
            if !run.passed() {
                entry.failures += 1;
            }
        }

        let now_ms = self.clock.now().as_millis() as i64;
        let observations: Vec<valyria_memory::Observation> = folded.into_values().collect();
        let entries = valyria_memory::extract(&observations, now_ms);
        if entries.is_empty() {
            return;
        }
        if let Err(e) = memory.write_all(entries).await {
            tracing::warn!(error = %e, "failed to write extracted repository memory");
        }
    }

    /// [`Self::journal_context_retrieved`]'s sibling for
    /// [`system_and_task_messages`](Self::system_and_task_messages)'s
    /// `ContextEngine`/`EngineInput` pipeline (M2), which assembles an
    /// [`AssembledPrompt`] (a [`valyria_context::ContextSnapshot`] of
    /// [`valyria_context::assemble::AssembledItem`]s) rather than the
    /// Discovery step's [`AssembledContext`] of `ContextItem`s — distinct
    /// types from two still-separate pipelines, so this can't share the
    /// other method's body, but produces the identical `context_retrieved`
    /// journal/event shape. Per-item token counts are estimated
    /// (`rendered.chars().count() / 4`, the common rule-of-thumb ratio) —
    /// `AssembledItem` doesn't carry a real count the way `ContextItem`
    /// does, and this is a diagnostic field only, not budget enforcement
    /// (`assembled.total_tokens`, used for `budget_used` below, *is* the
    /// pipeline's real, budget-accurate figure).
    async fn journal_prompt_context_retrieved(
        &self,
        task_id: TaskId,
        assembled: &AssembledPrompt,
        budget_total: usize,
    ) -> Result<()> {
        let items: Vec<serde_json::Value> = assembled
            .snapshot
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
                    "tokens": item.rendered.chars().count() / 4,
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

    /// Whether a `RepairDecision::SwitchRole` has escalated this task away
    /// from `FastCoder` for the remainder of the run — a cheap peek at
    /// `repair_role_primary` (bumped in `step_diagnosing`'s `SwitchRole`
    /// arm) without the clone-out/clone-back `take_verify_state`/
    /// `put_verify_state` pair every other reader of `VerifyState` uses,
    /// since every non-repair caller (the main Implementing loop) only
    /// ever needs this one field and never mutates it. Defaults to
    /// unescalated for a task with no verify state yet — i.e. every task's
    /// first turn.
    pub(crate) fn is_role_escalated(&self, task_id: TaskId) -> bool {
        self.verify_states
            .lock()
            .unwrap()
            .get(&task_id)
            .map(|s| s.repair_role_primary)
            .unwrap_or(false)
    }

    /// Which role the next Reason step should call. `FastCoder` is the
    /// default entry point — both for a task's very first Implementing turn
    /// and every one after, main loop or repair — unless this task has been
    /// escalated to `PrimaryCoder` for the rest of its run (§4.22's
    /// FastCoder→PrimaryCoder escalation, driven by a `SwitchRole` repair
    /// decision), or `FastCoder` simply isn't bound to anything. That second
    /// case is the common one today: `valyria-app`'s real-inference wiring
    /// only ever binds `PrimaryCoder` (multi-role catalog selection is
    /// COMPLETION-PLAN.md M6), so this degrades to the unconditional
    /// `Role::PrimaryCoder` every call site used before M1 with no observed
    /// behaviour change for a single-model install.
    pub(crate) fn model_role(&self, escalated: bool) -> Role {
        if escalated || !self.orchestrator.is_bound(Role::FastCoder) {
            Role::PrimaryCoder
        } else {
            Role::FastCoder
        }
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
            store: self.store.clone(),
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

/// [`AgentDriver::graph_neighbors_for`]'s actual work, as a free function
/// so it's testable without a full `AgentDriver` (M2): every file whose
/// graph edges depend on one of `changed`'s files, within
/// `GraphStore::impact_of`'s standard depth, as `(changed_file, neighbor)`
/// pairs — the shape `valyria_verify::diagnose` matches a failure location
/// against. Empty, never an error, when there's no index generation yet
/// or a query fails — an enrichment, never a hard dependency.
async fn graph_neighbors(
    store: &Arc<valyria_store::Store>,
    changed: &[PathBuf],
) -> Vec<(PathBuf, PathBuf)> {
    let index = valyria_index::IndexStore::new(store.clone());
    let Ok(Some(info)) = index.current().await else {
        return Vec::new();
    };
    let graph = valyria_graph::GraphStore::new(store.clone());

    const IMPACT_DEPTH: usize = 2;
    let mut pairs = Vec::new();
    for path in changed {
        let Some(path_str) = path.to_str() else {
            continue;
        };
        if let Ok(impact) = graph
            .impact_of(info.generation, path_str, IMPACT_DEPTH)
            .await
        {
            for affected in impact.affected_files {
                pairs.push((path.clone(), PathBuf::from(affected)));
            }
        }
    }
    pairs
}

/// Wire shape for one parsed verification failure (§19, §35, G15).
fn failure_payload(f: &valyria_verify::Failure) -> serde_json::Value {
    let loc = |l: &valyria_verify::Location| serde_json::json!({ "path": l.file.display().to_string(), "line": l.line });
    let mut locations = Vec::new();
    if let Some(primary) = &f.primary_location {
        locations.push(loc(primary));
    }
    locations.extend(f.secondary_locations.iter().map(loc));
    serde_json::json!({
        "kind": serde_json::to_value(f.kind).unwrap_or(serde_json::json!("unknown")),
        "message": f.message,
        "failing_test": f.failing_test,
        "location": locations,
    })
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

/// M5, C8: rebuilds a [`LoopDetector`] and [`RepairLedger`] by replaying
/// one task's journal from the start, in order — the same technique
/// `plan_exec::plan_rejection_count` already uses for the plan-repair
/// budget. Every mutation the live driver ever makes to these two structs
/// is driven by data this function can also see in the journal (the
/// `DIAGNOSIS` entry carries the loop detector's inputs; `REPAIR_ATTEMPT`
/// and the `decision` field of `REPAIR_DECISION` carry the ledger's), so a
/// task with no gaps in its journal reconstructs to bit-for-bit the same
/// state a process that never crashed would have — a crash simply cannot
/// be distinguished from a resume by anything downstream of this.
fn reconstruct_verify_state(
    entries: &[valyria_task::JournalEntry],
    max_repair_attempts: u32,
) -> (LoopDetector, RepairLedger) {
    let mut detector = LoopDetector::default();
    let mut repair = RepairLedger::new(max_repair_attempts);
    let mut files_touched: BTreeSet<PathBuf> = BTreeSet::new();

    for entry in entries {
        let JournalEntryKind::EffectCompleted {
            outcome_kind,
            payload,
            ..
        } = &entry.kind
        else {
            continue;
        };
        match outcome_kind.as_str() {
            kinds::VERIFY_RESULT => {
                if payload.get("passed").and_then(|v| v.as_bool()) == Some(true) {
                    detector.observe_failure(None);
                }
            }
            kinds::DIAGNOSIS => {
                let fingerprint = payload
                    .get("fingerprint")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let file_state_hash: ContentHash = payload
                    .get("file_state_hash")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_else(|| ContentHash::of_bytes(b""));
                let verification_frontier = payload
                    .get("verification_frontier")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let failure_count = payload
                    .get("failure_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                if let Some(paths) = payload.get("files_touched").and_then(|v| v.as_array()) {
                    for p in paths {
                        if let Some(s) = p.as_str() {
                            files_touched.insert(PathBuf::from(s));
                        }
                    }
                }

                let step_sig = StepSignature::default()
                    .with_error(fingerprint)
                    .with_file_state(file_state_hash);
                let _finding = detector
                    .observe_step(step_sig)
                    .or_else(|| detector.observe_failure(Some(fingerprint)))
                    .or_else(|| {
                        detector.observe_progress(ProgressMetric {
                            verification_frontier,
                            failure_count,
                            files_touched: files_touched.clone(),
                        })
                    });
            }
            kinds::REPAIR_ATTEMPT => {
                let diagnosis_fingerprint = payload
                    .get("diagnosis_fingerprint")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let edit_summary = payload
                    .get("edit_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let outcome = match payload.get("outcome").and_then(|v| v.as_str()) {
                    Some("fixed") => RepairOutcome::Fixed,
                    Some("improved") => RepairOutcome::Improved,
                    Some("regressed") => RepairOutcome::Regressed,
                    _ => RepairOutcome::NoChange,
                };
                repair.record(RepairAttempt {
                    attempt: 0,
                    diagnosis_fingerprint,
                    edit_summary,
                    outcome,
                });
            }
            kinds::REPAIR_DECISION => {
                let decision = payload.get("decision").and_then(|v| v.as_str());
                match decision {
                    Some("escalate_strategy") => repair.mark_escalated(),
                    Some("switch_role") => repair.mark_switched_role(),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    (detector, repair)
}

fn repair_outcome_tag(outcome: RepairOutcome) -> &'static str {
    match outcome {
        RepairOutcome::Fixed => "fixed",
        RepairOutcome::Improved => "improved",
        RepairOutcome::NoChange => "no_change",
        RepairOutcome::Regressed => "regressed",
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

#[cfg(test)]
mod diagnostics_tests {
    use super::*;
    use valyria_verify::{Failure, FailureKind, Location};

    #[test]
    fn failure_payload_carries_parsed_locations() {
        let mut f = Failure::new(FailureKind::TestFailure, "assertion failed: 1 == 2");
        f.primary_location = Some(Location::at("src/math.rs", 42, 5));
        f.secondary_locations = vec![Location::new("tests/math_test.rs")];
        f.failing_test = Some("math::adds".into());

        let v = failure_payload(&f);
        assert_eq!(v["kind"], "test_failure");
        assert_eq!(v["failing_test"], "math::adds");
        let locs = v["location"].as_array().unwrap();
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[0]["path"], "src/math.rs");
        assert_eq!(locs[0]["line"], 42);
        assert_eq!(locs[1]["path"], "tests/math_test.rs");
        assert!(locs[1]["line"].is_null());
    }

    #[test]
    fn failure_payload_with_no_location_is_still_well_formed() {
        let f = Failure::new(FailureKind::Timeout, "exceeded 60s");
        let v = failure_payload(&f);
        assert_eq!(v["kind"], "timeout");
        assert_eq!(v["location"].as_array().unwrap().len(), 0);
        assert!(v["failing_test"].is_null());
    }
}

#[cfg(test)]
mod reconstruct_verify_state_tests {
    use super::*;
    use valyria_task::JournalEntry;
    use valyria_types::Timestamp;

    fn entry(kind: JournalEntryKind) -> JournalEntry {
        JournalEntry {
            seq: JournalSeq::ZERO,
            task_id: TaskId::new(),
            kind,
            created_at: Timestamp::from_millis(0),
        }
    }

    fn verify_result(passed: bool) -> JournalEntry {
        entry(JournalEntryKind::EffectCompleted {
            effect_id: EffectId::new(),
            step_id: StepId::new(),
            outcome_kind: kinds::VERIFY_RESULT.into(),
            payload: serde_json::json!({"passed": passed}),
        })
    }

    /// `frontier` also seeds the file-state hash — a real repair cycle
    /// edits the file between diagnoses, so the same failure fingerprint
    /// recurring across cycles never carries an *identical* file state too
    /// (that combination is what `ExactRepeat` — "the literal same step
    /// again" — detects, a distinct class from `RepeatedFailure` — "the
    /// same failure, but the agent keeps trying different things").
    fn diagnosis(fingerprint: &str, frontier: usize) -> JournalEntry {
        entry(JournalEntryKind::EffectCompleted {
            effect_id: EffectId::new(),
            step_id: StepId::new(),
            outcome_kind: kinds::DIAGNOSIS.into(),
            payload: serde_json::json!({
                "summary": "boom",
                "fingerprint": fingerprint,
                "suspects": [],
                "digest": "",
                "file_state_hash": ContentHash::of_bytes(format!("{fingerprint}#{frontier}").as_bytes()),
                "verification_frontier": frontier,
                "failure_count": 1,
                "files_touched": ["src/lib.rs"],
            }),
        })
    }

    fn repair_attempt(fingerprint: &str, outcome: &str) -> JournalEntry {
        entry(JournalEntryKind::EffectCompleted {
            effect_id: EffectId::new(),
            step_id: StepId::new(),
            outcome_kind: kinds::REPAIR_ATTEMPT.into(),
            payload: serde_json::json!({
                "diagnosis_fingerprint": fingerprint,
                "edit_summary": "an edit",
                "outcome": outcome,
            }),
        })
    }

    fn repair_decision(decision: &str) -> JournalEntry {
        entry(JournalEntryKind::EffectCompleted {
            effect_id: EffectId::new(),
            step_id: StepId::new(),
            outcome_kind: kinds::REPAIR_DECISION.into(),
            payload: serde_json::json!({"decision": decision}),
        })
    }

    #[test]
    fn an_empty_journal_reconstructs_to_the_same_empty_state_a_fresh_task_starts_with() {
        let (detector, repair) = reconstruct_verify_state(&[], MAX_REPAIR_ATTEMPTS);
        assert_eq!(detector.step_count(), 0);
        assert_eq!(repair.count(), 0);
        assert_eq!(repair.decide("fp", None), RepairDecision::Continue);
    }

    /// The core C8 guarantee: a repeated-failure loop that would have
    /// tripped `RepeatedFailure` had the process never restarted still
    /// trips it after reconstruction — the third occurrence of the same
    /// fingerprint counts the two that happened "before the crash" too.
    #[test]
    fn repeated_failure_history_survives_reconstruction() {
        let entries = vec![
            diagnosis("same-fingerprint", 0),
            diagnosis("same-fingerprint", 1),
        ];
        let (mut detector, _) = reconstruct_verify_state(&entries, MAX_REPAIR_ATTEMPTS);

        // A third occurrence of the identical fingerprint (different file
        // state, as a real edit-between-cycles would produce), observed as
        // the live driver would on the next cycle, must trip
        // RepeatedFailure — proving the first two are genuinely counted,
        // not discarded.
        let step_sig = StepSignature::default()
            .with_error("same-fingerprint")
            .with_file_state(ContentHash::of_bytes(b"same-fingerprint#2"));
        let finding = detector
            .observe_step(step_sig)
            .or_else(|| detector.observe_failure(Some("same-fingerprint")));
        assert!(
            matches!(finding, Some(LoopFinding::RepeatedFailure { count: 3, .. })),
            "{finding:?}"
        );
    }

    /// A passing VERIFY_RESULT between two failures must clear the streak
    /// on reconstruction exactly as it does live (`observe_failure(None)`
    /// on a pass) — otherwise a resumed task would spuriously trip
    /// RepeatedFailure on failures that aren't actually consecutive.
    #[test]
    fn a_pass_in_the_journal_clears_the_failure_streak_on_reconstruction() {
        let entries = vec![
            diagnosis("fp", 0),
            diagnosis("fp", 1),
            verify_result(true), // clears the streak
        ];
        let (mut detector, _) = reconstruct_verify_state(&entries, MAX_REPAIR_ATTEMPTS);
        let step_sig = StepSignature::default()
            .with_error("fp")
            .with_file_state(ContentHash::of_bytes(b"fp#2"));
        let finding = detector
            .observe_step(step_sig)
            .or_else(|| detector.observe_failure(Some("fp")));
        assert_eq!(finding, None, "the pass should have reset the streak");
    }

    /// The repair budget itself must not reset on a crash — replaying the
    /// same number of REPAIR_ATTEMPT entries the live ledger would have
    /// recorded must exhaust the same budget, or a resumed task could
    /// retry forever across repeated restarts (exactly the bug C8 exists
    /// to close).
    #[test]
    fn repair_attempt_budget_survives_reconstruction() {
        let entries = vec![
            repair_attempt("fp", "improved"),
            repair_attempt("fp", "no_change"),
        ];
        let (_, repair) = reconstruct_verify_state(&entries, 2);
        assert_eq!(repair.count(), 2);
        assert!(matches!(
            repair.decide("fp", None),
            RepairDecision::GiveUp { .. }
        ));
    }

    /// `mark_escalated`/`mark_switched_role` are replayed from the
    /// `REPAIR_DECISION` entries' own `decision` field — reconstruction
    /// must land on `SwitchRole` next, not re-offer `EscalateStrategy`,
    /// for a task that had already escalated before the crash.
    #[test]
    fn escalation_state_survives_reconstruction() {
        let entries = vec![
            repair_attempt("fp", "no_change"),
            repair_attempt("fp", "no_change"),
            repair_decision("escalate_strategy"),
        ];
        let (_, repair) = reconstruct_verify_state(&entries, 9);
        assert_eq!(repair.decide("fp", None), RepairDecision::SwitchRole);
    }

    #[test]
    fn files_touched_accumulates_across_diagnosis_entries_in_order() {
        let entries = vec![diagnosis("fp1", 0), diagnosis("fp2", 1)];
        // Two distinct fingerprints and frontiers: the progress metric only
        // fires once the earlier detectors return None on both cycles, but
        // reconstruction must not panic or lose track of accumulation
        // regardless — this is primarily a no-panic / determinism check.
        let (detector, _) = reconstruct_verify_state(&entries, MAX_REPAIR_ATTEMPTS);
        assert_eq!(detector.step_count(), 2);
    }
}

#[cfg(test)]
mod graph_neighbors_tests {
    use super::*;
    use std::path::Path;
    use valyria_embed::{EmbedPipeline, EmbedStore, HashingEmbedder};
    use valyria_graph::GraphStore as TestGraphStore;
    use valyria_index::{IndexPipeline, IndexStore};
    use valyria_lang::LanguageRegistry;

    fn migrations() -> Vec<valyria_store::Migration> {
        let mut m: Vec<valyria_store::Migration> = valyria_index::MIGRATIONS.to_vec();
        m.extend(valyria_graph::MIGRATIONS.iter().copied());
        m.extend(valyria_embed::MIGRATIONS.iter().copied());
        m
    }

    /// M2: a caller depending on a changed callee shows up as a graph
    /// neighbour of the changed file — the property `diagnose`'s
    /// `GraphNeighbor` suspect reason relies on to flag "you broke this
    /// caller by changing what it depends on", not just "this file itself
    /// changed". Direct unit test of the free function (no full
    /// `AgentDriver`/model/task machinery needed — see the function's own
    /// doc comment for why it's split out).
    #[tokio::test]
    async fn a_caller_is_a_graph_neighbour_of_a_changed_callee() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write(
            "src/callee.rs",
            "//! The changed file.\n\
             pub fn compute_total(items: &[f64]) -> f64 {\n\
             \x20   items.iter().sum()\n\
             }\n",
        );
        ws.write(
            "src/caller.rs",
            "//! Depends on callee — where the failure actually shows up.\n\
             use crate::callee::compute_total;\n\
             \n\
             pub fn checkout(items: &[f64]) -> f64 {\n\
             \x20   compute_total(items)\n\
             }\n",
        );

        let store = Arc::new(valyria_store::Store::open_in_memory(&migrations()).unwrap());
        let index = IndexStore::new(store.clone());
        let graph = TestGraphStore::new(store.clone());
        let embed = EmbedStore::new(store.clone());

        let pipeline = IndexPipeline::new(
            ws.path().to_path_buf(),
            LanguageRegistry::with_builtin_languages().unwrap(),
            index.clone(),
        );
        let delta = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();
        graph.build_for(&index, delta.generation).await.unwrap();
        EmbedPipeline::new(
            ws.path().to_path_buf(),
            LanguageRegistry::with_builtin_languages().unwrap(),
            Arc::new(HashingEmbedder::default()),
            embed,
        )
        .bootstrap(&index, delta.generation)
        .await
        .unwrap();

        let changed = vec![PathBuf::from("src/callee.rs")];
        let pairs = graph_neighbors(&store, &changed).await;

        assert!(
            pairs.iter().any(
                |(changed_file, neighbor)| changed_file == Path::new("src/callee.rs")
                    && neighbor == Path::new("src/caller.rs")
            ),
            "expected (callee.rs, caller.rs) among the graph-neighbour pairs, got {pairs:?}"
        );
    }

    /// No store — the honest, error-free empty result the live driver's
    /// `graph_neighbors_for` falls back to.
    #[tokio::test]
    async fn no_generation_yet_is_an_empty_result_not_an_error() {
        let store = Arc::new(valyria_store::Store::open_in_memory(&migrations()).unwrap());
        let changed = vec![PathBuf::from("src/anything.rs")];
        let pairs = graph_neighbors(&store, &changed).await;
        assert!(pairs.is_empty());
    }
}
