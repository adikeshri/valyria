//! `search` and `symbol_search` (§17) — registered so the tool registry is
//! complete against the PRD's tool list, but not implemented: both need
//! the repository index (`valyria-index`) and/or search fusion
//! (`valyria-search`), which land in Phase 5.

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

use super::helpers::object_schema;

macro_rules! not_yet_implemented_tool {
    ($ty:ident, $name:literal, $desc:literal) => {
        pub struct $ty {
            descriptor: ToolDescriptor,
        }
        impl Default for $ty {
            fn default() -> Self {
                Self {
                    descriptor: ToolDescriptor {
                        name: $name,
                        description: $desc,
                        input_schema: object_schema(serde_json::json!({"query": {"type": "string"}}), &["query"]),
                        side_effect: SideEffect::ReadOnly,
                    },
                }
            }
        }

        #[async_trait]
        impl Tool for $ty {
            fn descriptor(&self) -> &ToolDescriptor {
                &self.descriptor
            }

            fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
                Ok(PermissionRequest {
                    task_id: ctx.task_id,
                    step_id: ctx.step_id,
                    tool: $name,
                    category: PermissionCategory::Filesystem,
                    action: ActionKind::Read,
                    risk: RiskLevel::Safe,
                    input_hash: canonical_input_hash(input),
                    target: "repository index".into(),
                    in_plan_scope: true,
                })
            }

            async fn execute(&self, _ctx: &ToolCtx, _auth: &Authorization, _input: Value) -> ToolOutcome {
                ToolOutcome::failure(
                    "tools.not_yet_implemented",
                    concat!($name, " is not implemented yet (needs valyria-index / valyria-search, Phase 5)"),
                )
            }
        }
    };
}

not_yet_implemented_tool!(
    SearchTool,
    "search",
    "Lexical/semantic/AST-aware search across the repository."
);
not_yet_implemented_tool!(
    SymbolSearchTool,
    "symbol_search",
    "Find symbols by name across the repository."
);
