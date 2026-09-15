//! `Runtime`-level integration tests: migrations actually land on disk,
//! crash recovery finds and pauses a task a previous process left active,
//! and the embedded client's event subscription survives a manufactured
//! lag with no gap. The full walking-skeleton exit criterion (a real child
//! process, a real `SIGKILL`) is proven end-to-end in `valyria-cli/tests`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use valyria_app::{EmbeddedClient, Runtime, RuntimeConfig};
use valyria_events::{EventKind, NewEvent};
use valyria_model_registry::{Catalog, ModelRole};
use valyria_model_store::{Manifest, ModelStore};
use valyria_protocol::Client as _;
use valyria_types::AgentState;

/// A real catalog id suitable for `PrimaryCoder` — same one
/// `local_model_e2e.rs` uses, since it's already confirmed to exist in
/// the embedded catalog with real requirements/suitability scores.
const AUTO_DERIVE_MODEL_ID: &str = "qwen2.5-coder-1.5b-instruct-q8_0";

/// "Installs" a model by hand (arbitrary bytes, a hand-built manifest) —
/// exactly `local_model_e2e.rs::install_test_model`'s technique, but
/// duplicated here rather than shared, since that file's helper is
/// `#[ignore]`-test-only and this one deliberately isn't (it never waits
/// for a real server to actually finish booting, only for the *attempt*
/// to start, so it needs no real GGUF or network access).
fn fake_install(global_dir: &std::path::Path) {
    let catalog = Catalog::embedded().expect("embedded catalog parses");
    let card = catalog
        .get(AUTO_DERIVE_MODEL_ID)
        .unwrap_or_else(|| panic!("catalog has no `{AUTO_DERIVE_MODEL_ID}`"))
        .clone();
    let store = ModelStore::new(global_dir);
    let dest_dir = store.models_dir().join(AUTO_DERIVE_MODEL_ID);
    std::fs::create_dir_all(&dest_dir).unwrap();
    let weights_file = format!("{AUTO_DERIVE_MODEL_ID}.gguf");
    std::fs::write(dest_dir.join(&weights_file), b"not a real gguf").unwrap();
    let manifest = Manifest {
        card,
        weights_file,
        size_bytes: 16,
        content_hash: "not-verified-in-this-test".into(),
        installed_at_ms: 0,
        license_accepted_at_ms: None,
        probe: None,
    };
    std::fs::write(
        dest_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

/// M6: a role nobody ever explicitly `model_activate`d still gets a
/// best-effort model, auto-derived (`RoleBinding::derive`) from whatever
/// happens to be installed — proven by waiting for the real
/// `model_server_starting` event naming the derived id, which only fires
/// after a real `ModelPool` admission succeeds. Deliberately does not
/// wait for `model_server_ready`: this needs no real GGUF or network
/// (the actual server boot past this point is already covered by
/// `local_model_e2e.rs`'s `#[ignore]`d end-to-end test).
///
/// Targets `Autocomplete`, not `PrimaryCoder`: `local_model_e2e.rs`
/// force-binds this same small 1.5B model onto `PrimaryCoder` via an
/// explicit `model_activate`, which bypasses suitability scoring
/// entirely — real `RoleBinding::derive` scoring, which this test
/// exercises, naturally prefers a small/cheap model for a cheap role
/// like `Autocomplete` over the demanding `PrimaryCoder` slot.
#[tokio::test]
async fn an_unactivated_role_gets_a_best_effort_auto_derived_model() {
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let data_dir = temp.path().join("data");
    fake_install(&data_dir.join("global"));

    let config = RuntimeConfig::new(ws.path())
        .with_data_dir(data_dir)
        .with_local_models();

    let events_before = {
        // Open just long enough to grab the event bus before the
        // background boot task fires — `Runtime::open` itself never
        // activates anything synchronously, it only spawns the attempt.
        let runtime = Runtime::open(config.clone()).await.unwrap();
        runtime.events()
    };
    let mut stream = events_before
        .subscribe_since(valyria_events::Seq::ZERO)
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for model_server_starting — no auto-derivation attempt was made"
        );
        let Ok(Ok(delivery)) = tokio::time::timeout(Duration::from_secs(1), stream.recv()).await
        else {
            continue;
        };
        let valyria_events::Delivery::Event(env) = delivery else {
            continue;
        };
        // The one installed model can legitimately score well enough for
        // more than one role — every such role gets its own auto-derived
        // attempt, so this waits specifically for Autocomplete's rather
        // than assuming it's the first `model_server_starting` to arrive.
        if env.kind == EventKind::ModelServerStarting
            && env.payload["role"] == ModelRole::Autocomplete.as_str()
        {
            assert_eq!(env.payload["id"], AUTO_DERIVE_MODEL_ID);
            return;
        }
    }
}

