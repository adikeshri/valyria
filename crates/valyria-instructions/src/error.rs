use std::path::PathBuf;

use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum InstructionError {
    /// A file that exists could not be read. A *missing* file is not an
    /// error — a workspace with no `AGENTS.md` simply has no `AGENTS.md`
    /// source.
    #[error("failed to read instruction file `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file was read but is not valid UTF-8. Instruction files are
    /// prose; a binary blob at `CLAUDE.md` is a mistake worth surfacing
    /// rather than lossily decoding.
    #[error("instruction file `{path}` is not valid UTF-8")]
    NotUtf8 { path: PathBuf },
}

impl ErrorCode for InstructionError {
    fn code(&self) -> &'static str {
        match self {
            InstructionError::Io { .. } => "instructions.io",
            InstructionError::NotUtf8 { .. } => "instructions.not_utf8",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, InstructionError>;
