//! LSP errors.
//!
//! Almost every variant here is *survivable*. LSP is enrichment (§4.13:
//! "never a dependency"), so a server that is missing, slow, crashed, or
//! simply wrong must degrade the answer rather than fail the task — see
//! [`LspError::is_degradation`].

use valyria_types::ErrorCode;

pub type Result<T> = std::result::Result<T, LspError>;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    /// The server binary is not installed. The overwhelmingly common
    /// case on a fresh machine, and not a problem: index-derived results
    /// stand on their own.
    #[error("no language server for `{language}` is installed (looked for `{program}`)")]
    NotInstalled { language: String, program: String },

    #[error("failed to start the `{language}` language server: {source}")]
    Spawn {
        language: String,
        #[source]
        source: std::io::Error,
    },

    #[error("the `{language}` language server exited")]
    ServerGone { language: String },

    #[error("`{method}` timed out after {millis}ms")]
    Timeout { method: &'static str, millis: u64 },

    /// The server answered with a JSON-RPC error object.
    #[error("language server rejected `{method}`: {message} (code {code})")]
    Rejected {
        method: &'static str,
        code: i64,
        message: String,
    },

    #[error("malformed message from the language server: {0}")]
    Protocol(String),

    #[error("io error talking to the language server: {0}")]
    Io(#[from] std::io::Error),
}

impl LspError {
    /// Whether this failure should degrade the answer rather than fail the
    /// caller's operation.
    ///
    /// Every variant qualifies today, which is the point: there is no
    /// LSP failure that should be able to fail a task. The method exists
    /// so that a caller reads as deliberate rather than as a `let _ =`,
    /// and so a future non-survivable variant has somewhere to say so.
    pub fn is_degradation(&self) -> bool {
        true
    }
}

impl ErrorCode for LspError {
    fn code(&self) -> &'static str {
        match self {
            LspError::NotInstalled { .. } => "lsp.not_installed",
            LspError::Spawn { .. } => "lsp.spawn_failed",
            LspError::ServerGone { .. } => "lsp.server_gone",
            LspError::Timeout { .. } => "lsp.timeout",
            LspError::Rejected { .. } => "lsp.rejected",
            LspError::Protocol(_) => "lsp.protocol",
            LspError::Io(_) => "lsp.io",
        }
    }

    fn retryable(&self) -> bool {
        // A timeout or a dead server may well succeed on a restart; a
        // missing binary or a rejected request will not.
        matches!(
            self,
            LspError::Timeout { .. } | LspError::ServerGone { .. } | LspError::Io(_)
        )
    }
}
