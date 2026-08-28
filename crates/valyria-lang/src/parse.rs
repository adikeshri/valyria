//! Grammar + query compilation, and parsing one file.
//!
//! [`CompiledLanguage`] is the expensive half of a provider: compiling a
//! `.scm` query against a grammar costs milliseconds, so it happens once
//! per language when the registry is built, never per file.

use std::sync::Arc;

use tree_sitter::{Language, Node, Parser, Query, Tree};

use crate::error::{LangError, Result};
use crate::provider::LanguageProvider;

/// A provider with its grammar and query set compiled and ready to use.
/// Cheap to clone conceptually (it lives behind an `Arc` in the registry)
/// and safe to share across rayon workers: the compiled `Query` and
/// `Language` are both `Sync`; only the short-lived `Parser` is not, and
/// one is created per parse.
#[derive(Debug)]
pub struct CompiledLanguage {
    provider: Arc<dyn LanguageProvider>,
    language: Language,
    pub(crate) symbols: Query,
    pub(crate) imports: Option<Query>,
    pub(crate) calls: Option<Query>,
    pub(crate) tests: Option<Query>,
}

impl CompiledLanguage {
    pub fn compile(provider: Arc<dyn LanguageProvider>) -> Result<Self> {
        let language = provider.ts_language();
        let queries = provider.queries();
        let id = provider.id();

        let compile_one = |name: &'static str, src: &'static str| -> Result<Query> {
            Query::new(&language, src).map_err(|source| LangError::Query {
                language: id,
                query: name,
                source,
            })
        };

        Ok(Self {
            symbols: compile_one("symbols.scm", queries.symbols)?,
            imports: queries
                .imports
                .map(|src| compile_one("imports.scm", src))
                .transpose()?,
            calls: queries
                .calls
                .map(|src| compile_one("calls.scm", src))
                .transpose()?,
            tests: queries
                .tests
                .map(|src| compile_one("tests.scm", src))
                .transpose()?,
            language,
            provider,
        })
    }

    pub fn provider(&self) -> &dyn LanguageProvider {
        self.provider.as_ref()
    }

    pub fn id(&self) -> &'static str {
        self.provider.id()
    }

    /// Parse `source` into a syntax tree.
    ///
    /// A fresh `Parser` per call rather than a pooled one: `Parser` is not
    /// `Sync`, and pooling it would either serialize the parallel
    /// bootstrap indexer or require thread-local plumbing for a saving
    /// that is small next to the parse itself.
    pub fn parse<'a>(&self, source: &'a str) -> Result<ParsedFile<'a>> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language)
            .map_err(|source| LangError::Grammar {
                language: self.provider.id(),
                source,
            })?;
        let tree = parser.parse(source, None).ok_or(LangError::ParseFailed {
            language: self.provider.id(),
        })?;
        Ok(ParsedFile { tree, source })
    }

    /// Re-parse `source` reusing `old` for incremental parsing. The caller
    /// must have already applied the corresponding [`tree_sitter::InputEdit`]
    /// to `old`; when it hasn't, tree-sitter still produces a correct tree,
    /// just without the speedup.
    pub fn reparse<'a>(&self, source: &'a str, old: &Tree) -> Result<ParsedFile<'a>> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language)
            .map_err(|source| LangError::Grammar {
                language: self.provider.id(),
                source,
            })?;
        let tree = parser
            .parse(source, Some(old))
            .ok_or(LangError::ParseFailed {
                language: self.provider.id(),
            })?;
        Ok(ParsedFile { tree, source })
    }
}

/// A parsed file: the tree plus the exact bytes it was parsed from. The
/// two travel together because every span in the tree is meaningless
/// against different source.
#[derive(Debug)]
pub struct ParsedFile<'a> {
    tree: Tree,
    source: &'a str,
}

impl<'a> ParsedFile<'a> {
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn source(&self) -> &'a str {
        self.source
    }

    pub fn root(&self) -> Node<'_> {
        self.tree.root_node()
    }

    /// Whether tree-sitter's error recovery fired anywhere in the file.
    /// `has_error` covers both `ERROR` nodes and `MISSING` nodes.
    pub fn has_errors(&self) -> bool {
        self.tree.root_node().has_error()
    }
}
