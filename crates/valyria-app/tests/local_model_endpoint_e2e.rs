//! Real network, real external server: proves `model_endpoint_add` +
//! `model_activate` + a real task actually work end to end against a
//! server Core did **not** spawn or supervise — the defining trait of an
//! "external endpoint" as opposed to the managed llama.cpp/MLX adapters.
//!
//! Deliberately spawns the target server by hand with a plain
//! `std::process::Command` (not through `valyria-runtime-mlx`) so this
//! test exercises exactly the code path a real Ollama/LM Studio/vLLM user
//! would hit: `Runtime` never sees the process, only `base_url`.
//!
//! `#[ignore]`d — needs the network (mlx-lm + a real small model, both
//! likely already cached from `local_mlx_e2e.rs`) and Apple Silicon
//! macOS, where MLX runs:
//!
//! ```text
//! cargo test -p valyria-app --test local_model_endpoint_e2e -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use valyria_app::{ModelEndpointOptions, Runtime, RuntimeConfig};
use valyria_model_registry::ModelRole;
use valyria_types::PermissionMode;

const REMOTE_MODEL_NAME: &str = "mlx-community/SmolLM-135M-Instruct-4bit";

struct Killed(Child);
impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Provision (or reuse) a real `mlx-lm` venv the same way `valyria-
/// engine-store::MlxVenvStore` does, rooted under this test's own scratch
/// dir rather than `~/.valyria` — this test spawns its server by hand
/// precisely so it exercises the "Core never provisioned this" case, but
/// it still needs *some* real interpreter with `mlx-lm` installed to
/// spawn from.
async fn provisioned_python() -> Option<PathBuf> {
    let root = std::env::temp_dir().join("valyria-endpoint-e2e-mlx-venv");
    let store = valyria_engine_store::MlxVenvStore::new(&root);
    if let Some(python) = store.resolve(valyria_engine_store::MLX_LM_VERSION) {
        return Some(python);
    }
    let base_python = valyria_engine_store::find_system_python()?;
    store
        .provision(&base_python, valyria_engine_store::MLX_LM_VERSION)
        .await
        .ok()
}

#[tokio::test]
#[ignore]
async fn external_endpoint_activates_and_completes_a_real_task() {
    if !(cfg!(target_os = "macos") && cfg!(target_arch = "aarch64")) {
        eprintln!("skipping: this test's server needs MLX (Apple Silicon macOS)");
        return;
    }
    let Some(python) = provisioned_python().await else {
        eprintln!("skipping: could not provision an mlx-lm venv (offline?)");
        return;
    };

    let port = free_port();
    let mut cmd = Command::new(&python);
    cmd.args([
        "-m",
        "mlx_lm",
        "server",
        "--model",
        REMOTE_MODEL_NAME,
        "--host",
        "127.0.0.1",
        "--port",
        &port.to_string(),
        "--use-default-chat-template",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let child = cmd.spawn().expect("spawn a real mlx_lm server by hand");
    let _guard = Killed(child);

    let base_url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if client
            .get(format!("{base_url}/health"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "hand-spawned mlx_lm server never answered /health"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let config = RuntimeConfig::new(ws.path())
        .with_data_dir(temp.path().join("data"))
        .with_local_models()
        .with_permission_mode(PermissionMode::Autonomous);
    let runtime = Runtime::open(config).await.unwrap();

    runtime
        .model_endpoint_add(
            "hand-spawned-mlx",
            &base_url,
            ModelEndpointOptions {
                display_name: Some("Hand-spawned MLX (test)"),
                remote_model_name: Some(REMOTE_MODEL_NAME),
                context_length: Some(4096),
                supports_native_tools: Some(true),
                supports_grammar: Some(false),
            },
        )
        .await
        .expect("endpoint registers");

    runtime
        .model_activate("hand-spawned-mlx", ModelRole::PrimaryCoder)
        .await
        .expect("endpoint activates — no process for Core to wait on, so this is synchronous");

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
    assert_ne!(
        final_state,
        valyria_types::AgentState::Cancelled,
        "a task nobody cancelled should not end up Cancelled"
    );

    // Removing the endpoint while it's active must unbind PrimaryCoder —
    // proven here (unlike the fake-backend unit test) against the real
    // in-memory orchestrator state.
    runtime
        .model_endpoint_remove("hand-spawned-mlx")
        .await
        .expect("endpoint removes");
    let post_removal = runtime
        .create_and_start_task("Say a short hello, no tools needed.".to_string())
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
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
        "a task started after model_endpoint_remove should fail fast against NoModelRuntime"
    );
    println!("post-removal task correctly failed fast: {state_after_removal:?}");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
