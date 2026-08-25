//! Git tools (§17): `git_status`, `git_diff`, `git_log`, `git_show`. Scoped
//! to exactly what `valyria-git` implements today — `git_blame` is
//! registered but not yet implemented (blame lands with a future
//! `valyria-git` pass).

use async_trait::async_trait;
use serde_json::Value;
use valyria_git::Repo;
use valyria_permissions::{ActionKind, Authorization, PermissionRequest, RiskLevel};
use valyria_types::PermissionCategory;

use crate::canonical::canonical_input_hash;
use crate::ctx::ToolCtx;
use crate::descriptor::{SideEffect, ToolDescriptor};
use crate::error::{Result, ToolError};
use crate::outcome::ToolOutcome;
use crate::tool_trait::Tool;

use super::helpers::{object_schema, optional_u64, require_str, verify_authorization};

fn git_read_request(ctx: &ToolCtx, tool: &'static str, input: &Value) -> Result<PermissionRequest> {
    Ok(PermissionRequest {
        task_id: ctx.task_id,
        step_id: ctx.step_id,
        tool,
        category: PermissionCategory::Filesystem,
        action: ActionKind::Read,
        risk: RiskLevel::Safe,
        input_hash: canonical_input_hash(input),
        target: "repository".into(),
        in_plan_scope: true,
    })
}

fn open_repo(ctx: &ToolCtx, tool: &'static str) -> Result<Repo> {
    Repo::open(ctx.workspace_root.as_path()).map_err(|e| {
        tracing::debug!(tool, "git open failed: {e}");
        ToolError::Git(e)
    })
}

macro_rules! simple_readonly_git_tool {
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
                        input_schema: object_schema(serde_json::json!({}), &[]),
                        side_effect: SideEffect::ReadOnly,
                    },
                }
            }
        }
        impl $ty {
            fn descriptor_ref(&self) -> &ToolDescriptor {
                &self.descriptor
            }
        }
    };
}

// ---------------------------------------------------------------- git_status

simple_readonly_git_tool!(
    GitStatusTool,
    "git_status",
    "Report staged/unstaged/untracked file status."
);

#[async_trait]
impl Tool for GitStatusTool {
    fn descriptor(&self) -> &ToolDescriptor {
        self.descriptor_ref()
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        git_read_request(ctx, "git_status", input)
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) = verify_authorization(auth, ctx.task_id, ctx.step_id, "git_status", &input) {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }
        let result = (|| -> Result<ToolOutcome> {
            let repo = open_repo(ctx, "git_status")?;
            let status = repo.status().map_err(ToolError::Git)?;
            let rendered = if status.is_clean() {
                "clean".to_string()
            } else {
                status
                    .files
                    .iter()
                    .map(|f| {
                        format!(
                            "{:?} {} {}",
                            f.kind,
                            if f.staged { "staged" } else { "unstaged" },
                            f.path
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let structured = serde_json::json!(status
                .files
                .iter()
                .map(|f| serde_json::json!({"path": f.path, "kind": format!("{:?}", f.kind), "staged": f.staged}))
                .collect::<Vec<_>>());
            Ok(ToolOutcome::success(structured, rendered))
        })();
        result.unwrap_or_else(|e| ToolOutcome::failure(e.code_str(), e.to_string()))
    }
}

// ------------------------------------------------------------------ git_diff

simple_readonly_git_tool!(
    GitDiffTool,
    "git_diff",
    "Summarize the working-tree diff (staged and unstaged file-level changes)."
);

#[async_trait]
impl Tool for GitDiffTool {
    fn descriptor(&self) -> &ToolDescriptor {
        self.descriptor_ref()
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        git_read_request(ctx, "git_diff", input)
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) = verify_authorization(auth, ctx.task_id, ctx.step_id, "git_diff", &input) {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }
        let result = (|| -> Result<ToolOutcome> {
            let repo = open_repo(ctx, "git_diff")?;
            let status = repo.status().map_err(ToolError::Git)?;
            let structured = serde_json::json!(status
                .files
                .iter()
                .map(|f| serde_json::json!({"path": f.path, "kind": format!("{:?}", f.kind), "staged": f.staged}))
                .collect::<Vec<_>>());
            let rendered = structured.to_string();
            Ok(ToolOutcome::success(structured, rendered))
        })();
        result.unwrap_or_else(|e| ToolOutcome::failure(e.code_str(), e.to_string()))
    }
}

// ------------------------------------------------------------------- git_log

pub struct GitLogTool {
    descriptor: ToolDescriptor,
}

impl Default for GitLogTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "git_log",
                description: "Walk commit history from HEAD, newest first.",
                input_schema: object_schema(
                    serde_json::json!({"max_count": {"type": "integer"}}),
                    &[],
                ),
                side_effect: SideEffect::ReadOnly,
            },
        }
    }
}

