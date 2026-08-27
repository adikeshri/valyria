use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum FakeRuntimeError {
    #[error("failed to read scenario file `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse scenario file `{path}`: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("scenario `{scenario}` has no turn at index {index} (only {len} turns)")]
    TurnIndexOutOfRange {
        scenario: String,
        index: usize,
        len: usize,
    },
}

impl ErrorCode for FakeRuntimeError {
    fn code(&self) -> &'static str {
        match self {
            FakeRuntimeError::Io { .. } => "runtime_fake.io",
            FakeRuntimeError::Parse { .. } => "runtime_fake.parse",
            FakeRuntimeError::TurnIndexOutOfRange { .. } => "runtime_fake.turn_index_out_of_range",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, FakeRuntimeError>;
