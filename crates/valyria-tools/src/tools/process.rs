//! Process-execution tools (§17): `run_command`, `run_test`,
//! `run_formatter`, `run_linter`. All four share the same mechanics —
//! execute an explicit argv the caller provides — differing only in the
//! semantic label attached to the invocation. *Discovering* which test/
//! formatter/linter command to run is `valyria-verify`'s job (Phase 7,
//! not yet built); these tools execute whatever they're told.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use valyria_permissions::{classify_command, ActionKind, Authorization, PermissionRequest};
use valyria_process::{CommandSpec, EnvPolicy};
use valyria_types::PermissionCategory;

use crate::canonical::canonical_input_hash;
use crate::ctx::ToolCtx;
use crate::descriptor::{SideEffect, ToolDescriptor};
use crate::error::Result;
use crate::outcome::ToolOutcome;
use crate::tool_trait::Tool;

use super::helpers::{object_schema, optional_u64, require_str, verify_authorization};

fn args_of(input: &Value) -> Vec<String> {
    input
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn build_spec(ctx: &ToolCtx, input: &Value, tool: &'static str) -> Result<CommandSpec> {
    let program = require_str(input, "program", tool)?;
    let args = args_of(input);
    let timeout_secs = optional_u64(input, "timeout_secs").unwrap_or(120);
    let env = EnvPolicy::inherit_filtered().build(&std::env::vars().collect());

    let spec = CommandSpec::new(program, ctx.workspace_root.as_path())
        .args(args)
        .env(env)
        .timeout(Duration::from_secs(timeout_secs));
    // D10: every process-executing tool runs through whatever confinement
    // this workspace's launcher provides (real on platforms that have it,
    // `PermissiveSandbox`'s honest no-op otherwise) rather than calling
    // `valyria_process::run` directly.
    Ok(ctx.launcher.wrap(spec, &ctx.sandbox_profile)?)
}

fn shell_request(ctx: &ToolCtx, tool: &'static str, input: &Value) -> Result<PermissionRequest> {
    let program = require_str(input, "program", tool)?;
    let args = args_of(input);
    let risk = classify_command(program, &args);
    let target = format!("{program} {}", args.join(" "));
    Ok(PermissionRequest {
        task_id: ctx.task_id,
        step_id: ctx.step_id,
        tool,
        category: PermissionCategory::Shell,
        action: ActionKind::Execute,
        risk,
        input_hash: canonical_input_hash(input),
        target,
        in_plan_scope: true,
    })
}

async fn run_and_render(ctx: &ToolCtx, spec: CommandSpec) -> ToolOutcome {
    match valyria_process::run(&spec, ctx.cancel.clone()).await {
        Ok(result) => {
            let structured = serde_json::json!({
                "exit_status": result.exit_code,
                "end_reason": format!("{:?}", result.end_reason),
                "stdout": result.stdout.text,
                "stderr": result.stderr.text,
            });
            let rendered = format!(
                "exit={:?} reason={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                result.exit_code, result.end_reason, result.stdout.text, result.stderr.text
            );
            if result.success() {
                ToolOutcome::success(structured, rendered)
            } else {
                ToolOutcome::Failure {
                    code: "tools.command_failed",
                    message: format!(
                        "command exited with {:?} ({:?})",
                        result.exit_code, result.end_reason
                    ),
                    rendered,
                }
            }
        }
        Err(e) => ToolOutcome::failure(e.code_str_alias(), e.to_string()),
    }
}

// helper trait so we don't need `use valyria_types::ErrorCode` sprinkled
// through this file just for one conversion.
trait ProcessErrorCodeExt {
    fn code_str_alias(&self) -> &'static str;
}
impl ProcessErrorCodeExt for valyria_process::ProcessError {
    fn code_str_alias(&self) -> &'static str {
        use valyria_types::ErrorCode;
        self.code()
    }
}

macro_rules! process_tool {
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
                        input_schema: object_schema(
                            serde_json::json!({
                                "program": {"type": "string"},
                                "args": {"type": "array", "items": {"type": "string"}},
                                "timeout_secs": {"type": "integer"},
                            }),
                            &["program"],
                        ),
                        side_effect: SideEffect::ExecutesProcess,
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
                shell_request(ctx, $name, input)
            }

            async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome {
                if let Err(e) = verify_authorization(auth, ctx.task_id, ctx.step_id, $name, &input) {
                    return ToolOutcome::failure(e.code_str(), e.to_string());
                }
                let spec = match build_spec(ctx, &input, $name) {
                    Ok(s) => s,
                    Err(e) => return ToolOutcome::failure(e.code_str(), e.to_string()),
                };
                run_and_render(ctx, spec).await
            }
        }
    };
}

process_tool!(
    RunCommandTool,
    "run_command",
    "Run an arbitrary command within the workspace."
);
process_tool!(
    RunTestTool,
    "run_test",
    "Run an explicit test command within the workspace (does not discover which command to use)."
);
process_tool!(
    RunFormatterTool,
    "run_formatter",
    "Run an explicit formatter command within the workspace (does not discover which formatter to use)."
);
process_tool!(
    RunLinterTool,
    "run_linter",
    "Run an explicit linter command within the workspace (does not discover which linter to use)."
);

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use valyria_ledger::Ledger;
    use valyria_sandbox::{Confinement, ProcessLauncher, SandboxProfile};
    use valyria_types::{StepId, TaskId};
    use valyria_util::CancellationToken;
    use valyria_vfs::{HashCache, WorkspaceRoot};

    use super::*;

    struct SpyLauncher {
        called: Arc<AtomicBool>,
    }

    impl ProcessLauncher for SpyLauncher {
        fn confinement_level(&self) -> Confinement {
            Confinement::None
        }

        fn wrap(
            &self,
            spec: CommandSpec,
            _profile: &SandboxProfile,
        ) -> valyria_sandbox::Result<CommandSpec> {
            self.called.store(true, Ordering::SeqCst);
            Ok(spec)
        }
    }

    #[test]
    fn build_spec_wraps_through_the_configured_launcher() {
        let ws = valyria_testkit::TempWorkspace::new();
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let ctx = ToolCtx {
            workspace_root: root,
            hash_cache: Arc::new(HashCache::new()),
            ledger: Arc::new(Ledger::new(blob_dir.path()).unwrap()),
            task_id: TaskId::new(),
            step_id: StepId::new(),
            cancel: CancellationToken::new(),
            launcher: Arc::new(SpyLauncher {
                called: called.clone(),
            }),
            sandbox_profile: SandboxProfile::new(),
        };
        let input = serde_json::json!({"program": "echo", "args": ["hi"]});
        build_spec(&ctx, &input, "run_command").unwrap();
        assert!(called.load(Ordering::SeqCst));
    }
}