#[async_trait]
impl Tool for GitLogTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        git_read_request(ctx, "git_log", input)
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) = verify_authorization(auth, ctx.task_id, ctx.step_id, "git_log", &input) {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }
        let result = (|| -> Result<ToolOutcome> {
            let max_count = optional_u64(&input, "max_count").unwrap_or(20) as usize;
            let repo = open_repo(ctx, "git_log")?;
            let log = repo.log(max_count).map_err(ToolError::Git)?;
            let structured = serde_json::json!(log
                .iter()
                .map(|c| serde_json::json!({"sha": c.sha, "author": c.author_name, "message": c.message}))
                .collect::<Vec<_>>());
            let rendered = log
                .iter()
                .map(|c| format!("{} {}", &c.sha[..c.sha.len().min(10)], c.message))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(ToolOutcome::success(structured, rendered))
        })();
        result.unwrap_or_else(|e| ToolOutcome::failure(e.code_str(), e.to_string()))
    }
}

// ------------------------------------------------------------------ git_show

pub struct GitShowTool {
    descriptor: ToolDescriptor,
}

impl Default for GitShowTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "git_show",
                description:
                    "Show the file-level changes a commit introduced relative to its first parent.",
                input_schema: object_schema(
                    serde_json::json!({"sha": {"type": "string"}}),
                    &["sha"],
                ),
                side_effect: SideEffect::ReadOnly,
            },
        }
    }
}

#[async_trait]
impl Tool for GitShowTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        git_read_request(ctx, "git_show", input)
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) = verify_authorization(auth, ctx.task_id, ctx.step_id, "git_show", &input) {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }
        let result = (|| -> Result<ToolOutcome> {
            let sha = require_str(&input, "sha", "git_show")?;
            let repo = open_repo(ctx, "git_show")?;
            let diff = repo.show(sha).map_err(ToolError::Git)?;
            let structured = serde_json::json!(diff
                .iter()
                .map(|d| serde_json::json!({"path": d.path, "status": format!("{:?}", d.status)}))
                .collect::<Vec<_>>());
            let rendered = diff
                .iter()
                .map(|d| format!("{:?} {}", d.status, d.path))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(ToolOutcome::success(structured, rendered))
        })();
        result.unwrap_or_else(|e| ToolOutcome::failure(e.code_str(), e.to_string()))
    }
}

// ----------------------------------------------------------------- git_blame

pub struct GitBlameTool {
    descriptor: ToolDescriptor,
}

impl Default for GitBlameTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "git_blame",
                description: "Not yet implemented — lands with a future valyria-git pass.",
                input_schema: object_schema(
                    serde_json::json!({"path": {"type": "string"}}),
                    &["path"],
                ),
                side_effect: SideEffect::ReadOnly,
            },
        }
    }
}

#[async_trait]
impl Tool for GitBlameTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        git_read_request(ctx, "git_blame", input)
    }

    async fn execute(&self, _ctx: &ToolCtx, _auth: &Authorization, _input: Value) -> ToolOutcome {
        ToolOutcome::failure(
            "tools.not_yet_implemented",
            "git_blame is not implemented yet",
        )
    }
}
