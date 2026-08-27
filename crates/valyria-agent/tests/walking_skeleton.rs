//! In-process integration tests proving Phase 3's walking-skeleton
//! exit criterion at the driver level: a full run against the fake model
//! completes end to end, journal replay after "reopening" the store is
//! byte-identical, and a task interrupted mid-scenario resumes and
//! completes from the correct point. The real OS-level `kill -9` variant
//! of this (an actual child process, an actual SIGKILL) lives in
//! `valyria-cli/tests`, once the CLI exists to drive.

use std::sync::Arc;
use std::time::Duration;

use valyria_agent::AgentDriver;
use valyria_context::ContextAssembler;
use valyria_events::{Delivery, EventBus, EventKind, Seq};
use valyria_ledger::Ledger;
use valyria_orchestrator::{Orchestrator, Role};
use valyria_permissions::PermissionEngine;
use valyria_runtime_fake::{FakeModelRuntime, Scenario};
use valyria_sandbox::{detect_platform_launcher, ProcessLauncher, SandboxProfile};
use valyria_store::{Migration, Store};
use valyria_task::{Budget, TaskManager};
use valyria_tools::ToolRuntime;
use valyria_types::{AgentState, PermissionMode, WorkspaceId};
use valyria_util::{CancellationToken, Clock, FixedClock};
use valyria_vfs::{HashCache, WorkspaceRoot};

const FIXTURE_LIB_RS: &str = "pub fn existing(a: i32) -> i32 {\n    a\n}\n";

fn combined_migrations() -> Vec<Migration> {
    let mut migrations: Vec<Migration> = valyria_events::MIGRATIONS.to_vec();
    migrations.extend(valyria_task::MIGRATIONS.iter().copied());
    migrations
}

/// Everything long-lived enough to survive across a simulated "restart":
/// the on-disk-shaped store, event bus, and workspace. A fresh
/// `TaskManager`/`AgentDriver` pair can always be rebuilt from just these.
struct Backing {
    store: Arc<Store>,
    events: Arc<EventBus>,
    ws: valyria_testkit::TempWorkspace,
    blob_dir: tempfile::TempDir,
}

fn backing() -> Backing {
    let store = Arc::new(Store::open_in_memory(&combined_migrations()).unwrap());
    let events = Arc::new(EventBus::new(store.clone()));
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write("src/lib.rs", FIXTURE_LIB_RS);
    let blob_dir = tempfile::tempdir().unwrap();
    Backing {
        store,
        events,
        ws,
        blob_dir,
    }
}

fn build_driver(
    backing: &Backing,
    scenario: Scenario,
    mode: PermissionMode,
) -> (Arc<TaskManager>, AgentDriver) {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::at_millis(1_000_000));
    let tasks = Arc::new(TaskManager::new(
        backing.store.clone(),
        backing.events.clone(),
        clock.clone(),
    ));

    let root = WorkspaceRoot::new(backing.ws.path()).unwrap();
    let ledger = Arc::new(Ledger::new(backing.blob_dir.path()).unwrap());
    let engine = Arc::new(PermissionEngine::new(mode, clock.clone()));
    let tool_runtime = Arc::new(ToolRuntime::new(
        valyria_tools::all_tools(),
        engine.clone(),
        clock.clone(),
    ));

    let mut orch = Orchestrator::new();
    orch.bind(
        Role::PrimaryCoder,
        Arc::new(FakeModelRuntime::from_scenario(scenario)),
    );
    let orchestrator = Arc::new(orch);

    let context = Arc::new(ContextAssembler::new(tool_runtime.clone()));
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
        root,
        hash_cache,
        clock,
        launcher,
        sandbox_profile,
    );

    (tasks, driver)
}

fn fixture_content(backing: &Backing) -> String {
    std::fs::read_to_string(backing.ws.full_path("src/lib.rs")).unwrap()
}

async fn wait_for(sub: &mut valyria_events::Subscription, events: &EventBus, kind: EventKind) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match sub.recv().await.unwrap() {
                Delivery::Event(env) if env.kind == kind => return,
                Delivery::Lagged { resume_from } => {
                    *sub = events.subscribe_since(resume_from).await.unwrap();
                }
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for event")
}

