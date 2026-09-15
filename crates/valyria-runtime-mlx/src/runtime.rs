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
    Capabilities, Chunk, Completion, GenerateRequest, Health, ModelError, ModelRuntime,
};
use valyria_model_registry::ModelCard;
use valyria_runtime_openai_compat::{HttpTransport, OpenAiCompatRuntime, ReqwestTransport};
use valyria_util::CancellationToken;

use crate::error::Result;
use crate::server::{MlxServer, MlxServerConfig, DEFAULT_READY_TIMEOUT};

/// A locally-spawned model server: it can serve `ModelRuntime` calls and
/// it can be told to stop. `LlamaServerRuntime` implements the same
/// trait; a caller holding a `Box<dyn LocalModelServer>` doesn't need to
/// know which engine is actually running underneath.
#[async_trait::async_trait]
pub trait LocalModelServer: ModelRuntime {
    async fn shutdown(&self);
    fn model_id(&self) -> &str;
    fn port(&self) -> u16;
}

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
