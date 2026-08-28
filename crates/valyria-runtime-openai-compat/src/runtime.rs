//! `OpenAiCompatRuntime`: a [`ModelRuntime`] backed by any local
//! OpenAI-compatible server (llama-server, vLLM, Ollama, LM Studio) reached
//! through an [`HttpTransport`].

use futures::stream::{self, BoxStream, StreamExt};
use valyria_model::{
    Capabilities, Chunk, Completion, GenerateRequest, Health, ModelError, ModelRuntime,
};
use valyria_util::CancellationToken;

use crate::transport::{HttpError, HttpTransport};
use crate::wire;

const CHAT_PATH: &str = "/v1/chat/completions";
const HEALTH_PATH: &str = "/health";

pub struct OpenAiCompatRuntime<T: HttpTransport> {
    transport: T,
    model: String,
    capabilities: Capabilities,
}

impl<T: HttpTransport> OpenAiCompatRuntime<T> {
    /// `capabilities` is what the caller probed at install time (or a
    /// conservative default). The adapter does not re-probe on every call.
    pub fn new(transport: T, model: impl Into<String>, capabilities: Capabilities) -> Self {
        Self {
            transport,
            model: model.into(),
            capabilities,
        }
    }

    /// A conservative capability set for a chat server whose context length
    /// is known but whose tool-calling reliability is not yet probed.
    pub fn conservative_capabilities(context_length: u32) -> Capabilities {
        Capabilities {
            context_length,
            supports_native_tools: true,
            supports_grammar: false,
            supports_streaming: true,
        }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }
}

fn map_http(e: HttpError) -> ModelError {
    match e {
        HttpError::Unreachable(r) => ModelError::Unavailable { reason: r },
        HttpError::Status { status, body } => ModelError::Unavailable {
            reason: format!("HTTP {status}: {body}"),
        },
        HttpError::Malformed(d) => ModelError::MalformedOutput { detail: d },
    }
}

#[async_trait::async_trait]
impl<T: HttpTransport + 'static> ModelRuntime for OpenAiCompatRuntime<T> {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    async fn health(&self) -> Health {
        match self.transport.get(HEALTH_PATH).await {
            Ok(_) => Health::Healthy,
            Err(HttpError::Unreachable(reason)) => Health::Unavailable { reason },
            Err(e) => Health::Degraded {
                reason: e.to_string(),
            },
        }
    }

    fn count_tokens(&self, text: &str) -> usize {
        // The server's `/tokenize` endpoint is async and this method is
        // sync; budget math uses the same ~4-chars/token heuristic as
        // `valyria_util::HeuristicTokenCounter` until a real tokenizer is
        // wired in (Phase 9 follow-up).
        text.chars()
            .count()
            .div_ceil(4)
            .max(usize::from(!text.is_empty()))
    }

    async fn generate(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> Result<Completion, ModelError> {
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let body = wire::build_chat_request(&self.model, &req, false);
        let bytes = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ModelError::Cancelled),
            res = self.transport.post_json(CHAT_PATH, body) => res.map_err(map_http)?,
        };
        wire::parse_completion(&bytes).map_err(|detail| ModelError::MalformedOutput { detail })
    }

    fn stream(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<Chunk, ModelError>> {
        if cancel.is_cancelled() {
            return stream::once(async { Err(ModelError::Cancelled) }).boxed();
        }
        let body = wire::build_chat_request(&self.model, &req, true);
        let sse = self.transport.post_sse(CHAT_PATH, body);

        stream::unfold(
            (sse, cancel, false),
            |(mut sse, cancel, finished)| async move {
                if finished {
                    return None;
                }
                if cancel.is_cancelled() {
                    return Some((Err(ModelError::Cancelled), (sse, cancel, true)));
                }
                match sse.next().await {
                    None => None,
                    Some(Err(e)) => Some((Err(map_http(e)), (sse, cancel, true))),
                    Some(Ok(payload)) => {
                        let payload = payload.trim().to_string();
                        if payload == "[DONE]" {
                            let done = Chunk {
                                delta: String::new(),
                                tool_call_delta: None,
                                done: true,
                            };
                            return Some((Ok(done), (sse, cancel, true)));
                        }
                        match wire::parse_stream_chunk(&payload) {
                            Ok(chunk) => {
                                let stop = chunk.done;
                                Some((Ok(chunk), (sse, cancel, stop)))
                            }
                            Err(detail) => Some((
                                Err(ModelError::MalformedOutput { detail }),
                                (sse, cancel, true),
                            )),
                        }
                    }
                }
            },
        )
        .boxed()
    }
}
