//! M2 (`docs/COMPLETION-PLAN.md`): the live agent loop, given a real
//! `LiveRetriever::Search`, actually retrieves repository content into a
//! turn's context — not just `LiveRetriever`'s own unit tests of the
//! `Retriever` trait in isolation, and not `valyria-context`'s own
//! `search_retrieval.rs` (which proves the pipeline, not that `AgentDriver`
//! calls it). This is the driver-level proof: a fixture file whose content
//! answers the objective shows up in the journaled `context_retrieved`
//! payload before the model is ever called.

use std::sync::Arc;

use valyria_agent::AgentDriver;
use valyria_context::{ContextAssembler, LiveRetriever, SearchRetriever};
use valyria_embed::{EmbedPipeline, EmbedStore, HashingEmbedder};
use valyria_events::{EventBus, EventKind, Seq};
use valyria_graph::GraphStore;
use valyria_index::{IndexPipeline, IndexStore};
use valyria_lang::LanguageRegistry;
use valyria_ledger::Ledger;
use valyria_orchestrator::{Role, RoleRouter};
use valyria_permissions::PermissionEngine;
use valyria_runtime_fake::{FakeModelRuntime, Scenario, ScriptedTurn};
use valyria_sandbox::{detect_platform_launcher, ProcessLauncher, SandboxProfile};
use valyria_search::SearchEngine;
use valyria_store::{Migration, Store};
use valyria_task::{Budget, TaskManager};
use valyria_tools::ToolRuntime;
use valyria_types::{PermissionMode, WorkspaceId};
use valyria_util::{CancellationToken, Clock, FixedClock};
use valyria_verify::VerificationLog;
use valyria_vfs::{HashCache, WorkspaceRoot};

fn migrations() -> Vec<Migration> {
    let mut m: Vec<Migration> = valyria_events::MIGRATIONS.to_vec();
    m.extend(valyria_task::MIGRATIONS.iter().copied());
    m.extend(valyria_verify::MIGRATIONS.iter().copied());
    m.extend(valyria_plan::MIGRATIONS.iter().copied());
    m.extend(valyria_index::MIGRATIONS.iter().copied());
    m.extend(valyria_graph::MIGRATIONS.iter().copied());
    m.extend(valyria_embed::MIGRATIONS.iter().copied());
    m
}

/// A repo where the objective ("how does checkout compute the total?") has
/// exactly one obviously-relevant file among several distractors — the
/// signal a fake model (which ignores context) can't fake, but a real
/// search retrieval must actually find.
fn fixture() -> valyria_testkit::TempWorkspace {
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write(
        "src/checkout.rs",
        "//! Checkout total computation.\n\
         \n\
         /// Sum line items and apply the discount code, if any.\n\
         pub fn compute_total(items: &[f64], discount_pct: f64) -> f64 {\n\
         \x20   let subtotal: f64 = items.iter().sum();\n\
         \x20   subtotal * (1.0 - discount_pct / 100.0)\n\
         }\n",
    )
    .write(
        "src/greeter.rs",
        "//! Says hello. Unrelated to checkout.\n\
         pub fn greet(name: &str) -> String {\n\
         \x20   format!(\"hello, {name}\")\n\
         }\n",
    )
    .write(
        "src/weather.rs",
        "//! Fetches a weather report. Also unrelated.\n\
         pub fn report() -> &'static str {\n\
         \x20   \"sunny\"\n\
         }\n",
    );
    ws
}

/// Bootstraps a real index/graph/embed stack over `ws` (the same recipe
/// `valyria-context/tests/search_retrieval.rs` uses) and wraps it as the
/// `LiveRetriever::Search` a real `Runtime::open` would build once an
/// index generation exists.
async fn live_retriever(ws: &valyria_testkit::TempWorkspace, store: Arc<Store>) -> LiveRetriever {
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

    let engine = SearchEngine::new(
        ws.path().to_path_buf(),
        index.clone(),
        graph,
        embed,
        Arc::new(HashingEmbedder::default()),
        LanguageRegistry::with_builtin_languages().unwrap(),
    );
    LiveRetriever::Search(SearchRetriever::new(engine, index))
}

#[tokio::test]
async fn a_real_search_retriever_pulls_the_relevant_file_into_context_retrieved() {
    let ws = fixture();
    let store = Arc::new(Store::open_in_memory(&migrations()).unwrap());
    let events = Arc::new(EventBus::new(store.clone()));
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::at_millis(1_000_000));
    let tasks = Arc::new(TaskManager::new(
        store.clone(),
        events.clone(),
        clock.clone(),
    ));

    let root = WorkspaceRoot::new(ws.path()).unwrap();
    let blob_dir = tempfile::tempdir().unwrap();
    let ledger = Arc::new(Ledger::new(blob_dir.path()).unwrap());
    let engine = Arc::new(PermissionEngine::new(
        PermissionMode::Assisted,
        clock.clone(),
    ));
    let tool_runtime = Arc::new(ToolRuntime::new(
        valyria_tools::all_tools(),
        engine.clone(),
        clock.clone(),
    ));

    let orch = RoleRouter::new();
    orch.bind_single(
        Role::PrimaryCoder,
        "fake",
        Arc::new(FakeModelRuntime::from_scenario(Scenario {
            name: "noop".into(),
            turns: vec![ScriptedTurn::Finish {
                summary: "looked at it".into(),
            }],
        })),
    );

    let context = Arc::new(ContextAssembler::new(tool_runtime.clone()));
    let verification_log = Arc::new(VerificationLog::new(store.clone()));
    let plan_store = Arc::new(valyria_plan::PlanStore::new(store.clone()));
    let launcher: Arc<dyn ProcessLauncher> = Arc::from(detect_platform_launcher());
    let sandbox_profile = SandboxProfile::new().allow_write(root.as_path());

    let retriever = live_retriever(&ws, store.clone()).await;

    let driver = AgentDriver::new(
        tasks.clone(),
        tool_runtime,
        Arc::new(orch),
        context,
        ledger,
        engine,
        verification_log,
        plan_store,
        root,
        Arc::new(HashCache::new()),
        clock,
        launcher,
        sandbox_profile,
    )
    .with_retriever(retriever);

    let task = tasks
        .create(
            WorkspaceId::new(),
            "how does checkout compute the total, including the discount?".into(),
            Budget::default(),
        )
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    let events_seen = events.replay_since(Seq::ZERO).await.unwrap();
    let context_events: Vec<_> = events_seen
        .iter()
        .filter(|e| e.kind == EventKind::ContextRetrieved)
        .collect();
    assert!(
        !context_events.is_empty(),
        "expected at least one context_retrieved event, got {events_seen:?}"
    );

    let paths_seen: Vec<String> = context_events
        .iter()
        .flat_map(|e| e.payload["items"].as_array().cloned().unwrap_or_default())
        .filter_map(|item| item["path"].as_str().map(str::to_string))
        .collect();

    assert!(
        paths_seen.iter().any(|p| p.contains("checkout")),
        "expected src/checkout.rs among the retrieved items, got {paths_seen:?}"
    );
}
