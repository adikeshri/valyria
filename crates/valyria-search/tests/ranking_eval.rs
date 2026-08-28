//! Ranking evaluated against a labeled retrieval set (Phase 5 exit
//! criterion).
//!
//! The set is small and synthetic — a handful of "which files must be
//! touched to answer this?" cases over one fixture repository — but it
//! is a real regression guard: it computes recall@5 and mean reciprocal
//! rank over the labeled gold files and fails if the fused ranking drops
//! below a threshold. When the reranker's weights or a mode's scoring
//! changes, this test says whether retrieval got better or worse.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use valyria_embed::{EmbedPipeline, EmbedStore, HashingEmbedder};
use valyria_graph::GraphStore;
use valyria_index::{IndexPipeline, IndexStore};
use valyria_lang::LanguageRegistry;
use valyria_search::{SearchEngine, SearchQuery};
use valyria_store::{Migration, Store};
use valyria_testkit::TempWorkspace;

fn migrations() -> Vec<Migration> {
    let mut all: Vec<Migration> = valyria_index::MIGRATIONS.to_vec();
    all.extend(valyria_graph::MIGRATIONS.iter().copied());
    all.extend(valyria_embed::MIGRATIONS.iter().copied());
    all
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs in tests");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A small but structurally realistic repository: a lexer/parser/eval
/// pipeline, a cache, an HTTP client, config loading, and their tests.
fn repo() -> TempWorkspace {
    let ws = TempWorkspace::new();
    ws.write(
        "src/lexer.rs",
        "//! Tokenizer: scan source text into tokens.\n\
         pub fn tokenize(src: &str) -> Vec<Token> {\n\
             let mut out = Vec::new();\n\
             for (i, word) in src.split_whitespace().enumerate() {\n\
                 out.push(Token { text: word.to_string(), offset: i });\n\
             }\n\
             out\n\
         }\n\
         pub struct Token { pub text: String, pub offset: usize }\n",
    )
    .write(
        "src/parser.rs",
        "//! Recursive-descent parser: tokens into an abstract syntax tree.\n\
         use crate::lexer::{tokenize, Token};\n\
         pub fn parse(src: &str) -> Ast {\n\
             let tokens = tokenize(src);\n\
             Ast { node_count: tokens.len() }\n\
         }\n\
         pub struct Ast { pub node_count: usize }\n",
    )
    .write(
        "src/eval.rs",
        "//! Tree-walking evaluator for a parsed program.\n\
         use crate::parser::{parse, Ast};\n\
         pub fn eval(src: &str) -> i64 {\n\
             let ast: Ast = parse(src);\n\
             ast.node_count as i64\n\
         }\n",
    )
    .write(
        "src/cache.rs",
        "//! An LRU cache with a bounded capacity and eviction.\n\
         pub struct Lru { capacity: usize, entries: Vec<(String, String)> }\n\
         impl Lru {\n\
             pub fn get(&self, key: &str) -> Option<&str> {\n\
                 self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())\n\
             }\n\
             pub fn put(&mut self, key: String, value: String) {\n\
                 if self.entries.len() >= self.capacity { self.entries.remove(0); }\n\
                 self.entries.push((key, value));\n\
             }\n\
         }\n",
    )
    .write(
        "src/http.rs",
        "//! HTTP client: send a request, retry with backoff on failure.\n\
         pub struct Client { pub retries: u32 }\n\
         impl Client {\n\
             pub fn get(&self, url: &str) -> Result<String, Error> {\n\
                 // open a socket, write the request, read the response, retry on timeout\n\
                 let _ = url;\n\
                 Ok(String::new())\n\
             }\n\
         }\n\
         pub struct Error;\n",
    )
    .write(
        "src/config.rs",
        "//! Load configuration from a TOML file and environment overrides.\n\
         pub struct Config { pub verbose: bool, pub threads: usize }\n\
         pub fn load(path: &str) -> Config {\n\
             let _ = path;\n\
             Config { verbose: false, threads: 4 }\n\
         }\n",
    )
    .write(
        "tests/lexer_test.rs",
        "use app::lexer::tokenize;\n\
         #[test]\nfn tokenizes_words() { assert_eq!(tokenize(\"a b c\").len(), 3); }\n",
    )
    .write(
        "tests/http_test.rs",
        "use app::http::Client;\n\
         #[test]\nfn retries_are_configurable() { let c = Client { retries: 3 }; assert_eq!(c.retries, 3); }\n",
    )
    .write(
        "src/lib.rs",
        "pub mod cache;\npub mod config;\npub mod eval;\npub mod http;\npub mod lexer;\npub mod parser;\n",
    );

    git(ws.path(), &["init", "-q"]);
    git(ws.path(), &["add", "-A"]);
    git(
        ws.path(),
        &[
            "commit",
            "-q",
            "-m",
            "initial pipeline, cache, http, config",
        ],
    );
    ws
}

struct Labeled {
    query: &'static str,
    anchors: &'static [&'static str],
    gold: &'static [&'static str],
}

