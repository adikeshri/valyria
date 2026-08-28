use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("no model in the catalog with id {id:?}")]
    UnknownModel { id: String },
    #[error("no model in the catalog is suitable for role {role} on this hardware")]
    NoSuitableModel { role: String },
    #[error("embedded catalog is malformed: {detail}")]
    MalformedCatalog { detail: String },
}

impl ErrorCode for RegistryError {
    fn code(&self) -> &'static str {
        match self {
            RegistryError::UnknownModel { .. } => "registry.unknown_model",
            RegistryError::NoSuitableModel { .. } => "registry.no_suitable_model",
            RegistryError::MalformedCatalog { .. } => "registry.malformed_catalog",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, RegistryError>;
