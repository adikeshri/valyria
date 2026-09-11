//! [`NoModelRuntime`]: a null-object `ModelRuntime` bound to a role while
//! its real server is still loading, failed to start, or was torn down.
//! Every call fails fast and honestly rather than the role silently
//! falling back to something scripted — §36's "did the agent stop, is
//! user action needed" answered at the source instead of guessed at by a
//! caller three layers up.

use futures::stream::{self, BoxStream, StreamExt};
use valyria_model::{
    Capabilities, Chunk, Completion, GenerateRequest, Health, ModelError, ModelRuntime,
};
use valyria_util::{CancellationToken, HeuristicTokenCounter, TokenCounter};

pub struct NoModelRuntime {
    reason: String,
}

impl NoModelRuntime {
    /// The real server for `model_id` is booting (`Runtime::open`'s
    /// background boot task, or a fresh `model_activate`).
    pub fn starting(model_id: &str) -> Self {
        Self {
            reason: format!("model `{model_id}` is still starting up"),
        }
    }

    /// The real server for `model_id` failed to start; `detail` is the
    /// underlying error's `Display`.
    pub fn failed(model_id: &str, detail: &str) -> Self {
        Self {
            reason: format!("model `{model_id}` failed to start: {detail}"),
        }
    }

    /// No model is bound to this role at all (never activated, or just
    /// removed).
    pub fn none_bound() -> Self {
        Self {
            reason: "no model is activated for this role — open the Model Manager to install \
                      and activate one"
                .to_string(),
        }
    }
}

#[async_trait::async_trait]
impl ModelRuntime for NoModelRuntime {
    fn capabilities(&self) -> Capabilities {
        // Conservative and inert — nothing should ever successfully
        // generate against this runtime, so these values only matter for
        // display (e.g. a UI reading `capabilities()` before it has tried
        // a call).
        Capabilities {
            context_length: 8192,
            supports_native_tools: false,
            supports_grammar: false,
            supports_streaming: false,
        }
    }

    async fn health(&self) -> Health {
        Health::Unavailable {
            reason: self.reason.clone(),
        }
    }

    fn count_tokens(&self, text: &str) -> usize {
        HeuristicTokenCounter.count(text)
    }

    async fn generate(
        &self,
        _req: GenerateRequest,
        _cancel: CancellationToken,
    ) -> Result<Completion, ModelError> {
        Err(ModelError::Unavailable {
            reason: self.reason.clone(),
        })
    }

    fn stream(
        &self,
        _req: GenerateRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<Chunk, ModelError>> {
        let reason = self.reason.clone();
        stream::once(async move { Err(ModelError::Unavailable { reason }) }).boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> GenerateRequest {
        GenerateRequest::new(vec![valyria_model::Message::user("hi")])
    }

    #[tokio::test]
    async fn generate_is_a_clean_unavailable_error() {
        let rt = NoModelRuntime::starting("qwen-1.5b");
        let err = rt
            .generate(req(), CancellationToken::new())
            .await
            .unwrap_err();
        match err {
            ModelError::Unavailable { reason } => assert!(reason.contains("qwen-1.5b")),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn health_matches_generate() {
        let rt = NoModelRuntime::failed("qwen-1.5b", "engine not found");
        match rt.health().await {
            Health::Unavailable { reason } => {
                assert!(reason.contains("qwen-1.5b"));
                assert!(reason.contains("engine not found"));
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_yields_a_single_error_and_ends() {
        let rt = NoModelRuntime::none_bound();
        let items: Vec<_> = rt.stream(req(), CancellationToken::new()).collect().await;
        assert_eq!(items.len(), 1);
        assert!(items[0].is_err());
    }

    #[test]
    fn count_tokens_matches_the_heuristic() {
        let rt = NoModelRuntime::none_bound();
        assert_eq!(
            rt.count_tokens("hello world"),
            HeuristicTokenCounter.count("hello world")
        );
    }

    #[test]
    fn none_bound_reason_points_at_the_model_manager() {
        let rt = NoModelRuntime::none_bound();
        assert!(rt.reason.to_lowercase().contains("model manager"));
    }
}
