//! Filesystem tools (§17): `read_file`, `write_file`, `edit_file`,
//! `delete_file`, `move_file`, `list_directory`.

use std::str::FromStr;

use async_trait::async_trait;
use serde_json::Value;
use valyria_edit::{EditRequest, EditStrategy, EditTransaction, Precondition};
use valyria_permissions::{ActionKind, Authorization, PermissionRequest, RiskLevel};
use valyria_types::PermissionCategory;
use valyria_util::ContentHash;

use crate::canonical::canonical_input_hash;
use crate::ctx::ToolCtx;
use crate::descriptor::{SideEffect, ToolDescriptor};
use crate::error::{Result, ToolError};
use crate::outcome::ToolOutcome;
use crate::tool_trait::Tool;

use super::helpers::{
    object_schema, optional_bool, optional_str, optional_u64, require_str, verify_authorization,
    SystemClockRef,
};

fn read_request(
    ctx: &ToolCtx,
    tool: &'static str,
    target: &str,
    input: &Value,
) -> Result<PermissionRequest> {
    Ok(PermissionRequest {
        task_id: ctx.task_id,
        step_id: ctx.step_id,
        tool,
        category: PermissionCategory::Filesystem,
        action: ActionKind::Read,
        risk: RiskLevel::Safe,
        input_hash: canonical_input_hash(input),
        target: target.to_string(),
        in_plan_scope: true,
    })
}

fn write_request(
    ctx: &ToolCtx,
    tool: &'static str,
    target: &str,
    input: &Value,
) -> Result<PermissionRequest> {
    // Plan-scope checking arrives with `valyria-plan` (Phase 8); until
    // then, every in-workspace write is treated as in-scope, which is the
    // permissive-but-not-unsafe default (the workspace boundary itself is
    // still enforced by `WorkspaceRoot::resolve`).
    Ok(PermissionRequest {
        task_id: ctx.task_id,
        step_id: ctx.step_id,
        tool,
        category: PermissionCategory::Filesystem,
        action: ActionKind::Write,
        risk: RiskLevel::Safe,
        input_hash: canonical_input_hash(input),
        target: target.to_string(),
        in_plan_scope: true,
    })
}

fn parse_precondition(input: &Value, tool: &'static str) -> Result<Precondition> {
    match input.get("precondition") {
        None => Ok(Precondition::Any),
        Some(Value::String(s)) if s == "any" => Ok(Precondition::Any),
        Some(Value::String(s)) if s == "must_not_exist" => Ok(Precondition::MustNotExist),
        Some(Value::Object(o)) => {
            let hex = o
                .get("must_exist_with_hash")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::InvalidInput {
                    tool,
                    reason: "precondition object must have `must_exist_with_hash`".into(),
                })?;
            let hash = ContentHash::from_str(hex).map_err(|_| ToolError::InvalidInput {
                tool,
                reason: "invalid content hash in precondition".into(),
            })?;
            Ok(Precondition::MustExistWithHash(hash))
        }
        _ => Err(ToolError::InvalidInput {
            tool,
            reason: "invalid `precondition`".into(),
        }),
    }
}

fn parse_strategy(input: &Value, tool: &'static str) -> Result<EditStrategy> {
    let strategy = input
        .get("strategy")
        .ok_or_else(|| ToolError::InvalidInput {
            tool,
            reason: "missing `strategy`".into(),
        })?;
    let kind = strategy
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput {
            tool,
            reason: "strategy missing `type`".into(),
        })?;

    match kind {
        "exact_replacement" => Ok(EditStrategy::ExactReplacement {
            anchor: require_str(strategy, "anchor", tool)?.to_string(),
            replacement: require_str(strategy, "replacement", tool)?.to_string(),
        }),
        "unified_diff" => Ok(EditStrategy::UnifiedDiff {
            diff: require_str(strategy, "diff", tool)?.to_string(),
        }),
        "whole_file_replacement" => Ok(EditStrategy::WholeFileReplacement {
            content: require_str(strategy, "content", tool)?.to_string(),
            reason: require_str(strategy, "reason", tool)?.to_string(),
            force: optional_bool(strategy, "force", false),
        }),
        other => Err(ToolError::InvalidInput {
            tool,
            reason: format!("unknown strategy type `{other}`"),
        }),
    }
}

