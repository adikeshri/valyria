//! Phase 8 exit criteria at the driver level:
//!
//! 1. an invalid model-authored plan is rejected with structured feedback
//!    and repaired on the next turn;
//! 2. a multi-step plan executes step by step to completion, taking a
//!    checkpoint at a rollback boundary;
//! 3. rolling back to a checkpoint restores the tree exactly, and refuses
//!    when a file has been touched since.
//!
//! The cross-process `kill -9` + resume half of criterion 2 lives in
//! `valyria-cli/tests/walking_skeleton.rs`.

use std::sync::Arc;

use valyria_agent::{AgentDriver, PlanningMode};
use valyria_context::ContextAssembler;
use valyria_events::{EventBus, EventKind, Seq};
use valyria_ledger::Ledger;
use valyria_orchestrator::{Orchestrator, Role};
use valyria_permissions::PermissionEngine;
use valyria_plan::{PlanStore, RollbackError};
use valyria_runtime_fake::{FakeModelRuntime, Scenario, ScriptedTurn};
use valyria_sandbox::{detect_platform_launcher, ProcessLauncher, SandboxProfile};
use valyria_store::{Migration, Store};
use valyria_task::{kinds, Budget, JournalEntryKind, JournalSeq, TaskManager};
use valyria_tools::ToolRuntime;
use valyria_types::{AgentState, PermissionMode, WorkspaceId};
use valyria_util::{CancellationToken, Clock, FixedClock};
use valyria_verify::VerificationLog;
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

/// A workspace with a couple of plain text files and **no** build tooling,
/// so the final `Verifying` pass is a no-op pass-through (exactly as in
/// `repair_loop::a_workspace_with_no_tooling_completes_without_verifying`).
fn workspace() -> Backing {
    let store = Arc::new(Store::open_in_memory(&migrations()).unwrap());
    let events = Arc::new(EventBus::new(store.clone()));
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write("src/a.txt", "a0\n");
    ws.write("src/b.txt", "b0\n");
    let blob_dir = tempfile::tempdir().unwrap();
    Backing {
        store,
        events,
        ws,
        _blob_dir: blob_dir,
    }
}

fn build_driver(
    backing: &Backing,
    scenario: Scenario,
) -> (Arc<TaskManager>, Arc<PlanStore>, AgentDriver) {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::at_millis(1_000_000));
    let tasks = Arc::new(TaskManager::new(
        backing.store.clone(),
        backing.events.clone(),
        clock.clone(),
    ));
    let root = WorkspaceRoot::new(backing.ws.path()).unwrap();
    let ledger = Arc::new(Ledger::new(backing._blob_dir.path()).unwrap());
    let engine = Arc::new(PermissionEngine::new(
        PermissionMode::Autonomous,
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
    let plan_store = Arc::new(PlanStore::new(backing.store.clone()));
    let launcher: Arc<dyn ProcessLauncher> = Arc::from(detect_platform_launcher());
    let sandbox_profile = SandboxProfile::new().allow_write(root.as_path());

    let driver = AgentDriver::new(
        tasks.clone(),
        tools,
        Arc::new(orch),
        context,
        ledger,
        engine,
        verification_log,
        plan_store.clone(),
        root,
        Arc::new(HashCache::new()),
        clock,
        launcher,
        sandbox_profile,
    )
    .with_planning_mode(PlanningMode::ModelAuthored);
    (tasks, plan_store, driver)
}

fn submit_plan(plan: serde_json::Value) -> ScriptedTurn {
    ScriptedTurn::ToolCall {
        name: "submit_plan".into(),
        arguments: plan,
    }
}

fn edit_turn(path: &str, from: &str, to: &str) -> ScriptedTurn {
    ScriptedTurn::ToolCall {
        name: "edit_file".into(),
        arguments: serde_json::json!({
            "path": path,
            "precondition": "any",
            "strategy": {
                "type": "exact_replacement",
                "anchor": from,
                "replacement": to,
            }
        }),
    }
}

fn read(b: &Backing, path: &str) -> String {
    std::fs::read_to_string(b.ws.full_path(path)).unwrap()
}

async fn journal_outcomes(tasks: &TaskManager, task: valyria_types::TaskId) -> Vec<String> {
    tasks
        .journal_since(task, JournalSeq::ZERO)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            JournalEntryKind::EffectCompleted { outcome_kind, .. } => Some(outcome_kind),
            JournalEntryKind::EffectIssued { effect_kind, .. } => Some(effect_kind),
            _ => None,
        })
        .collect()
}

