use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("no ledger entry {0}")]
    UnknownEntry(valyria_types::LedgerEntryId),
    #[error("cannot roll back: the file was modified by someone other than the agent since this entry was recorded")]
    RollbackConflict,
    #[error("cannot roll back: original content for this entry was not retained")]
    ContentNotRetained,
    #[error("vfs error: {0}")]
    Vfs(#[from] valyria_vfs::VfsError),
    #[error("store error: {0}")]
    Store(#[from] valyria_store::StoreError),
}

impl ErrorCode for LedgerError {
    fn code(&self) -> &'static str {
        match self {
            LedgerError::UnknownEntry(_) => "ledger.unknown_entry",
            LedgerError::RollbackConflict => "ledger.rollback_conflict",
            LedgerError::ContentNotRetained => "ledger.content_not_retained",
            LedgerError::Vfs(_) => "ledger.vfs",
            LedgerError::Store(_) => "ledger.store",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, LedgerError>;
