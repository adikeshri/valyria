//! Rendering a candidate at a chosen [`CompressionLevel`], honoring a
//! token target by *dropping whole units* — trailing lines for text,
//! whole symbols for source — never by cutting through one.

use valyria_util::TokenCounter;

use crate::candidate::{CandidateContent, CompressionLevel, RetrievalCandidate, SymbolSpan};

/// The result of rendering one candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    /// The exact text to place in the prompt (before any fencing).
    pub text: String,
    pub tokens: usize,
    /// The level actually used (may be lower than requested if even that
    /// did not fit and a lower one was tried by the caller).
    pub level: CompressionLevel,
    /// How many whole units (lines or symbols) were dropped to fit.
    pub dropped_units: usize,
    /// `true` if anything was dropped.
    pub truncated: bool,
}

/// Render `candidate` at `level`, trying to stay within `target_tokens`.
/// If it cannot fit even after dropping every droppable unit, the smallest
/// possible rendering is returned anyway (the caller decides whether to
/// step down a level or drop the item entirely) — this function never
/// returns something larger than `level` at full size and never splits a
/// symbol body or a line.
pub fn render(
    candidate: &RetrievalCandidate,
    level: CompressionLevel,
    target_tokens: usize,
    counter: &dyn TokenCounter,
) -> Rendered {
    match &candidate.content {
        CandidateContent::Text { text } => render_text(text, level, target_tokens, counter),
        CandidateContent::Source {
            path,
            header,
            symbols,
        } => render_source(
            path,
            header.as_deref(),
            symbols,
            level,
            target_tokens,
            counter,
        ),
    }
}

fn render_text(
    text: &str,
    level: CompressionLevel,
    target_tokens: usize,
    counter: &dyn TokenCounter,
) -> Rendered {
    if level == CompressionLevel::Reference {
        let t = "[reference only — omitted to fit the context budget]".to_string();
        let tokens = counter.count(&t);
        return Rendered {
            text: t,
            tokens,
            level,
            dropped_units: 0,
            truncated: true,
        };
    }

    let full_tokens = counter.count(text);
    if full_tokens <= target_tokens {
        return Rendered {
            text: text.to_string(),
            tokens: full_tokens,
            level,
            dropped_units: 0,
            truncated: false,
        };
    }

    // Drop whole trailing lines until it fits, leaving a marker.
    let marker = "\n[… truncated to fit the context budget]";
    let marker_tokens = counter.count(marker);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut kept = lines.len();
    while kept > 1 {
        kept -= 1;
        let candidate_text: String = lines[..kept].concat();
        let tokens = counter.count(&candidate_text) + marker_tokens;
        if tokens <= target_tokens {
            return Rendered {
                text: format!("{candidate_text}{marker}"),
                tokens,
                level,
                dropped_units: lines.len() - kept,
                truncated: true,
            };
        }
    }

    // Even one line doesn't fit: emit that one line plus the marker.
    let first = lines.first().copied().unwrap_or("");
    let text = format!("{first}{marker}");
    let tokens = counter.count(&text);
    Rendered {
        text,
        tokens,
        level,
        dropped_units: lines.len().saturating_sub(1),
        truncated: true,
    }
}

fn render_source(
    path: &str,
    header: Option<&str>,
    symbols: &[SymbolSpan],
    level: CompressionLevel,
    target_tokens: usize,
    counter: &dyn TokenCounter,
) -> Rendered {
    if level == CompressionLevel::Reference {
        let names: Vec<&str> = symbols.iter().map(|s| s.symbol_path.as_str()).collect();
        let t = format!(
            "{path}: {} (bodies omitted to fit the context budget)",
            if names.is_empty() {
                "no indexed symbols".to_string()
            } else {
                names.join(", ")
            }
        );
        let tokens = counter.count(&t);
        return Rendered {
            text: t,
            tokens,
            level,
            dropped_units: 0,
            truncated: true,
        };
    }

    // Symbols least relevant first are the drop order; render most relevant
    // first.
    let mut ordered: Vec<&SymbolSpan> = symbols.iter().collect();
    ordered.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.start_line.cmp(&b.start_line))
    });

    let header_line = header
        .map(|h| format!("// {path}\n{}\n", h.trim_end()))
        .unwrap_or_else(|| format!("// {path}\n"));

    let render_symbol = |s: &SymbolSpan| -> String {
        match level {
            CompressionLevel::Full => {
                let doc = s
                    .doc
                    .as_deref()
                    .map(|d| format!("{}\n", d.trim_end()))
                    .unwrap_or_default();
                format!("{doc}{}", ensure_trailing_newline(&s.body))
            }
            CompressionLevel::Outline => {
                let doc = s
                    .doc
                    .as_deref()
                    .and_then(|d| d.lines().next())
                    .map(|first| format!("// {first}\n"))
                    .unwrap_or_default();
                format!("{doc}{}\n", s.signature.trim_end())
            }
            CompressionLevel::Signature => format!("{}\n", s.signature.trim_end()),
            CompressionLevel::Reference => unreachable!("handled above"),
        }
    };

    // Greedily keep symbols (most relevant first) while they fit.
    let mut body = String::new();
    let mut used_tokens = counter.count(&header_line);
    let mut kept = 0usize;
    for s in &ordered {
        let piece = render_symbol(s);
        let piece_tokens = counter.count(&piece);
        if kept > 0 && used_tokens + piece_tokens > target_tokens {
            break;
        }
        body.push_str(&piece);
        used_tokens += piece_tokens;
        kept += 1;
        if used_tokens >= target_tokens {
            break;
        }
    }

    let dropped = ordered.len() - kept;
    let mut text = format!("{header_line}{body}");
    if dropped > 0 {
        let note = format!("// (+{dropped} more symbol(s) omitted to fit the context budget)\n");
        text.push_str(&note);
    }
    let tokens = counter.count(&text);
    Rendered {
        text,
        tokens,
        level,
        dropped_units: dropped,
        truncated: dropped > 0,
    }
}

