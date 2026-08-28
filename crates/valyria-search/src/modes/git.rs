//! Git-aware search: files touched by recent commits whose message or
//! changed paths match the query.
//!
//! History is a real retrieval signal — "the fix for this kind of bug
//! last time touched these files" — and it is also where the ranking's
//! recency and churn features get their raw data. This mode surfaces the
//! message match directly; the reranker uses the same log for every hit.
//!
//! Not a git repository ⇒ contributes nothing.

use super::{ModeCtx, ModeHit, ModeOutcome};
use crate::Result;

/// How many commits back to look.
const LOG_DEPTH: usize = 200;

pub async fn run(ctx: &ModeCtx<'_>) -> Result<ModeOutcome> {
    let Some(repo) = ctx.repo else {
        return Ok(ModeOutcome::degraded("git: not a git repository"));
    };
    let terms = ctx.query.terms();
    if terms.is_empty() {
        return Ok(ModeOutcome::degraded("git: query has no searchable terms"));
    }

    let log = match repo.log(LOG_DEPTH) {
        Ok(log) => log,
        Err(valyria_git::GitError::UnbornHead) => {
            return Ok(ModeOutcome::degraded("git: repository has no commits yet"));
        }
        Err(e) => return Err(e.into()),
    };
    let commit_count = log.len().max(1) as f64;

    let mut hits: Vec<ModeHit> = Vec::new();
    for (age, commit) in log.iter().enumerate() {
        let message = commit.message.to_lowercase();
        let message_matches = terms
            .iter()
            .filter(|t| message.contains(t.as_str()))
            .count();
        if message_matches == 0 {
            continue;
        }
        // Newer commits weigh more: a linear decay over the window.
        let recency = 1.0 - (age as f64 / commit_count);
        let relevance = message_matches as f64 / terms.len() as f64;

        let Ok(files) = repo.show(&commit.sha) else {
            continue;
        };
        for file in files {
            hits.push(ModeHit {
                path: file.path,
                symbol_path: None,
                line: None,
                snippet: Some(commit.message.clone()),
                raw_score: relevance * (0.4 + 0.6 * recency),
            });
        }
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
