//! The language registry: the lookup that replaces the `match` on file
//! extension D9 forbids.
//!
//! Construction compiles every grammar and query up front and reports
//! which languages failed, rather than discovering a broken query in the
//! middle of indexing a repository.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::error::{LangError, Result};
use crate::extract;
use crate::parse::CompiledLanguage;
use crate::provider::{matches_path, LanguageProvider};
use crate::symbol::FileFacts;

#[derive(Debug, Default)]
pub struct LanguageRegistry {
    languages: BTreeMap<&'static str, Arc<CompiledLanguage>>,
}

impl LanguageRegistry {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a registry over every grammar compiled into this build (see
    /// the `lang-*` cargo features).
    pub fn with_builtin_languages() -> Result<Self> {
        let mut registry = Self::empty();
        for provider in crate::languages::builtin_providers() {
            registry.register(provider)?;
        }
        Ok(registry)
    }

    pub fn register(&mut self, provider: Arc<dyn LanguageProvider>) -> Result<()> {
        let compiled = CompiledLanguage::compile(provider)?;
        self.languages.insert(compiled.id(), Arc::new(compiled));
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Arc<CompiledLanguage>> {
        self.languages.get(id)
    }

    /// The language for `path`, or `None` when the file's type is not one
    /// this build understands. `None` is a normal outcome, not an error:
    /// the index still records the file (so lexical search finds it), it
    /// just carries no symbols.
    pub fn for_path(&self, path: &Path) -> Option<&Arc<CompiledLanguage>> {
        self.languages
            .values()
            .find(|lang| matches_path(lang.provider(), path))
    }

    pub fn language_id_for_path(&self, path: &Path) -> Option<&'static str> {
        self.for_path(path).map(|lang| lang.id())
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.languages.keys().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.languages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.languages.is_empty()
    }

    /// Parse and extract in one call. Returns `None` when no registered
    /// language claims `path`.
    pub fn extract_facts(&self, path: &Path, source: &str) -> Option<Result<FileFacts>> {
        let lang = self.for_path(path)?;
        Some(extract::extract(lang, source))
    }

    /// Extract using an explicitly named language, for callers that
    /// already know it (the index stores the language id alongside each
    /// file, so incremental updates skip the path match).
    pub fn extract_facts_as(&self, language_id: &str, source: &str) -> Result<FileFacts> {
        let lang = self
            .get(language_id)
            .ok_or_else(|| LangError::UnknownLanguage(language_id.to_string()))?;
        extract::extract(lang, source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_compiles_every_grammar_and_query() {
        // This is the test that catches a typo in any shipped `.scm` file:
        // query compilation is what fails, and it fails here rather than
        // midway through indexing someone's repository.
        let registry = LanguageRegistry::with_builtin_languages().unwrap();
        assert!(!registry.is_empty(), "no grammars compiled into this build");
    }

    #[test]
    fn unknown_extension_has_no_language() {
        let registry = LanguageRegistry::with_builtin_languages().unwrap();
        assert!(registry.for_path(Path::new("notes.xyz")).is_none());
    }

    #[test]
    fn extract_facts_as_rejects_an_unregistered_language() {
        let registry = LanguageRegistry::empty();
        let err = registry.extract_facts_as("klingon", "").unwrap_err();
        assert!(matches!(err, LangError::UnknownLanguage(_)));
    }

    #[test]
    fn extract_facts_returns_none_for_an_unclaimed_path() {
        let registry = LanguageRegistry::with_builtin_languages().unwrap();
        assert!(registry
            .extract_facts(Path::new("data.bin"), "\u{0}\u{1}")
            .is_none());
    }
}
