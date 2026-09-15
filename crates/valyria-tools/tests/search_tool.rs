//! M2 (`docs/COMPLETION-PLAN.md`): `search` / `symbol_search` against a
//! real workspace database — the happy path `search_tool_degrades_cleanly`
//! (in `integration.rs`) deliberately doesn't cover, since it has no
//! `store` at all. This proves the tools actually find real content once
//! `ToolCtx::store` is wired, exactly as `valyria-app::Runtime` wires it
//! after M2's index bootstrap.

use std::sync::Arc;

use valyria_embed::{EmbedPipeline, EmbedStore, HashingEmbedder};
use valyria_graph::GraphStore;
use valyria_index::{IndexPipeline, IndexStore};
use valyria_lang::LanguageRegistry;
use valyria_ledger::Ledger;
use valyria_permissions::PermissionEngine;
use valyria_sandbox::{detect_platform_launcher, SandboxProfile};
use valyria_store::{Migration, Store};
use valyria_tools::{all_tools, InvocationResult, ToolCtx, ToolOutcome, ToolRuntime};
use valyria_types::{PermissionMode, SessionId, StepId, TaskId};
use valyria_util::FixedClock;
use valyria_vfs::{HashCache, WorkspaceRoot};

fn migrations() -> Vec<Migration> {
    let mut m: Vec<Migration> = valyria_index::MIGRATIONS.to_vec();
    m.extend(valyria_graph::MIGRATIONS.iter().copied());
    m.extend(valyria_embed::MIGRATIONS.iter().copied());
    m
}

#[tokio::test]
async fn search_and_symbol_search_find_real_content_once_a_store_is_wired() {
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write(
        "src/discount.rs",
        "//! Discount calculation.\n\
         \n\
         /// Apply a percentage discount to `price`.\n\
         pub fn apply_discount(price: f64, pct: f64) -> f64 {\n\
         \x20   price * (1.0 - pct / 100.0)\n\
         }\n",
    );

    let store = Arc::new(Store::open_in_memory(&migrations()).unwrap());
    let index = IndexStore::new(store.clone());
    let graph = GraphStore::new(store.clone());
    let embed = EmbedStore::new(store.clone());

    let pipeline = IndexPipeline::new(
        ws.path().to_path_buf(),
        LanguageRegistry::with_builtin_languages().unwrap(),
        index.clone(),
    );
    let delta = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();
    graph.build_for(&index, delta.generation).await.unwrap();
    let embed_pipeline = EmbedPipeline::new(
        ws.path().to_path_buf(),
        LanguageRegistry::with_builtin_languages().unwrap(),
        Arc::new(HashingEmbedder::default()),
        embed.clone(),
    );
    embed_pipeline
        .bootstrap(&index, delta.generation)
        .await
        .unwrap();

    let root = WorkspaceRoot::new(ws.path()).unwrap();
    let blob_dir = tempfile::tempdir().unwrap();
    let ledger = Arc::new(Ledger::new(blob_dir.path()).unwrap());
    let clock = Arc::new(FixedClock::at_millis(1_000_000));
    let engine = Arc::new(
        PermissionEngine::new(PermissionMode::Autonomous, clock.clone())
            .with_session(SessionId::new()),
    );
    let runtime = ToolRuntime::new(all_tools(), engine, clock);

    let ctx = ToolCtx {
        sandbox_profile: SandboxProfile::new().allow_write(root.as_path()),
        workspace_root: root,
        hash_cache: Arc::new(HashCache::new()),
        ledger,
        task_id: TaskId::new(),
        step_id: StepId::new(),
        cancel: valyria_util::CancellationToken::new(),
        launcher: Arc::from(detect_platform_launcher()),
        store: Some(store),
    };

    let result = runtime
        .invoke(&ctx, "search", serde_json::json!({"query": "discount"}))
        .await;
    let InvocationResult::Executed { outcome, .. } = result else {
        panic!("expected Executed, got {result:?}");
    };
    let ToolOutcome::Success {
        structured,
        rendered,
    } = outcome
    else {
        panic!("expected search to succeed with a real store, got {outcome:?}");
    };
    assert!(
        rendered.contains("discount.rs"),
        "expected the rendered output to name the matching file: {rendered}"
    );
    let hits = structured["hits"].as_array().unwrap();
    assert!(
        hits.iter()
            .any(|h| h["path"].as_str().unwrap_or_default().contains("discount")),
        "expected a hit for discount.rs in {structured}"
    );

    let symbol_result = runtime
        .invoke(
            &ctx,
            "symbol_search",
            serde_json::json!({"query": "apply_discount"}),
        )
        .await;
    let InvocationResult::Executed { outcome, .. } = symbol_result else {
        panic!("expected Executed, got {symbol_result:?}");
    };
    let ToolOutcome::Success { rendered, .. } = outcome else {
        panic!("expected symbol_search to succeed, got {outcome:?}");
    };
    assert!(
        rendered.contains("apply_discount") || rendered.contains("discount.rs"),
        "expected the symbol or its file in: {rendered}"
    );
}
