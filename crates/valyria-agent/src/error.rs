use valyria_types::{ErrorCode, TaskId};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("task error: {0}")]
    Task(#[from] valyria_task::TaskError),
    #[error("orchestrator error: {0}")]
    Orchestrator(#[from] valyria_orchestrator::OrchestratorError),
    #[error("context error: {0}")]
    Context(#[from] valyria_context::ContextError),
    #[error("malformed model completion: {detail}")]
    MalformedCompletion { detail: String },
    #[error("tool runtime returned unknown tool `{0}`, but the registry is closed")]
    UnknownTool(String),
    #[error("task {0} is not currently waiting for a permission decision")]
    NotWaitingForPermission(TaskId),
    #[error("task {0} has no pending tool call to resolve")]
    NoPendingToolCall(TaskId),
    #[error("plan error: {0}")]
    Plan(String),
    #[error("checkpoint rollback failed: {0}")]
    Rollback(String),
}

impl ErrorCode for AgentError {
    fn code(&self) -> &'static str {
        match self {
            AgentError::Task(_) => "agent.task",
            AgentError::Orchestrator(_) => "agent.orchestrator",
            AgentError::Context(_) => "agent.context",
            AgentError::MalformedCompletion { .. } => "agent.malformed_completion",
            AgentError::UnknownTool(_) => "agent.unknown_tool",
            AgentError::NotWaitingForPermission(_) => "agent.not_waiting_for_permission",
            AgentError::NoPendingToolCall(_) => "agent.no_pending_tool_call",
            AgentError::Plan(_) => "agent.plan",
            AgentError::Rollback(_) => "agent.rollback",
        }
    }

    fn retryable(&self) -> bool {
        match self {
            AgentError::Task(e) => e.retryable(),
            AgentError::Orchestrator(e) => e.retryable(),
            AgentError::Context(e) => e.retryable(),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, AgentError>;
