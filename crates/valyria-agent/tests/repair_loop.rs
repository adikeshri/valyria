//! Phase 7 exit criteria at the driver level: a seeded bug is verified,
//! diagnosed and repaired end to end by the fake model, and an unfixable
//! bug trips loop detection and is handed off rather than spun on
//! forever.
//!
//! Verification here is driver-initiated — the fixture ships a `verify.sh`
//! that `valyria-verify::discovery` finds as a `Convention` command, so
//! `Verifying` runs it itself; the model's only job is the repair edit.

use std::sync::Arc;

use valyria_agent::AgentDriver;
use valyria_context::ContextAssembler;
use valyria_events::{EventBus, EventKind, Seq};
use valyria_ledger::Ledger;
use valyria_orchestrator::{Orchestrator, Role};
use valyria_permissions::PermissionEngine;
use valyria_runtime_fake::{FakeModelRuntime, Scenario, ScriptedTurn};
use valyria_sandbox::{detect_platform_launcher, ProcessLauncher, SandboxProfile};
use valyria_store::{Migration, Store};
use valyria_task::{Budget, TaskManager};
use valyria_tools::ToolRuntime;
use valyria_types::{AgentState, PermissionMode, WorkspaceId};
use valyria_util::{CancellationToken, Clock, FixedClock};
use valyria_verify::{CompletionReport, ReportStatus, VerificationLog};
use valyria_vfs::{HashCache, WorkspaceRoot};

fn migrations() -> Vec<Migration> {
    let mut m: Vec<Migration> = valyria_events::MIGRATIONS.to_vec();
    m.extend(valyria_task::MIGRATIONS.iter().copied());
    m.extend(valyria_verify::MIGRATIONS.iter().copied());
    m.extend(valyria_plan::MIGRATIONS.iter().copied());
    m
}

struct Backing {
    store: Arc<Store>,
    events: Arc<EventBus>,
    ws: valyria_testkit::TempWorkspace,
    _blob_dir: tempfile::TempDir,
}

/// A workspace whose only verification command is a `verify.sh` that
/// passes iff `src/config.txt` contains `ANSWER=42`.
fn seeded_bug_workspace() -> Backing {
    let store = Arc::new(Store::open_in_memory(&migrations()).unwrap());
    let events = Arc::new(EventBus::new(store.clone()));
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write("src/config.txt", "ANSWER=0\n");
    ws.write(
        "verify.sh",
        "#!/bin/sh\ngrep -q 'ANSWER=42' src/config.txt\n",
    );
    let blob_dir = tempfile::tempdir().unwrap();
    Backing {
        store,
        events,
        ws,
        _blob_dir: blob_dir,
    }
}

fn build_driver(backing: &Backing, scenario: Scenario) -> (Arc<TaskManager>, AgentDriver) {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::at_millis(1_000_000));
    let tasks = Arc::new(TaskManager::new(
        backing.store.clone(),
        backing.events.clone(),
        clock.clone(),
    ));
    let root = WorkspaceRoot::new(backing.ws.path()).unwrap();
    let ledger = Arc::new(Ledger::new(backing._blob_dir.path()).unwrap());
    let engine = Arc::new(PermissionEngine::new(
        PermissionMode::Assisted,
        clock.clone(),
    ));
    let tools = Arc::new(ToolRuntime::new(
        valyria_tools::all_tools(),
        engine.clone(),
        clock.clone(),
    ));
    let mut orch = Orchestrator::new();
    orch.bind(
        Role::PrimaryCoder,
        Arc::new(FakeModelRuntime::from_scenario(scenario)),
    );
    let context = Arc::new(ContextAssembler::new(tools.clone()));
    let verification_log = Arc::new(VerificationLog::new(backing.store.clone()));
    let plan_store = Arc::new(valyria_plan::PlanStore::new(backing.store.clone()));
    let launcher: Arc<dyn ProcessLauncher> = Arc::from(detect_platform_launcher());
    let sandbox_profile = SandboxProfile::new().allow_write(root.as_path());

    let driver = AgentDriver::new(
        tasks.clone(),
        tools,
        Arc::new(orch),
        context,
        ledger,
        engine,
        verification_log.clone(),
        plan_store,
        root,
        Arc::new(HashCache::new()),
        clock,
        launcher,
        sandbox_profile,
    );
    (tasks, driver)
}

fn edit_config_turn(from: &str, to: &str) -> ScriptedTurn {
    ScriptedTurn::ToolCall {
        name: "edit_file".into(),
        arguments: serde_json::json!({
            "path": "src/config.txt",
            "precondition": "any",
            "strategy": {
                "type": "exact_replacement",
                "anchor": from,
                "replacement": to,
            }
        }),
    }
}

fn config_contents(b: &Backing) -> String {
    std::fs::read_to_string(b.ws.full_path("src/config.txt")).unwrap()
}

