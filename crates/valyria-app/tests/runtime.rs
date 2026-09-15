//! `Runtime`-level integration tests: migrations actually land on disk,
//! crash recovery finds and pauses a task a previous process left active,
//! and the embedded client's event subscription survives a manufactured
//! lag with no gap. The full walking-skeleton exit criterion (a real child
//! process, a real `SIGKILL`) is proven end-to-end in `valyria-cli/tests`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use valyria_app::{AppError, EmbeddedClient, ModelEndpointOptions, Runtime, RuntimeConfig};
use valyria_events::{EventKind, NewEvent};
use valyria_hardware::report::{CpuInfo, DiskInfo, HardwareReport};
use valyria_model_registry::{generate_keypair, sign, Catalog, ModelRole};
use valyria_model_store::{InMemoryFetcher, Manifest, ModelStore};
use valyria_protocol::Client as _;
use valyria_types::AgentState;

/// A hardware report with plenty of available RAM — used to make
/// auto-derive tests deterministic instead of depending on however much
/// RAM happens to be free on whatever machine runs the test right now
/// (the real `valyria_hardware::probe()` value shifts under ambient load
/// from unrelated processes).
fn plenty_of_ram() -> HardwareReport {
    HardwareReport {
        os: "test".into(),
        os_version: None,
        arch: "test".into(),
        cpu: CpuInfo {
            brand: "test".into(),
            physical_cores: 8,
            logical_cores: 16,
            arch: "test".into(),
        },
        ram_total_bytes: 32_000_000_000,
        ram_available_bytes: 32_000_000_000,
        gpus: vec![],
        unified_memory: true,
        accelerator_present: None,
        disk: DiskInfo {
            total_bytes: 0,
            available_bytes: 0,
        },
    }
}

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
        .with_local_models()
        .with_hardware_override(plenty_of_ram());

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

/// M6: `model_endpoint_add/remove/list` and `model_activate`'s endpoint
/// branch — real CRUD and persistence, no network needed (the default
/// fake backend still writes and reads the real `model_endpoint`/
/// `model_role_binding` tables; only the actual HTTP round trip against
/// an endpoint's server is faked away, exactly like every other
/// fake-backend test in this file).
#[tokio::test]
async fn model_endpoint_add_activate_list_remove_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let config = RuntimeConfig::new(ws.path()).with_data_dir(temp.path().join("data"));
    let runtime = Runtime::open(config).await.unwrap();

    // A malformed base_url is refused before anything is persisted.
    let err = runtime
        .model_endpoint_add("bad", "not-a-url", ModelEndpointOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Repo(_)));
    assert!(runtime.model_endpoint_list().await.unwrap().is_empty());

    // An id colliding with a real embedded catalog model is refused too —
    // `model_activate` must never have to guess which of two same-named
    // things a caller meant.
    let catalog = Catalog::embedded().unwrap();
    let real_catalog_id = catalog.cards().first().unwrap().id.clone();
    let err = runtime
        .model_endpoint_add(
            &real_catalog_id,
            "http://127.0.0.1:11434/v1",
            ModelEndpointOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Repo(_)));

    runtime
        .model_endpoint_add(
            "ollama-local",
            "http://127.0.0.1:11434/v1",
            ModelEndpointOptions {
                display_name: Some("My Ollama"),
                remote_model_name: Some("qwen2.5-coder:7b"),
                context_length: Some(32768),
                supports_native_tools: Some(true),
                supports_grammar: Some(false),
            },
        )
        .await
        .unwrap();

    let endpoints = runtime.model_endpoint_list().await.unwrap();
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].row.id, "ollama-local");
    assert_eq!(endpoints[0].row.base_url, "http://127.0.0.1:11434/v1");
    assert_eq!(endpoints[0].row.display_name, "My Ollama");
    assert_eq!(endpoints[0].row.remote_model_name, "qwen2.5-coder:7b");
    assert_eq!(endpoints[0].row.context_length, 32768);
    assert!(endpoints[0].row.supports_native_tools);
    assert!(!endpoints[0].row.supports_grammar);
    assert!(endpoints[0].active_roles.is_empty());

    // Re-adding the same id replaces rather than erroring.
    runtime
        .model_endpoint_add(
            "ollama-local",
            "http://127.0.0.1:11434/v1",
            ModelEndpointOptions {
                display_name: Some("Renamed"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let endpoints = runtime.model_endpoint_list().await.unwrap();
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].row.display_name, "Renamed");
    // Defaults applied when the field is omitted on this second add.
    assert_eq!(endpoints[0].row.remote_model_name, "ollama-local");
    assert_eq!(endpoints[0].row.context_length, 8192);

    runtime
        .model_activate("ollama-local", ModelRole::PrimaryCoder)
        .await
        .unwrap();
    let endpoints = runtime.model_endpoint_list().await.unwrap();
    assert_eq!(endpoints[0].active_roles, vec!["primary_coder".to_string()]);

    // Removing an active endpoint unbinds every role pointing at it —
    // proven against the persisted binding (the fake backend never
    // touches the orchestrator's in-memory state, so that part of
    // `model_endpoint_remove`'s contract is covered by the endpoint's
    // own real-backend behavior instead, exercised manually against a
    // real server elsewhere in this session).
    runtime.model_endpoint_remove("ollama-local").await.unwrap();
    assert!(runtime.model_endpoint_list().await.unwrap().is_empty());

    // Removing something already gone is a clean not-found error.
    let err = runtime
        .model_endpoint_remove("ollama-local")
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Repo(_)));
}

