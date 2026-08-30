//! Integration coverage for approval identity + scope added in protocol
//! 1.8.0 (CORE-INTERFACE gap G2): `approval_requested` carries a stable
//! `request_id`, and `permission_resolve` takes `{ request_id?, decision:
//! once | task | deny }` — a stale id is refused with `approval.superseded`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use valyria_app::{EmbeddedClient, Runtime, RuntimeConfig};
use valyria_protocol::{
    Client as _, PermissionResolveRequest, Request, Response, TaskCreateRequest,
};

const FIXTURE_LIB_RS: &str = "pub fn existing(a: i32) -> i32 {\n    a\n}\n";

async fn manual_client() -> EmbeddedClient {
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write("src/lib.rs", FIXTURE_LIB_RS);
    let data = tempfile::tempdir().unwrap();
    let cfg = RuntimeConfig::new(ws.path())
        .with_data_dir(data.path().join("d"))
        .with_permission_mode(valyria_types::PermissionMode::Manual);
    let rt = Arc::new(Runtime::open(cfg).await.unwrap());
    std::mem::forget((ws, data));
    EmbeddedClient::new(rt)
}

async fn start(c: &EmbeddedClient) -> String {
    match c
        .call(Request::TaskCreate(TaskCreateRequest {
            objective: "add a function".into(),
            permission_mode: None,
        }))
        .await
    {
        Response::TaskCreate(r) => r.task_id,
        other => panic!("expected TaskCreate, got {other:?}"),
    }
}

async fn task_state(c: &EmbeddedClient, task_id: &str) -> String {
    match c
        .call(Request::TaskStatus(valyria_protocol::TaskStatusRequest {
            task_id: task_id.to_string(),
        }))
        .await
    {
        Response::TaskStatus(s) => s.state,
        other => panic!("expected TaskStatus, got {other:?}"),
    }
}

/// Wait for the next `approval_requested` for `task_id`, then wait for the
/// task to actually settle in `WAITING_FOR_PERMISSION`, and return the
/// request's `request_id`.
async fn next_request_id(c: &EmbeddedClient, task_id: &str, since: u64) -> (String, u64) {
    let mut stream = c.subscribe_events(since).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut found: Option<(String, u64)> = None;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
            Ok(Some(ev))
                if ev.task_id.as_deref() == Some(task_id) && ev.kind == "approval_requested" =>
            {
                let id = ev.payload["request_id"]
                    .as_str()
                    .expect("approval_requested must carry request_id")
                    .to_string();
                found = Some((id, ev.seq));
                break;
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    let (id, seq) = found.unwrap_or_else(|| panic!("no approval_requested for {task_id}"));
    for _ in 0..100 {
        if task_state(c, task_id).await == "WAITING_FOR_PERMISSION" {
            return (id, seq);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("task {task_id} never settled in WAITING_FOR_PERMISSION");
}

async fn resolve(
    c: &EmbeddedClient,
    task_id: &str,
    request_id: Option<&str>,
    decision: &str,
) -> Response {
    c.call(Request::PermissionResolve(PermissionResolveRequest {
        task_id: task_id.to_string(),
        approve: true,
        request_id: request_id.map(str::to_string),
        decision: Some(decision.to_string()),
    }))
    .await
}

#[tokio::test]
async fn approval_requested_carries_a_request_id_and_once_resolves_it() {
    let c = manual_client().await;
    let task = start(&c).await;
    let (rid, _seq) = next_request_id(&c, &task, 0).await;
    assert!(!rid.is_empty());

    match resolve(&c, &task, Some(&rid), "once").await {
        Response::Ack => {}
        other => panic!("expected Ack, got {other:?}"),
    }

    // The task is no longer blocked on that request.
    match c
        .call(Request::TaskStatus(valyria_protocol::TaskStatusRequest {
            task_id: task.clone(),
        }))
        .await
    {
        Response::TaskStatus(s) => assert_ne!(s.state, "WAITING_FOR_PERMISSION"),
        other => panic!("expected TaskStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn a_stale_request_id_is_refused_as_superseded() {
    let c = manual_client().await;
    let task = start(&c).await;
    let (rid, seq) = next_request_id(&c, &task, 0).await;

    // Resolve the real one so the task advances to its next approval.
    assert!(matches!(
        resolve(&c, &task, Some(&rid), "once").await,
        Response::Ack
    ));
    let (rid2, _) = next_request_id(&c, &task, seq).await;
    assert_ne!(rid, rid2, "the second approval has a fresh id");

    // Replaying the first id is refused.
    match resolve(&c, &task, Some(&rid), "once").await {
        Response::Error(e) => assert_eq!(e.code, "approval.superseded"),
        other => panic!("expected approval.superseded, got {other:?}"),
    }
}

#[tokio::test]
async fn deny_fails_the_task() {
    let c = manual_client().await;
    let task = start(&c).await;
    let (rid, _) = next_request_id(&c, &task, 0).await;

    assert!(matches!(
        resolve(&c, &task, Some(&rid), "deny").await,
        Response::Ack
    ));
    // Give the driver a beat, then check it failed.
    tokio::time::sleep(Duration::from_millis(200)).await;
    match c
        .call(Request::TaskStatus(valyria_protocol::TaskStatusRequest {
            task_id: task.clone(),
        }))
        .await
    {
        Response::TaskStatus(s) => assert_eq!(s.state, "FAILED"),
        other => panic!("expected TaskStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unknown_decision_is_rejected() {
    let c = manual_client().await;
    let task = start(&c).await;
    let (rid, _) = next_request_id(&c, &task, 0).await;

    match c
        .call(Request::PermissionResolve(PermissionResolveRequest {
            task_id: task.clone(),
            approve: true,
            request_id: Some(rid),
            decision: Some("maybe".into()),
        }))
        .await
    {
        Response::Error(e) => assert_eq!(e.code, "approval.unknown_decision"),
        other => panic!("expected approval.unknown_decision, got {other:?}"),
    }
}

#[tokio::test]
async fn allow_for_task_resolves_and_lets_the_task_finish() {
    let c = manual_client().await;
    let task = start(&c).await;
    let mut since = 0u64;

    // Approve each request with "task" scope until the task terminates.
    for _ in 0..10 {
        let (rid, seq) = next_request_id(&c, &task, since).await;
        since = seq;
        assert!(matches!(
            resolve(&c, &task, Some(&rid), "task").await,
            Response::Ack
        ));
        tokio::time::sleep(Duration::from_millis(150)).await;
        if let Response::TaskStatus(s) = c
            .call(Request::TaskStatus(valyria_protocol::TaskStatusRequest {
                task_id: task.clone(),
            }))
            .await
        {
            if s.state == "COMPLETED" || s.state == "FAILED" {
                assert_eq!(s.state, "COMPLETED");
                return;
            }
        }
    }
    panic!("task did not terminate after repeated `task`-scoped approvals");
}
