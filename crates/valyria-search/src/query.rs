//! What a caller asks for.

use serde::{Deserialize, Serialize};

/// The retrieval modes (§4.16). Each is a different way of finding
/// candidate locations; [`crate::SearchEngine`] runs several and fuses
/// their rankings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Whole-word and substring matching over file contents, plus the
    /// index's FTS5 symbol table.
    Lexical,
    /// A user-supplied regular expression over file contents.
    Regex,
    /// Symbol-name lookup through the index (`fn parse`, `struct Parser`).
    Symbol,
    /// Nearest-neighbour search over chunk embeddings. Silently
    /// contributes nothing when no embeddings exist for the generation.
    Semantic,
    /// A tree-sitter query pattern (`(call_expression) @c`) evaluated
    /// against files of the matching language.
    Ast,
    /// Graph traversal from the query's anchor files: what imports or
    /// calls into them, and what they reach.
    Dependency,
    /// Files touched by recent commits whose message or path matches the
    /// query text.
    Git,
}

impl SearchMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchMode::Lexical => "lexical",
            SearchMode::Regex => "regex",
            SearchMode::Symbol => "symbol",
            SearchMode::Semantic => "semantic",
            SearchMode::Ast => "ast",
            SearchMode::Dependency => "dependency",
            SearchMode::Git => "git",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "lexical" => SearchMode::Lexical,
            "regex" => SearchMode::Regex,
            "symbol" => SearchMode::Symbol,
            "semantic" => SearchMode::Semantic,
            "ast" => SearchMode::Ast,
            "dependency" => SearchMode::Dependency,
            "git" => SearchMode::Git,
            _ => return None,
        })
    }

    /// The modes tried when a query does not name any: the ones that make
    /// sense for a plain phrase. `Regex` and `Ast` need a deliberate
    /// pattern, so they are opt-in; the rest either contribute useful
    /// hits or step aside cheaply.
    pub fn default_set() -> Vec<SearchMode> {
        vec![
            SearchMode::Lexical,
            SearchMode::Symbol,
            SearchMode::Semantic,
            SearchMode::Dependency,
            SearchMode::Git,
        ]
    }
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// The query text: a phrase for lexical/semantic/symbol/git, or a
    /// pattern for regex/ast.
    pub text: String,
    /// Which modes to run. Empty means [`SearchMode::default_set`].
    pub modes: Vec<SearchMode>,
    /// Files the current task is anchored on. They seed dependency-mode
    /// traversal and pull nearby files up the ranking (import-graph
    /// distance is a rerank feature).
    pub anchors: Vec<String>,
    /// How many hits to return.
    pub limit: usize,
}

impl SearchQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            modes: Vec::new(),
            anchors: Vec::new(),
            limit: 20,
        }
    }

    pub fn mode(mut self, mode: SearchMode) -> Self {
        if !self.modes.contains(&mode) {
            self.modes.push(mode);
        }
        self
    }

    pub fn modes(mut self, modes: impl IntoIterator<Item = SearchMode>) -> Self {
        for m in modes {
            self = self.mode(m);
        }
        self
    }

    pub fn anchor(mut self, path: impl Into<String>) -> Self {
        self.anchors.push(path.into());
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// The modes this query will actually run.
    pub fn effective_modes(&self) -> Vec<SearchMode> {
        if self.modes.is_empty() {
            SearchMode::default_set()
        } else {
            self.modes.clone()
        }
    }

    /// Whitespace-separated lowercase terms, for the lexical scanner and
    /// the git message match.
    pub fn terms(&self) -> Vec<String> {
        self.text
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|t| !t.is_empty())
            .map(|t| t.to_lowercase())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip_through_their_strings() {
        for m in [
            SearchMode::Lexical,
            SearchMode::Regex,
            SearchMode::Symbol,
            SearchMode::Semantic,
            SearchMode::Ast,
            SearchMode::Dependency,
            SearchMode::Git,
        ] {
            assert_eq!(SearchMode::parse(m.as_str()), Some(m));
        }
    }

    #[test]
    fn an_empty_mode_list_expands_to_the_default_set() {
        let q = SearchQuery::new("parse tokens");
        assert_eq!(q.effective_modes(), SearchMode::default_set());
    }

    #[test]
    fn explicit_modes_are_kept_and_deduplicated() {
        let q = SearchQuery::new("x")
            .mode(SearchMode::Regex)
            .mode(SearchMode::Regex)
            .mode(SearchMode::Lexical);
        assert_eq!(
            q.effective_modes(),
            vec![SearchMode::Regex, SearchMode::Lexical]
        );
    }

    #[test]
    fn terms_are_lowercased_and_split_on_non_word_characters() {
        let q = SearchQuery::new("Parser::parse(tokens)");
        assert_eq!(q.terms(), vec!["parser", "parse", "tokens"]);
    }
}
