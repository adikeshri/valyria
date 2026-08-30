use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid toml in {path}: {source}")]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("config value at `{path}` failed to deserialize into settings: {source}")]
    Deserialize {
        path: &'static str,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not serialize the edited config document: {source}")]
    Serialize {
        #[source]
        source: toml::ser::Error,
    },
    #[error("`{key}` is not a writable config key")]
    UnknownKey { key: String },
    #[error("policy floor violation at `{key}`: configured value `{configured}` exceeds the floor `{floor}`")]
    PolicyFloorViolation {
        key: &'static str,
        configured: String,
        floor: String,
    },
}

impl ErrorCode for ConfigError {
    fn code(&self) -> &'static str {
        match self {
            ConfigError::Io { .. } => "config.io",
            ConfigError::Toml { .. } => "config.toml",
            ConfigError::Deserialize { .. } => "config.deserialize",
            ConfigError::Serialize { .. } => "config.serialize",
            ConfigError::UnknownKey { .. } => "config.unknown_key",
            ConfigError::PolicyFloorViolation { .. } => "config.policy_floor_violation",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, ConfigError>;
