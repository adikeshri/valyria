//! `LlamaServerRuntime`: a [`ModelRuntime`] backed by a managed
//! `llama-server` child process. Owns the process (via [`LlamaServer`])
//! *and* wraps [`OpenAiCompatRuntime`] pointed at its loopback port — this
//! crate contributes process supervision only; every byte of the wire
//! protocol is `valyria-runtime-openai-compat`'s, reused wholesale exactly
//! as that crate's own doc comment always said it would be.

use std::path::PathBuf;
use std::time::Duration;

use futures::stream::BoxStream;
use valyria_model::{
    Capabilities, Chunk, Completion, GenerateRequest, Health, ModelError, ModelRuntime,
};
use valyria_model_registry::ModelCard;
use valyria_runtime_openai_compat::{HttpTransport, OpenAiCompatRuntime, ReqwestTransport};
use valyria_util::CancellationToken;

use crate::error::Result;
use crate::server::{LlamaServer, LlamaServerConfig, DEFAULT_READY_TIMEOUT};

/// A locally-spawned model server: it can serve `ModelRuntime` calls and
/// it can be told to stop. `ModelRuntimeRegistry` (in `valyria-app`) is
/// the only thing meant to call `shutdown` — the orchestrator only ever
/// sees the `ModelRuntime` half.
#[async_trait::async_trait]
pub trait LocalModelServer: ModelRuntime {
    /// Graceful stop: `SIGTERM`, drain, hard-kill after a bounded
    /// timeout. Idempotent.
    async fn shutdown(&self);
    fn model_id(&self) -> &str;
    /// The loopback port it's serving on — carried on `model_server_ready`
    /// so the app can show it (and, one day, dial it directly for a
    /// health chip).
    fn port(&self) -> u16;
}

pub struct LlamaServerRuntime {
    server: LlamaServer,
    inner: OpenAiCompatRuntime<ReqwestTransport>,
    model_id: String,
}

impl LlamaServerRuntime {
    /// Spawn `binary` against `weights`, wait for it to answer `/health`,
    /// and wrap it as a `ModelRuntime` for `card`. `log_path` collects the
    /// child's interleaved stdout/stderr — surfaced in the error if
    /// startup fails, and on disk for later inspection either way.
    pub async fn start(
        binary: PathBuf,
        weights: PathBuf,
        card: &ModelCard,
        log_path: PathBuf,
    ) -> Result<Self> {
        Self::start_with_timeout(binary, weights, card, log_path, DEFAULT_READY_TIMEOUT).await
    }

    pub async fn start_with_timeout(
        binary: PathBuf,
        weights: PathBuf,
        card: &ModelCard,
        log_path: PathBuf,
        ready_timeout: Duration,
    ) -> Result<Self> {
        // llama-server's `-c 0` means "use the model's own trained
        // context"; anything else is clamped to the catalog's own
        // declared window so we never ask for more than the weights
        // support.
        let ctx_size = card.context_length.max(512);
        let config = LlamaServerConfig {
            binary,
            weights,
            ctx_size,
            extra_args: Vec::new(),
            log_path,
        };
        let server = LlamaServer::spawn(config).await?;
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
        let inner = OpenAiCompatRuntime::new(transport, card.id.clone(), capabilities);
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
impl ModelRuntime for LlamaServerRuntime {
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
impl LocalModelServer for LlamaServerRuntime {
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
