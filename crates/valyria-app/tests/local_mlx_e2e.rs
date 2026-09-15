//! Real local inference through the full `Runtime`, for the MLX engine —
//! the MLX sibling of `local_model_e2e.rs`. Unlike that test, there is no
//! weights file to hand-install: an `engine: "mlx"` catalog card's
//! `source_url` is a Hugging Face repo id, and `ModelStore::
//! install_with_progress` (see `valyria-model-store::store`'s
//! `EngineKind::Mlx` branch) deliberately skips its own byte-range
//! download for one, delegating the actual multi-file transfer to
//! `mlx_lm.server`/`huggingface_hub` on first boot. So this test drives
//! the exact same public `Runtime::model_install` a real user calls,
//! start to finish, rather than seeding a substituted file.
//!
//! `#[ignore]`d — needs the network (both for `pip install mlx-lm` the
//! first time, and to fetch the real ~4.3 GB catalog model from the
//! Hugging Face Hub) and only runs on Apple Silicon macOS, where MLX
//! itself runs:
//!
//! ```text
//! cargo test -p valyria-app --test local_mlx_e2e -- --ignored --nocapture
//! ```

use std::time::Duration;

use valyria_app::{Runtime, RuntimeConfig};
use valyria_model_registry::ModelRole;
use valyria_types::PermissionMode;

const MODEL_ID: &str = "qwen2.5-coder-7b-instruct-mlx-4bit";

#[tokio::test]
#[ignore]
async fn mlx_backend_installs_activates_and_completes_a_real_task() {
    if !(cfg!(target_os = "macos") && cfg!(target_arch = "aarch64")) {
        eprintln!("skipping: MLX only runs on Apple Silicon macOS");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let data_dir = temp.path().join("data");

    let config = RuntimeConfig::new(ws.path())
        .with_data_dir(data_dir)
        .with_local_models()
        .with_permission_mode(PermissionMode::Autonomous);
    let runtime = Runtime::open(config).await.unwrap();

    // Install: the real download+provisioning path this crate's own
    // `model_install_with` runs for any user. For an `engine: mlx` card
    // this provisions the mlx-lm venv (once, ~25s) and then runs the real
    // post-install probe (`ServerProber`), which itself boots a real
    // `mlx_lm.server` against the real model and asks a trivial prompt.
    runtime
        .model_install(MODEL_ID, true)
        .await
        .expect("install starts");

    // Generous: this covers the one-time `mlx-lm` venv provisioning plus
    // a genuine multi-GB first-time download of the real model from the
    // Hugging Face Hub (see `ServerProber`'s `MLX_PROBE_READY_TIMEOUT`
    // doc comment — the same real-world constraint this deadline exists
    // to accommodate).
    let install_deadline = tokio::time::Instant::now() + Duration::from_secs(2400);
    loop {
        let view = runtime.model_inspect(MODEL_ID).await.unwrap();
        if view.installed {
            break;
        }
        // Fail fast on a genuine `model_install_failed` rather than
        // blocking until the deadline for something that already isn't
        // going to succeed.
        let events = runtime
            .events()
            .replay_since(valyria_events::Seq(0))
            .await
            .unwrap_or_default();
        if let Some(failure) = events
            .iter()
            .find(|e| e.kind == valyria_events::EventKind::ModelInstallFailed)
        {
            panic!("model_install_failed: {}", failure.payload);
        }
        assert!(
            tokio::time::Instant::now() < install_deadline,
            "model did not finish installing (or failed) within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    println!("install completed");

    // Activation boots a second, independent `mlx_lm.server` (the probe's
    // own server was already shut down) and waits for it to answer
    // `/health` before this returns.
    runtime
        .model_activate(MODEL_ID, ModelRole::PrimaryCoder)
        .await
        .expect("mlx model activates and its server becomes ready");

    let task_id = runtime
        .create_and_start_task("Say a short hello, no tools needed.".to_string())
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
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
    // Same standard as `local_model_e2e.rs`: a live, un-tuned model
    // failing a particular prompt is an acceptable outcome. A hang
    // (caught by the deadline) or a panic is not.
    assert_ne!(
        final_state,
        valyria_types::AgentState::Cancelled,
        "a task nobody cancelled should not end up Cancelled"
    );

    let freed = runtime.model_remove(MODEL_ID).await.expect("model removes");
    // Reclaimed bytes are expected to be near-zero for an MLX model: the
    // actual weights live in mlx_lm's own Hugging Face cache
    // (`~/.cache/huggingface`), outside anything this store downloaded or
    // tracks — see `MLX_LAZY_DOWNLOAD_SENTINEL`'s doc comment. The point
    // of this call is that the *server* backing `PrimaryCoder` stops
    // before returning, proven below.
    println!("model_remove freed {freed} bytes from valyria's own store");

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
        assert!(
            tokio::time::Instant::now() < deadline,
            "post-removal task stuck in {:?} past deadline",
            task.state
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        state_after_removal,
        valyria_types::AgentState::Failed,
        "a task started after model_remove should fail fast against NoModelRuntime, not hang or succeed"
    );
    println!("post-removal task correctly failed fast: {state_after_removal:?}");
}