#[tokio::test]
async fn full_scenario_completes_end_to_end_and_edits_the_file() {
    let backing = backing();
    let (tasks, driver) = build_driver(
        &backing,
        Scenario::default_walking_skeleton(),
        PermissionMode::Assisted,
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

    let final_task = tasks.get(task.id).await.unwrap();
    assert_eq!(final_task.state, AgentState::Completed);

    let events = backing.events.replay_since(Seq::ZERO).await.unwrap();
    let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
    for expected in [
        EventKind::TaskStarted,
        EventKind::StateChanged,
        EventKind::ToolStarted,
        EventKind::ToolCompleted,
        EventKind::TaskCompleted,
    ] {
        assert!(
            kinds.contains(&expected),
            "missing {expected:?} in {kinds:?}"
        );
    }

    let content = fixture_content(&backing);
    assert!(
        content.contains("pub fn add(a: i32, b: i32) -> i32"),
        "{content}"
    );
    assert!(
        content.contains("pub fn existing"),
        "original content should remain: {content}"
    );
}

#[tokio::test]
async fn journal_replay_after_rebuilding_the_driver_is_byte_identical() {
    let backing = backing();
    let (tasks, driver) = build_driver(
        &backing,
        Scenario::default_walking_skeleton(),
        PermissionMode::Assisted,
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

    let journal_before = tasks
        .journal_since(task.id, valyria_task::JournalSeq::ZERO)
        .await
        .unwrap();

    drop(driver);
    drop(tasks);

    // "Reopen" against the same durable store/event bus: a completely
    // fresh TaskManager, holding no state the old one accumulated.
    let (tasks2, _driver2) = build_driver(
        &backing,
        Scenario::default_walking_skeleton(),
        PermissionMode::Assisted,
    );
    let recovered = tasks2.recover_incomplete_tasks().await.unwrap();
    assert!(
        recovered.is_empty(),
        "an already-terminal task needs no recovery"
    );

    let journal_after = tasks2
        .journal_since(task.id, valyria_task::JournalSeq::ZERO)
        .await
        .unwrap();
    assert_eq!(journal_before, journal_after);
    assert_eq!(
        tasks2.get(task.id).await.unwrap().state,
        AgentState::Completed
    );
}

#[tokio::test]
async fn resume_after_an_interruption_completes_the_remaining_turns_from_the_correct_index() {
    let backing = backing();
    let (tasks, driver) = build_driver(
        &backing,
        Scenario::default_walking_skeleton(),
        PermissionMode::Assisted,
    );

    let task = tasks
        .create(
            WorkspaceId::new(),
            "add a function".into(),
            Budget::default(),
        )
        .await
        .unwrap();

    // Subscribe to the live event stream so we can abort the driver right
    // after the first tool call completes (read_file) — simulating a
    // crash partway through the scenario, not a clean pause.
    let mut sub = backing.events.subscribe_since(Seq::ZERO).await.unwrap();
    let handle = tokio::spawn({
        let driver_task = task.id;
        async move { driver.run(driver_task, CancellationToken::new()).await }
    });

    wait_for(&mut sub, &backing.events, EventKind::ToolCompleted).await;

    // Simulate a crash: abort the driver task without letting it reach a
    // terminal or paused state cleanly.
    handle.abort();
    let _ = handle.await;

    let mid_flight = tasks.get(task.id).await.unwrap();
    assert!(
        !mid_flight.state.is_terminal(),
        "task should still be mid-flight: {:?}",
        mid_flight.state
    );

    // Fresh manager + driver against the same backing store, exactly as a
    // restarted process would build them.
    let (tasks2, driver2) = build_driver(
        &backing,
        Scenario::default_walking_skeleton(),
        PermissionMode::Assisted,
    );
    let recovered = tasks2.recover_incomplete_tasks().await.unwrap();
    assert_eq!(recovered, vec![task.id]);

    let paused = tasks2.get(task.id).await.unwrap();
    assert_eq!(paused.state, AgentState::Paused);
    assert!(paused.recovery_note.is_some());
    let resume_target = paused.paused_from.unwrap();

    // Explicit resume: transition back to where it was paused from, then
    // let the driver run to completion.
    tasks2.transition(task.id, resume_target).await.unwrap();
    driver2
        .run(task.id, CancellationToken::new())
        .await
        .unwrap();

    let done = tasks2.get(task.id).await.unwrap();
    assert_eq!(done.state, AgentState::Completed);

    // Correctness, not just "didn't crash": the scripted edit is an
    // exact_replacement keyed to the original anchor text, so if it had
    // been replayed twice the second attempt would fail to find its
    // anchor (already-edited content no longer matches) — the file must
    // contain the addition exactly once.
    let content = fixture_content(&backing);
    assert_eq!(
        content.matches("pub fn add(a: i32, b: i32)").count(),
        1,
        "{content}"
    );
}

// The two tests below request pause/cancel *before* starting `run()`,
// rather than racing a live spawned driver: `pending_signal` is durable and
// checked at the top of every loop iteration regardless of which iteration
// that is, so this deterministically exercises the exact same code path a
// mid-flight `valyria task pause <id>`/`cancel` would hit, without a timing
// race against how many (fast, in-memory) steps the fake-model scenario
// happens to complete before the request lands. The "still made real
// progress, then got interrupted" scenario is covered deterministically by
// `resume_after_an_interruption_...` (via task abort) and, for the real
// cross-process case with real wall-clock timing, by `valyria-cli/tests`.

#[tokio::test]
async fn durable_pause_request_is_honored_and_resume_continues_to_completion() {
    let backing = backing();
    let (tasks, driver) = build_driver(
        &backing,
        Scenario::default_walking_skeleton(),
        PermissionMode::Assisted,
    );

    let task = tasks
        .create(
            WorkspaceId::new(),
            "add a function".into(),
            Budget::default(),
        )
        .await
        .unwrap();

    // A durable, out-of-band pause request — exactly what a separate
    // `valyria task pause <id>` CLI process would issue.
    tasks.request_pause(task.id).await.unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    let paused = tasks.get(task.id).await.unwrap();
    assert_eq!(paused.state, AgentState::Paused);
    assert_eq!(paused.paused_from, Some(AgentState::Idle));

    tasks.transition(task.id, AgentState::Idle).await.unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();
    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::Completed
    );
}

#[tokio::test]
async fn durable_cancel_request_is_honored() {
    let backing = backing();
    let (tasks, driver) = build_driver(
        &backing,
        Scenario::default_walking_skeleton(),
        PermissionMode::Assisted,
    );

    let task = tasks
        .create(
            WorkspaceId::new(),
            "add a function".into(),
            Budget::default(),
        )
        .await
        .unwrap();

    tasks.request_cancel(task.id).await.unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::Cancelled
    );
}

