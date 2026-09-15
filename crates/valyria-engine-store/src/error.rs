use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum EngineStoreError {
    #[error("no catalog entry for {component} on {os}/{arch}")]
    UnsupportedTarget {
        component: String,
        os: String,
        arch: String,
    },
    #[error("download of {component} {version} failed: {detail}")]
    Download {
        component: String,
        version: String,
        detail: String,
    },
    #[error(
        "integrity check failed for {component} {version}: expected blake3 {expected}, got {actual} — the archive was deleted"
    )]
    IntegrityMismatch {
        component: String,
        version: String,
        expected: String,
        actual: String,
    },
    #[error("could not unpack {component} {version}: {detail}")]
    Unpack {
        component: String,
        version: String,
        detail: String,
    },
    #[error("{component} {version} archive did not contain the expected binary at {member:?}")]
    MissingBinary {
        component: String,
        version: String,
        member: String,
    },
    #[error("download of {component} {version} was cancelled")]
    Cancelled { component: String, version: String },
    /// A step of Python-venv provisioning (`python -m venv`, `pip
    /// install`, …) failed — carries which step and the subprocess's own
    /// stderr, since these are the only diagnostics a user (or an agent
    /// debugging on their behalf) has to go on.
    #[error("mlx venv provisioning failed at `{step}`: {detail}")]
    VenvProvision { step: String, detail: String },
    /// `pip install {component}=={expected}` succeeded but the package
    /// reports a different version than what was pinned — treated as an
    /// error rather than silently trusted, the same discipline as the
    /// engine archive's own blake3 check.
    #[error("{component} version mismatch: pinned {expected}, installed reports {actual}")]
    VersionMismatch {
        component: String,
        expected: String,
        actual: String,
    },
    #[error("engine store i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("engine store serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl ErrorCode for EngineStoreError {
    fn code(&self) -> &'static str {
        match self {
            EngineStoreError::UnsupportedTarget { .. } => "engine_store.unsupported_target",
            EngineStoreError::Download { .. } => "engine_store.download",
            EngineStoreError::IntegrityMismatch { .. } => "engine_store.integrity_mismatch",
            EngineStoreError::Unpack { .. } => "engine_store.unpack",
            EngineStoreError::MissingBinary { .. } => "engine_store.missing_binary",
            EngineStoreError::Cancelled { .. } => "engine_store.cancelled",
            EngineStoreError::VenvProvision { .. } => "engine_store.venv_provision",
            EngineStoreError::VersionMismatch { .. } => "engine_store.version_mismatch",
            EngineStoreError::Io(_) => "engine_store.io",
            EngineStoreError::Serde(_) => "engine_store.serde",
        }
    }

    fn retryable(&self) -> bool {
        matches!(
            self,
            EngineStoreError::Download { .. }
                | EngineStoreError::Io(_)
                | EngineStoreError::Cancelled { .. }
                | EngineStoreError::VenvProvision { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, EngineStoreError>;
