use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("invalid input for {tool}: {reason}")]
    InvalidInput { tool: &'static str, reason: String },
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("permission required: {0}")]
    PermissionRequired(String),
    #[error("authorization does not match the request that was actually executed")]
    AuthorizationMismatch,
    #[error("vfs error: {0}")]
    Vfs(#[from] valyria_vfs::VfsError),
    #[error("edit error: {0}")]
    Edit(#[from] valyria_edit::EditError),
    #[error("ledger error: {0}")]
    Ledger(#[from] valyria_ledger::LedgerError),
    #[error("process error: {0}")]
    Process(#[from] valyria_process::ProcessError),
    #[error("git error: {0}")]
    Git(#[from] valyria_git::GitError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl ErrorCode for ToolError {
    fn code(&self) -> &'static str {
        match self {
            ToolError::UnknownTool(_) => "tools.unknown_tool",
            ToolError::InvalidInput { .. } => "tools.invalid_input",
            ToolError::PermissionDenied(_) => "tools.permission_denied",
            ToolError::PermissionRequired(_) => "tools.permission_required",
            ToolError::AuthorizationMismatch => "tools.authorization_mismatch",
            ToolError::Vfs(_) => "tools.vfs",
            ToolError::Edit(_) => "tools.edit",
            ToolError::Ledger(_) => "tools.ledger",
            ToolError::Process(_) => "tools.process",
            ToolError::Git(_) => "tools.git",
            ToolError::Io(_) => "tools.io",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

impl ToolError {
    /// Shorthand for `ErrorCode::code`, so call sites building a
    /// `ToolOutcome::failure(code, message)` from a `ToolError` don't need
    /// `use valyria_types::ErrorCode` at every call site.
    pub fn code_str(&self) -> &'static str {
        self.code()
    }
}

pub type Result<T> = std::result::Result<T, ToolError>;
