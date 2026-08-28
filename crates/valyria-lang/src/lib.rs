//! `valyria-lang` — layer 2 (Repository intelligence).
//!
//! Language support as **data behind one trait** (D9). A
//! [`LanguageProvider`] contributes a tree-sitter grammar and a directory
//! of `.scm` queries; a single extraction engine
//! ([`extract`]) turns query captures into language-neutral
//! [`FileFacts`] — symbols, imports, call sites, tests. Nothing in this
//! crate matches on a file extension, and nothing above it knows that
//! tree-sitter exists.
//!
//! Adding a language is adding `queries/<lang>/` plus a small provider.
//! It is never an edit to extraction, indexing, the graph, or search.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod chunk;
pub mod error;
pub mod extract;
pub mod languages;
pub mod parse;
pub mod provider;
pub mod query;
pub mod registry;
pub mod symbol;

pub use chunk::{chunk_file, Chunk, DEFAULT_MAX_CHUNK_BYTES};
pub use error::{LangError, Result};
pub use parse::{CompiledLanguage, ParsedFile};
pub use provider::{LanguageProvider, LanguageQueries, Tier};
pub use query::{identifier_spans, query_spans};
pub use registry::LanguageRegistry;
pub use symbol::{Call, FileFacts, Import, Span, Symbol, SymbolKind, TestCase};