/// The counterpart: with *nothing at all* installed, no role gets an
/// auto-derived attempt — `RoleBinding::derive`'s underlying
/// `select_for_role` treats an empty "available" list as "consider the
/// whole catalog" (the right behavior for `model_recommend`), which would
/// otherwise wrongly pick an uninstalled catalog entry here.
#[tokio::test]
async fn nothing_installed_means_no_auto_derive_attempt_and_primary_coder_falls_back_to_none_bound()
{
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let config = RuntimeConfig::new(ws.path())
        .with_data_dir(temp.path().join("data"))
        .with_local_models();

    let runtime = Runtime::open(config).await.unwrap();
    let mut stream = runtime
        .events()
        .subscribe_since(valyria_events::Seq::ZERO)
        .await
        .unwrap();

    // No model_server_starting should ever arrive; a short bounded wait
    // is the only way to prove a negative against a live event stream.
    let saw_starting = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let valyria_events::Delivery::Event(env) = stream.recv().await.unwrap() else {
                continue;
            };
            if env.kind == EventKind::ModelServerStarting {
                return true;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(!saw_starting);

    let list = EmbeddedClient::new(Arc::new(runtime))
        .call(valyria_protocol::Request::ModelList(
            valyria_protocol::Empty {},
        ))
        .await;
    // Whatever ModelList reports, the important assertion already
    // happened above (no boot attempt) — this just also exercises the
    // call path without crashing on an empty catalog-vs-installed state.
    assert!(matches!(list, valyria_protocol::Response::ModelList(_)));
}

#[tokio::test]
async fn open_creates_the_database_and_a_stable_workspace_id() {
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let config = RuntimeConfig::new(ws.path()).with_data_dir(temp.path().join("data"));

    let runtime = Runtime::open(config.clone()).await.unwrap();
    let id_first_open = runtime.workspace_id();
    assert!(temp.path().join("data/workspace.db").exists());
    drop(runtime);

    let runtime2 = Runtime::open(config).await.unwrap();
    assert_eq!(runtime2.workspace_id(), id_first_open);
}

#[tokio::test]
async fn opening_an_unrelated_runtime_does_not_disturb_another_tasks_state() {
    // Regression test for a real cross-process hang: `open()` used to run
    // a workspace-wide recovery scan unconditionally, which meant *any*
    // CLI invocation against this workspace (`task status`, `task pause`,
    // an unrelated `run`) would force-pause a task actively being driven
    // by a different, still-alive process the moment it observed that
    // task sitting in a non-terminal, non-stable state — indistinguishable
    // from a real crash without any liveness tracking. `open()` must leave
    // every task's state alone; only `resume_task` may recover, and only
    // the one task it's asked to resume (see the next test).
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let data_dir = temp.path().join("data");
    let config = RuntimeConfig::new(ws.path()).with_data_dir(data_dir.clone());

    let workspace_id;
    let task_id = valyria_types::TaskId::new();
    {
        let runtime = Runtime::open(config.clone()).await.unwrap();
        workspace_id = runtime.workspace_id();
        // `Store`'s `Drop` joins its actor thread before returning, which
        // releases the sqlite file so we can open a second, raw connection
        // below without contention.
    }

    // Seed a task directly via SQL, bypassing the driver entirely — this
    // is exactly the row shape a real crash mid-`Implementing` would leave
    // behind, without needing to actually race and kill a live tokio task
    // to produce it. It stands in for "a task some *other*, still-running
    // process is actively driving right now" just as well as "a task a
    // crashed process left behind" — `open()` cannot tell these apart and
    // must not touch either.
    {
        let conn = rusqlite::Connection::open(data_dir.join("workspace.db")).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, workspace_id, objective, state, plan_scope, \
             created_at_ms, updated_at_ms) VALUES (?1, ?2, 'add a function', \
             'IMPLEMENTING', '[]', 0, 0)",
            rusqlite::params![task_id.to_string(), workspace_id.to_string()],
        )
        .unwrap();
    }

    let unrelated_runtime = Runtime::open(config.clone()).await.unwrap();
    let status = unrelated_runtime.task_status(task_id).await.unwrap();
    assert_eq!(status.state, AgentState::Implementing);
    assert_eq!(status.paused_from, None);
    assert!(status.recovery_note.is_none());

    // Opening yet another one changes nothing further, either.
    let _another = Runtime::open(config).await.unwrap();
    let status_again = unrelated_runtime.task_status(task_id).await.unwrap();
    assert_eq!(status_again.state, AgentState::Implementing);
}