// ---------------------------------------------------------------- read_file

pub struct ReadFileTool {
    descriptor: ToolDescriptor,
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "read_file",
                description: "Read a UTF-8 text file within the workspace.",
                input_schema: object_schema(
                    serde_json::json!({
                        "path": {"type": "string"},
                        "max_bytes": {"type": "integer", "minimum": 1},
                    }),
                    &["path"],
                ),
                side_effect: SideEffect::ReadOnly,
            },
        }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        let path = require_str(input, "path", "read_file")?;
        read_request(ctx, "read_file", path, input)
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) = verify_authorization(auth, ctx.task_id, ctx.step_id, "read_file", &input) {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }
        let path = match require_str(&input, "path", "read_file") {
            Ok(p) => p,
            Err(e) => return ToolOutcome::failure(e.code_str(), e.to_string()),
        };
        let max_bytes = optional_u64(&input, "max_bytes").unwrap_or(1_000_000) as usize;

        let resolved = match ctx.workspace_root.resolve(path) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::failure("tools.vfs", e.to_string()),
        };

        let is_binary = valyria_vfs::looks_binary_file(&resolved).unwrap_or(false);
        if is_binary {
            return ToolOutcome::failure(
                "tools.binary_file",
                format!("{path} looks like a binary file"),
            );
        }

        match std::fs::read_to_string(&resolved) {
            Ok(content) => {
                let truncated = content.len() > max_bytes;
                let shown: String = if truncated {
                    content.chars().take(max_bytes).collect()
                } else {
                    content.clone()
                };
                ToolOutcome::success(
                    serde_json::json!({
                        "content": shown,
                        "truncated": truncated,
                        "size_bytes": content.len(),
                    }),
                    shown,
                )
            }
            Err(e) => ToolOutcome::failure("tools.io", e.to_string()),
        }
    }
}

// --------------------------------------------------------------- write_file

pub struct WriteFileTool {
    descriptor: ToolDescriptor,
}

impl Default for WriteFileTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "write_file",
                description: "Write (create or wholly replace) a file within the workspace.",
                input_schema: object_schema(
                    serde_json::json!({
                        "path": {"type": "string"},
                        "content": {"type": "string"},
                        "reason": {"type": "string"},
                        "force": {"type": "boolean"},
                        "precondition": {"description": "\"any\" | \"must_not_exist\" | {\"must_exist_with_hash\": \"<hex>\"}"},
                    }),
                    &["path", "content"],
                ),
                side_effect: SideEffect::WritesFilesystem,
            },
        }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        let path = require_str(input, "path", "write_file")?;
        write_request(ctx, "write_file", path, input)
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) = verify_authorization(auth, ctx.task_id, ctx.step_id, "write_file", &input) {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }

        let result = (|| -> Result<ToolOutcome> {
            let path = require_str(&input, "path", "write_file")?;
            let content = require_str(&input, "content", "write_file")?;
            let reason = optional_str(&input, "reason")
                .unwrap_or("agent write")
                .to_string();
            let force = optional_bool(&input, "force", false);
            let precondition = parse_precondition(&input, "write_file")?;

            let tx = EditTransaction::new(&ctx.workspace_root, &ctx.hash_cache);
            let outcome = tx.apply(EditRequest {
                path: path.into(),
                precondition,
                strategy: EditStrategy::WholeFileReplacement {
                    content: content.to_string(),
                    reason,
                    force,
                },
            })?;

            ctx.ledger.record_write(
                ctx.task_id,
                ctx.step_id,
                None,
                outcome.path.clone(),
                outcome.before_hash,
                outcome.before_content.as_deref().map(str::as_bytes),
                outcome.after_hash,
                &SystemClockRef,
            )?;

            Ok(ToolOutcome::success(
                serde_json::json!({
                    "path": outcome.path,
                    "before_hash": outcome.before_hash.map(|h| h.to_hex()),
                    "after_hash": outcome.after_hash.to_hex(),
                }),
                outcome.diff,
            ))
        })();

        result.unwrap_or_else(|e| ToolOutcome::failure(e.code_str(), e.to_string()))
    }
}

