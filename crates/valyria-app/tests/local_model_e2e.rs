//! Real local inference through the full `Runtime`: activate a model and
//! drive a real task against a genuine `llama-server` + a genuine small
//! GGUF — the thing every unit/integration test elsewhere in this
//! workspace stops just short of, because they all inject a fake or a
//! mock. This is the one place that doesn't.
//!
//! The model is "installed" by hand (copying an already-downloaded GGUF
//! into the model store's expected layout under a real catalog id) rather
//! than through `model_install`, so this test's cost is one small model
//! load, not a multi-GB network fetch on every run — `model_install`'s own
//! download path is exercised separately, offline, in
//! `crates/valyria-model-store` and against a real HTTP server in
//! `crates/valyria-engine-store`.
//!
//! `#[ignore]`d — needs a real `llama-server` (fetched automatically the
//! first time) and a real small instruct GGUF:
//!
//! ```text
//! VALYRIA_TEST_GGUF=/path/to/small-instruct.gguf \
//!   cargo test -p valyria-app --test local_model_e2e -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::Duration;

use valyria_app::{Runtime, RuntimeConfig};
use valyria_model_registry::{Catalog, ModelRole};
use valyria_model_store::{Manifest, ModelStore};
use valyria_types::PermissionMode;

/// A real catalog id. Its declared card (context length, transport
/// preference, ...) drives how the server is started; the bytes on disk
/// are swapped for a small test model so this doesn't need a multi-GB
/// download to run.
const MODEL_ID: &str = "qwen2.5-coder-1.5b-instruct-q8_0";

fn install_test_model(global_dir: &std::path::Path, weights_src: &std::path::Path) {
    let catalog = Catalog::embedded().expect("embedded catalog parses");
    let card = catalog
        .get(MODEL_ID)
        .unwrap_or_else(|| panic!("catalog has no `{MODEL_ID}` — pick a different MODEL_ID"))
        .clone();
    let store = ModelStore::new(global_dir);
    let dest_dir = store.models_dir().join(MODEL_ID);
    std::fs::create_dir_all(&dest_dir).unwrap();
    let weights_file = format!("{MODEL_ID}.gguf");
    std::fs::copy(weights_src, dest_dir.join(&weights_file)).expect("copy test weights");

    let manifest = Manifest {
        card,
        weights_file,
        size_bytes: std::fs::metadata(weights_src).unwrap().len(),
        // Not the real hash of the substituted bytes — `Runtime::open`'s
        // boot path never re-verifies it (that's `verify_integrity`'s
        // job, called only from `doctor`), so this is fine for the boot
        // path this test exercises.
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

#[tokio::test]
#[ignore]
async fn local_backend_activates_and_completes_a_real_task() {
    let Ok(weights) = std::env::var("VALYRIA_TEST_GGUF") else {
        eprintln!("skipping: set VALYRIA_TEST_GGUF to a small instruct .gguf file");
        return;
    };
    let weights = PathBuf::from(weights);

    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let data_dir = temp.path().join("data");
    install_test_model(&data_dir.join("global"), &weights);

    let config = RuntimeConfig::new(ws.path())
        .with_data_dir(data_dir)
        .with_local_models()
        .with_permission_mode(PermissionMode::Autonomous);
    let runtime = Runtime::open(config).await.unwrap();

    // Nothing is bound yet: a task right now would fail fast against
    // `NoModelRuntime`. Activation — the real thing under test — starts a
    // genuine `llama-server` and waits for it to answer `/health`.
    runtime
        .model_activate(MODEL_ID, ModelRole::PrimaryCoder)
        .await
        .expect("model activates and its server becomes ready");

    let task_id = runtime
        .create_and_start_task("Say a short hello, no tools needed.".to_string())
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let final_state = loop {
        let task = runtime.task_status(task_id).await.unwrap();
        if task.state.is_terminal() {
            break task.state;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task stuck in {:?} past the deadline",
            task.state
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };

    println!("task reached terminal state: {final_state:?}");
    // `Failed` is an acceptable (if disappointing) outcome for a live,
    // un-tuned small model — the point of this test is that the run
    // happens through the real inference stack at all, not that this
    // particular 1.5-parameter-class model nails every prompt. What must
    // never happen is a hang (caught by the deadline above) or a panic.
    assert_ne!(
        final_state,
        valyria_types::AgentState::Cancelled,
        "a task nobody cancelled should not end up Cancelled"
    );

    // Removal: the server backing `PrimaryCoder` must stop *before* the
    // weights are deleted, and a `generate` afterward must fail cleanly
    // (no orphaned process still answering on the old port).
    let freed = runtime.model_remove(MODEL_ID).await.expect("model removes");
    assert!(freed > 0, "expected reclaimed bytes, got {freed}");

    let post_removal = runtime
        .create_and_start_task("Say a short hello, no tools needed.".to_string())
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let state_after_removal = loop {
        let task = runtime.task_status(post_removal).await.unwrap();
        if task.state.is_terminal() {
            break task.state;
        }
        if tokio::time::Instant::now() >= deadline {
            let events = runtime.events();
            let all = events
                .replay_since(valyria_events::Seq(0))
                .await
                .unwrap_or_default();
            for e in all.iter().filter(|e| e.task_id == Some(post_removal)) {
                println!(
                    "event: seq={} kind={:?} payload={}",
                    e.seq, e.kind, e.payload
                );
            }
            panic!("post-removal task stuck in {:?} past deadline", task.state);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        state_after_removal,
        valyria_types::AgentState::Failed,
        "a task started after model_remove should fail fast against NoModelRuntime, not hang or succeed"
    );
    println!("post-removal task correctly failed fast: {state_after_removal:?}");

    // `ServerProber` (the real post-install probe `model_install` uses for
    // this backend) is not separately end-to-end tested here: it is the
    // same `LlamaServerRuntime::start_with_timeout` -> `generate` ->
    // `shutdown` sequence this test already ran for real above via
    // `model_activate`, called from one more place. Exercising it through
    // `model_install_with` honestly would need the real ~1.6 GB catalog
    // download (its content-hash check is against the *real* weights, not
    // this test's substituted small file) — out of proportion to the
    // marginal coverage it would add over what's already proven.
}
