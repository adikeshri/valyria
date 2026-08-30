//! Integration coverage for the CORE-INTERFACE gaps closed in protocol
//! 1.1.0:
//!
//! * **G1** — a per-task `permission_mode` on `task_create`. Two tasks on
//!   one runtime, at one daemon-global mode, diverge: one pinned `manual`
//!   stops to ask, one pinned `autonomous` runs straight through — no
//!   daemon restart between them.
//! * **G6** — `config_set`: a write lands in a Core-owned file and the
//!   re-resolved view reflects it; a write that would breach the policy
//!   floor is refused with `config.policy_floor_violation` and nothing
//!   changes; an unknown scope is `config.invalid_scope`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use valyria_app::{EmbeddedClient, Runtime, RuntimeConfig};
use valyria_protocol::{
    Client as _, ConfigSetRequest, Empty, Request, Response, TaskCreateRequest,
};
use valyria_types::PermissionMode;

async fn runtime(mode: PermissionMode) -> (Arc<Runtime>, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let config = RuntimeConfig::new(ws.path())
        .with_data_dir(temp.path().join("data"))
        .with_permission_mode(mode);
    std::mem::forget(ws); // keep the workspace dir alive for the test
    (Arc::new(Runtime::open(config).await.unwrap()), temp)
}

/// Drive `client`'s event stream until `task_id` reaches a terminal
/// `state_changed`, recording whether an `approval_requested` for it was
/// seen along the way. Times out (returning what it has) after `budget`.
async fn watch(
    client: &EmbeddedClient,
    task_id: &str,
    budget: Duration,
) -> (
    bool,           /* asked */
    Option<String>, /* terminal state */
) {
    let mut stream = client.subscribe_events(0).await;
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return (false, None);
        }
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(ev)) if ev.task_id.as_deref() == Some(task_id) => {
                if ev.kind == "approval_requested" {
                    return (true, None);
                }
                if ev.kind == "state_changed" {
                    if let Some(to) = ev.payload.get("to").and_then(|v| v.as_str()) {
                        if matches!(to, "COMPLETED" | "FAILED" | "CANCELLED") {
                            return (false, Some(to.to_string()));
                        }
                        if to == "WAITING_FOR_PERMISSION" {
                            return (true, None);
                        }
                    }
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => return (false, None),
            Err(_) => return (false, None),
        }
    }
}

async fn create(client: &EmbeddedClient, mode: Option<&str>) -> String {
    match client
        .call(Request::TaskCreate(TaskCreateRequest {
            objective: "add a function".into(),
            permission_mode: mode.map(str::to_string),
        }))
        .await
    {
        Response::TaskCreate(r) => r.task_id,
        other => panic!("expected TaskCreate, got {other:?}"),
    }
}

#[tokio::test]
async fn per_task_permission_mode_diverges_without_a_daemon_restart() {
    // Daemon-global mode is Autonomous.
    let (rt, _tmp) = runtime(PermissionMode::Autonomous).await;
    let client = EmbeddedClient::new(rt.clone());

    // A task pinned to Manual has to stop and ask for the edit.
    let manual = create(&client, Some("manual")).await;
    let (asked, _term) = watch(&client, &manual, Duration::from_secs(20)).await;
    assert!(
        asked,
        "a task pinned to `manual` must raise an approval for the file edit \
         even though the daemon-global mode is `autonomous`"
    );

    // A second task on the *same* runtime, pinned Autonomous, runs to
    // completion with no approval — no restart happened between them.
    let auto = create(&client, Some("autonomous")).await;
    let (asked_auto, term_auto) = watch(&client, &auto, Duration::from_secs(20)).await;
    assert!(!asked_auto, "an `autonomous` task should not stop to ask");
    assert_eq!(term_auto.as_deref(), Some("COMPLETED"));
}

#[tokio::test]
async fn omitting_permission_mode_keeps_the_daemon_default() {
    // Daemon-global mode is Manual; a task that omits the override inherits
    // it and therefore asks.
    let (rt, _tmp) = runtime(PermissionMode::Manual).await;
    let client = EmbeddedClient::new(rt.clone());

    let inherit = create(&client, None).await;
    let (asked, _term) = watch(&client, &inherit, Duration::from_secs(20)).await;
    assert!(
        asked,
        "with no override and a Manual daemon, the task must ask"
    );
}

