//! Integration coverage for context provenance + change ownership added in
//! protocol 1.4.0 (CORE-INTERFACE gaps G7, G8):
//!
//! * **G7** — a `context_retrieved` event is emitted per Discovery step,
//!   carrying the retrieved items and the budget used.
//! * **G8** — `ledger_changes { task_id }` reports each agent-touched file
//!   with `valyria-ledger`'s current classification.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use valyria_app::{EmbeddedClient, Runtime, RuntimeConfig};
use valyria_protocol::{Client as _, LedgerChangesRequest, Request, Response, TaskCreateRequest};

/// The `src/lib.rs` the bundled walking-skeleton scenario's `edit_file`
/// anchor expects, verbatim.
const FIXTURE_LIB_RS: &str = "pub fn existing(a: i32) -> i32 {\n    a\n}\n";

async fn runtime() -> (
    Arc<Runtime>,
    valyria_testkit::TempWorkspace,
    tempfile::TempDir,
) {
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write("src/lib.rs", FIXTURE_LIB_RS);
    let data = tempfile::tempdir().unwrap();
    let config = RuntimeConfig::new(ws.path()).with_data_dir(data.path().join("d"));
    (Arc::new(Runtime::open(config).await.unwrap()), ws, data)
}

/// Create the default task and drain events until it terminates. Returns
/// `(task_id, saw_context_retrieved, last_context_payload)`.
async fn run_task(client: &EmbeddedClient) -> (String, bool, Option<serde_json::Value>) {
    let task_id = match client
        .call(Request::TaskCreate(TaskCreateRequest {
            objective: "add a function".into(),
            permission_mode: Some("autonomous".into()),
        }))
        .await
    {
        Response::TaskCreate(r) => r.task_id,
        other => panic!("expected TaskCreate, got {other:?}"),
    };

    let mut stream = client.subscribe_events(0).await;
    let mut saw = false;
    let mut payload = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
            Ok(Some(ev)) => {
                if ev.task_id.as_deref() == Some(task_id.as_str()) && ev.kind == "context_retrieved"
                {
                    saw = true;
                    payload = Some(ev.payload.clone());
                }
                if ev.task_id.as_deref() == Some(task_id.as_str())
                    && ev.kind == "state_changed"
                    && ev.payload.get("to").and_then(|v| v.as_str()) == Some("COMPLETED")
                {
                    break;
                }
            }
            _ => break,
        }
    }
    (task_id, saw, payload)
}

#[tokio::test]
async fn context_retrieved_event_is_emitted_with_a_budget() {
    let (rt, _ws, _d) = runtime().await;
    let client = EmbeddedClient::new(rt);

    let (_task, saw, payload) = run_task(&client).await;
    assert!(saw, "expected a context_retrieved event during the task");

    let p = payload.unwrap();
    assert!(p.get("items").and_then(|v| v.as_array()).is_some());
    assert!(p.get("budget_used").and_then(|v| v.as_u64()).is_some());
    assert_eq!(p.get("budget_total").and_then(|v| v.as_u64()), Some(50_000));
}

#[tokio::test]
async fn ledger_changes_classify_the_agent_edit() {
    let (rt, _ws, _d) = runtime().await;
    let client = EmbeddedClient::new(rt);

    let (task_id, _saw, _p) = run_task(&client).await;

    let Response::LedgerChanges(l) = client
        .call(Request::LedgerChanges(LedgerChangesRequest {
            task_id: task_id.clone(),
        }))
        .await
    else {
        panic!("expected LedgerChanges");
    };

    let edit = l
        .changes
        .iter()
        .find(|c| c.path == "src/lib.rs")
        .expect("the agent edited src/lib.rs, so the ledger must list it");
    assert_eq!(edit.kind, "write");
    // The file on disk is exactly the agent's last write, so it is
    // agent-authored, not a concurrent user modification.
    assert_eq!(edit.classification, "agent_authored");
    assert_eq!(edit.task_id, task_id);
    assert!(!edit.step_id.is_empty());
}

#[tokio::test]
async fn ledger_changes_flags_a_concurrent_user_edit() {
    let (rt, ws, _d) = runtime().await;
    let client = EmbeddedClient::new(rt);

    let (task_id, _saw, _p) = run_task(&client).await;

    // A human edits the file after the agent did.
    ws.write(
        "src/lib.rs",
        "pub fn existing(a: i32) -> i32 { a }\n// human touched this\n",
    );

    let Response::LedgerChanges(l) = client
        .call(Request::LedgerChanges(LedgerChangesRequest {
            task_id: task_id.clone(),
        }))
        .await
    else {
        panic!("expected LedgerChanges");
    };
    let edit = l
        .changes
        .iter()
        .find(|c| c.path == "src/lib.rs")
        .expect("still listed");
    assert_eq!(edit.classification, "concurrent_user_modification");
}

#[tokio::test]
async fn ledger_changes_is_empty_for_an_unknown_task() {
    let (rt, _ws, _d) = runtime().await;
    let client = EmbeddedClient::new(rt);

    let Response::LedgerChanges(l) = client
        .call(Request::LedgerChanges(LedgerChangesRequest {
            task_id: valyria_types::TaskId::new().to_string(),
        }))
        .await
    else {
        panic!("expected LedgerChanges");
    };
    assert!(l.changes.is_empty());
}