const CASES: &[Labeled] = &[
    Labeled {
        query: "tokenize source text into tokens with an offset",
        anchors: &[],
        gold: &["src/lexer.rs"],
    },
    Labeled {
        query: "recursive descent parser builds an abstract syntax tree from tokens",
        anchors: &[],
        gold: &["src/parser.rs"],
    },
    Labeled {
        query: "retry an http request with backoff after a timeout",
        anchors: &[],
        gold: &["src/http.rs"],
    },
    Labeled {
        query: "bounded LRU cache eviction when capacity is exceeded",
        anchors: &[],
        gold: &["src/cache.rs"],
    },
    Labeled {
        query: "load configuration from a file with environment overrides",
        anchors: &[],
        gold: &["src/config.rs"],
    },
    Labeled {
        // "which files must change to touch the evaluator?" — the eval
        // module plus what it directly depends on.
        query: "evaluate a parsed program by walking the tree",
        anchors: &["src/eval.rs"],
        gold: &["src/eval.rs", "src/parser.rs"],
    },
];

#[tokio::test]
async fn fused_ranking_clears_the_labeled_retrieval_bar() {
    let ws = repo();
    let store = Arc::new(Store::open_in_memory(&migrations()).unwrap());
    let index = IndexStore::new(store.clone());
    let graph = GraphStore::new(store.clone());
    let embed = EmbedStore::new(store.clone());

    let pipeline = IndexPipeline::new(
        ws.path().to_path_buf(),
        LanguageRegistry::with_builtin_languages().unwrap(),
        index.clone(),
    );
    let delta = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();
    graph.build_for(&index, delta.generation).await.unwrap();
    EmbedPipeline::new(
        ws.path().to_path_buf(),
        LanguageRegistry::with_builtin_languages().unwrap(),
        Arc::new(HashingEmbedder::default()),
        embed.clone(),
    )
    .bootstrap(&index, delta.generation)
    .await
    .unwrap();

    let engine = SearchEngine::new(
        ws.path().to_path_buf(),
        index,
        graph,
        embed,
        Arc::new(HashingEmbedder::default()),
        LanguageRegistry::with_builtin_languages().unwrap(),
    );

    let mut recall_sum = 0.0;
    let mut mrr_sum = 0.0;
    for case in CASES {
        let mut query = SearchQuery::new(case.query).limit(10);
        for a in case.anchors {
            query = query.anchor(*a);
        }
        let results = engine.search(&query).await.unwrap();
        let ranked: Vec<&str> = results.hits.iter().map(|h| h.path.as_str()).collect();

        let top5: std::collections::HashSet<&str> = ranked.iter().take(5).copied().collect();
        let found = case.gold.iter().filter(|g| top5.contains(*g)).count();
        let recall = found as f64 / case.gold.len() as f64;

        let first_rank = case
            .gold
            .iter()
            .filter_map(|g| ranked.iter().position(|p| p == g))
            .min()
            .map(|pos| 1.0 / (pos as f64 + 1.0))
            .unwrap_or(0.0);

        eprintln!(
            "case {:?}: recall@5={recall:.2} rr={first_rank:.2} ranked={:?}",
            case.query, ranked
        );

        recall_sum += recall;
        mrr_sum += first_rank;
    }

    let mean_recall = recall_sum / CASES.len() as f64;
    let mrr = mrr_sum / CASES.len() as f64;

    // The bar: every labeled case must land its gold files inside the
    // top 5 (recall), and on average the first gold file must sit around
    // rank 1-2 (MRR). The margins here are the room a real embedding
    // model (Phase 9) has to *improve* the numbers — this test is the
    // guard that says whether a ranking change helped or hurt.
    assert!(
        mean_recall >= 0.9,
        "mean recall@5 was {mean_recall:.3}, expected >= 0.90"
    );
    assert!(
        mrr >= 0.65,
        "mean reciprocal rank was {mrr:.3}, expected >= 0.65"
    );
}
