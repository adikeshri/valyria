//! `inspect_environment` (§17, feeds §46 `doctor`): hardware and workspace
//! facts, read-only.

use async_trait::async_trait;
use serde_json::Value;
use valyria_permissions::{ActionKind, Authorization, PermissionRequest, RiskLevel};
use valyria_types::PermissionCategory;

use crate::canonical::canonical_input_hash;
use crate::ctx::ToolCtx;
use crate::descriptor::{SideEffect, ToolDescriptor};
use crate::error::Result;
use crate::outcome::ToolOutcome;
use crate::tool_trait::Tool;

use super::helpers::{object_schema, verify_authorization};

pub struct InspectEnvironmentTool {
    descriptor: ToolDescriptor,
}

impl Default for InspectEnvironmentTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "inspect_environment",
                description: "Report hardware capabilities and workspace location.",
                input_schema: object_schema(serde_json::json!({}), &[]),
                side_effect: SideEffect::ReadOnly,
            },
        }
    }
}

#[async_trait]
impl Tool for InspectEnvironmentTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        Ok(PermissionRequest {
            task_id: ctx.task_id,
            step_id: ctx.step_id,
            tool: "inspect_environment",
            category: PermissionCategory::Filesystem,
            action: ActionKind::Read,
            risk: RiskLevel::Safe,
            input_hash: canonical_input_hash(input),
            target: "local environment".into(),
            in_plan_scope: true,
        })
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) = verify_authorization(
            auth,
            ctx.task_id,
            ctx.step_id,
            "inspect_environment",
            &input,
        ) {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }
        let hw = valyria_hardware::probe();
        let structured = serde_json::json!({
            "os": hw.os,
            "arch": hw.arch,
            "cpu_logical_cores": hw.cpu.logical_cores,
            "ram_total_bytes": hw.ram_total_bytes,
            "ram_available_bytes": hw.ram_available_bytes,
            "unified_memory": hw.unified_memory,
            "workspace_root": ctx.workspace_root.as_path(),
        });
        let rendered = structured.to_string();
        ToolOutcome::success(structured, rendered)
    }
}
