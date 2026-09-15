//! The `ModelRuntime` trait (§4.20): the one interface every adapter
//! (fake, llama.cpp, MLX, OpenAI-compatible, ...) implements, so
//! `valyria-orchestrator` never needs to know which backend is serving a
//! request.

use futures::stream::BoxStream;
use valyria_util::CancellationToken;

use crate::capabilities::{Capabilities, Health};
use crate::completion::{Chunk, Completion};
use crate::error::ModelError;
use crate::request::GenerateRequest;

#[async_trait::async_trait]
pub trait ModelRuntime: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    async fn health(&self) -> Health;

    /// Cheap, synchronous token estimate — real adapters back this with
    /// their loaded tokenizer; the fake adapter and any budget logic that
    /// runs before a model is loaded fall back to
    /// `valyria_util::HeuristicTokenCounter`.
    fn count_tokens(&self, text: &str) -> usize;

    async fn generate(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> Result<Completion, ModelError>;

    fn stream(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<Chunk, ModelError>>;
}

/// A locally-spawned model server: it can serve [`ModelRuntime`] calls and
/// it can be told to stop. One shared trait rather than a per-engine copy
/// so a caller (`valyria-app`'s `ModelRuntimeRegistry`, the boot path) can
/// hold `Arc<dyn LocalModelServer>` without caring which engine
/// (`valyria-runtime-llamacpp`, `valyria-runtime-mlx`, ...) is actually
/// running underneath.
#[async_trait::async_trait]
pub trait LocalModelServer: ModelRuntime {
    /// Graceful stop: `SIGTERM`, drain, hard-kill after a bounded timeout.
    /// Idempotent.
    async fn shutdown(&self);
    fn model_id(&self) -> &str;
    /// The loopback port it's serving on — carried on `model_server_ready`
    /// so the app can show it (and, one day, dial it directly for a health
    /// chip).
    fn port(&self) -> u16;
}
