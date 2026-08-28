//! Semantic search: nearest-neighbour lookup over chunk embeddings.
//!
//! This is the mode that most needs to degrade gracefully. If no
//! embeddings have been built for the generation — because no embedder is
//! configured, or the pipeline has not run — it returns a `degraded`
//! note and search carries on with the lexical, symbol, dependency and
//! git modes. "Search works fully with embeddings disabled" (Phase 5
//! exit criteria) is this function choosing to contribute nothing rather
//! than raising.

use super::{ModeCtx, ModeHit, ModeOutcome};
use crate::Result;

pub async fn run(ctx: &ModeCtx<'_>) -> Result<ModeOutcome> {
    if ctx.query.text.trim().is_empty() {
        return Ok(ModeOutcome::degraded("semantic: empty query"));
    }
    if !ctx.embed.is_built(ctx.generation).await? {
        return Ok(ModeOutcome::degraded(format!(
            "semantic: no embeddings for index generation {}",
            ctx.generation
        )));
    }

    let query = ctx.embedder.embed(&ctx.query.text);
    if query.norm() == 0.0 {
        return Ok(ModeOutcome::degraded(
            "semantic: query has no embeddable tokens",
        ));
    }

    let k = ctx.query.limit.max(10) * 3;
    let vector_hits = ctx.embed.search(ctx.generation, &query, k).await?;

    let hits = vector_hits
        .into_iter()
        .map(|h| ModeHit {
            path: h.path,
            symbol_path: h.symbol_path,
            line: Some(h.span.start_line),
            snippet: None,
            // Cosine is in [-1, 1]; shift into [0, 1] so a mildly
            // negative similarity does not look like a strong signal.
            raw_score: ((h.score as f64) + 1.0) / 2.0,
        })
        .collect();

    Ok(ModeOutcome {
        hits,
        degraded: None,
    }
    .into_ranked_files())
}
