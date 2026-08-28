//! Combining the modes' rankings into one, and explaining the result.
//!
//! Two stages, matching §4.16:
//!
//! 1. **Reciprocal-rank fusion.** Each mode contributes `1 / (k + rank)`
//!    for every location it ranked; the sums are the fused score. RRF
//!    needs only the rank, not the mode's raw score, so modes with
//!    wildly different score scales (cosine similarity vs term counts)
//!    combine sensibly.
//! 2. **Feature reranking.** A small set of task-aware features —
//!    recency, git churn, import-graph distance from the anchors, test
//!    proximity, a path prior — each with a weight. The final score is
//!    the plain weighted sum, and every hit carries the feature
//!    breakdown that produced it.
//!
//! This module is pure: it takes the modes' outputs and a few
//! precomputed maps and returns ranked [`SearchHit`]s. That is what makes
//! the ranking testable without a repository (§8 risk register:
//! "provenance + explainability from day one so tuning is data-driven").

use std::collections::{HashMap, HashSet};

use crate::modes::ModeHit;
use crate::query::SearchMode;
use crate::result::{Feature, ScoreExplanation, SearchHit, StageScore};

/// The RRF constant. 60 is the value from the original RRF paper and the
/// de-facto default; it damps the influence of the very top ranks so a
/// single mode cannot dominate.
const RRF_K: f64 = 60.0;

/// Feature weights for the rerank stage. Tuning these is the main knob on
/// ranking behaviour; the defaults lean on fusion (`rrf`) and treat the
/// rest as adjustments.
#[derive(Debug, Clone, Copy)]
pub struct FeatureWeights {
    pub rrf: f64,
    pub recency: f64,
    pub churn: f64,
    pub import_distance: f64,
    pub test_proximity: f64,
    pub path_prior: f64,
}

impl Default for FeatureWeights {
    fn default() -> Self {
        Self {
            rrf: 1.0,
            recency: 0.25,
            churn: 0.15,
            import_distance: 0.4,
            test_proximity: 0.2,
            path_prior: 0.15,
        }
    }
}

/// Everything the reranker needs beyond the modes' outputs, precomputed
/// by the engine.
#[derive(Debug, Default)]
pub struct RankContext {
    pub anchors: Vec<String>,
    /// Path → recency in `[0, 1]`, 1.0 = touched by the newest commit in
    /// the window.
    pub git_recency: HashMap<String, f64>,
    /// Path → churn in `[0, 1]`, normalized commit count over the window.
    pub git_churn: HashMap<String, f64>,
    /// Path → fewest import/call hops to any anchor file.
    pub import_distance: HashMap<String, usize>,
    /// Files the index classifies as tests.
    pub test_files: HashSet<String>,
    pub weights: FeatureWeights,
}

/// Fuse and rerank. `per_mode` is `(mode, ranked file hits)` in the order
/// the modes were run.
pub fn fuse(
    per_mode: &[(SearchMode, Vec<ModeHit>)],
    ctx: &RankContext,
    limit: usize,
) -> Vec<SearchHit> {
    // --- stage 1: reciprocal-rank fusion ---
    let mut rrf: HashMap<String, f64> = HashMap::new();
    let mut stages: HashMap<String, Vec<StageScore>> = HashMap::new();
    let mut best_hit: HashMap<String, ModeHit> = HashMap::new();

    for (mode, hits) in per_mode {
        for (i, hit) in hits.iter().enumerate() {
            let rank = i + 1;
            *rrf.entry(hit.path.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank as f64);
            stages
                .entry(hit.path.clone())
                .or_default()
                .push(StageScore {
                    mode: *mode,
                    rank,
                    raw_score: hit.raw_score,
                });
            // Keep the display sub-location from whichever mode gave the
            // most specific one (a real line beats no line; earlier mode
            // wins ties, so lexical/symbol tend to supply it).
            best_hit
                .entry(hit.path.clone())
                .and_modify(|existing| {
                    if existing.line.is_none() && hit.line.is_some() {
                        *existing = hit.clone();
                    }
                })
                .or_insert_with(|| hit.clone());
        }
    }

    let max_rrf = rrf
        .values()
        .cloned()
        .fold(0.0_f64, f64::max)
        .max(f64::MIN_POSITIVE);

    // --- stage 2: feature reranking ---
    let mut out: Vec<SearchHit> = rrf
        .keys()
        .map(|path| {
            let features = features_for(path, rrf[path] / max_rrf, ctx);
            let score = features.iter().map(|f| f.contribution).sum();

            let mut stage_scores = stages.remove(path).unwrap_or_default();
            stage_scores.sort_by(|a, b| a.mode.as_str().cmp(b.mode.as_str()));

            let mut retrieval_paths: Vec<String> = stage_scores
                .iter()
                .map(|s| format!("search:{}#{}", s.mode.as_str(), s.rank))
                .collect();
            retrieval_paths.push("rank:rrf".to_string());
            retrieval_paths.push("rank:features".to_string());

            let sub = best_hit.get(path);
            SearchHit {
                path: path.clone(),
                symbol_path: sub.and_then(|h| h.symbol_path.clone()),
                line: sub.and_then(|h| h.line),
                snippet: sub.and_then(|h| h.snippet.clone()),
                score,
                explanation: ScoreExplanation {
                    stage_scores,
                    features,
                    retrieval_paths,
                },
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.path.cmp(&b.path))
    });
    out.truncate(limit);
    out
}