#[tokio::test]
async fn in_process_cancellation_token_reaches_terminal_cancelled_state() {
    let backing = backing();
    let (tasks, driver) = build_driver(
        &backing,
        Scenario::default_walking_skeleton(),
        PermissionMode::Assisted,
    );

    let task = tasks
        .create(
            WorkspaceId::new(),
            "add a function".into(),
            Budget::default(),
        )
        .await
        .unwrap();

    let cancel = CancellationToken::new();
    cancel.cancel();
    driver.run(task.id, cancel).await.unwrap();

    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::Cancelled
    );
}

/// The permission Ask -> approve path (D2), using a command outside
/// `valyria_permissions`' `SAFE_PROGRAMS` allowlist so `Assisted` mode
/// actually asks instead of auto-allowing.
fn ask_scenario() -> Scenario {
    Scenario {
        name: "ask_then_finish".into(),
        turns: vec![
            valyria_runtime_fake::ScriptedTurn::ToolCall {
                name: "run_command".into(),
                arguments: serde_json::json!({"program": "some-unlisted-tool", "args": []}),
            },
            valyria_runtime_fake::ScriptedTurn::Finish {
                summary: "done".into(),
            },
        ],
    }
}

#[tokio::test]
async fn unresolved_permission_ask_pauses_the_driver_without_erroring() {
    let backing = backing();
    let (tasks, driver) = build_driver(&backing, ask_scenario(), PermissionMode::Assisted);

    let task = tasks
        .create(
            WorkspaceId::new(),
            "run something".into(),
            Budget::default(),
        )
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::WaitingForPermission
    );
}

#[tokio::test]
async fn resolving_a_permission_ask_lets_the_task_complete() {
    let backing = backing();
    let (tasks, driver) = build_driver(&backing, ask_scenario(), PermissionMode::Assisted);

    let task = tasks
        .create(
            WorkspaceId::new(),
            "run something".into(),
            Budget::default(),
        )
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();
    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::WaitingForPermission
    );

    driver.resolve_permission(task.id, true).await.unwrap();
    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::Implementing
    );

    driver.run(task.id, CancellationToken::new()).await.unwrap();
    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::Completed
    );
}

#[tokio::test]
async fn denying_a_permission_ask_fails_the_task() {
    let backing = backing();
    let (tasks, driver) = build_driver(&backing, ask_scenario(), PermissionMode::Assisted);

    let task = tasks
        .create(
            WorkspaceId::new(),
            "run something".into(),
            Budget::default(),
        )
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    driver.resolve_permission(task.id, false).await.unwrap();
    assert_eq!(tasks.get(task.id).await.unwrap().state, AgentState::Failed);
}
