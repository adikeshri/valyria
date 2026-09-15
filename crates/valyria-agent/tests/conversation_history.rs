//! Proves the live agent loop actually sends a real system prompt, tool
//! schemas, and multi-turn tool-result history to the model — not just the
//! bare task objective every turn. Captures the exact `GenerateRequest`
//! each turn sends (mirroring `valyria-orchestrator/tests/generate_action.rs`'s
//! `CountingRuntime`, since `FakeModelRuntime` itself discards the request).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::json;
use valyria_agent::AgentDriver;
use valyria_context::ContextAssembler;
use valyria_events::EventBus;
use valyria_ledger::Ledger;
use valyria_model::{
    Capabilities, Chunk, Completion, GenerateRequest, Health, ModelError, ModelRuntime,
    Role as MessageRole,
};
use valyria_orchestrator::{Role, RoleRouter};
use valyria_permissions::PermissionEngine;
use valyria_runtime_fake::{FakeModelRuntime, Scenario, ScriptedTurn};
use valyria_sandbox::{detect_platform_launcher, ProcessLauncher, SandboxProfile};
use valyria_store::{Migration, Store};
use valyria_task::{Budget, TaskManager};
use valyria_tools::ToolRuntime;
use valyria_types::{AgentState, PermissionMode, WorkspaceId};
use valyria_util::{CancellationToken, Clock, FixedClock};
use valyria_verify::VerificationLog;
use valyria_vfs::{HashCache, WorkspaceRoot};

fn combined_migrations() -> Vec<Migration> {
    let mut migrations: Vec<Migration> = valyria_events::MIGRATIONS.to_vec();
    migrations.extend(valyria_task::MIGRATIONS.iter().copied());
    migrations.extend(valyria_verify::MIGRATIONS.iter().copied());
    migrations.extend(valyria_plan::MIGRATIONS.iter().copied());
    migrations
}

/// Wraps `FakeModelRuntime` and records every `GenerateRequest` it's asked
/// to serve, in order — the only way to inspect what the driver actually
/// sent, since the fake itself decides purely from `turn_hint`.
struct CapturingRuntime {
    inner: FakeModelRuntime,
    seen: Arc<Mutex<Vec<GenerateRequest>>>,
}

#[async_trait]
impl ModelRuntime for CapturingRuntime {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    async fn health(&self) -> Health {
        self.inner.health().await
    }
    fn count_tokens(&self, text: &str) -> usize {
        self.inner.count_tokens(text)
    }
    async fn generate(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> Result<Completion, ModelError> {
        self.seen.lock().unwrap().push(req.clone());
        self.inner.generate(req, cancel).await
    }
    fn stream(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<Chunk, ModelError>> {
        self.inner.stream(req, cancel)
    }
}

#[tokio::test]
async fn implementing_turns_carry_a_system_prompt_tools_and_replay_tool_history() {
    let store = Arc::new(Store::open_in_memory(&combined_migrations()).unwrap());
    let events = Arc::new(EventBus::new(store.clone()));
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write("src/lib.rs", "pub fn existing(a: i32) -> i32 {\n    a\n}\n");
    let blob_dir = tempfile::tempdir().unwrap();

    let clock: Arc<dyn Clock> = Arc::new(FixedClock::at_millis(1_000_000));
    let tasks = Arc::new(TaskManager::new(
        store.clone(),
        events.clone(),
        clock.clone(),
    ));

    let root = WorkspaceRoot::new(ws.path()).unwrap();
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

    let seen = Arc::new(Mutex::new(Vec::new()));
    let capturing = CapturingRuntime {
        inner: FakeModelRuntime::from_scenario(Scenario {
            name: "conversation_history".into(),
            turns: vec![
                ScriptedTurn::ToolCall {
                    name: "read_file".into(),
                    arguments: json!({"path": "src/lib.rs"}),
                },
                ScriptedTurn::Finish {
                    summary: "done".into(),
                },
            ],
        }),
        seen: seen.clone(),
    };
    let orch = RoleRouter::new();
    orch.bind_single(Role::PrimaryCoder, "fake", Arc::new(capturing));
    let orchestrator = Arc::new(orch);

    let context = Arc::new(ContextAssembler::new(tool_runtime.clone()));
    let verification_log = Arc::new(VerificationLog::new(store.clone()));
    let plan_store = Arc::new(valyria_plan::PlanStore::new(store.clone()));
    let hash_cache = Arc::new(HashCache::new());
    let launcher: Arc<dyn ProcessLauncher> = Arc::from(detect_platform_launcher());
    let sandbox_profile = SandboxProfile::new().allow_write(root.as_path());

    let driver = AgentDriver::new(
        tasks.clone(),
        tool_runtime,
        orchestrator,
        context,
        ledger,
        engine,
        verification_log,
        plan_store,
        root,
        hash_cache,
        clock,
        launcher,
        sandbox_profile,
    );

    let task = tasks
        .create(
            WorkspaceId::new(),
            "add a function".into(),
            Budget::default(),
        )
        .await
        .unwrap();

    driver.run(task.id, CancellationToken::new()).await.unwrap();
    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::Completed
    );

    let requests = seen.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "one implementing turn per scripted model call"
    );

    // Turn 1: system prompt + task objective, tools bound, no history yet.
    let first = &requests[0];
    assert!(
        first.messages.iter().any(|m| m.role == MessageRole::System),
        "first turn carries a system message: {:?}",
        first.messages
    );
    assert!(
        first
            .messages
            .iter()
            .any(|m| m.role == MessageRole::User && m.content.contains("add a function")),
        "first turn's user message carries the objective: {:?}",
        first.messages
    );
    assert!(!first.tools.is_empty(), "tools are bound to the request");
    assert!(first.tools.iter().any(|t| t.name == "read_file"));
    // M2: search/symbol_search are real now and offered to the model;
    // git_blame is still the one genuinely not-yet-implemented tool
    // (valyria-git has no blame implementation at all — see
    // docs/COMPLETION-PLAN.md).
    assert!(first.tools.iter().any(|t| t.name == "search"));
    assert!(first.tools.iter().any(|t| t.name == "symbol_search"));
    assert!(
        !first.tools.iter().any(|t| t.name == "git_blame"),
        "not-yet-implemented tools are excluded: {:?}",
        first.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    // Turn 2: the read_file call and its result are replayed as history,
    // on top of the same system prompt.
    let second = &requests[1];
    assert!(second
        .messages
        .iter()
        .any(|m| m.role == MessageRole::System));
    assert!(
        second
            .messages
            .iter()
            .any(|m| m.role == MessageRole::Assistant && m.content.contains("read_file")),
        "second turn replays the tool-call turn: {:?}",
        second.messages
    );
    assert!(
        second
            .messages
            .iter()
            .any(|m| m.role == MessageRole::Tool && m.tool_call_id.is_some()),
        "second turn replays the tool result: {:?}",
        second.messages
    );
    assert!(
        !second.tools.is_empty(),
        "tools stay bound on later turns too"
    );
}
