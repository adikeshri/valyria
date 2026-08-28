//! Symbol search: name lookup through the index.
//!
//! Distinct from the lexical mode's use of the same FTS table: this mode
//! is *only* symbols, it resolves each query term to exact-name matches
//! as well as fuzzy ones, and it scores by how precisely the name
//! matched (exact identifier > prefix > FTS hit) with a small prior on
//! the symbol kind — a `struct` or `trait` is more likely to be what a
//! bare-name query wants than a local `variable`.

use valyria_lang::SymbolKind;

use super::{ModeCtx, ModeHit, ModeOutcome};
use crate::Result;

fn kind_prior(kind: SymbolKind) -> f64 {
    match kind {
        SymbolKind::Struct
        | SymbolKind::Class
        | SymbolKind::Trait
        | SymbolKind::Interface
        | SymbolKind::Enum => 1.0,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Module | SymbolKind::TypeAlias => {
            0.8
        }
        SymbolKind::Constant | SymbolKind::Macro | SymbolKind::Test => 0.5,
        SymbolKind::Field | SymbolKind::Variable => 0.2,
    }
}

pub async fn run(ctx: &ModeCtx<'_>) -> Result<ModeOutcome> {
    let terms = ctx.query.terms();
    if terms.is_empty() {
        return Ok(ModeOutcome::degraded(
            "symbol: query has no searchable terms",
        ));
    }

    let generation = ctx.generation;
    let mut hits: Vec<ModeHit> = Vec::new();

    // Exact-name matches per term.
    for term in &terms {
        for sym in ctx.index.symbols_named(generation, term).await? {
            hits.push(ModeHit {
                path: sym.path.clone(),
                symbol_path: Some(sym.symbol_path.clone()),
                line: Some(sym.name_span.start_line),
                snippet: Some(sym.signature.clone()).filter(|s| !s.is_empty()),
                raw_score: 4.0 + kind_prior(sym.kind),
            });
        }
    }

    // Fuzzy matches from FTS, ranked by return position.
    let fuzzy = ctx
        .index
        .search_symbols(&ctx.query.text, ctx.query.limit.max(20))
        .await?;
    for (rank, sym) in fuzzy.iter().enumerate() {
        let name = sym.name.to_lowercase();
        let precision = if terms.contains(&name) {
            2.0
        } else if terms.iter().any(|t| name.starts_with(t.as_str())) {
            1.0
        } else {
            0.4
        };
        hits.push(ModeHit {
            path: sym.path.clone(),
            symbol_path: Some(sym.symbol_path.clone()),
            line: Some(sym.name_span.start_line),
            snippet: Some(sym.signature.clone()).filter(|s| !s.is_empty()),
            raw_score: precision + kind_prior(sym.kind) + 1.0 / (rank as f64 + 1.0),
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

    #[test]
    fn structural_kinds_outrank_locals() {
        assert!(kind_prior(SymbolKind::Struct) > kind_prior(SymbolKind::Variable));
        assert!(kind_prior(SymbolKind::Trait) > kind_prior(SymbolKind::Field));
    }
}
