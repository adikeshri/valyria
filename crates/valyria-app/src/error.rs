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
    #[error("memory error: {0}")]
    Memory(#[from] valyria_memory::MemoryError),
    #[error("verify error: {0}")]
    Verify(#[from] valyria_verify::VerifyError),
    #[error("git error: {0}")]
    Git(#[from] valyria_git::GitError),
    #[error("index error: {0}")]
    Index(#[from] valyria_index::IndexError),
    #[error("search error: {0}")]
    Search(#[from] valyria_search::SearchError),
    #[error("config error: {0}")]
    Config(#[from] valyria_config::ConfigError),
    /// A `config_set` write failure. Kept distinct from [`Self::Config`]
    /// (which is `#[from]`, so it cannot also carry the specific code) so
    /// the wire error preserves `config.policy_floor_violation` /
    /// `config.unknown_key` / … rather than flattening to `app.config`.
    #[error("config write error: {0}")]
    ConfigWrite(valyria_config::ConfigError),
    #[error("model store error: {0}")]
    ModelStore(#[from] valyria_model_store::ModelStoreError),
    #[error("invalid task id `{0}`")]
    InvalidTaskId(String),
    #[error("unknown purge scope `{0}` (expected: memory, cache, tasks, logs)")]
    UnknownPurgeScope(String),
    #[error("invalid checkpoint id `{0}`")]
    InvalidCheckpointId(String),
    #[error("corrupt workspace id stored in workspace_meta: `{0}`")]
    CorruptWorkspaceId(String),
    #[error("task {0} was not paused, so there is no state to resume it into")]
    NotPaused(valyria_types::TaskId),
    #[error("plan error: {0}")]
    Plan(String),
    #[error("repository surface error: {0}")]
    Repo(String),
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
            AppError::Memory(_) => "app.memory",
            AppError::Verify(_) => "app.verify",
            AppError::Git(e) => e.code(),
            AppError::Index(e) => e.code(),
            AppError::Search(e) => e.code(),
            AppError::Config(_) => "app.config",
            AppError::ConfigWrite(e) => e.code(),
            AppError::ModelStore(e) => e.code(),
            AppError::InvalidTaskId(_) => "app.invalid_task_id",
            AppError::UnknownPurgeScope(_) => "app.unknown_purge_scope",
            AppError::InvalidCheckpointId(_) => "app.invalid_checkpoint_id",
            AppError::CorruptWorkspaceId(_) => "app.corrupt_workspace_id",
            AppError::NotPaused(_) => "app.not_paused",
            AppError::Plan(_) => "app.plan",
            AppError::Repo(_) => "app.repo",
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
            AppError::Memory(_) => false,
            AppError::Verify(_) => false,
            AppError::Git(_) => false,
            AppError::Index(e) => e.retryable(),
            AppError::Search(_) => false,
            AppError::Config(_) => false,
            AppError::ConfigWrite(_) => false,
            AppError::ModelStore(e) => e.retryable(),
            AppError::InvalidTaskId(_) => false,
            AppError::UnknownPurgeScope(_) => false,
            AppError::InvalidCheckpointId(_) => false,
            AppError::CorruptWorkspaceId(_) => false,
            AppError::NotPaused(_) => false,
            AppError::Plan(_) => false,
            AppError::Repo(_) => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