// -------------------------------------------------------------- delete_file

pub struct DeleteFileTool {
    descriptor: ToolDescriptor,
}

impl Default for DeleteFileTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "delete_file",
                description: "Delete a file within the workspace.",
                input_schema: object_schema(
                    serde_json::json!({
                        "path": {"type": "string"},
                        "must_exist_with_hash": {"type": "string"},
                    }),
                    &["path"],
                ),
                side_effect: SideEffect::WritesFilesystem,
            },
        }
    }
}

#[async_trait]
impl Tool for DeleteFileTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        let path = require_str(input, "path", "delete_file")?;
        write_request(ctx, "delete_file", path, input)
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) = verify_authorization(auth, ctx.task_id, ctx.step_id, "delete_file", &input)
        {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }

        let result = (|| -> Result<ToolOutcome> {
            let path = require_str(&input, "path", "delete_file")?;
            let resolved = ctx.workspace_root.resolve(path)?;

            let content = std::fs::read(&resolved).map_err(ToolError::Io)?;
            let before_hash = ContentHash::of_bytes(&content);

            if let Some(expected_hex) = optional_str(&input, "must_exist_with_hash") {
                let expected =
                    ContentHash::from_str(expected_hex).map_err(|_| ToolError::InvalidInput {
                        tool: "delete_file",
                        reason: "invalid `must_exist_with_hash`".into(),
                    })?;
                if expected != before_hash {
                    return Err(ToolError::Ledger(
                        valyria_ledger::LedgerError::RollbackConflict,
                    ));
                }
            }

            std::fs::remove_file(&resolved).map_err(ToolError::Io)?;
            ctx.hash_cache.invalidate(&resolved);

            ctx.ledger.record_delete(
                ctx.task_id,
                ctx.step_id,
                None,
                path.into(),
                before_hash,
                Some(&content),
                &SystemClockRef,
            )?;

            Ok(ToolOutcome::success(
                serde_json::json!({"path": path, "deleted_hash": before_hash.to_hex()}),
                format!("deleted {path}"),
            ))
        })();

        result.unwrap_or_else(|e| ToolOutcome::failure(e.code_str(), e.to_string()))
    }
}

// ---------------------------------------------------------------- move_file

pub struct MoveFileTool {
    descriptor: ToolDescriptor,
}

impl Default for MoveFileTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "move_file",
                description: "Move/rename a file within the workspace. The destination must not already exist.",
                input_schema: object_schema(
                    serde_json::json!({
                        "from": {"type": "string"},
                        "to": {"type": "string"},
                    }),
                    &["from", "to"],
                ),
                side_effect: SideEffect::WritesFilesystem,
            },
        }
    }
}

#[async_trait]
impl Tool for MoveFileTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        let from = require_str(input, "from", "move_file")?;
        let to = require_str(input, "to", "move_file")?;
        write_request(ctx, "move_file", &format!("{from} -> {to}"), input)
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) = verify_authorization(auth, ctx.task_id, ctx.step_id, "move_file", &input) {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }

        let result = (|| -> Result<ToolOutcome> {
            let from = require_str(&input, "from", "move_file")?;
            let to = require_str(&input, "to", "move_file")?;

            let resolved_from = ctx.workspace_root.resolve(from)?;
            let resolved_to = ctx.workspace_root.resolve(to)?;

            if resolved_to.exists() {
                return Err(ToolError::InvalidInput {
                    tool: "move_file",
                    reason: format!("destination `{to}` already exists"),
                });
            }

            let content = std::fs::read(&resolved_from).map_err(ToolError::Io)?;
            let before_hash = ContentHash::of_bytes(&content);

            std::fs::rename(&resolved_from, &resolved_to).map_err(ToolError::Io)?;
            ctx.hash_cache.invalidate(&resolved_from);
            ctx.hash_cache.invalidate(&resolved_to);

            ctx.ledger.record_delete(
                ctx.task_id,
                ctx.step_id,
                None,
                from.into(),
                before_hash,
                Some(&content),
                &SystemClockRef,
            )?;
            ctx.ledger.record_write(
                ctx.task_id,
                ctx.step_id,
                None,
                to.into(),
                None,
                None,
                before_hash,
                &SystemClockRef,
            )?;

            Ok(ToolOutcome::success(
                serde_json::json!({"from": from, "to": to, "hash": before_hash.to_hex()}),
                format!("moved {from} -> {to}"),
            ))
        })();

        result.unwrap_or_else(|e| ToolOutcome::failure(e.code_str(), e.to_string()))
    }
}

