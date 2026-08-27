//! The minimal Phase 3 query/result shape: an explicit list of files to
//! include (no retrieval/ranking — that's Phase 6) and a token budget.

use crate::item::{ContextBody, ContextItem};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextQuery {
    pub explicit_paths: Vec<String>,
    pub budget_tokens: usize,
}

impl ContextQuery {
    pub fn new(budget_tokens: usize) -> Self {
        Self {
            explicit_paths: Vec::new(),
            budget_tokens,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.explicit_paths.push(path.into());
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssembledContext {
    pub items: Vec<ContextItem>,
    pub total_tokens: usize,
}

impl AssembledContext {
    /// A minimal, direct rendering of each item into a user-role message.
    /// The full trust-ordered, nonce-fenced prompt assembly (D3: nothing
    /// below `Instruction` may occupy a system position, everything at
    /// `Evidence` or below is fenced) is Phase 6's job — Phase 3 only
    /// carries explicit `RepoData` file contents, so there is no
    /// injection-relevant trust boundary being glossed over here yet.
    pub fn to_messages(&self) -> Vec<valyria_model::Message> {
        self.items
            .iter()
            .map(|item| {
                let ContextBody::Text(text) = &item.body;
                valyria_model::Message::user(format!("[{}]\n{}", item.label(), text))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_accumulates_paths() {
        let query = ContextQuery::new(1000).with_path("a.rs").with_path("b.rs");
        assert_eq!(query.explicit_paths, vec!["a.rs", "b.rs"]);
        assert_eq!(query.budget_tokens, 1000);
    }
}
