use valyria_types::ErrorCode;

use crate::role::Role;

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("no model bound to role {role:?}")]
    NoBinding { role: Role },
    #[error("model error: {0}")]
    Model(#[from] valyria_model::ModelError),
    #[error("every model in the fallback chain for role {role:?} failed; last error: {last}")]
    AllFallbacksFailed { role: Role, last: String },
    #[error(
        "model output could not be parsed into a tool call after {attempts} attempt(s): {detail}"
    )]
    UnparseableToolCall { attempts: u32, detail: String },
}

impl ErrorCode for OrchestratorError {
    fn code(&self) -> &'static str {
        match self {
            OrchestratorError::NoBinding { .. } => "orchestrator.no_binding",
            OrchestratorError::Model(_) => "orchestrator.model",
            OrchestratorError::AllFallbacksFailed { .. } => "orchestrator.all_fallbacks_failed",
            OrchestratorError::UnparseableToolCall { .. } => "orchestrator.unparseable_tool_call",
        }
    }

    fn retryable(&self) -> bool {
        match self {
            OrchestratorError::NoBinding { .. } => false,
            OrchestratorError::Model(e) => e.retryable(),
            OrchestratorError::AllFallbacksFailed { .. } => true,
            OrchestratorError::UnparseableToolCall { .. } => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, OrchestratorError>;
