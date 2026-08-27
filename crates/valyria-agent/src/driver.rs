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
//! effect. Planning is a formality this driver passes straight through
//! (no `valyria-plan` yet — Phase 8) since the state graph still requires
//! transiting `Planning` on the way to `Implementing`.

use std::path::PathBuf;
use std::sync::Arc;

use valyria_context::{ContextAssembler, ContextQuery};
use valyria_ledger::Ledger;
use valyria_model::{GenerateRequest, Message};
use valyria_orchestrator::{Orchestrator, Role};
use valyria_permissions::{GrantScope, PermissionEngine};
use valyria_sandbox::{ProcessLauncher, SandboxProfile};
use valyria_task::{kinds, ControlSignal, JournalEntryKind, TaskManager};
use valyria_tools::{InvocationResult, ToolCtx, ToolOutcome, ToolRuntime};
use valyria_types::{AgentState, EffectId, StepId, TaskId};
use valyria_util::{CancellationToken, Clock};
use valyria_vfs::{HashCache, WorkspaceRoot};

use crate::action::ActionRequest;
use crate::error::{AgentError, Result};

/// Token budget for the Phase 3 explicit-file context stage. Arbitrary but
/// generous — the real, configurable budget model is Phase 6's job.
const DEFAULT_CONTEXT_BUDGET_TOKENS: usize = 50_000;

