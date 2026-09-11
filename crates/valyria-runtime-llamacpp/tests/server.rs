//! Real `llama-server` + a real (small) GGUF, driven through the actual
//! `LlamaServerRuntime`/`ModelRuntime` surface — the thing `Orchestrator`
//! calls in production. Complements the unit tests in `src/server.rs`
//! (which use a synthetic child process) by proving the whole stack: spawn
//! → `/health` → a real chat completion → graceful shutdown → the process
//! is actually gone.
//!
//! `#[ignore]`d — needs the network the first time (to fetch the engine
//! via `valyria-engine-store`) and a real model file. Run explicitly:
//!
//! ```text
//! VALYRIA_TEST_GGUF=/path/to/small.gguf \
//!   cargo test -p valyria-runtime-llamacpp --test server -- --ignored --nocapture
//! ```
//!
//! `VALYRIA_LLAMA_SERVER` overrides the engine binary directly (skips the
//! download) when already resolved elsewhere, e.g. by a prior
//! `valyria-engine-store` install.

use std::path::PathBuf;
use std::time::Duration;

use valyria_hardware::ModelRequirement;
use valyria_model::{GenerateRequest, Message, ModelRuntime, SamplingParams};
use valyria_model_registry::{ModelCard, Quantization, TransportPreference};
use valyria_runtime_llamacpp::LlamaServerRuntime;
use valyria_util::CancellationToken;

fn test_card(weights: &PathBuf) -> ModelCard {
    ModelCard {
        id: "test-model".into(),
        family: "qwen2.5".into(),
        display_name: "Test model".into(),
        parameters_b: 0.5,
        quantization: Quantization::Q8_0,
        context_length: 2048,
        file_size_bytes: std::fs::metadata(weights).map(|m| m.len()).unwrap_or(0),
        chat_template: None,
        recommended_sampling: SamplingParams::default(),
        role_suitability: Default::default(),
        requirement: ModelRequirement {
            min_ram_bytes: 1_000_000_000,
            min_vram_bytes: None,
        },
        transport_preference: TransportPreference::Native,
        supports_native_tools: true,
        supports_grammar: false,
        source_url: "test://local".into(),
        content_hash: "test".into(),
        license_name: "test".into(),
        license_url: None,
    }
}

async fn resolve_engine_binary() -> PathBuf {
    if let Ok(p) = std::env::var("VALYRIA_LLAMA_SERVER") {
        return PathBuf::from(p);
    }
    // Fetch it ourselves through the real engine store, same as
    // `valyria-app` would on first boot.
    let dir = std::env::var("VALYRIA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_home());
    let store = valyria_engine_store::EngineStore::new(&dir);
    if let Some(bin) = store.resolve("llama.cpp") {
        return bin;
    }
    let catalog = valyria_engine_store::Catalog::embedded().unwrap();
    let fetcher = valyria_engine_store::HttpFetcher::new().unwrap();
    let cancel = CancellationToken::new();
    store
        .install_with_progress(&catalog, "llama.cpp", &fetcher, &cancel, &|_| {})
        .await
        .expect("engine install")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[tokio::test]
#[ignore]
async fn boot_generate_and_shut_down_a_real_server() {
    let Ok(weights) = std::env::var("VALYRIA_TEST_GGUF") else {
        eprintln!("skipping: set VALYRIA_TEST_GGUF to a small .gguf file");
        return;
    };
    let weights = PathBuf::from(weights);
    let binary = resolve_engine_binary().await;
    let card = test_card(&weights);

    let rt = LlamaServerRuntime::start_with_timeout(
        binary,
        weights,
        &card,
        std::env::temp_dir().join("llama-server-test.log"),
        Duration::from_secs(60),
    )
    .await
    .expect("server starts and becomes ready");

    // health() goes through the same transport as generate().
    let health = rt.health().await;
    assert!(health.is_usable(), "expected usable health, got {health:?}");

    let req = GenerateRequest::new(vec![Message::user("Reply with the single word: hello")])
        .with_sampling(SamplingParams {
            temperature: 0.0,
            top_p: 1.0,
            max_tokens: Some(8),
            stop: vec![],
        });
    let completion = rt
        .generate(req, CancellationToken::new())
        .await
        .expect("a real completion");
    assert!(
        !completion.text.trim().is_empty(),
        "expected non-empty text, got {completion:?}"
    );

    use valyria_runtime_llamacpp::LocalModelServer;
    rt.shutdown().await;
    let health_after = rt.health().await;
    assert!(
        !health_after.is_usable(),
        "server should be unreachable after shutdown, got {health_after:?}"
    );
}
