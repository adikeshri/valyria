use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("no model in the catalog with id {id:?}")]
    UnknownModel { id: String },
    #[error("no model in the catalog is suitable for role {role} on this hardware")]
    NoSuitableModel { role: String },
    #[error("embedded catalog is malformed: {detail}")]
    MalformedCatalog { detail: String },
    /// A refreshed catalog's detached ed25519 signature did not verify
    /// against the compiled-in public key. Never partially trusted — the
    /// bytes are discarded, not merged with what's already cached.
    #[error("catalog signature does not verify against the trusted public key")]
    BadSignature,
    /// A refreshed catalog verified but its `version` was not strictly
    /// greater than the one already cached — refused as a rollback
    /// (replay of a stale-but-validly-signed catalog), not applied.
    #[error("refreshed catalog version {offered} is not newer than the cached version {current}")]
    NotNewer { offered: u32, current: u32 },
}

impl ErrorCode for RegistryError {
    fn code(&self) -> &'static str {
        match self {
            RegistryError::UnknownModel { .. } => "registry.unknown_model",
            RegistryError::NoSuitableModel { .. } => "registry.no_suitable_model",
            RegistryError::MalformedCatalog { .. } => "registry.malformed_catalog",
            RegistryError::BadSignature => "registry.bad_signature",
            RegistryError::NotNewer { .. } => "registry.not_newer",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, RegistryError>;
