//! Lexical search: whole-word and substring matching over file contents,
//! folded together with the index's FTS5 symbol table.
//!
//! Two sources, one ranked list. The content scan rewards lines where the
//! query's terms appear — more so when they appear as whole words, and
//! more so again when every term is on the same line. The symbol FTS
//! contributes identifier matches the content scan would also find but
//! ranks poorly (a one-line `fn parse` among a thousand uses of
//! `parse`). Scores from the two are put on the same scale before they
//! are merged.

use super::{contains_word, snippet_of, ModeCtx, ModeHit, ModeOutcome};
use crate::Result;

pub async fn run(ctx: &ModeCtx<'_>) -> Result<ModeOutcome> {
    let terms = ctx.query.terms();
    if terms.is_empty() {
        return Ok(ModeOutcome::degraded(
            "lexical: query has no searchable terms",
        ));
    }
    let phrase = ctx.query.text.to_lowercase();

    // Inverse document frequency: a term that appears in almost every
    // file (a module name in a barrel file, a ubiquitous helper) is a
    // weak signal; a rare one is a strong one. Without this, a file that
    // merely lists every module tends to match every query.
    let lowered: Vec<String> = ctx.files.iter().map(|f| f.text.to_lowercase()).collect();
    let n_files = lowered.len().max(1) as f64;
    let idf = |term: &str| -> f64 {
        let df = lowered.iter().filter(|t| t.contains(term)).count() as f64;
        (1.0 + (n_files / (1.0 + df)).ln()).max(0.2)
    };
    let term_idf: Vec<(String, f64)> = terms.iter().map(|t| (t.clone(), idf(t))).collect();

    let mut hits: Vec<ModeHit> = Vec::new();

    for (file, haystack) in ctx.files.iter().zip(&lowered) {
        if !terms.iter().any(|t| haystack.contains(t.as_str())) {
            continue;
        }
        for (i, line) in file.text.lines().enumerate() {
            let lower = line.to_lowercase();
            let mut score = 0.0f64;
            let mut present = 0usize;
            for (term, weight) in &term_idf {
                let occurrences = lower.matches(term.as_str()).count();
                if occurrences == 0 {
                    continue;
                }
                present += 1;
                score += occurrences as f64 * weight;
                if contains_word(&lower, term) {
                    score += 1.5 * weight;
                }
            }
            if present == 0 {
                continue;
            }
            // All terms on one line, or the exact phrase: strong signals.
            if present == terms.len() {
                score += 3.0;
            }
            if !phrase.is_empty() && lower.contains(&phrase) {
                score += 5.0;
            }
            hits.push(ModeHit {
                path: file.path.clone(),
                symbol_path: None,
                line: Some(i as u32 + 1),
                snippet: Some(snippet_of(line)),
                raw_score: score,
            });
        }
    }

    // Identifier matches from the symbol index. FTS rank is position in
    // the returned list; map it onto a score comparable with the content
    // scan's whole-word bonus.
    let symbols = ctx
        .index
        .search_symbols(&ctx.query.text, ctx.query.limit.max(20))
        .await?;
    for (rank, sym) in symbols.iter().enumerate() {
        let exact = terms.iter().any(|t| *t == sym.name.to_lowercase());
        let base = if exact { 6.0 } else { 3.0 };
        hits.push(ModeHit {
            path: sym.path.clone(),
            symbol_path: Some(sym.symbol_path.clone()),
            line: Some(sym.name_span.start_line),
            snippet: Some(sym.signature.clone()).filter(|s| !s.is_empty()),
            raw_score: base + 1.0 / (rank as f64 + 1.0),
        });
    }

    if hits.is_empty() {
        return Ok(ModeOutcome::empty());
    }
    Ok(ModeOutcome {
        hits,
        degraded: None,
    }
    .into_ranked_files())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::LoadedFile;

    // The content-scan half is pure over `ctx.files`; exercise it through
    // a tiny in-memory context in the engine tests. Here, just the
    // scoring shape:

    #[test]
    fn a_line_with_the_whole_phrase_outscores_a_line_with_one_term() {
        let file = LoadedFile {
            path: "x.rs".into(),
            text: "fn parse tokens here\nsomething about tokens only\n".into(),
            language: Some("rust".into()),
        };
        // Reproduce the per-line scoring the run() loop does.
        let terms = ["parse".to_string(), "tokens".to_string()];
        let phrase = "parse tokens";
        let score_line = |line: &str| {
            let lower = line.to_lowercase();
            let mut score = 0.0f64;
            let mut present = 0usize;
            for t in &terms {
                let occ = lower.matches(t.as_str()).count();
                if occ == 0 {
                    continue;
                }
                present += 1;
                score += occ as f64;
                if contains_word(&lower, t) {
                    score += 1.5;
                }
            }
            if present == terms.len() {
                score += 3.0;
            }
            if lower.contains(phrase) {
                score += 5.0;
            }
            score
        };
        let lines: Vec<&str> = file.text.lines().collect();
        assert!(score_line(lines[0]) > score_line(lines[1]));
    }
}