async fn kinds_of(b: &Backing) -> Vec<EventKind> {
    b.events
        .replay_since(Seq::ZERO)
        .await
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect()
}

#[tokio::test]
async fn seeded_bug_is_verified_diagnosed_and_repaired_end_to_end() {
    let backing = seeded_bug_workspace();
    // turn 0: finish without fixing → drives into Verifying (fails).
    // turn 1: the repair edit.  turn 2: finish.
    let scenario = Scenario {
        name: "repair".into(),
        turns: vec![
            ScriptedTurn::Finish {
                summary: "done (but it isn't)".into(),
            },
            edit_config_turn("ANSWER=0\n", "ANSWER=42\n"),
            ScriptedTurn::Finish {
                summary: "fixed the answer".into(),
            },
        ],
    };
    let (tasks, driver) = build_driver(&backing, scenario);

    let task = tasks
        .create(
            WorkspaceId::new(),
            "set the answer to 42".into(),
            Budget::default(),
        )
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    let final_task = tasks.get(task.id).await.unwrap();
    assert_eq!(
        final_task.state,
        AgentState::Completed,
        "expected COMPLETED, journal-visible state was {:?}",
        final_task.state
    );

    assert_eq!(config_contents(&backing), "ANSWER=42\n");

    let kinds = kinds_of(&backing).await;
    for expected in [
        EventKind::TestStarted,
        EventKind::TestFailed,
        EventKind::TestPassed,
        EventKind::VerificationEvidence,
    ] {
        assert!(
            kinds.contains(&expected),
            "missing {expected:?} in {kinds:?}"
        );
    }

    // Two runs recorded: the first failing, the last passing.
    let log = VerificationLog::new(backing.store.clone());
    let runs = log.list_for_task(task.id).await.unwrap();
    assert!(
        runs.len() >= 2,
        "expected ≥2 verification runs, got {}",
        runs.len()
    );
    assert!(!runs.first().unwrap().passed());
    assert!(runs.last().unwrap().passed());

    // The completion report — built only from those rows — says Verified.
    let report = CompletionReport::from_runs(task.id, &runs, &["tests pass".into()]);
    assert_eq!(report.status, ReportStatus::Verified, "{}", report.render());
}

#[tokio::test]
async fn an_unfixable_bug_trips_loop_detection_and_is_handed_off() {
    let backing = seeded_bug_workspace();
    // The model never actually edits anything — every turn just "finishes".
    let mut turns = vec![ScriptedTurn::Finish {
        summary: "looks fine to me".into(),
    }];
    for _ in 0..12 {
        turns.push(ScriptedTurn::Finish {
            summary: "still looks fine".into(),
        });
    }
    let (tasks, driver) = build_driver(
        &backing,
        Scenario {
            name: "stuck".into(),
            turns,
        },
    );

    let task = tasks
        .create(
            WorkspaceId::new(),
            "set the answer to 42".into(),
            Budget::default(),
        )
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    let final_task = tasks.get(task.id).await.unwrap();
    assert!(
        matches!(
            final_task.state,
            AgentState::WaitingForUser | AgentState::Failed
        ),
        "a non-converging repair loop must hand off, not spin — ended in {:?}",
        final_task.state
    );

    // Never fixed.
    assert_eq!(config_contents(&backing), "ANSWER=0\n");

    // The loop was detected and surfaced.
    let kinds = kinds_of(&backing).await;
    assert!(
        kinds.contains(&EventKind::ProgressStalled),
        "expected a ProgressStalled event in {kinds:?}"
    );

    // And it did not run away: a bounded number of verification runs.
    let log = VerificationLog::new(backing.store.clone());
    let runs = log.list_for_task(task.id).await.unwrap();
    assert!(
        (1..=12).contains(&runs.len()),
        "verification runs should be bounded, got {}",
        runs.len()
    );
    assert!(runs.iter().all(|r| !r.passed()));
}

#[tokio::test]
async fn a_workspace_with_no_tooling_completes_without_verifying() {
    // No verify.sh, no manifest — discovery finds nothing, Verifying is a
    // pass-through exactly as in Phase 3.
    let store = Arc::new(Store::open_in_memory(&migrations()).unwrap());
    let events = Arc::new(EventBus::new(store.clone()));
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write("src/lib.rs", "pub fn x() {}\n");
    let blob_dir = tempfile::tempdir().unwrap();
    let backing = Backing {
        store,
        events,
        ws,
        _blob_dir: blob_dir,
    };
    let scenario = Scenario {
        name: "noop".into(),
        turns: vec![ScriptedTurn::Finish {
            summary: "nothing to do".into(),
        }],
    };
    let (tasks, driver) = build_driver(&backing, scenario);
    let task = tasks
        .create(WorkspaceId::new(), "do nothing".into(), Budget::default())
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::Completed
    );
    let log = VerificationLog::new(backing.store.clone());
    assert!(log.list_for_task(task.id).await.unwrap().is_empty());
}
