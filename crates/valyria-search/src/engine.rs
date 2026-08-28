//! [`SearchEngine`]: run the modes a query asked for, fuse them, and
//! explain the result.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use valyria_embed::{EmbedStore, Embedder};
use valyria_graph::{Direction, GraphStore, NodeId};
use valyria_index::IndexStore;
use valyria_lang::LanguageRegistry;
use valyria_types::Generation;

use crate::fusion::{self, FeatureWeights, RankContext};
use crate::modes::{self, ModeCtx, ModeHit};
use crate::query::{SearchMode, SearchQuery};
use crate::result::SearchResults;
use crate::{Result, SearchError};

/// How many commits back the ranking's recency/churn features look.
const HISTORY_DEPTH: usize = 200;

/// How far the import-distance feature traverses from the anchors.
const DISTANCE_DEPTH: usize = 4;

pub struct SearchEngine {
    root: PathBuf,
    index: IndexStore,
    graph: GraphStore,
    embed: EmbedStore,
    embedder: Arc<dyn Embedder>,
    registry: LanguageRegistry,
    weights: FeatureWeights,
}

impl std::fmt::Debug for SearchEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchEngine")
            .field("root", &self.root)
            .field("embedder", &self.embedder.id())
            .finish()
    }
}

impl SearchEngine {
    pub fn new(
        root: impl Into<PathBuf>,
        index: IndexStore,
        graph: GraphStore,
        embed: EmbedStore,
        embedder: Arc<dyn Embedder>,
        registry: LanguageRegistry,
    ) -> Self {
        Self {
            root: root.into(),
            index,
            graph,
            embed,
            embedder,
            registry,
            weights: FeatureWeights::default(),
        }
    }

    pub fn with_weights(mut self, weights: FeatureWeights) -> Self {
        self.weights = weights;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn search(&self, query: &SearchQuery) -> Result<SearchResults> {
        let generation = match self.index.current().await? {
            Some(info) => info.generation,
            None => return Err(SearchError::NotIndexed),
        };

        // The set of paths that are actually part of this generation.
        // Modes that read outside the index (git `show` reports tree
        // entries, a stale anchor names a deleted file) must not be able
        // to surface a path the index does not know.
        let indexed_paths: HashSet<String> = self
            .index
            .files(generation)
            .await?
            .into_iter()
            .map(|f| f.path)
            .collect();

        let modes = query.effective_modes();
        let needs_text = modes
            .iter()
            .any(|m| matches!(m, SearchMode::Lexical | SearchMode::Regex | SearchMode::Ast));
        let files = if needs_text {
            modes::load_files(&self.root, &self.index, generation).await?
        } else {
            Vec::new()
        };

        let repo = valyria_git::Repo::open(&self.root).ok();

        let ctx = ModeCtx {
            generation,
            root: &self.root,
            index: &self.index,
            graph: &self.graph,
            embed: &self.embed,
            embedder: self.embedder.as_ref(),
            registry: &self.registry,
            repo: repo.as_ref(),
            files: &files,
            query,
        };

        let mut per_mode: Vec<(SearchMode, Vec<ModeHit>)> = Vec::new();
        let mut degraded: Vec<String> = Vec::new();

        for mode in &modes {
            let outcome = match mode {
                SearchMode::Lexical => modes::lexical::run(&ctx).await?,
                SearchMode::Regex => modes::regex::run(&ctx).await?,
                SearchMode::Symbol => modes::symbol::run(&ctx).await?,
                SearchMode::Semantic => modes::semantic::run(&ctx).await?,
                SearchMode::Ast => modes::ast::run(&ctx).await?,
                SearchMode::Dependency => modes::dependency::run(&ctx).await?,
                SearchMode::Git => modes::git::run(&ctx).await?,
            };
            if let Some(note) = outcome.degraded {
                degraded.push(note);
            }
            let hits: Vec<ModeHit> = outcome
                .hits
                .into_iter()
                .filter(|h| indexed_paths.contains(&h.path))
                .collect();
            if !hits.is_empty() {
                per_mode.push((*mode, hits));
            }
        }

        let rank_ctx = self
            .build_rank_context(generation, query, repo.as_ref())
            .await?;

        let hits = fusion::fuse(&per_mode, &rank_ctx, query.limit);

        Ok(SearchResults {
            hits,
            modes_run: modes,
            degraded,
        })
    }

    async fn build_rank_context(
        &self,
        generation: Generation,
        query: &SearchQuery,
        repo: Option<&valyria_git::Repo>,
    ) -> Result<RankContext> {
        let (git_recency, git_churn) = match repo {
            Some(repo) => history_features(repo),
            None => (HashMap::new(), HashMap::new()),
        };

        let import_distance =
            if !query.anchors.is_empty() && self.graph.is_built(generation).await? {
                self.graph_distances(generation, &query.anchors).await?
            } else {
                HashMap::new()
            };

        let test_files: HashSet<String> = self
            .index
            .tests(generation)
            .await?
            .into_iter()
            .map(|t| t.path)
            .collect();

        Ok(RankContext {
            anchors: query.anchors.clone(),
            git_recency,
            git_churn,
            import_distance,
            test_files,
            weights: self.weights,
        })
    }

    /// Fewest import/call hops from any anchor to each reachable file.
    async fn graph_distances(
        &self,
        generation: Generation,
        anchors: &[String],
    ) -> Result<HashMap<String, usize>> {
        let mut distance: HashMap<String, usize> = HashMap::new();
        let mut frontier: Vec<String> = Vec::new();
        for anchor in anchors {
            distance.insert(anchor.clone(), 0);
            frontier.push(anchor.clone());
        }

        for depth in 1..=DISTANCE_DEPTH {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for path in &frontier {
                let node = NodeId::file(path);
                let edges = self
                    .graph
                    .neighbors(generation, &node, Direction::Both, &[])
                    .await?;
                for edge in edges {
                    for endpoint in [&edge.from, &edge.to] {
                        if let Some(file) = endpoint.file_path() {
                            if !distance.contains_key(file) {
                                distance.insert(file.to_string(), depth);
                                next.push(file.to_string());
                            }
                        }
                    }
                }
            }
            frontier = next;
        }
        Ok(distance)
    }
}

/// Per-file recency and churn from the last [`HISTORY_DEPTH`] commits.
/// Recency is `1.0` for a file in the newest commit and decays linearly
/// with the age of the newest commit that touched it; churn is the
/// commit count over that window, normalized to the busiest file.
fn history_features(repo: &valyria_git::Repo) -> (HashMap<String, f64>, HashMap<String, f64>) {
    let log = match repo.log(HISTORY_DEPTH) {
        Ok(log) => log,
        Err(_) => return (HashMap::new(), HashMap::new()),
    };
    let depth = log.len().max(1) as f64;

    let mut newest_age: HashMap<String, usize> = HashMap::new();
    let mut counts: HashMap<String, usize> = HashMap::new();

    for (age, commit) in log.iter().enumerate() {
        let Ok(files) = repo.show(&commit.sha) else {
            continue;
        };
        for file in files {
            counts
                .entry(file.path.clone())
                .and_modify(|c| *c += 1)
                .or_insert(1);
            newest_age.entry(file.path).or_insert(age);
        }
    }

    let max_count = counts.values().copied().max().unwrap_or(1) as f64;

    let recency = newest_age
        .into_iter()
        .map(|(path, age)| (path, 1.0 - age as f64 / depth))
        .collect();
    let churn = counts
        .into_iter()
        .map(|(path, c)| (path, c as f64 / max_count))
        .collect();

    (recency, churn)
}
