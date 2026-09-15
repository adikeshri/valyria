//! M5 exit criteria at the driver level: the Researcher → Planner →
//! Implementer → Tester → Reviewer role pipeline runs end to end against
//! four independently-scripted fake models (one per role, proving the
//! roles are genuinely routed to their own model binding, not sharing
//! one), produces a real file edit, and persists a typed `Artifact` for
//! every role plus child tasks linked back to the coordinator.

use std::sync::Arc;

use valyria_agent::AgentDriver;
use valyria_context::ContextAssembler;
use valyria_events::EventBus;
use valyria_ledger::Ledger;
use valyria_orchestrator::{Role, RoleRouter};
use valyria_permissions::PermissionEngine;
use valyria_plan::{AgentRole, Artifact, PlanStore};
use valyria_runtime_fake::{FakeModelRuntime, Scenario, ScriptedTurn};
use valyria_sandbox::{detect_platform_launcher, ProcessLauncher, SandboxProfile};
use valyria_store::{Migration, Store};
use valyria_task::{Budget, TaskManager};
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

/// No build tooling at all — the mandatory full verification pass (run
/// once, inside the Implementer child, as part of its own ordinary
/// Verifying state) is a no-op pass-through, exactly as
/// `repair_loop::a_workspace_with_no_tooling_completes_without_verifying`.
/// Keeps this test about the role-pipeline orchestration, not verification
/// machinery already proven elsewhere.
fn workspace() -> Backing {
    let store = Arc::new(Store::open_in_memory(&migrations()).unwrap());
    let events = Arc::new(EventBus::new(store.clone()));
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write("src/a.txt", "a0\n");
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
    researcher: Scenario,
    planner: Scenario,
    implementer: Scenario,
    reviewer: Scenario,
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
    let orch = RoleRouter::new();
    // Four distinct bindings, one per pipeline role — proves each role is
    // actually routed to its own model, not sharing a single one (the
    // scripts below would fail turn-index lookups immediately if they
    // were).
    orch.bind_single(
        Role::FastCoder,
        "fake-researcher",
        Arc::new(FakeModelRuntime::from_scenario(researcher)),
    );
    orch.bind_single(
        Role::Planner,
        "fake-planner",
        Arc::new(FakeModelRuntime::from_scenario(planner)),
    );
    orch.bind_single(
        Role::PrimaryCoder,
        "fake-implementer",
        Arc::new(FakeModelRuntime::from_scenario(implementer)),
    );
    orch.bind_single(
        Role::Reviewer,
        "fake-reviewer",
        Arc::new(FakeModelRuntime::from_scenario(reviewer)),
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
    );
    (tasks, plan_store, driver)
}

