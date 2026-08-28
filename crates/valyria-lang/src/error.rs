//! Errors from grammar loading, query compilation, and extraction.

use valyria_types::ErrorCode;

pub type Result<T> = std::result::Result<T, LangError>;

#[derive(Debug, thiserror::Error)]
pub enum LangError {
    #[error("no language provider is registered for `{0}`")]
    UnknownLanguage(String),

    #[error("grammar for `{language}` was rejected by tree-sitter: {source}")]
    Grammar {
        language: &'static str,
        source: tree_sitter::LanguageError,
    },

    /// A `.scm` query file failed to compile against its grammar. This is a
    /// programming error in this crate (the queries ship with the binary),
    /// not something a user input can cause — but it is surfaced as a
    /// typed error rather than a panic so a single broken query degrades
    /// one language instead of taking the process down.
    #[error("query `{query}` for `{language}` failed to compile: {source}")]
    Query {
        language: &'static str,
        query: &'static str,
        source: tree_sitter::QueryError,
    },

    #[error("parsing `{language}` source failed (tree-sitter returned no tree)")]
    ParseFailed { language: &'static str },

    #[error("source is not valid UTF-8")]
    NotUtf8,

    /// A caller-supplied `.scm` pattern (see [`crate::query`]) was
    /// rejected. Unlike [`LangError::Query`], this is normal input
    /// validation rather than a bug in a shipped query file.
    #[error("ad-hoc query is invalid: {0}")]
    AdHocQuery(String),
}

impl ErrorCode for LangError {
    fn code(&self) -> &'static str {
        match self {
            LangError::UnknownLanguage(_) => "lang.unknown_language",
            LangError::Grammar { .. } => "lang.grammar_rejected",
            LangError::Query { .. } => "lang.query_compile_failed",
            LangError::ParseFailed { .. } => "lang.parse_failed",
            LangError::NotUtf8 => "lang.not_utf8",
            LangError::AdHocQuery(_) => "lang.adhoc_query_invalid",
        }
    }

    fn retryable(&self) -> bool {
        // Every variant here is deterministic in its input: re-running the
        // same parse against the same bytes produces the same outcome.
        false
    }
}
