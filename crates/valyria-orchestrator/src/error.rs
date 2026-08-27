use valyria_types::ErrorCode;

use crate::role::Role;

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("no model bound to role {role:?}")]
    NoBinding { role: Role },
    #[error("model error: {0}")]
    Model(#[from] valyria_model::ModelError),
}

impl ErrorCode for OrchestratorError {
    fn code(&self) -> &'static str {
        match self {
            OrchestratorError::NoBinding { .. } => "orchestrator.no_binding",
            OrchestratorError::Model(_) => "orchestrator.model",
        }
    }

    fn retryable(&self) -> bool {
        match self {
            OrchestratorError::NoBinding { .. } => false,
            OrchestratorError::Model(e) => e.retryable(),
        }
    }
}

pub type Result<T> = std::result::Result<T, OrchestratorError>;
