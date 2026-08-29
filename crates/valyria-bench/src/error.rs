//! Harness errors. Layer 6, so `thiserror` with a stable `code` like the
//! rest of the workspace.

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    #[error("bench i/o at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("opening the runtime for bench task `{task}`: {source}")]
    Runtime {
        task: String,
        // Boxed: `AppError` is large and would bloat every `Result` in
        // the harness (`clippy::result_large_err`).
        #[source]
        source: Box<valyria_app::AppError>,
    },

    #[error("bench task `{task}` did not reach a terminal state within {}s", timeout.as_secs())]
    Timeout { task: String, timeout: Duration },

    #[error("bench event replay for `{task}`: {detail}")]
    Events { task: String, detail: String },

    #[error("bench report json: {0}")]
    Json(#[from] serde_json::Error),
}

impl BenchError {
    /// `Runtime` variant builder that boxes the `AppError` for callers.
    pub fn runtime(task: &str, source: valyria_app::AppError) -> Self {
        BenchError::Runtime {
            task: task.to_string(),
            source: Box::new(source),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            BenchError::Io { .. } => "bench.io",
            BenchError::Runtime { .. } => "bench.runtime",
            BenchError::Timeout { .. } => "bench.timeout",
            BenchError::Events { .. } => "bench.events",
            BenchError::Json(_) => "bench.json",
        }
    }
}

pub type Result<T> = std::result::Result<T, BenchError>;