fn ensure_trailing_newline(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_types::{Provenance, ProvenanceSource, Trust};
    use valyria_util::HeuristicTokenCounter;

    use crate::budget::SectionKind;
    use crate::candidate::{CandidateContent, RetrievalCandidate};

    fn counter() -> HeuristicTokenCounter {
        HeuristicTokenCounter
    }

    fn sym(name: &str, body: &str, relevance: f64) -> SymbolSpan {
        SymbolSpan {
            symbol_path: name.to_string(),
            kind: "fn".to_string(),
            signature: format!("fn {name}()"),
            doc: Some(format!("Does {name} things.\nMore detail.")),
            start_line: 1,
            end_line: 10,
            body: body.to_string(),
            relevance,
        }
    }

    fn source_candidate(symbols: Vec<SymbolSpan>) -> RetrievalCandidate {
        RetrievalCandidate::new(
            Trust::RepoData,
            Provenance::new(ProvenanceSource::File {
                path: "src/x.rs".into(),
            }),
            SectionKind::Repository,
            0.5,
            CandidateContent::Source {
                path: "src/x.rs".to_string(),
                header: Some("//! module x".to_string()),
                symbols,
            },
        )
    }

    #[test]
    fn full_source_emits_every_symbol_body_verbatim() {
        let bodies = ["fn a() { 1 }\n", "fn b() { 2 }\n"];
        let cand = source_candidate(vec![sym("a", bodies[0], 0.9), sym("b", bodies[1], 0.8)]);
        let r = render(&cand, CompressionLevel::Full, 100_000, &counter());
        assert!(!r.truncated);
        for b in bodies {
            assert!(r.text.contains(b.trim_end()), "missing body: {b}");
        }
    }

    #[test]
    fn a_tight_budget_drops_whole_symbols_never_splits_one() {
        let big_body = format!("fn big() {{\n{}\n}}\n", "    let x = 1;\n".repeat(50));
        let cand = source_candidate(vec![
            sym("keep", "fn keep() { 1 }\n", 0.99),
            sym("drop", &big_body, 0.10),
        ]);
        let r = render(&cand, CompressionLevel::Full, 40, &counter());
        assert!(r.truncated);
        assert_eq!(r.dropped_units, 1);
        // The kept symbol is intact...
        assert!(r.text.contains("fn keep() { 1 }"));
        // ...and no fragment of the dropped one leaked in.
        assert!(!r.text.contains("let x = 1;"));
    }

    #[test]
    fn signature_level_emits_only_signatures() {
        let cand = source_candidate(vec![sym("a", "fn a() { 1 }\n", 0.9)]);
        let r = render(&cand, CompressionLevel::Signature, 100_000, &counter());
        assert!(r.text.contains("fn a()"));
        assert!(!r.text.contains("{ 1 }"));
    }

    #[test]
    fn reference_level_is_a_one_liner_with_the_symbol_names() {
        let cand = source_candidate(vec![sym("alpha", "..\n", 0.9), sym("beta", "..\n", 0.9)]);
        let r = render(&cand, CompressionLevel::Reference, 5, &counter());
        assert!(r.text.contains("alpha"));
        assert!(r.text.contains("beta"));
        assert!(r.truncated);
    }

    #[test]
    fn text_shrinks_by_whole_lines_with_a_marker() {
        let text = (0..40).map(|i| format!("line {i}\n")).collect::<String>();
        let cand = RetrievalCandidate::new(
            Trust::Evidence,
            Provenance::new(ProvenanceSource::ModelTurn),
            SectionKind::Evidence,
            0.5,
            CandidateContent::text(text),
        );
        let r = render(&cand, CompressionLevel::Full, 20, &counter());
        assert!(r.truncated);
        assert!(r.text.contains("line 0\n"));
        assert!(r.text.contains("truncated to fit"));
        // Whatever survived is whole lines only.
        let before_marker = r.text.split("[… truncated").next().unwrap();
        assert!(before_marker.ends_with('\n'));
    }
}
