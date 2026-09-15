//! `search` and `symbol_search` (§17): the fused repository search engine
//! (`valyria-search`) over whatever index generation `valyria-app::Runtime`
//! bootstrapped at `open` (M2, `docs/COMPLETION-PLAN.md`). Registered but
//! degrades to a plain, honest failure — never a panic — when `ToolCtx`
//! has no workspace database (a `ToolCtx` built without `with_store`, or a
//! workspace whose index bootstrap never ran or failed).

use async_trait::async_trait;
use serde_json::Value;
use valyria_permissions::{ActionKind, Authorization, PermissionRequest, RiskLevel};
use valyria_search::{SearchMode, SearchQuery, SearchResults};
use valyria_types::PermissionCategory;

use crate::canonical::canonical_input_hash;
use crate::ctx::ToolCtx;
use crate::descriptor::{SideEffect, ToolDescriptor};
use crate::error::Result;
use crate::outcome::ToolOutcome;
use crate::tool_trait::Tool;

use super::helpers::{object_schema, optional_u64, require_str};

/// Run the fused search engine to completion against `ctx`'s store.
///
/// `SearchEngine::search` returns a `!Send` future (it holds a `gix`
/// repository handle across an `.await`), so — exactly like
/// `valyria_context::retrieve::SearchRetriever::run_search` and
/// `valyria_app::Runtime::search`, the same shape by necessity, not by
/// choice — it runs on a dedicated current-thread runtime on a scoped OS
/// thread; only the plain-data `SearchResults` crosses back into this
/// `Send` `execute`.
fn run_search(ctx: &ToolCtx, query: &SearchQuery) -> std::result::Result<SearchResults, String> {
    let Some(store) = ctx.store.clone() else {
        return Err("no repository index is available in this context".into());
    };
    let root = ctx.workspace_root.as_path().to_path_buf();

    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| format!("search runtime: {e}"))?;
                let registry = valyria_lang::LanguageRegistry::with_builtin_languages()
                    .map_err(|e| format!("language registry: {e}"))?;
                let index = valyria_index::IndexStore::new(store.clone());
                let engine = valyria_search::SearchEngine::new(
                    root,
                    index,
                    valyria_graph::GraphStore::new(store.clone()),
                    valyria_embed::EmbedStore::new(store),
                    std::sync::Arc::new(valyria_embed::HashingEmbedder::default()),
                    registry,
                );
                rt.block_on(engine.search(query)).map_err(|e| e.to_string())
            })
            .join()
            .map_err(|_| "search thread panicked".to_string())?
    })
}

/// Render hits + any degraded-mode notes into what the model sees — a
/// short, ranked, scannable list, never the raw structured result (D3:
/// tool output is `Trust::Evidence`, rendered budget-aware).
fn render(results: &SearchResults) -> String {
    if results.hits.is_empty() {
        let mut out = "no matches".to_string();
        if !results.degraded.is_empty() {
            out.push_str(&format!(" ({})", results.degraded.join("; ")));
        }
        return out;
    }
    let mut lines: Vec<String> = results
        .hits
        .iter()
        .map(|h| {
            let loc = match h.line {
                Some(l) => format!("{}:{l}", h.path),
                None => h.path.clone(),
            };
            let symbol = h
                .symbol_path
                .as_deref()
                .map(|s| format!(" [{s}]"))
                .unwrap_or_default();
            let snippet = h
                .snippet
                .as_deref()
                .map(|s| format!(" — {}", s.trim()))
                .unwrap_or_default();
            format!("{loc}{symbol} (score {:.3}){snippet}", h.score)
        })
        .collect();
    if !results.degraded.is_empty() {
        lines.push(format!("degraded: {}", results.degraded.join("; ")));
    }
    lines.join("\n")
}

fn structured(results: &SearchResults) -> Value {
    serde_json::json!({
        "hits": results.hits.iter().map(|h| serde_json::json!({
            "path": h.path,
            "symbol_path": h.symbol_path,
            "line": h.line,
            "snippet": h.snippet,
            "score": h.score,
        })).collect::<Vec<_>>(),
        "modes_run": results.modes_run.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>(),
        "degraded": results.degraded,
    })
}

fn search_request(ctx: &ToolCtx, tool: &'static str, input: &Value) -> Result<PermissionRequest> {
    Ok(PermissionRequest {
        task_id: ctx.task_id,
        step_id: ctx.step_id,
        tool,
        category: PermissionCategory::Filesystem,
        action: ActionKind::Read,
        risk: RiskLevel::Safe,
        input_hash: canonical_input_hash(input),
        target: "repository index".into(),
        in_plan_scope: true,
    })
}

pub struct SearchTool {
    descriptor: ToolDescriptor,
}

impl Default for SearchTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "search",
                description: "Lexical/regex/symbol/semantic/AST/dependency/git-aware search \
                    across the repository, fused and ranked. Optionally scope to a limit.",
                input_schema: object_schema(
                    serde_json::json!({
                        "query": {"type": "string", "description": "The search text."},
                        "limit": {"type": "integer", "description": "Max results (default 20)."},
                    }),
                    &["query"],
                ),
                side_effect: SideEffect::ReadOnly,
            },
        }
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        search_request(ctx, "search", input)
    }

    async fn execute(&self, ctx: &ToolCtx, _auth: &Authorization, input: Value) -> ToolOutcome {
        let query_text = match require_str(&input, "query", "search") {
            Ok(q) => q,
            Err(e) => return ToolOutcome::failure("tools.invalid_input", e.to_string()),
        };
        let limit = optional_u64(&input, "limit").unwrap_or(20).max(1) as usize;
        let sq = SearchQuery::new(query_text).limit(limit);
        match run_search(ctx, &sq) {
            Ok(results) => ToolOutcome::success(structured(&results), render(&results)),
            Err(e) => ToolOutcome::failure("tools.search_unavailable", e),
        }
    }
}

pub struct SymbolSearchTool {
    descriptor: ToolDescriptor,
}

impl Default for SymbolSearchTool {
    fn default() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "symbol_search",
                description: "Find symbols (functions, types, methods) by name across the \
                    repository, ranked by relevance.",
                input_schema: object_schema(
                    serde_json::json!({
                        "query": {"type": "string", "description": "The symbol name (or a fragment of it)."},
                        "limit": {"type": "integer", "description": "Max results (default 20)."},
                    }),
                    &["query"],
                ),
                side_effect: SideEffect::ReadOnly,
            },
        }
    }
}

#[async_trait]
impl Tool for SymbolSearchTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest> {
        search_request(ctx, "symbol_search", input)
    }

    async fn execute(&self, ctx: &ToolCtx, _auth: &Authorization, input: Value) -> ToolOutcome {
        let query_text = match require_str(&input, "query", "symbol_search") {
            Ok(q) => q,
            Err(e) => return ToolOutcome::failure("tools.invalid_input", e.to_string()),
        };
        let limit = optional_u64(&input, "limit").unwrap_or(20).max(1) as usize;
        let sq = SearchQuery::new(query_text)
            .mode(SearchMode::Symbol)
            .limit(limit);
        match run_search(ctx, &sq) {
            Ok(results) => ToolOutcome::success(structured(&results), render(&results)),
            Err(e) => ToolOutcome::failure("tools.search_unavailable", e),
        }
    }
}