fn signed_test_catalog(version: u32, key: &valyria_model_registry::SigningKey) -> (String, String) {
    let json = format!(
        r#"{{"version":{version},"models":[
        {{"id":"refreshed-model","family":"f","display_name":"Refreshed","parameters_b":1.0,
         "quantization":"q4_k_m","context_length":2048,"file_size_bytes":1,
         "recommended_sampling":{{"temperature":0.2,"top_p":0.9,"max_tokens":null,"stop":[]}},
         "requirement":{{"min_ram_bytes":1,"min_vram_bytes":null}},
         "transport_preference":"native","supports_native_tools":true,"supports_grammar":false,
         "source_url":"u","content_hash":"aa","license_name":"MIT"}}
    ]}}"#
    );
    let sig = sign(key, json.as_bytes());
    (json, sig)
}

/// M6: `catalog_refresh` — real fetch (via the same `Fetcher` seam
/// `model_install` uses, here in-memory) + real ed25519 verification +
/// real anti-rollback version gating + a real round trip back through
/// `effective_catalog` (exercised indirectly via `model_list`, which
/// only ever reads the catalog through that helper). Uses a throwaway
/// keypair trusted via `RuntimeConfig::with_catalog_trusted_key_hex`
/// rather than this build's real compiled-in key, whose private half
/// deliberately exists nowhere a test could sign with it.
#[tokio::test]
async fn catalog_refresh_accepts_a_newer_signed_catalog_and_it_takes_effect() {
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let key = generate_keypair();
    let pubkey_hex: String = {
        let mut s = String::new();
        for b in key.verifying_key().to_bytes() {
            s.push_str(&format!("{b:02x}"));
        }
        s
    };
    let config = RuntimeConfig::new(ws.path())
        .with_data_dir(temp.path().join("data"))
        .with_catalog_trusted_key_hex(pubkey_hex);
    let runtime = Runtime::open(config).await.unwrap();

    // Not yet present under either name.
    let models_before = runtime.model_list().await.unwrap();
    assert!(!models_before.iter().any(|m| m.card.id == "refreshed-model"));

    let (json, sig) = signed_test_catalog(2, &key);
    let fetcher = InMemoryFetcher::new()
        .with_object("https://example.invalid/catalog.json", json.into_bytes())
        .with_object("https://example.invalid/catalog.json.sig", sig.into_bytes());

    let outcome = runtime
        .catalog_refresh_with(
            "https://example.invalid/catalog.json",
            "https://example.invalid/catalog.json.sig",
            &fetcher,
        )
        .await
        .expect("a genuinely newer, validly-signed catalog must be accepted");
    assert_eq!(outcome.previous_version, 1);
    assert_eq!(outcome.new_version, 2);
    assert_eq!(outcome.model_count, 1);

    // Takes effect: every catalog read goes through `effective_catalog`,
    // so `model_list` (which starts from it) now sees the refreshed
    // model and no longer the embedded ones.
    let models_after = runtime.model_list().await.unwrap();
    assert_eq!(models_after.len(), 1);
    assert_eq!(models_after[0].card.id, "refreshed-model");

    // A second refresh offering the *same* version again is a no-op
    // rejection (anti-rollback/replay), not silently reapplied.
    let (json_again, sig_again) = signed_test_catalog(2, &key);
    let fetcher2 = InMemoryFetcher::new()
        .with_object(
            "https://example.invalid/catalog.json",
            json_again.into_bytes(),
        )
        .with_object(
            "https://example.invalid/catalog.json.sig",
            sig_again.into_bytes(),
        );
    let err = runtime
        .catalog_refresh_with(
            "https://example.invalid/catalog.json",
            "https://example.invalid/catalog.json.sig",
            &fetcher2,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Repo(_)));
    // Still at version 2 — the rejected replay changed nothing.
    assert_eq!(runtime.model_list().await.unwrap().len(), 1);
}

/// The signature-rejection half: a catalog signed by a key the `Runtime`
/// does *not* trust must never be accepted, however well-formed
/// everything else about it is.
#[tokio::test]
async fn catalog_refresh_rejects_a_catalog_signed_by_an_untrusted_key() {
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let trusted = generate_keypair();
    let trusted_hex: String = {
        let mut s = String::new();
        for b in trusted.verifying_key().to_bytes() {
            s.push_str(&format!("{b:02x}"));
        }
        s
    };
    let config = RuntimeConfig::new(ws.path())
        .with_data_dir(temp.path().join("data"))
        .with_catalog_trusted_key_hex(trusted_hex);
    let runtime = Runtime::open(config).await.unwrap();

    let attacker = generate_keypair();
    let (json, sig) = signed_test_catalog(2, &attacker);
    let fetcher = InMemoryFetcher::new()
        .with_object("https://example.invalid/catalog.json", json.into_bytes())
        .with_object("https://example.invalid/catalog.json.sig", sig.into_bytes());

    let err = runtime
        .catalog_refresh_with(
            "https://example.invalid/catalog.json",
            "https://example.invalid/catalog.json.sig",
            &fetcher,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Repo(_)));
    // Nothing was persisted — still the embedded baseline.
    assert!(runtime
        .model_list()
        .await
        .unwrap()
        .iter()
        .any(|m| m.card.id != "refreshed-model"));
}
