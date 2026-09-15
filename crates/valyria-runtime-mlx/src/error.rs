use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum MlxError {
    /// The MLX engine (a provisioned Python venv with `mlx-lm` installed)
    /// could not be resolved or provisioned — not a model problem, an
    /// engine problem.
    #[error("MLX engine unavailable: {0}")]
    EngineUnavailable(String),
    #[error("could not spawn `python -m mlx_lm.server`: {0}")]
    Spawn(#[from] std::io::Error),
    /// The server never answered `/health` within the deadline. Carries
    /// the tail of its own log so the caller doesn't have to go find it.
    #[error("mlx_lm.server did not become ready within {timeout_secs}s: {detail}\n--- log tail ---\n{log_tail}")]
    NotReady {
        timeout_secs: u64,
        detail: String,
        log_tail: String,
    },
    /// The child exited (crashed, missing model, a bad flag) before or
    /// during the readiness poll.
    #[error(
        "mlx_lm.server exited (code {code:?}) before becoming ready\n--- log tail ---\n{log_tail}"
    )]
    Exited { code: Option<i32>, log_tail: String },
    #[error("mlx_lm.server request failed: {0}")]
    Model(#[from] valyria_model::ModelError),
    #[error("could not build the client for mlx_lm.server: {0}")]
    Transport(#[from] valyria_runtime_openai_compat::HttpError),
}

impl ErrorCode for MlxError {
    fn code(&self) -> &'static str {
        match self {
            MlxError::EngineUnavailable(_) => "mlx.engine_unavailable",
            MlxError::Spawn(_) => "mlx.spawn",
            MlxError::NotReady { .. } => "mlx.not_ready",
            MlxError::Exited { .. } => "mlx.exited",
            MlxError::Model(_) => "mlx.model",
            MlxError::Transport(_) => "mlx.transport",
        }
    }

    fn retryable(&self) -> bool {
        matches!(
            self,
            MlxError::EngineUnavailable(_) | MlxError::NotReady { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, MlxError>;
