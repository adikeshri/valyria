use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("failed to spawn `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to wait on child process: {0}")]
    Wait(std::io::Error),
}

impl ErrorCode for ProcessError {
    fn code(&self) -> &'static str {
        match self {
            ProcessError::Spawn { .. } => "process.spawn",
            ProcessError::Wait(_) => "process.wait",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, ProcessError>;
