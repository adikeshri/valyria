use valyria_types::ErrorCode;
use valyria_util::ContentHash;

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("workspace path error: {0}")]
    Vfs(#[from] valyria_vfs::VfsError),
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("file was modified since the agent last observed it: expected {expected:?}, found {actual:?} (see D6/§25 — this is external-modification protection, not a bug)")]
    PreconditionFailed {
        expected: Option<ContentHash>,
        actual: Option<ContentHash>,
    },
    #[error("anchor text not found in file")]
    AnchorNotFound,
    #[error("anchor text is ambiguous: found {count} occurrences, expected exactly 1")]
    AnchorAmbiguous { count: usize },
    #[error("cannot apply an exact-replacement or diff strategy to a file that doesn't exist yet")]
    NoExistingContent,
    #[error("failed to parse unified diff: {0}")]
    PatchParse(String),
    #[error("patch did not apply cleanly: {0}")]
    PatchApply(String),
    #[error("whole-file replacement would shrink the file by {shrink_pct}% without `force`; pass force:true if this is intentional")]
    SizeGuardTripped { shrink_pct: u32 },
    #[error("whole-file replacement requires a non-empty reason")]
    MissingReason,
    #[error("{strategy} is not implemented yet (lands with {owning_crate} in a later phase)")]
    NotYetImplemented {
        strategy: &'static str,
        owning_crate: &'static str,
    },
    #[error("the resulting content does not match what the strategy was expected to produce: {0}")]
    VerificationFailed(String),
}

impl ErrorCode for EditError {
    fn code(&self) -> &'static str {
        match self {
            EditError::Vfs(_) => "edit.vfs",
            EditError::Io { .. } => "edit.io",
            EditError::PreconditionFailed { .. } => "edit.precondition_failed",
            EditError::AnchorNotFound => "edit.anchor_not_found",
            EditError::AnchorAmbiguous { .. } => "edit.anchor_ambiguous",
            EditError::NoExistingContent => "edit.no_existing_content",
            EditError::PatchParse(_) => "edit.patch_parse",
            EditError::PatchApply(_) => "edit.patch_apply",
            EditError::SizeGuardTripped { .. } => "edit.size_guard_tripped",
            EditError::MissingReason => "edit.missing_reason",
            EditError::NotYetImplemented { .. } => "edit.not_yet_implemented",
            EditError::VerificationFailed(_) => "edit.verification_failed",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, EditError>;
