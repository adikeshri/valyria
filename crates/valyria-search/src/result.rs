//! Search results, and the explanation attached to every one of them.
//!
//! > Explainability is a hard requirement, not a debug feature: every
//! > result carries a `ScoreExplanation { stage_scores, features,
//! > retrieval_paths }`.
//!
//! [`ScoreExplanation`] is populated for *every* hit, whether or not a
//! caller asked to see it, and [`ScoreExplanation::recompute`] proves the
//! final score is exactly the weighted sum of the features shown — the
//! number cannot drift from its own explanation.

use serde::{Deserialize, Serialize};
use valyria_types::{Provenance, ProvenanceSource};

use crate::query::SearchMode;

/// One hit: a location, and why it ranked where it did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    /// The definition the hit fell inside, when it fell inside one.
    pub symbol_path: Option<String>,
    /// 1-based line of the match, when the mode produces a line.
    pub line: Option<u32>,
    /// A short excerpt around the match, for display.
    pub snippet: Option<String>,
    /// Final rank score. Equal to `explanation.recompute()`.
    pub score: f64,
    pub explanation: ScoreExplanation,
}

impl SearchHit {
    /// The `Provenance` record this hit would carry into model context
    /// (D3, §14): the ordered retrieval path and the final score, ready
    /// for `context.explain` to render.
    pub fn provenance(&self) -> Provenance {
        let mut p = Provenance::new(ProvenanceSource::File {
            path: self.path.clone(),
        });
        for step in &self.explanation.retrieval_paths {
            p = p.with_step(step.clone());
        }
        p.with_score(self.score)
    }
}

/// How one retrieval mode ranked this location before fusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageScore {
    pub mode: SearchMode,
    /// 1-based rank within that mode's own result list.
    pub rank: usize,
    /// The mode's raw score for this location, in whatever units that
    /// mode produces (cosine similarity, term-frequency, 1.0 for a graph
    /// hit, …). Recorded for transparency; fusion uses the rank, not
    /// this.
    pub raw_score: f64,
}

/// One reranking feature and its contribution to the final score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    pub name: String,
    /// The feature's value, normalized to roughly `[0, 1]`.
    pub value: f64,
    pub weight: f64,
    /// `value * weight` — what this feature added to the score.
    pub contribution: f64,
}

impl Feature {
    pub fn new(name: impl Into<String>, value: f64, weight: f64) -> Self {
        Self {
            name: name.into(),
            value,
            weight,
            contribution: value * weight,
        }
    }
}

/// The complete derivation of a hit's score.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScoreExplanation {
    /// Every mode that returned this location, and where it ranked.
    pub stage_scores: Vec<StageScore>,
    /// Every reranking feature and its weighted contribution.
    pub features: Vec<Feature>,
    /// The ordered pipeline this hit passed through, e.g.
    /// `["search:lexical#2", "search:semantic#5", "rank:rrf",
    /// "rank:features"]` — the same shape `Provenance::retrieval_path`
    /// wants.
    pub retrieval_paths: Vec<String>,
}

impl ScoreExplanation {
    /// The score implied by the features shown. [`SearchHit::score`] is
    /// set from exactly this, so a caller (or a test) can confirm the
    /// number matches its explanation.
    pub fn recompute(&self) -> f64 {
        self.features.iter().map(|f| f.contribution).sum()
    }

    /// Every mode that contributed, in the order they were run.
    pub fn contributing_modes(&self) -> Vec<SearchMode> {
        self.stage_scores.iter().map(|s| s.mode).collect()
    }

    pub fn is_complete(&self) -> bool {
        !self.stage_scores.is_empty()
            && !self.features.is_empty()
            && !self.retrieval_paths.is_empty()
    }
}

/// The outcome of a search: the ranked hits, which modes ran, and which
/// modes were asked for but could not contribute (and why).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    pub modes_run: Vec<SearchMode>,
    /// Human-readable notes about modes that stepped aside — "semantic:
    /// no embeddings for generation 3", "git: not a git repository".
    /// Never an error: a missing mode degrades the result, it does not
    /// fail it.
    pub degraded: Vec<String>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_features_contribution_is_value_times_weight() {
        let f = Feature::new("recency", 0.5, 0.2);
        assert!((f.contribution - 0.1).abs() < 1e-12);
    }

    #[test]
    fn recompute_is_the_sum_of_contributions() {
        let expl = ScoreExplanation {
            features: vec![
                Feature::new("rrf", 0.8, 1.0),
                Feature::new("recency", 0.5, 0.2),
                Feature::new("path_prior", -1.0, 0.1),
            ],
            ..Default::default()
        };
        assert!((expl.recompute() - (0.8 + 0.1 - 0.1)).abs() < 1e-12);
    }

    #[test]
    fn an_explanation_needs_all_three_parts_to_be_complete() {
        let mut expl = ScoreExplanation {
            stage_scores: vec![StageScore {
                mode: SearchMode::Lexical,
                rank: 1,
                raw_score: 1.0,
            }],
            features: vec![Feature::new("rrf", 1.0, 1.0)],
            retrieval_paths: vec![],
        };
        assert!(!expl.is_complete());
        expl.retrieval_paths.push("rank:rrf".into());
        assert!(expl.is_complete());
    }

    #[test]
    fn provenance_carries_the_retrieval_path_and_score() {
        let hit = SearchHit {
            path: "src/x.rs".into(),
            symbol_path: None,
            line: Some(3),
            snippet: None,
            score: 0.75,
            explanation: ScoreExplanation {
                retrieval_paths: vec!["search:lexical#1".into(), "rank:rrf".into()],
                ..Default::default()
            },
        };
        let p = hit.provenance();
        assert_eq!(p.retrieval_path, vec!["search:lexical#1", "rank:rrf"]);
        assert_eq!(p.score, Some(0.75));
    }
}