pub struct AgentDriver {
    tasks: Arc<TaskManager>,
    tools: Arc<ToolRuntime>,
    orchestrator: Arc<Orchestrator>,
    context: Arc<ContextAssembler>,
    ledger: Arc<Ledger>,
    permissions: Arc<PermissionEngine>,
    workspace_root: WorkspaceRoot,
    hash_cache: Arc<HashCache>,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
    launcher: Arc<dyn ProcessLauncher>,
    sandbox_profile: SandboxProfile,
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
            workspace_root,
            hash_cache,
            clock,
            launcher,
            sandbox_profile,
        }
    }

    /// Runs `task_id` until it reaches a terminal state, is paused, or asks
    /// to wait on the user/a permission decision. Pause/cancel are checked
    /// only between steps (never mid-effect) so a pause always lands
    /// cleanly on a step boundary.
    ///
    /// Two independent signal paths, both checked every iteration:
    /// `cancel` is an in-process `CancellationToken` (e.g. a future
    /// same-process Ctrl+C handler in `valyria-cli`) that also propagates
    /// into any in-flight tool call via `cancel.child()`, so it can stop
    /// work that's already running; `task.pending_signal` is the durable,
    /// row-stored signal `TaskManager::request_pause`/`request_cancel`
    /// write, which is what lets a *separate* `valyria task pause <id>`
    /// process reach a task being driven by a different `valyria run`
    /// process (Phase 3 has no daemon yet to share an in-memory handle
    /// across processes) — at the cost of only being noticed between
    /// steps, not mid-effect.
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
                    // No explicit files configured in the default demo —
                    // this is a real call through the pipeline that simply
                    // has nothing to fetch. Real retrieval is Phase 6.
                    self.context
                        .assemble(&ctx, ContextQuery::new(DEFAULT_CONTEXT_BUDGET_TOKENS))
                        .await?;
                    self.tasks.transition(task_id, AgentState::Planning).await?;
                }
                AgentState::Planning => {
                    // "Plans nothing" (docs/PLAN.md Phase 3 exit criterion):
                    // no valyria-plan yet (Phase 8). The state graph still
                    // requires transiting Planning on the way to
                    // Implementing, so this is a one-step formality.
                    self.tasks
                        .transition(task_id, AgentState::Implementing)
                        .await?;
                }
                AgentState::Implementing => {
                    self.step_implementing(task_id, &cancel).await?;
                }
                AgentState::Verifying => {
                    // Real verification strategy (§27-28) is Phase 7; the
                    // walking skeleton treats "the model said finish" as
                    // sufficient to complete.
                    self.tasks
                        .transition(task_id, AgentState::Completed)
                        .await?;
                    return Ok(());
                }
                AgentState::WaitingForPermission | AgentState::WaitingForUser => {
                    // Resumed externally: `resolve_permission` for the
                    // former, a future `WaitingForUser` answer path for the
                    // latter (not exercised by Phase 3's default scenario).
                    return Ok(());
                }
                AgentState::Completed | AgentState::Failed | AgentState::Cancelled => {
                    return Ok(());
                }
                AgentState::Diagnosing | AgentState::Repairing | AgentState::Paused => {
                    // Not reachable by this driver: no repair loop yet
                    // (Phase 7), and a task resumed out of `Paused` lands on
                    // whatever state it was paused from, never stays here.
                    return Ok(());
                }
            }
        }
    }

    async fn step_implementing(&self, task_id: TaskId, cancel: &CancellationToken) -> Result<()> {
        // D1: re-issue any effect that was issued but never completed.
        // Checked first, every time this state is entered, so a crash
        // strictly between `EffectIssued` and any `EffectCompleted` for a
        // tool call is healed automatically before anything new happens.
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

        // Phase 3's fake model is a pure function of `turn_hint`, not
        // conversation history, so a single message carrying the task
        // objective is enough to drive it; reconstructing the full
        // transcript from the journal for a real model's benefit is
        // deferred to whichever phase adds a real adapter (Phase 9).
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

    /// Authorize -> Execute -> Observe for one tool call: journals
    /// `EffectIssued` before anything runs, then exactly one
    /// `EffectCompleted` reflecting whatever `ToolRuntime::invoke` returned.
    /// Shared by the normal `Implementing` path and the interrupted-call
    /// redo path — both need identical journaling/permission/ledger
    /// behavior, just with a different origin for `tool`/`input`.
    async fn issue_and_execute_tool_call(
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
                // Stays in Implementing either way (Phase 3 has no repair
                // loop): the next `Reason` step decides what happens next,
                // including reacting to a failed tool outcome via its own
                // next scripted/real turn.
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

    /// Records this path's pre-touch baseline (first-touch-wins) if the
    /// tool input names one, so `Ledger::classify` can later tell
    /// pre-existing content apart from a concurrent user modification
    /// (§25). Safe to call unconditionally — `record_baseline` is a no-op
    /// on every call after the first for a given path.
    fn record_baseline_if_path(&self, ctx: &ToolCtx, input: &serde_json::Value) {
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

    /// Resumes a task sitting in `WAITING_FOR_PERMISSION`: re-derives the
    /// `PermissionRequest` from the tool's own (pure, side-effect-free)
    /// `preflight` rather than trusting anything cached, mints an
    /// `Authorization` if approved, and journals the resolution as a fresh
    /// `EffectCompleted` that supersedes the earlier `permission_ask` one.
    /// Does not itself continue the task loop — the caller re-invokes
    /// `run` afterward (`WaitingForPermission -> Implementing` is then a
    /// legal, ordinary next step).
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
        // Reuse the *original* EffectIssued's id: this EffectCompleted must
        // supersede the earlier `permission_ask` completion for the same
        // effect, not create an unrelated, permanently-dangling one.
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
                // A successful/denied resolution lands back in Implementing
                // only on success; `observe_tool_result` already sent a
                // denial to Failed. On success, explicitly continue.
                if self.tasks.get(task_id).await?.state == AgentState::WaitingForPermission {
                    self.tasks
                        .transition(task_id, AgentState::Implementing)
                        .await?;
                }
                Ok(())
            }
            InvocationResult::AskRequired { .. } => {
                unreachable!("invoke_with_authorization executes directly and never re-asks")
            }
            InvocationResult::UnknownTool(name) => Err(AgentError::UnknownTool(name.clone())),
        }
    }

    fn build_ctx(&self, task_id: TaskId, step_id: StepId, cancel: CancellationToken) -> ToolCtx {
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
