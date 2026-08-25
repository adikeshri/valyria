use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("failed to canonicalize sandbox path {path}: {source}")]
    Canonicalize {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl ErrorCode for SandboxError {
    fn code(&self) -> &'static str {
        match self {
            SandboxError::Canonicalize { .. } => "sandbox.canonicalize",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, SandboxError>;
