//! AST search: a tree-sitter query pattern evaluated against files of the
//! matching language (§4.16, "functions calling `unwrap` inside a
//! loop").
//!
//! The pattern is `ctx.query.text`; the capture to report is the first
//! `@name` token in it. A pattern is language-specific — it compiles
//! against one grammar's node types — so it is run against every indexed
//! language and only reported as a [`SearchError::BadPattern`] if *no*
//! language accepted it.

use super::{snippet_of, ModeCtx, ModeHit, ModeOutcome};
use crate::{Result, SearchError};

fn first_capture(pattern: &str) -> Option<String> {
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_' || bytes[end] == b'.')
            {
                end += 1;
            }
            if end > start {
                return Some(pattern[start..end].to_string());
            }
        }
        i += 1;
    }
    None
}

pub async fn run(ctx: &ModeCtx<'_>) -> Result<ModeOutcome> {
    let pattern = ctx.query.text.trim();
    let capture = first_capture(pattern).ok_or(SearchError::BadPattern {
        kind: "ast",
        message: "pattern contains no @capture to report".to_string(),
    })?;

    let mut hits = Vec::new();
    let mut last_err: Option<String> = None;
    let mut any_ok = false;

    for file in ctx.files {
        let Some(language_id) = file.language.as_deref() else {
            continue;
        };
        let Some(lang) = ctx.registry.get(language_id) else {
            continue;
        };
        match valyria_lang::query_spans(lang, &file.text, pattern, &capture) {
            Ok(spans) => {
                any_ok = true;
                for span in spans {
                    let line = span.start_line;
                    let snippet = file
                        .text
                        .lines()
                        .nth(line.saturating_sub(1) as usize)
                        .map(snippet_of);
                    hits.push(ModeHit {
                        path: file.path.clone(),
                        symbol_path: None,
                        line: Some(line),
                        snippet,
                        raw_score: 1.0,
                    });
                }
            }
            Err(e) => last_err = Some(e.to_string()),
        }
    }

    if !any_ok {
        if let Some(message) = last_err {
            return Err(SearchError::BadPattern {
                kind: "ast",
                message,
            });
        }
        return Ok(ModeOutcome::degraded(
            "ast: no indexed file is of a language this pattern could run against",
        ));
    }

    // Count matches per file — a file with three matching nodes is a
    // stronger hit than one with a single match.
    Ok(ModeOutcome {
        hits,
        degraded: None,
    }
    .tally())
}

impl ModeOutcome {
    /// Collapse to one hit per file whose score is the number of matches
    /// in it, keeping the earliest line for display.
    fn tally(mut self) -> Self {
        use std::collections::HashMap;
        let mut by_file: HashMap<String, ModeHit> = HashMap::new();
        for hit in self.hits.drain(..) {
            by_file
                .entry(hit.path.clone())
                .and_modify(|h| {
                    h.raw_score += 1.0;
                    if hit.line.unwrap_or(u32::MAX) < h.line.unwrap_or(u32::MAX) {
                        h.line = hit.line;
                        h.snippet = hit.snippet.clone();
                    }
                })
                .or_insert(hit);
        }
        let mut hits: Vec<ModeHit> = by_file.into_values().collect();
        hits.sort_by(|a, b| {
            b.raw_score
                .partial_cmp(&a.raw_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.path.cmp(&b.path))
        });
        self.hits = hits;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_capture_finds_the_capture_name() {
        assert_eq!(
            first_capture("(call_expression) @call"),
            Some("call".to_string())
        );
        assert_eq!(
            first_capture("(function_item name: (identifier) @fn.name)"),
            Some("fn.name".to_string())
        );
        assert_eq!(first_capture("(identifier)"), None);
    }
}
