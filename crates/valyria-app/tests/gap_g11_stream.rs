//! Integration coverage for the per-task event filter added in protocol
//! 1.7.0 (CORE-INTERFACE gap G11): `subscribe_events_for_task` (and the
//! `task_id` on the subscribe frame) restrict the stream to one task's
//! events plus workspace-global ones.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use valyria_app::{EmbeddedClient, Runtime, RuntimeConfig};
use valyria_protocol::{Client, Request, Response, TaskCreateRequest};

const FIXTURE_LIB_RS: &str = "pub fn existing(a: i32) -> i32 {\n    a\n}\n";

async fn client() -> EmbeddedClient {
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write("src/lib.rs", FIXTURE_LIB_RS);
    let data = tempfile::tempdir().unwrap();
    let cfg = RuntimeConfig::new(ws.path()).with_data_dir(data.path().join("d"));
    let rt = Arc::new(Runtime::open(cfg).await.unwrap());
    std::mem::forget((ws, data)); // keep the dirs alive for the test
    EmbeddedClient::new(rt)
}

async fn create(c: &EmbeddedClient) -> String {
    match c
        .call(Request::TaskCreate(TaskCreateRequest {
            objective: "add a function".into(),
            permission_mode: Some("autonomous".into()),
        }))
        .await
    {
        Response::TaskCreate(r) => r.task_id,
        other => panic!("expected TaskCreate, got {other:?}"),
    }
}

/// Collect the distinct `task_id`s (and whether any task-less event
/// appeared) from `stream` until it goes quiet.
async fn drain(
    mut stream: futures::stream::BoxStream<'_, valyria_protocol::WireEvent>,
) -> (HashSet<String>, bool) {
    let mut task_ids = HashSet::new();
    let mut saw_global = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(750), stream.next()).await {
            Ok(Some(ev)) => match ev.task_id {
                Some(t) => {
                    task_ids.insert(t);
                }
                None => saw_global = true,
            },
            Ok(None) => break,
            Err(_) => break, // quiet for 750ms — both tasks are done
        }
    }
    (task_ids, saw_global)
}

#[tokio::test]
async fn a_task_scoped_subscription_excludes_other_tasks() {
    let c = client().await;
    let a = create(&c).await;
    let b = create(&c).await;

    // Filtered to A: must see A, must NOT see B.
    let scoped = c.subscribe_events_for_task(0, Some(a.clone())).await;
    let (seen, _global) = drain(scoped).await;
    assert!(
        seen.contains(&a),
        "the scoped stream must carry task A's events"
    );
    assert!(
        !seen.contains(&b),
        "the scoped stream must not carry task B's events, saw: {seen:?}"
    );
}

#[tokio::test]
async fn no_filter_is_the_full_stream() {
    let c = client().await;
    let a = create(&c).await;
    let b = create(&c).await;

    let full = c.subscribe_events_for_task(0, None).await;
    let (seen, _global) = drain(full).await;
    assert!(
        seen.contains(&a) && seen.contains(&b),
        "the full stream carries both, saw: {seen:?}"
    );
}