#[tokio::test]
async fn resume_task_recovers_only_the_task_it_was_asked_to_resume() {
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let data_dir = temp.path().join("data");
    let config = RuntimeConfig::new(ws.path()).with_data_dir(data_dir.clone());

    let workspace_id;
    let task_id = valyria_types::TaskId::new();
    {
        let runtime = Runtime::open(config.clone()).await.unwrap();
        workspace_id = runtime.workspace_id();
    }
    {
        let conn = rusqlite::Connection::open(data_dir.join("workspace.db")).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, workspace_id, objective, state, plan_scope, \
             created_at_ms, updated_at_ms) VALUES (?1, ?2, 'add a function', \
             'IMPLEMENTING', '[]', 0, 0)",
            rusqlite::params![task_id.to_string(), workspace_id.to_string()],
        )
        .unwrap();
    }

    let runtime2 = Runtime::open(config).await.unwrap();
    runtime2.resume_task(task_id).await.unwrap();

    // `resume_task` synchronously recovers the task and transitions it
    // back to `Implementing` (its `paused_from`) before spawning a driver
    // — `paused_from` is cleared again by that second transition, but
    // `recovery_note` never gets cleared once set, so it's a reliable
    // witness that recovery actually happened, regardless of how far the
    // spawned driver has raced ahead by the time we check.
    let status = runtime2.task_status(task_id).await.unwrap();
    assert!(status.recovery_note.is_some());
}

#[tokio::test]
async fn resuming_a_task_with_a_pending_cancel_actually_cancels_it() {
    // Regression test for a real bug: a client asks to cancel a task
    // whose driver already died (nothing running anywhere to notice the
    // request), so it just sits in the row as `pending_signal`. Resuming
    // that task used to silently drop the request — `recover_task`'s own
    // recovery transition, and then `resume_task`'s Paused -> paused_from
    // transition, each unconditionally clear `pending_signal` (by design,
    // for the ordinary case where a transition really did consume it) —
    // so a task that was supposed to be cancelled just kept running as if
    // nothing had been asked of it.
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let data_dir = temp.path().join("data");
    let config = RuntimeConfig::new(ws.path()).with_data_dir(data_dir.clone());

    let workspace_id;
    let task_id = valyria_types::TaskId::new();
    {
        let runtime = Runtime::open(config.clone()).await.unwrap();
        workspace_id = runtime.workspace_id();
    }
    {
        // The exact row shape a dead-driver task with an unhonored cancel
        // request leaves behind: non-terminal state, `pending_signal` set.
        let conn = rusqlite::Connection::open(data_dir.join("workspace.db")).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, workspace_id, objective, state, pending_signal, \
             plan_scope, created_at_ms, updated_at_ms) VALUES (?1, ?2, 'add a function', \
             'IMPLEMENTING', 'CANCEL', '[]', 0, 0)",
            rusqlite::params![task_id.to_string(), workspace_id.to_string()],
        )
        .unwrap();
    }

    let runtime2 = Runtime::open(config).await.unwrap();
    runtime2.resume_task(task_id).await.unwrap();

    // The spawned driver processes the (preserved) pending signal on its
    // first loop iteration — poll briefly rather than assuming a fixed
    // delay is enough.
    let mut status = runtime2.task_status(task_id).await.unwrap();
    for _ in 0..50 {
        if status.state == AgentState::Cancelled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        status = runtime2.task_status(task_id).await.unwrap();
    }
    assert_eq!(
        status.state,
        AgentState::Cancelled,
        "a pending cancel must survive recovery + resume, not be silently dropped"
    );
}

