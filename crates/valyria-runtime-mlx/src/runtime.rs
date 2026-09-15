//! `MlxServerRuntime`: the [`ModelRuntime`] adapter that owns one
//! [`MlxServer`] child process and speaks to it through the shared
//! [`OpenAiCompatRuntime`] wire-protocol client — the exact same
//! composition `valyria-runtime-llamacpp::LlamaServerRuntime` uses, just
//! with a different process-supervision half. Every method delegates to
//! `inner`; this type's own job is process lifecycle only.

use std::path::PathBuf;
use std::time::Duration;

use futures::stream::BoxStream;
use valyria_model::{
    Capabilities, Chunk, Completion, GenerateRequest, Health, LocalModelServer, ModelError,
    ModelRuntime,
};
use valyria_model_registry::ModelCard;
use valyria_runtime_openai_compat::{HttpTransport, OpenAiCompatRuntime, ReqwestTransport};
use valyria_util::CancellationToken;

use crate::error::Result;
use crate::server::{MlxServer, MlxServerConfig, DEFAULT_READY_TIMEOUT};

pub struct MlxServerRuntime {
    server: MlxServer,
    inner: OpenAiCompatRuntime<ReqwestTransport>,
    model_id: String,
}

impl MlxServerRuntime {
    /// Spawn `python -m mlx_lm.server` against `model_dir` (a local
    /// Hugging Face-layout directory — MLX has no single-file format the
    /// way GGUF is one), wait for it to answer `/health`, and wrap it as
    /// a `ModelRuntime` for `card`.
    pub async fn start(
        python: PathBuf,
        model_dir: PathBuf,
        card: &ModelCard,
        log_path: PathBuf,
    ) -> Result<Self> {
        Self::start_with_timeout(python, model_dir, card, log_path, DEFAULT_READY_TIMEOUT).await
    }

    pub async fn start_with_timeout(
        python: PathBuf,
        model_dir: PathBuf,
        card: &ModelCard,
        log_path: PathBuf,
        ready_timeout: Duration,
    ) -> Result<Self> {
        // `mlx_lm.server` supports per-request model switching: it reads
        // the request body's own `"model"` field (defaulting to the CLI
        // `--model` only when that field is absent) and, on any mismatch,
        // tries to *load a different model by that name* — treating it as
        // a fresh Hugging Face repo id, not a display label. Confirmed
        // live: sending `card.id` (valyria's catalog id, e.g. `"qwen2.5-
        // coder-7b-instruct-mlx-4bit"`) instead of the real repo id here
        // made a real, already-booted server 404 trying to "load" that
        // catalog id as a repo. So unlike `LlamaServerRuntime` (where
        // `llama-server` ignores the field entirely and this distinction
        // never mattered), the wire `"model"` name here *must* be the
        // same string the process was actually started with —
        // `model_dir` — not the catalog id.
        let wire_model_name = model_dir.to_string_lossy().into_owned();
        let config = MlxServerConfig {
            python,
            model_dir,
            extra_args: Vec::new(),
            log_path,
        };
        let server = MlxServer::spawn(config).await?;
        let base = format!("http://127.0.0.1:{}", server.port());
        let transport = ReqwestTransport::new(base)?;

        let probe_transport = transport.clone();
        server
            .await_ready(ready_timeout, || {
                let t = probe_transport.clone();
                async move { t.get("/health").await.is_ok() }
            })
            .await?;

        let capabilities = caps_from_card(card);
        let inner = OpenAiCompatRuntime::new(transport, wire_model_name, capabilities);
        Ok(Self {
            server,
            inner,
            model_id: card.id.clone(),
        })
    }

    pub fn port(&self) -> u16 {
        self.server.port()
    }
}

fn caps_from_card(card: &ModelCard) -> Capabilities {
    Capabilities {
        context_length: card.context_length,
        supports_native_tools: card.supports_native_tools,
        supports_grammar: card.supports_grammar,
        supports_streaming: true,
    }
}

#[async_trait::async_trait]
impl ModelRuntime for MlxServerRuntime {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn health(&self) -> Health {
        self.inner.health().await
    }

    fn count_tokens(&self, text: &str) -> usize {
        self.inner.count_tokens(text)
    }

    async fn generate(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> std::result::Result<Completion, ModelError> {
        self.inner.generate(req, cancel).await
    }

    fn stream(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, std::result::Result<Chunk, ModelError>> {
        self.inner.stream(req, cancel)
    }
}

#[async_trait::async_trait]
impl LocalModelServer for MlxServerRuntime {
    async fn shutdown(&self) {
        self.server.shutdown().await;
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn port(&self) -> u16 {
        self.server.port()
    }
}
