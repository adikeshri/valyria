//! Verification errors.

use valyria_types::ErrorCode;

pub type Result<T> = std::result::Result<T, VerifyError>;

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("process: {0}")]
    Process(#[from] valyria_process::ProcessError),

    #[error("verification storage: {0}")]
    Store(#[from] valyria_store::StoreError),

    #[error("git: {0}")]
    Git(#[from] valyria_git::GitError),

    #[error("workspace: {0}")]
    Vfs(#[from] valyria_vfs::VfsError),

    #[error("serialize verification record: {0}")]
    Json(#[from] serde_json::Error),

    /// Discovery found no build/test/lint command anywhere in the
    /// workspace — no manifest, no CI workflow, no script convention. The
    /// caller decides what to do (a repo genuinely may have nothing to
    /// run); this is not on its own a failure of the task.
    #[error("no verification tooling could be discovered in the workspace")]
    NoToolingDiscovered,

    /// A command the strategy wanted to run names a program the caller
    /// never validated (see `discovery::validate`). Running an
    /// unvalidated program is refused rather than risked.
    #[error("verification command `{0}` was never validated by execution")]
    Unvalidated(String),
}

impl ErrorCode for VerifyError {
    fn code(&self) -> &'static str {
        match self {
            VerifyError::Process(_) => "verify.process",
            VerifyError::Store(_) => "verify.store",
            VerifyError::Git(_) => "verify.git",
            VerifyError::Vfs(_) => "verify.vfs",
            VerifyError::Json(_) => "verify.json",
            VerifyError::NoToolingDiscovered => "verify.no_tooling",
            VerifyError::Unvalidated(_) => "verify.unvalidated",
        }
    }

    fn retryable(&self) -> bool {
        matches!(self, VerifyError::Process(e) if e.retryable())
    }
}
