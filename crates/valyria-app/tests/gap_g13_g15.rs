//! Integration coverage for the diagnostics enrichments added in protocol
//! 1.5.0 (CORE-INTERFACE gaps G13, G14, G15):
//!
//! * **G14** — `tool_started` and `tool_completed` share a
//!   `tool_invocation_id`, and `tool_completed` carries structured
//!   `{ exit_code, stdout, stderr, duration_ms }` alongside `rendered`.
//! * **G13** — `PlanStepSummary` gains an optional `checkpoint_id`
//!   (round-trip checked here; the live `plan_checkpoint` event and the
//!   projection are unit-covered in `valyria-events` / `valyria-task`).
//! * **G15** — parsed `failure_payload` shape is unit-covered in
//!   `valyria-agent`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use valyria_app::{EmbeddedClient, Runtime, RuntimeConfig};
use valyria_protocol::{Client as _, PlanStepSummary, Request, Response, TaskCreateRequest};

const FIXTURE_LIB_RS: &str = "pub fn existing(a: i32) -> i32 {\n    a\n}\n";

#[tokio::test]
async fn tool_events_pair_by_invocation_id_and_carry_structured_fields() {
    let ws = valyria_testkit::TempWorkspace::new();
    ws.write("src/lib.rs", FIXTURE_LIB_RS);
    let data = tempfile::tempdir().unwrap();
    let config = RuntimeConfig::new(ws.path()).with_data_dir(data.path().join("d"));
    let rt = Arc::new(Runtime::open(config).await.unwrap());
    let client = EmbeddedClient::new(rt);

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
    let mut started: HashMap<String, String> = HashMap::new(); // id -> tool
    let mut completed: Vec<serde_json::Value> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
            Ok(Some(ev)) if ev.task_id.as_deref() == Some(task_id.as_str()) => {
                match ev.kind.as_str() {
                    "tool_started" => {
                        let id = ev.payload["tool_invocation_id"]
                            .as_str()
                            .expect("tool_started must carry tool_invocation_id")
                            .to_string();
                        started.insert(id, ev.payload["tool"].as_str().unwrap_or("").to_string());
                    }
                    "tool_completed" => completed.push(ev.payload.clone()),
                    "state_changed"
                        if ev.payload.get("to").and_then(|v| v.as_str()) == Some("COMPLETED") =>
                    {
                        break;
                    }
                    _ => {}
                }
            }
            _ => break,
        }
    }

    assert!(!started.is_empty(), "the task ran tools");
    assert!(!completed.is_empty(), "the task completed tools");

    for c in &completed {
        let id = c["tool_invocation_id"]
            .as_str()
            .expect("tool_completed must carry tool_invocation_id");
        assert!(
            started.contains_key(id),
            "every tool_completed id must match a tool_started id ({id})"
        );
        assert!(
            c.get("duration_ms").and_then(|v| v.as_u64()).is_some(),
            "tool_completed must carry duration_ms"
        );
        // The keys are always present (may be null for a non-shell tool).
        assert!(c.get("exit_code").is_some());
        assert!(c.get("stdout").is_some());
        assert!(c.get("stderr").is_some());
    }

    // The walking-skeleton scenario runs `cat src/lib.rs` — that
    // completion must have exit 0 and non-empty stdout.
    let shell = completed
        .iter()
        .find(|c| c["exit_code"].as_i64() == Some(0) && c["stdout"].as_str().is_some())
        .expect("expected a shell tool completion with exit 0 and stdout");
    assert!(shell["stdout"]
        .as_str()
        .unwrap()
        .contains("pub fn existing"));
}

#[test]
fn plan_step_summary_checkpoint_id_is_optional_and_round_trips() {
    // Absent -> None, and not serialized.
    let bare: PlanStepSummary = serde_json::from_str(
        r#"{"id":"step_1","intent":"x","targets":[],"depends_on":[],"rollback_boundary":true,"checkpoint":true}"#,
    )
    .unwrap();
    assert_eq!(bare.checkpoint_id, None);
    assert!(!serde_json::to_string(&bare)
        .unwrap()
        .contains("checkpoint_id"));

    // Present -> carried through.
    let with = PlanStepSummary {
        id: "step_1".into(),
        intent: "x".into(),
        targets: vec![],
        depends_on: vec![],
        rollback_boundary: true,
        checkpoint: true,
        checkpoint_id: Some("ckpt_01H".into()),
    };
    let back: PlanStepSummary =
        serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
    assert_eq!(back.checkpoint_id.as_deref(), Some("ckpt_01H"));
}
