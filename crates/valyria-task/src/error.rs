use valyria_types::ErrorCode;
use valyria_types::{AgentState, TaskId};

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("store error: {0}")]
    Store(#[from] valyria_store::StoreError),
    #[error("events error: {0}")]
    Events(#[from] valyria_events::EventsError),
    #[error("serialize error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("task not found: {0}")]
    NotFound(TaskId),
    #[error("corrupt task id in storage: {0}")]
    CorruptId(String),
    #[error("corrupt state value `{raw}` stored for task {task}")]
    CorruptState { task: TaskId, raw: String },
    #[error("illegal state transition for task {task}: {from} -> {to}")]
    IllegalTransition {
        task: TaskId,
        from: AgentState,
        to: AgentState,
    },
    #[error("cannot resume task {task}: it was paused from {expected}, not {actual}")]
    WrongResumeTarget {
        task: TaskId,
        expected: AgentState,
        actual: AgentState,
    },
}

impl ErrorCode for TaskError {
    fn code(&self) -> &'static str {
        match self {
            TaskError::Store(_) => "task.store",
            TaskError::Events(_) => "task.events",
            TaskError::Json(_) => "task.json",
            TaskError::NotFound(_) => "task.not_found",
            TaskError::CorruptId(_) => "task.corrupt_id",
            TaskError::CorruptState { .. } => "task.corrupt_state",
            TaskError::IllegalTransition { .. } => "task.illegal_transition",
            TaskError::WrongResumeTarget { .. } => "task.wrong_resume_target",
        }
    }

    fn retryable(&self) -> bool {
        match self {
            TaskError::Store(e) => e.retryable(),
            TaskError::Events(e) => e.retryable(),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, TaskError>;
