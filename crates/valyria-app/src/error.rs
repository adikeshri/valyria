use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("store error: {0}")]
    Store(#[from] valyria_store::StoreError),
    #[error("events error: {0}")]
    Events(#[from] valyria_events::EventsError),
    #[error("task error: {0}")]
    Task(#[from] valyria_task::TaskError),
    #[error("agent error: {0}")]
    Agent(#[from] valyria_agent::AgentError),
    #[error("vfs error: {0}")]
    Vfs(#[from] valyria_vfs::VfsError),
    #[error("ledger error: {0}")]
    Ledger(#[from] valyria_ledger::LedgerError),
    #[error("scenario error: {0}")]
    Scenario(#[from] valyria_runtime_fake::FakeRuntimeError),
    #[error("invalid task id `{0}`")]
    InvalidTaskId(String),
    #[error("corrupt workspace id stored in workspace_meta: `{0}`")]
    CorruptWorkspaceId(String),
    #[error("task {0} was not paused, so there is no state to resume it into")]
    NotPaused(valyria_types::TaskId),
}

impl ErrorCode for AppError {
    fn code(&self) -> &'static str {
        match self {
            AppError::Store(_) => "app.store",
            AppError::Events(_) => "app.events",
            AppError::Task(_) => "app.task",
            AppError::Agent(_) => "app.agent",
            AppError::Vfs(_) => "app.vfs",
            AppError::Ledger(_) => "app.ledger",
            AppError::Scenario(_) => "app.scenario",
            AppError::InvalidTaskId(_) => "app.invalid_task_id",
            AppError::CorruptWorkspaceId(_) => "app.corrupt_workspace_id",
            AppError::NotPaused(_) => "app.not_paused",
        }
    }

    fn retryable(&self) -> bool {
        match self {
            AppError::Store(e) => e.retryable(),
            AppError::Events(e) => e.retryable(),
            AppError::Task(e) => e.retryable(),
            AppError::Agent(e) => e.retryable(),
            AppError::Vfs(_) => false,
            AppError::Ledger(_) => false,
            AppError::Scenario(_) => false,
            AppError::InvalidTaskId(_) => false,
            AppError::CorruptWorkspaceId(_) => false,
            AppError::NotPaused(_) => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