fn features_for(path: &str, rrf_norm: f64, ctx: &RankContext) -> Vec<Feature> {
    let w = ctx.weights;

    let recency = ctx.git_recency.get(path).copied().unwrap_or(0.0);
    let churn = ctx.git_churn.get(path).copied().unwrap_or(0.0);

    let import_distance = match ctx.import_distance.get(path) {
        Some(&d) => 1.0 / (1.0 + d as f64),
        None => 0.0,
    };

    let near_anchor = ctx
        .import_distance
        .get(path)
        .map(|&d| d <= 2)
        .unwrap_or(false);
    let test_proximity = if ctx.test_files.contains(path) && near_anchor {
        1.0
    } else {
        0.0
    };

    vec![
        Feature::new("rrf", rrf_norm, w.rrf),
        Feature::new("recency", recency, w.recency),
        Feature::new("churn", churn, w.churn),
        Feature::new("import_distance", import_distance, w.import_distance),
        Feature::new("test_proximity", test_proximity, w.test_proximity),
        Feature::new("path_prior", path_prior(path), w.path_prior),
    ]
}

/// A coarse prior on where source that matters tends to live. Positive
/// for conventional source roots, strongly negative for build output and
/// vendored code, zero otherwise.
fn path_prior(path: &str) -> f64 {
    let p = path.to_lowercase();
    const JUNK: [&str; 7] = [
        "target/",
        "node_modules/",
        "vendor/",
        "dist/",
        "build/",
        ".min.",
        "generated",
    ];
    if JUNK.iter().any(|j| p.contains(j)) {
        return -1.0;
    }
    if p.starts_with("src/") || p.starts_with("lib/") || p.starts_with("crates/") {
        return 0.5;
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, score: f64) -> ModeHit {
        ModeHit {
            path: path.into(),
            symbol_path: None,
            line: Some(1),
            snippet: None,
            raw_score: score,
        }
    }

    #[test]
    fn a_file_ranked_by_two_modes_beats_one_ranked_by_a_single_mode() {
        let per_mode = vec![
            (
                SearchMode::Lexical,
                vec![hit("a.rs", 9.0), hit("b.rs", 1.0)],
            ),
            (
                SearchMode::Semantic,
                vec![hit("a.rs", 0.9), hit("c.rs", 0.8)],
            ),
        ];
        let hits = fuse(&per_mode, &RankContext::default(), 10);
        assert_eq!(hits[0].path, "a.rs");
    }

    #[test]
    fn every_hit_score_equals_its_explanation() {
        let per_mode = vec![
            (
                SearchMode::Lexical,
                vec![hit("src/a.rs", 5.0), hit("target/b.rs", 4.0)],
            ),
            (SearchMode::Symbol, vec![hit("src/a.rs", 3.0)]),
        ];
        let mut ctx = RankContext::default();
        ctx.git_recency.insert("src/a.rs".into(), 0.7);
        ctx.import_distance.insert("src/a.rs".into(), 1);

        let hits = fuse(&per_mode, &ctx, 10);
        for h in &hits {
            assert!(
                (h.score - h.explanation.recompute()).abs() < 1e-12,
                "score {} != recompute {} for {}",
                h.score,
                h.explanation.recompute(),
                h.path
            );
            assert!(
                h.explanation.is_complete(),
                "incomplete explanation for {}",
                h.path
            );
        }
    }

    #[test]
    fn the_path_prior_punishes_build_output() {
        assert_eq!(path_prior("target/debug/thing.rs"), -1.0);
        assert_eq!(path_prior("src/parser.rs"), 0.5);
        assert_eq!(path_prior("README.md"), 0.0);
    }

    #[test]
    fn retrieval_path_names_every_contributing_mode_then_the_rank_stages() {
        let per_mode = vec![
            (SearchMode::Lexical, vec![hit("a.rs", 1.0)]),
            (SearchMode::Git, vec![hit("a.rs", 1.0)]),
        ];
        let hits = fuse(&per_mode, &RankContext::default(), 10);
        let rp = &hits[0].explanation.retrieval_paths;
        assert!(rp.contains(&"search:lexical#1".to_string()));
        assert!(rp.contains(&"search:git#1".to_string()));
        assert_eq!(rp[rp.len() - 2], "rank:rrf");
        assert_eq!(rp[rp.len() - 1], "rank:features");
    }

    #[test]
    fn a_test_file_near_an_anchor_gets_the_proximity_feature() {
        let per_mode = vec![(SearchMode::Lexical, vec![hit("tests/parse_test.rs", 1.0)])];
        let mut ctx = RankContext::default();
        ctx.test_files.insert("tests/parse_test.rs".into());
        ctx.import_distance.insert("tests/parse_test.rs".into(), 1);

        let hits = fuse(&per_mode, &ctx, 10);
        let f = hits[0]
            .explanation
            .features
            .iter()
            .find(|f| f.name == "test_proximity")
            .unwrap();
        assert_eq!(f.value, 1.0);
    }
}
