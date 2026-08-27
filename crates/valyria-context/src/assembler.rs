//! `ContextAssembler`: Phase 3's minimal, explicit-file-only context
//! pipeline. Every read goes through `ToolRuntime::invoke("read_file", ..)`
//! — a real permissioned, ledgered, journaled tool call — never a side
//! channel that bypasses D2. Fails loudly on budget overflow (§4.17)
//! rather than silently truncating; no partial-item truncation in Phase 3.

use std::sync::Arc;

use valyria_tools::{InvocationResult, ToolCtx, ToolOutcome, ToolRuntime};
use valyria_types::{Provenance, ProvenanceSource, Trust};
use valyria_util::{HeuristicTokenCounter, TokenCounter};

use crate::error::{ContextError, Result};
use crate::item::{ContextBody, ContextItem};
use crate::query::{AssembledContext, ContextQuery};

pub struct ContextAssembler {
    tools: Arc<ToolRuntime>,
    counter: Arc<dyn TokenCounter>,
}

impl ContextAssembler {
    pub fn new(tools: Arc<ToolRuntime>) -> Self {
        Self {
            tools,
            counter: Arc::new(HeuristicTokenCounter),
        }
    }

    pub fn with_token_counter(mut self, counter: Arc<dyn TokenCounter>) -> Self {
        self.counter = counter;
        self
    }

    pub async fn assemble(&self, ctx: &ToolCtx, query: ContextQuery) -> Result<AssembledContext> {
        let mut items = Vec::with_capacity(query.explicit_paths.len());
        let mut total_tokens = 0usize;

        for path in &query.explicit_paths {
            let content = self.read_file(ctx, path).await?;
            let tokens = self.counter.count(&content);
            let remaining = query.budget_tokens.saturating_sub(total_tokens);
            if tokens > remaining {
                return Err(ContextError::BudgetExceeded {
                    path: path.clone(),
                    needed: tokens,
                    remaining,
                });
            }
            total_tokens += tokens;
            items.push(ContextItem {
                trust: Trust::RepoData,
                provenance: Provenance::new(ProvenanceSource::File { path: path.clone() })
                    .with_step("explicit_file_request"),
                tokens,
                body: ContextBody::Text(content),
            });
        }

        Ok(AssembledContext {
            items,
            total_tokens,
        })
    }

    async fn read_file(&self, ctx: &ToolCtx, path: &str) -> Result<String> {
        let input = serde_json::json!({"path": path});
        match self.tools.invoke(ctx, "read_file", input).await {
            InvocationResult::Executed {
                outcome: ToolOutcome::Success { structured, .. },
                ..
            } => Ok(structured
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()),
            InvocationResult::Executed {
                outcome: ToolOutcome::Failure { message, .. },
                ..
            } => Err(ContextError::ReadFailed {
                path: path.to_string(),
                message,
            }),
            InvocationResult::Denied { reason } => Err(ContextError::ReadFailed {
                path: path.to_string(),
                message: reason,
            }),
            InvocationResult::AskRequired { prompt, .. } => Err(ContextError::ReadFailed {
                path: path.to_string(),
                message: prompt,
            }),
            InvocationResult::UnknownTool(name) => {
                unreachable!("`{name}` is always registered by valyria_tools::all_tools()")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;
    use valyria_ledger::Ledger;
    use valyria_permissions::PermissionEngine;
    use valyria_sandbox::{detect_platform_launcher, SandboxProfile};
    use valyria_types::{PermissionMode, StepId, TaskId};
    use valyria_util::FixedClock;
    use valyria_vfs::{HashCache, WorkspaceRoot};

    fn harness(
        ws: &valyria_testkit::TempWorkspace,
    ) -> (ToolCtx, ContextAssembler, tempfile::TempDir) {
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
        let ledger = StdArc::new(Ledger::new(blob_dir.path()).unwrap());
        let clock: StdArc<dyn valyria_util::Clock> = StdArc::new(FixedClock::at_millis(0));
        let engine = StdArc::new(PermissionEngine::new(
            PermissionMode::Assisted,
            clock.clone(),
        ));
        let registry = valyria_tools::all_tools();
        let runtime = StdArc::new(ToolRuntime::new(registry, engine, clock));
        let ctx = ToolCtx {
            sandbox_profile: SandboxProfile::new().allow_write(root.as_path()),
            workspace_root: root,
            hash_cache: StdArc::new(HashCache::new()),
            ledger,
            task_id: TaskId::new(),
            step_id: StepId::new(),
            cancel: valyria_util::CancellationToken::new(),
            launcher: StdArc::from(detect_platform_launcher()),
        };
        (ctx, ContextAssembler::new(runtime), blob_dir)
    }

    #[tokio::test]
    async fn assembling_zero_files_is_empty() {
        let ws = valyria_testkit::TempWorkspace::new();
        let (ctx, assembler, _blobs) = harness(&ws);
        let assembled = assembler
            .assemble(&ctx, ContextQuery::new(1000))
            .await
            .unwrap();
        assert!(assembled.items.is_empty());
        assert_eq!(assembled.total_tokens, 0);
    }

    #[tokio::test]
    async fn assembling_one_file_carries_repo_data_trust_and_file_provenance() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("a.txt", "hello world");
        let (ctx, assembler, _blobs) = harness(&ws);
        let assembled = assembler
            .assemble(&ctx, ContextQuery::new(1000).with_path("a.txt"))
            .await
            .unwrap();
        assert_eq!(assembled.items.len(), 1);
        let item = &assembled.items[0];
        assert_eq!(item.trust, Trust::RepoData);
        assert!(matches!(
            &item.provenance.source,
            ProvenanceSource::File { path } if path == "a.txt"
        ));
        assert!(matches!(&item.body, ContextBody::Text(t) if t == "hello world"));
        assert_eq!(assembled.total_tokens, item.tokens);
    }

    #[tokio::test]
    async fn assembling_two_files_sums_tokens() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("a.txt", "hello");
        ws.write("b.txt", "world");
        let (ctx, assembler, _blobs) = harness(&ws);
        let assembled = assembler
            .assemble(
                &ctx,
                ContextQuery::new(1000)
                    .with_path("a.txt")
                    .with_path("b.txt"),
            )
            .await
            .unwrap();
        assert_eq!(assembled.items.len(), 2);
        let sum: usize = assembled.items.iter().map(|i| i.tokens).sum();
        assert_eq!(sum, assembled.total_tokens);
    }

    #[tokio::test]
    async fn budget_exceeded_fails_loudly_instead_of_truncating() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("big.txt", "x".repeat(1000));
        let (ctx, assembler, _blobs) = harness(&ws);
        let err = assembler
            .assemble(&ctx, ContextQuery::new(1).with_path("big.txt"))
            .await
            .unwrap_err();
        assert!(matches!(err, ContextError::BudgetExceeded { .. }));
    }

    #[tokio::test]
    async fn nonexistent_path_surfaces_as_read_failed_not_a_panic() {
        let ws = valyria_testkit::TempWorkspace::new();
        let (ctx, assembler, _blobs) = harness(&ws);
        let err = assembler
            .assemble(&ctx, ContextQuery::new(1000).with_path("missing.txt"))
            .await
            .unwrap_err();
        assert!(matches!(err, ContextError::ReadFailed { .. }));
    }
}
