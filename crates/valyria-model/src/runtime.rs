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
