//! The tool runtime: registry + the permission-gated invocation path every
//! tool call goes through. This is the concrete enforcement point for D2 —
//! there is no method here that reaches `Tool::execute` without first
//! getting a `Decision::Allow` (or `Decision::Ask` -> external approval ->
//! [`ToolRuntime::invoke_with_authorization`]) out of the permission
//! engine.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use valyria_permissions::{Authorization, Decision, PermissionEngine, PermissionRequest};
use valyria_types::ToolInvocationId;
use valyria_util::Clock;

use crate::ctx::ToolCtx;
use crate::descriptor::ToolDescriptor;
use crate::invocation::ToolInvocationRecord;
use crate::outcome::ToolOutcome;
use crate::tool_trait::Tool;

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.descriptor().name, tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools
            .values()
            .map(|t| t.descriptor().clone())
            .collect()
    }
}

#[derive(Debug)]
pub enum InvocationResult {
    Executed {
        outcome: ToolOutcome,
        record: ToolInvocationRecord,
    },
    Denied {
        reason: String,
    },
    AskRequired {
        prompt: String,
        request: PermissionRequest,
    },
    UnknownTool(String),
}

pub struct ToolRuntime {
    registry: ToolRegistry,
    engine: Arc<PermissionEngine>,
    clock: Arc<dyn Clock>,
}

impl ToolRuntime {
    pub fn new(
        registry: ToolRegistry,
        engine: Arc<PermissionEngine>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            registry,
            engine,
            clock,
        }
    }

    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.registry.descriptors()
    }

    /// Look up a registered tool by name, e.g. to call `Tool::preflight`
    /// directly and re-derive a `PermissionRequest` — the agent loop's
    /// `WAITING_FOR_PERMISSION` resume path needs this (both the normal
    /// "client resolved the ask" case and crash recovery reconstruct the
    /// request from durable data rather than caching it).
    pub fn get_tool(&self, tool_name: &str) -> Option<Arc<dyn Tool>> {
        self.registry.get(tool_name)
    }

    /// Evaluate permission for `tool_name(input)` and, if allowed, execute
    /// it immediately.
    pub async fn invoke(&self, ctx: &ToolCtx, tool_name: &str, input: Value) -> InvocationResult {
        let Some(tool) = self.registry.get(tool_name) else {
            return InvocationResult::UnknownTool(tool_name.to_string());
        };

        let request = match tool.preflight(ctx, &input) {
            Ok(r) => r,
            Err(e) => {
                return InvocationResult::Denied {
                    reason: e.to_string(),
                }
            }
        };

        match self.engine.evaluate(request.clone()) {
            Decision::Allow(auth) => self.run(ctx, &tool, input, auth).await,
            Decision::Deny { reason } => InvocationResult::Denied { reason },
            Decision::Ask { prompt } => InvocationResult::AskRequired { prompt, request },
        }
    }

    /// Execute `tool_name(input)` with an `Authorization` obtained
    /// out-of-band (the caller already resolved an `Ask` via
    /// `PermissionEngine::approve` and is presenting the result). Used by
    /// the agent loop's `WAITING_FOR_PERMISSION -> Implementing` path.
    pub async fn invoke_with_authorization(
        &self,
        ctx: &ToolCtx,
        tool_name: &str,
        input: Value,
        auth: Authorization,
    ) -> InvocationResult {
        let Some(tool) = self.registry.get(tool_name) else {
            return InvocationResult::UnknownTool(tool_name.to_string());
        };
        self.run(ctx, &tool, input, auth).await
    }

    async fn run(
        &self,
        ctx: &ToolCtx,
        tool: &Arc<dyn Tool>,
        input: Value,
        auth: Authorization,
    ) -> InvocationResult {
        let start_time = self.clock.now();
        let outcome = tool.execute(ctx, &auth, input.clone()).await;
        let end_time = self.clock.now();

        let (success, exit_status, stdout, stderr, error) = match &outcome {
            ToolOutcome::Success { structured, .. } => (
                true,
                structured
                    .get("exit_status")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32),
                structured
                    .get("stdout")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                structured
                    .get("stderr")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                None,
            ),
            ToolOutcome::Failure { message, .. } => {
                (false, None, None, None, Some(message.clone()))
            }
        };

        let record = ToolInvocationRecord {
            id: ToolInvocationId::new(),
            task_id: ctx.task_id,
            step_id: ctx.step_id,
            tool: tool.descriptor().name,
            input,
            authorized: true,
            start_time,
            end_time,
            success,
            exit_status,
            stdout,
            stderr,
            error,
        };

        InvocationResult::Executed { outcome, record }
    }
}
