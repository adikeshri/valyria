//! Regex search: a caller-supplied pattern run line-by-line over file
//! contents.
//!
//! This mode is never auto-selected, so a pattern that does not compile
//! is a real [`SearchError::BadPattern`] rather than a silent
//! degradation.

use ::regex::RegexBuilder;

use super::{snippet_of, ModeCtx, ModeHit, ModeOutcome};
use crate::{Result, SearchError};

pub async fn run(ctx: &ModeCtx<'_>) -> Result<ModeOutcome> {
    let re = RegexBuilder::new(&ctx.query.text)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| SearchError::BadPattern {
            kind: "regex",
            message: e.to_string(),
        })?;

    let mut hits = Vec::new();
    for file in ctx.files {
        for (i, line) in file.text.lines().enumerate() {
            let count = re.find_iter(line).count();
            if count == 0 {
                continue;
            }
            hits.push(ModeHit {
                path: file.path.clone(),
                symbol_path: None,
                line: Some(i as u32 + 1),
                snippet: Some(snippet_of(line)),
                raw_score: count as f64,
            });
        }
    }

    Ok(ModeOutcome {
        hits,
        degraded: None,
    }
    .into_ranked_files())
}
