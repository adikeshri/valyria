//! Dependency-aware search: graph traversal from the query's anchor
//! files.
//!
//! "Which files are close to the ones I am already working on?" is a
//! retrieval signal the other modes cannot see. Given anchor files, this
//! walks the knowledge graph both ways — what imports or calls into them
//! (likely to break if they change) and what they reach (needed to
//! understand them) — and scores each file by how few hops away it is.
//!
//! With no anchors, or no graph built for the generation, it contributes
//! nothing.

use std::collections::HashMap;

use valyria_graph::{Direction, NodeId};

use super::{ModeCtx, ModeHit, ModeOutcome};
use crate::Result;

const MAX_DEPTH: usize = 3;

pub async fn run(ctx: &ModeCtx<'_>) -> Result<ModeOutcome> {
    if ctx.query.anchors.is_empty() {
        return Ok(ModeOutcome::degraded(
            "dependency: no anchor files to traverse from",
        ));
    }
    if !ctx.graph.is_built(ctx.generation).await? {
        return Ok(ModeOutcome::degraded(format!(
            "dependency: no graph for index generation {}",
            ctx.generation
        )));
    }

    // BFS over file nodes, recording the shortest hop distance to any
    // anchor.
    let mut distance: HashMap<String, usize> = HashMap::new();
    let mut frontier: Vec<String> = Vec::new();
    for anchor in &ctx.query.anchors {
        distance.insert(anchor.clone(), 0);
        frontier.push(anchor.clone());
    }

    for depth in 1..=MAX_DEPTH {
        if frontier.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for path in &frontier {
            let node = NodeId::file(path);
            let edges = ctx
                .graph
                .neighbors(ctx.generation, &node, Direction::Both, &[])
                .await?;
            for edge in edges {
                for endpoint in [&edge.from, &edge.to] {
                    let Some(file) = endpoint.file_path() else {
                        continue;
                    };
                    if !distance.contains_key(file) {
                        distance.insert(file.to_string(), depth);
                        next.push(file.to_string());
                    }
                }
            }
        }
        frontier = next;
    }

    let hits: Vec<ModeHit> = distance
        .into_iter()
        .filter(|(path, d)| *d > 0 && !ctx.query.anchors.contains(path))
        .map(|(path, d)| ModeHit {
            path,
            symbol_path: None,
            line: None,
            snippet: None,
            // 1 hop -> 1.0, 2 -> 0.5, 3 -> 0.33.
            raw_score: 1.0 / d as f64,
        })
        .collect();

    if hits.is_empty() {
        return Ok(ModeOutcome::empty());
    }
    Ok(ModeOutcome {
        hits,
        degraded: None,
    }
    .into_ranked_files())
}
