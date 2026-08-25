//! The shared error taxonomy (cross-cutting conventions, §3).
//!
//! Every error type in the workspace (below layer 6) is a `thiserror` enum
//! local to its crate. What makes errors composable across the protocol
//! boundary is [`ErrorCode`]: a small trait every such enum implements, so
//! any error can be converted to a stable `code`, a `retryable` flag, and a
//! human message without the protocol layer needing to know about every
//! crate's internal error type.

use serde::{Deserialize, Serialize};

/// Implemented by every crate-local error enum. `code` must be stable
/// across releases (it is transported over the protocol and may be matched
/// on by clients); `retryable` tells a caller whether retrying the same
/// operation unchanged could plausibly succeed.
pub trait ErrorCode: std::error::Error {
    fn code(&self) -> &'static str;
    fn retryable(&self) -> bool;
}

/// A type-erased, wire-safe representation of any [`ErrorCode`] error.
/// This is what actually crosses the protocol boundary (§44) and what gets
/// journaled (D1) — never a `Box<dyn Error>`, which isn't serializable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodedError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl CodedError {
    pub fn from_error<E: ErrorCode>(err: &E) -> Self {
        Self {
            code: err.code().to_string(),
            message: err.to_string(),
            retryable: err.retryable(),
        }
    }
}

impl std::fmt::Display for CodedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for CodedError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    enum FakeError {
        #[error("path escapes workspace root: {0}")]
        PathTraversal(String),
        #[error("timed out after {0}ms")]
        Timeout(u64),
    }

    impl ErrorCode for FakeError {
        fn code(&self) -> &'static str {
            match self {
                FakeError::PathTraversal(_) => "vfs.path_traversal",
                FakeError::Timeout(_) => "process.timeout",
            }
        }

        fn retryable(&self) -> bool {
            matches!(self, FakeError::Timeout(_))
        }
    }

    #[test]
    fn converts_to_coded_error() {
        let err = FakeError::PathTraversal("../../etc/passwd".into());
        let coded = CodedError::from_error(&err);
        assert_eq!(coded.code, "vfs.path_traversal");
        assert!(!coded.retryable);
        assert!(coded.message.contains("escapes"));
    }

    #[test]
    fn retryable_flag_is_per_variant() {
        let timeout = CodedError::from_error(&FakeError::Timeout(5000));
        assert!(timeout.retryable);
    }

    #[test]
    fn coded_error_round_trips_json() {
        let coded = CodedError::from_error(&FakeError::Timeout(1));
        let json = serde_json::to_string(&coded).unwrap();
        let back: CodedError = serde_json::from_str(&json).unwrap();
        assert_eq!(coded, back);
    }
}