#[tokio::test]
async fn invalid_permission_mode_is_rejected_before_the_task_is_created() {
    let (rt, _tmp) = runtime(PermissionMode::Assisted).await;
    let client = EmbeddedClient::new(rt.clone());

    let resp = client
        .call(Request::TaskCreate(TaskCreateRequest {
            objective: "add a function".into(),
            permission_mode: Some("turbo".into()),
        }))
        .await;
    match resp {
        Response::Error(e) => assert_eq!(e.code, "protocol.invalid_permission_mode"),
        other => panic!("expected an error, got {other:?}"),
    }
    // Nothing was created.
    match client.call(Request::TaskList(Empty {})).await {
        Response::TaskList(l) => assert!(l.tasks.is_empty()),
        other => panic!("expected TaskList, got {other:?}"),
    }
}

fn entry<'a>(resp: &'a Response, key: &str) -> &'a valyria_protocol::ConfigEntryWire {
    match resp {
        Response::ConfigShow(s) => s
            .entries
            .iter()
            .find(|e| e.key == key)
            .unwrap_or_else(|| panic!("no `{key}` in config_show: {:?}", s.entries)),
        other => panic!("expected ConfigShow, got {other:?}"),
    }
}

#[tokio::test]
async fn config_set_writes_a_leaf_and_the_reresolved_view_reflects_it() {
    let (rt, tmp) = runtime(PermissionMode::Assisted).await;
    let client = EmbeddedClient::new(rt.clone());

    // Before: the default.
    let before = client.call(Request::ConfigShow(Empty {})).await;
    assert_eq!(entry(&before, "log.format").value, "pretty");
    assert_eq!(entry(&before, "log.format").origin, "default");

    // Write it at workspace scope; the response is the fresh view.
    let after = client
        .call(Request::ConfigSet(ConfigSetRequest {
            key: "log.format".into(),
            value: "json".into(),
            scope: "workspace".into(),
        }))
        .await;
    assert_eq!(entry(&after, "log.format").value, "json");
    assert_eq!(entry(&after, "log.format").origin, "workspace");

    // It really is on disk, and a plain `config_show` still sees it.
    let cfg = tmp.path().join("data").join("config.toml");
    assert!(cfg.exists(), "config.toml was not written");
    let reread = client.call(Request::ConfigShow(Empty {})).await;
    assert_eq!(entry(&reread, "log.format").value, "json");
}

#[tokio::test]
async fn config_set_refuses_a_policy_floor_breach_and_changes_nothing() {
    let (rt, tmp) = runtime(PermissionMode::Assisted).await;
    let client = EmbeddedClient::new(rt.clone());

    let resp = client
        .call(Request::ConfigSet(ConfigSetRequest {
            key: "network.credentials".into(),
            value: "allowed".into(),
            scope: "workspace".into(),
        }))
        .await;
    match resp {
        Response::Error(e) => assert_eq!(e.code, "config.policy_floor_violation"),
        other => panic!("expected a floor violation error, got {other:?}"),
    }

    assert!(
        !tmp.path().join("data").join("config.toml").exists(),
        "a refused write must not create the file"
    );
    let show = client.call(Request::ConfigShow(Empty {})).await;
    assert_eq!(entry(&show, "network.credentials").value, "denied");
}

#[tokio::test]
async fn config_set_rejects_an_unknown_scope() {
    let (rt, _tmp) = runtime(PermissionMode::Assisted).await;
    let client = EmbeddedClient::new(rt.clone());

    let resp = client
        .call(Request::ConfigSet(ConfigSetRequest {
            key: "log.format".into(),
            value: "json".into(),
            scope: "global".into(),
        }))
        .await;
    match resp {
        Response::Error(e) => assert_eq!(e.code, "config.invalid_scope"),
        other => panic!("expected an invalid-scope error, got {other:?}"),
    }
}

#[tokio::test]
async fn config_set_rejects_an_unknown_key() {
    let (rt, _tmp) = runtime(PermissionMode::Assisted).await;
    let client = EmbeddedClient::new(rt.clone());

    let resp = client
        .call(Request::ConfigSet(ConfigSetRequest {
            key: "iteration.limit".into(),
            value: "10".into(),
            scope: "workspace".into(),
        }))
        .await;
    match resp {
        Response::Error(e) => assert_eq!(e.code, "config.unknown_key"),
        other => panic!("expected an unknown-key error, got {other:?}"),
    }
}