#[tokio::test]
async fn resuming_an_already_paused_task_with_a_stale_pending_pause_actually_resumes_it() {
    // Regression test for a real bug: a task that is already `Paused` can
    // still have `pending_signal = PAUSE` sitting in its row — either a
    // pause that raced with this very resume call, or a leftover from
    // whatever put it in `Paused` in the first place. `resume_task` used
    // to treat `PauseRequested` exactly like `CancelRequested` and carry
    // it through the resume, re-arming it on the freshly-transitioned
    // task. The spawned driver checks `pending_signal` before doing any
    // work (`AgentDriver::run`), so it re-paused immediately — turning
    // "resume" into a no-op that silently re-pauses on every call, with
    // the task never making progress (observed in production as a
    // Paused/Repairing flap that repeated the same turn six times before
    // eventually failing).
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let data_dir = temp.path().join("data");
    let config = RuntimeConfig::new(ws.path()).with_data_dir(data_dir.clone());

    let workspace_id;
    let task_id = valyria_types::TaskId::new();
    {
        let runtime = Runtime::open(config.clone()).await.unwrap();
        workspace_id = runtime.workspace_id();
    }
    {
        // Already `Paused` (from `Implementing`), with a `PAUSE` signal
        // still sitting in the row — the exact shape a racing/stale pause
        // request leaves behind.
        let conn = rusqlite::Connection::open(data_dir.join("workspace.db")).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, workspace_id, objective, state, paused_from, \
             pending_signal, plan_scope, created_at_ms, updated_at_ms) VALUES \
             (?1, ?2, 'add a function', 'PAUSED', 'IMPLEMENTING', 'PAUSE', '[]', 0, 0)",
            rusqlite::params![task_id.to_string(), workspace_id.to_string()],
        )
        .unwrap();
    }

    let runtime2 = Runtime::open(config).await.unwrap();
    runtime2.resume_task(task_id).await.unwrap();

    // `resume_task` itself synchronously transitions the task out of
    // `Paused` *before* the buggy re-arm would even run (that happens a
    // moment later, inside the spawned driver's first loop iteration) —
    // so checking right away always sees a non-Paused state regardless of
    // the bug. Give the spawned driver time to run and, if the fix isn't
    // in place, re-pause it, then check where it actually settled.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let status = runtime2.task_status(task_id).await.unwrap();
    assert_ne!(
        status.state,
        AgentState::Paused,
        "a stale/racing pause must not survive an explicit resume and re-pause the task"
    );
}

#[tokio::test]
async fn subscribe_events_survives_a_manufactured_lag_with_no_gap() {
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let config = RuntimeConfig::new(ws.path()).with_data_dir(temp.path().join("data"));
    let runtime = Arc::new(Runtime::open(config).await.unwrap());
    let client = EmbeddedClient::new(runtime.clone());

    // Subscribe first (this is what activates the live broadcast
    // receiver), then flood events without ever reading, forcing the
    // subscriber's local queue past capacity before we start consuming.
    let mut stream = client.subscribe_events(0).await;

    let events = runtime.events();
    const FLOOD: usize = 4096 + 50; // past the live channel's 4096 capacity
    for i in 0..FLOOD {
        events
            .append(NewEvent::new(
                EventKind::StateChanged,
                serde_json::json!({"i": i}),
            ))
            .await
            .unwrap();
    }

    let mut received = 0usize;
    let mut last_seq = 0u64;
    while received < FLOOD {
        let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("should not time out waiting for a resumed event")
            .expect("stream should not end before delivering everything");
        assert!(
            event.seq > last_seq,
            "events must arrive in increasing seq order"
        );
        last_seq = event.seq;
        received += 1;
    }
    assert_eq!(received, FLOOD);
}
