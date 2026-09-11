use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum LlamaError {
    /// The inference engine (`llama-server`) could not be resolved or
    /// fetched — not a model problem, an engine problem.
    #[error("inference engine unavailable: {0}")]
    EngineUnavailable(String),
    #[error("could not spawn llama-server: {0}")]
    Spawn(#[from] std::io::Error),
    /// The server never answered `/health` within the deadline. Carries
    /// the tail of its own log so the caller doesn't have to go find it.
    #[error("llama-server did not become ready within {timeout_secs}s: {detail}\n--- log tail ---\n{log_tail}")]
    NotReady {
        timeout_secs: u64,
        detail: String,
        log_tail: String,
    },
    /// The child exited (crashed, or a bad flag) before or during the
    /// readiness poll.
    #[error(
        "llama-server exited (code {code:?}) before becoming ready\n--- log tail ---\n{log_tail}"
    )]
    Exited { code: Option<i32>, log_tail: String },
    #[error("llama-server request failed: {0}")]
    Model(#[from] valyria_model::ModelError),
    #[error("could not build the client for llama-server: {0}")]
    Transport(#[from] valyria_runtime_openai_compat::HttpError),
}

impl ErrorCode for LlamaError {
    fn code(&self) -> &'static str {
        match self {
            LlamaError::EngineUnavailable(_) => "llamacpp.engine_unavailable",
            LlamaError::Spawn(_) => "llamacpp.spawn",
            LlamaError::NotReady { .. } => "llamacpp.not_ready",
            LlamaError::Exited { .. } => "llamacpp.exited",
            LlamaError::Model(_) => "llamacpp.model",
            LlamaError::Transport(_) => "llamacpp.transport",
        }
    }

    fn retryable(&self) -> bool {
        matches!(
            self,
            LlamaError::EngineUnavailable(_) | LlamaError::NotReady { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, LlamaError>;
