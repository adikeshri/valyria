use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("failed to open repository at {path}: {source}")]
    Open {
        path: String,
        #[source]
        source: Box<gix::open::Error>,
    },
    #[error("git operation failed: {0}")]
    Op(String),
    #[error("no commits yet (unborn HEAD)")]
    UnbornHead,
}

impl ErrorCode for GitError {
    fn code(&self) -> &'static str {
        match self {
            GitError::Open { .. } => "git.open",
            GitError::Op(_) => "git.op",
            GitError::UnbornHead => "git.unborn_head",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, GitError>;