// ----------------------------------------------------------------- edit_file

pub struct EditFileTool {
    descriptor: ToolDescriptor,
}

impl Default for EditFileTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "edit_file",
                description:
                    "Apply a targeted edit to an existing file using one of several strategies.",
                input_schema: object_schema(
                    serde_json::json!({
                        "path": {"type": "string"},
                        "precondition": {"description": "\"any\" | {\"must_exist_with_hash\": \"<hex>\"}"},
                        "strategy": {"description": "{type: exact_replacement|unified_diff|whole_file_replacement, ...}"},
                    }),
                    &["path", "strategy"],
                ),
                side_effect: SideEffect::WritesFilesystem,
            },
        }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        let path = require_str(input, "path", "edit_file")?;
        write_request(ctx, "edit_file", path, input)
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) = verify_authorization(auth, ctx.task_id, ctx.step_id, "edit_file", &input) {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }

        let result = (|| -> Result<ToolOutcome> {
            let path = require_str(&input, "path", "edit_file")?;
            let precondition = parse_precondition(&input, "edit_file")?;
            let strategy = parse_strategy(&input, "edit_file")?;

            let tx = EditTransaction::new(&ctx.workspace_root, &ctx.hash_cache);
            let outcome = tx.apply(EditRequest {
                path: path.into(),
                precondition,
                strategy,
            })?;

            ctx.ledger.record_write(
                ctx.task_id,
                ctx.step_id,
                None,
                outcome.path.clone(),
                outcome.before_hash,
                outcome.before_content.as_deref().map(str::as_bytes),
                outcome.after_hash,
                &SystemClockRef,
            )?;

            Ok(ToolOutcome::success(
                serde_json::json!({
                    "path": outcome.path,
                    "before_hash": outcome.before_hash.map(|h| h.to_hex()),
                    "after_hash": outcome.after_hash.to_hex(),
                }),
                outcome.diff,
            ))
        })();

        result.unwrap_or_else(|e| ToolOutcome::failure(e.code_str(), e.to_string()))
    }
}

// ----------------------------------------------------------- list_directory

pub struct ListDirectoryTool {
    descriptor: ToolDescriptor,
}

impl Default for ListDirectoryTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "list_directory",
                description: "List files under a workspace directory, honoring .gitignore.",
                input_schema: object_schema(
                    serde_json::json!({
                        "path": {"type": "string"},
                        "recursive": {"type": "boolean"},
                    }),
                    &["path"],
                ),
                side_effect: SideEffect::ReadOnly,
            },
        }
    }
}

#[async_trait]
impl Tool for ListDirectoryTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        let path = require_str(input, "path", "list_directory")?;
        read_request(ctx, "list_directory", path, input)
    }

    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
        if let Err(e) =
            verify_authorization(auth, ctx.task_id, ctx.step_id, "list_directory", &input)
        {
            return ToolOutcome::failure(e.code_str(), e.to_string());
        }

        let result = (|| -> Result<ToolOutcome> {
            let path = require_str(&input, "path", "list_directory")?;
            let recursive = optional_bool(&input, "recursive", false);
            let resolved = ctx.workspace_root.resolve(path)?;

            let entries: Vec<String> = if recursive {
                valyria_vfs::list_files(&resolved)?
                    .into_iter()
                    .filter_map(|p| {
                        p.strip_prefix(&resolved)
                            .ok()
                            .map(|p| p.display().to_string())
                    })
                    .collect()
            } else {
                std::fs::read_dir(&resolved)
                    .map_err(ToolError::Io)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            };

            Ok(ToolOutcome::success(
                serde_json::json!({"path": path, "entries": entries}),
                entries.join("\n"),
            ))
        })();

        result.unwrap_or_else(|e| ToolOutcome::failure(e.code_str(), e.to_string()))
    }
}