async fn journal_payloads(
    tasks: &TaskManager,
    task: valyria_types::TaskId,
    kind: &str,
) -> Vec<serde_json::Value> {
    tasks
        .journal_since(task, JournalSeq::ZERO)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            JournalEntryKind::EffectCompleted {
                outcome_kind,
                payload,
                ..
            } if outcome_kind == kind => Some(payload),
            JournalEntryKind::EffectIssued {
                effect_kind,
                payload,
                ..
            } if effect_kind == kind => Some(payload),
            _ => None,
        })
        .collect()
}

async fn event_kinds(b: &Backing) -> Vec<EventKind> {
    b.events
        .replay_since(Seq::ZERO)
        .await
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect()
}

#[tokio::test]
async fn invalid_plan_is_rejected_with_structured_feedback_and_repaired() {
    let backing = workspace();
    let cyclic = serde_json::json!({
        "plan_scope": [],
        "steps": [
            {"id": "s1", "intent": "first", "depends_on": ["s2"]},
            {"id": "s2", "intent": "second", "depends_on": ["s1"]},
        ]
    });
    let good = serde_json::json!({
        "plan_scope": ["src/"],
        "steps": [
            {"id": "only", "intent": "set a to a1", "targets": ["src/a.txt"],
             "verification": {"mode": "inherit"}}
        ]
    });
    let scenario = Scenario {
        name: "plan-repair".into(),
        turns: vec![
            submit_plan(cyclic),                    // turn 0: rejected
            submit_plan(good),                      // turn 1: accepted
            edit_turn("src/a.txt", "a0\n", "a1\n"), // turn 2: step `only`
            ScriptedTurn::Finish {
                summary: "step done".into(),
            }, // turn 3
        ],
    };
    let (tasks, plan_store, driver) = build_driver(&backing, scenario);

    let task = tasks
        .create(WorkspaceId::new(), "set a to a1".into(), Budget::default())
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::Completed
    );
    assert_eq!(read(&backing, "src/a.txt"), "a1\n");

    // The rejection was recorded, with the machine code.
    let rejections = journal_payloads(&tasks, task.id, kinds::PLAN_REJECTED).await;
    assert_eq!(rejections.len(), 1, "expected exactly one rejection");
    let codes = rejections[0]["error_codes"].as_array().unwrap();
    assert!(
        codes.iter().any(|c| c == "cyclic_dependency"),
        "rejection should name the cycle: {codes:?}"
    );

    // Exactly one plan was accepted and persisted.
    assert!(event_kinds(&backing)
        .await
        .contains(&EventKind::PlanCreated));
    assert_eq!(plan_store.all_revisions(task.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_multi_step_plan_executes_step_by_step_with_a_checkpoint() {
    let backing = workspace();
    let plan = serde_json::json!({
        "plan_scope": ["src/"],
        "steps": [
            {"id": "edit_a", "intent": "set a", "targets": ["src/a.txt"],
             "verification": {"mode": "inherit"}},
            {"id": "edit_b", "intent": "set b", "targets": ["src/b.txt"],
             "verification": {"mode": "inherit"}, "depends_on": ["edit_a"],
             "checkpoint": true, "rollback_boundary": true},
        ]
    });
    let scenario = Scenario {
        name: "multi-step".into(),
        turns: vec![
            submit_plan(plan),                      // 0
            edit_turn("src/a.txt", "a0\n", "a1\n"), // 1: edit_a
            ScriptedTurn::Finish {
                summary: "a done".into(),
            }, // 2: edit_a
            edit_turn("src/b.txt", "b0\n", "b1\n"), // 3: edit_b
            ScriptedTurn::Finish {
                summary: "b done".into(),
            }, // 4: edit_b
        ],
    };
    let (tasks, plan_store, driver) = build_driver(&backing, scenario);
    let task = tasks
        .create(
            WorkspaceId::new(),
            "edit a then b".into(),
            Budget::default(),
        )
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    assert_eq!(
        tasks.get(task.id).await.unwrap().state,
        AgentState::Completed
    );
    assert_eq!(read(&backing, "src/a.txt"), "a1\n");
    assert_eq!(read(&backing, "src/b.txt"), "b1\n");

    let outcomes = journal_outcomes(&tasks, task.id).await;
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| *o == kinds::PLAN_STEP_STARTED)
            .count(),
        2
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| *o == kinds::PLAN_STEP_COMPLETED)
            .count(),
        2
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| *o == kinds::PLAN_CHECKPOINT)
            .count(),
        1,
        "one checkpoint, for the rollback-boundary step"
    );
    assert_eq!(
        plan_store
            .checkpoints_for_task(task.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

fn rollback_plan() -> serde_json::Value {
    serde_json::json!({
        "plan_scope": ["src/"],
        "steps": [
            {"id": "edit_a", "intent": "set a", "targets": ["src/a.txt"],
             "verification": {"mode": "inherit"}},
            {"id": "edit_b", "intent": "set b", "targets": ["src/b.txt"],
             "verification": {"mode": "inherit"}, "depends_on": ["edit_a"],
             "checkpoint": true, "rollback_boundary": true},
        ]
    })
}

fn rollback_scenario() -> Scenario {
    Scenario {
        name: "rollback".into(),
        turns: vec![
            submit_plan(rollback_plan()),
            edit_turn("src/a.txt", "a0\n", "a1\n"),
            ScriptedTurn::Finish {
                summary: "a".into(),
            },
            edit_turn("src/b.txt", "b0\n", "b1\n"),
            ScriptedTurn::Finish {
                summary: "b".into(),
            },
        ],
    }
}

#[tokio::test]
async fn rollback_to_a_checkpoint_restores_the_tree_exactly() {
    let backing = workspace();
    let (tasks, plan_store, driver) = build_driver(&backing, rollback_scenario());
    let task = tasks
        .create(
            WorkspaceId::new(),
            "edit a then b".into(),
            Budget::default(),
        )
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();
    assert_eq!(read(&backing, "src/b.txt"), "b1\n");

    let cp = plan_store.checkpoints_for_task(task.id).await.unwrap()[0].clone();
    let report = driver
        .rollback_to_checkpoint(task.id, cp.id)
        .await
        .expect("rollback should succeed");

    // b was edited after the checkpoint → reverted to its checkpoint state.
    assert_eq!(read(&backing, "src/b.txt"), "b0\n");
    // a was edited before the checkpoint → left exactly as it was.
    assert_eq!(read(&backing, "src/a.txt"), "a1\n");
    assert!(report.reverted.iter().any(|p| p.ends_with("b.txt")));

    assert!(event_kinds(&backing)
        .await
        .contains(&EventKind::FileChanged));
}

#[tokio::test]
async fn rollback_refuses_and_leaves_the_tree_alone_when_a_file_was_touched_since() {
    let backing = workspace();
    let (tasks, plan_store, driver) = build_driver(&backing, rollback_scenario());
    let task = tasks
        .create(
            WorkspaceId::new(),
            "edit a then b".into(),
            Budget::default(),
        )
        .await
        .unwrap();
    driver.run(task.id, CancellationToken::new()).await.unwrap();

    // A human edits b.txt out of band, with no ledger entry.
    std::fs::write(backing.ws.full_path("src/b.txt"), "HUMAN EDIT\n").unwrap();

    let cp = plan_store.checkpoints_for_task(task.id).await.unwrap()[0].clone();
    let err = driver
        .rollback_to_checkpoint(task.id, cp.id)
        .await
        .expect_err("rollback must refuse");
    assert!(
        matches!(err, RollbackError::UserModified { ref path } if path.ends_with("b.txt")),
        "expected UserModified for b.txt, got {err:?}"
    );

    // Nothing was rolled back.
    assert_eq!(read(&backing, "src/b.txt"), "HUMAN EDIT\n");
    assert_eq!(read(&backing, "src/a.txt"), "a1\n");
}