fn submit(name: &str, arguments: serde_json::Value) -> ScriptedTurn {
    ScriptedTurn::ToolCall {
        name: name.into(),
        arguments,
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

#[tokio::test]
async fn the_full_role_pipeline_runs_end_to_end_and_persists_every_artifact() {
    let backing = workspace();

    let researcher_scenario = Scenario {
        name: "researcher".into(),
        turns: vec![submit(
            "submit_research_brief",
            serde_json::json!({
                "summary": "src/a.txt holds the value to change",
                "relevant_files": ["src/a.txt"],
                "open_questions": [],
            }),
        )],
    };
    let planner_scenario = Scenario {
        name: "planner".into(),
        turns: vec![submit(
            "submit_plan",
            serde_json::json!({
                "plan_scope": ["src/"],
                "steps": [
                    {"id": "set_a", "intent": "set a to a1", "targets": ["src/a.txt"],
                     "verification": {"mode": "inherit"}}
                ]
            }),
        )],
    };
    let implementer_scenario = Scenario {
        name: "implementer".into(),
        turns: vec![
            edit_turn("src/a.txt", "a0\n", "a1\n"), // turn 0: the step's edit
            ScriptedTurn::Finish {
                summary: "step done".into(),
            }, // turn 1: completes the step
        ],
    };
    let reviewer_scenario = Scenario {
        name: "reviewer".into(),
        turns: vec![submit(
            "submit_review_findings",
            serde_json::json!({"approved": true, "findings": []}),
        )],
    };

    let (tasks, plan_store, driver) = build_driver(
        &backing,
        researcher_scenario,
        planner_scenario,
        implementer_scenario,
        reviewer_scenario,
    );

    let coordinator = tasks
        .create(WorkspaceId::new(), "set a to a1".into(), Budget::default())
        .await
        .unwrap();

    driver
        .run_role_pipeline(coordinator.id, &CancellationToken::new())
        .await
        .unwrap();

    // The coordinator itself converged to Completed — both the Tester
    // (no tooling, trivially passes) and Reviewer (scripted `approved:
    // true`) signed off.
    assert_eq!(
        tasks.get(coordinator.id).await.unwrap().state,
        AgentState::Completed
    );

    // The real edit actually happened.
    assert_eq!(read(&backing, "src/a.txt"), "a1\n");

    // Every role's child is linked back to the coordinator, oldest first,
    // and each reached a terminal state of its own.
    let children = tasks.children_of(coordinator.id).await.unwrap();
    assert_eq!(children.len(), 4, "{children:#?}");
    for child in &children {
        assert!(
            child.state.is_terminal(),
            "child {:?} for role should be terminal, got {:?}",
            child.objective,
            child.state
        );
        assert_eq!(child.parent_task, Some(coordinator.id));
    }
    // Implementer's child specifically must have actually Completed (not
    // merely reached *a* terminal state) — it's the one that ran the real
    // edit/verify loop.
    let implementer_child = children
        .iter()
        .find(|c| c.objective == "set a to a1")
        .expect("the implementer child's objective is the bare task objective");
    assert_eq!(implementer_child.state, AgentState::Completed);

    // Every role produced and persisted exactly the artifact it owns.
    let artifacts = plan_store.artifacts_for_task(coordinator.id).await.unwrap();
    let roles_seen: std::collections::HashSet<AgentRole> =
        artifacts.iter().map(|a| a.produced_by).collect();
    assert_eq!(
        roles_seen,
        [
            AgentRole::Researcher,
            AgentRole::Planner,
            AgentRole::Implementer,
            AgentRole::Tester,
            AgentRole::Reviewer,
        ]
        .into_iter()
        .collect(),
        "{artifacts:#?}"
    );

    let researcher_artifact = artifacts
        .iter()
        .find(|a| a.produced_by == AgentRole::Researcher)
        .unwrap();
    assert!(matches!(
        &researcher_artifact.artifact,
        Artifact::ResearchBrief { relevant_files, .. } if relevant_files == &["src/a.txt".to_string()]
    ));

    let tester_artifact = artifacts
        .iter()
        .find(|a| a.produced_by == AgentRole::Tester)
        .unwrap();
    assert!(matches!(
        &tester_artifact.artifact,
        Artifact::VerificationReport { passed: true, .. }
    ));

    let reviewer_artifact = artifacts
        .iter()
        .find(|a| a.produced_by == AgentRole::Reviewer)
        .unwrap();
    assert!(matches!(
        &reviewer_artifact.artifact,
        Artifact::ReviewFindings { approved: true, .. }
    ));

    let changeset_artifact = artifacts
        .iter()
        .find(|a| a.produced_by == AgentRole::Implementer)
        .unwrap();
    assert!(matches!(
        &changeset_artifact.artifact,
        Artifact::ChangeSet { files_changed, .. } if files_changed.iter().any(|f| f.contains("a.txt"))
    ));
}

/// A Reviewer that flags a problem must not let the coordinator complete —
/// the auto-repair-revision loop is deliberately not wired yet (M5,
/// deferred), so this hands off to a human instead of silently ignoring
/// the finding.
#[tokio::test]
async fn a_reviewer_rejection_hands_the_coordinator_to_waiting_for_user_not_completed() {
    let backing = workspace();

    let researcher_scenario = Scenario {
        name: "researcher".into(),
        turns: vec![submit(
            "submit_research_brief",
            serde_json::json!({"summary": "n/a", "relevant_files": [], "open_questions": []}),
        )],
    };
    let planner_scenario = Scenario {
        name: "planner".into(),
        turns: vec![submit(
            "submit_plan",
            serde_json::json!({
                "plan_scope": ["src/"],
                "steps": [
                    {"id": "set_a", "intent": "set a to a1", "targets": ["src/a.txt"],
                     "verification": {"mode": "inherit"}}
                ]
            }),
        )],
    };
    let implementer_scenario = Scenario {
        name: "implementer".into(),
        turns: vec![
            edit_turn("src/a.txt", "a0\n", "a1\n"),
            ScriptedTurn::Finish {
                summary: "step done".into(),
            },
        ],
    };
    let reviewer_scenario = Scenario {
        name: "reviewer".into(),
        turns: vec![submit(
            "submit_review_findings",
            serde_json::json!({
                "approved": false,
                "findings": ["the edit is technically correct but undocumented"],
            }),
        )],
    };

    let (tasks, _plan_store, driver) = build_driver(
        &backing,
        researcher_scenario,
        planner_scenario,
        implementer_scenario,
        reviewer_scenario,
    );
    let coordinator = tasks
        .create(WorkspaceId::new(), "set a to a1".into(), Budget::default())
        .await
        .unwrap();
    driver
        .run_role_pipeline(coordinator.id, &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        tasks.get(coordinator.id).await.unwrap().state,
        AgentState::WaitingForUser
    );
    // The edit still happened — only the coordinator's own completion is
    // withheld pending human input, nothing is rolled back.
    assert_eq!(read(&backing, "src/a.txt"), "a1\n");
}
