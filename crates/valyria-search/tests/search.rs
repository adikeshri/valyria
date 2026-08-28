//! Search end to end: index a real fixture repository, build the graph
//! and embeddings, and run every mode through [`SearchEngine`] —
//! including the two Phase 5 exit criteria: every hit is fully
//! explained, and search still works with embeddings switched off.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use valyria_embed::{EmbedPipeline, EmbedStore, HashingEmbedder};
use valyria_graph::GraphStore;
use valyria_index::{IndexPipeline, IndexStore};
use valyria_lang::LanguageRegistry;
use valyria_search::{SearchEngine, SearchError, SearchMode, SearchQuery};
use valyria_store::{Migration, Store};
use valyria_testkit::TempWorkspace;

fn migrations() -> Vec<Migration> {
    let mut all: Vec<Migration> = valyria_index::MIGRATIONS.to_vec();
    all.extend(valyria_graph::MIGRATIONS.iter().copied());
    all.extend(valyria_embed::MIGRATIONS.iter().copied());
    all
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs in tests");
    assert!(status.status.success(), "git {args:?} failed: {status:?}");
}

fn fixture() -> TempWorkspace {
    let ws = TempWorkspace::new();
    ws.write(
        "src/lexer.rs",
        "//! Turn source text into a stream of tokens.\n\
         pub fn tokenize(input: &str) -> Vec<Token> {\n\
             // scan the input characters and emit lexical tokens\n\
             input.split_whitespace().map(Token::word).collect()\n\
         }\n\
         pub struct Token;\n\
         impl Token { pub fn word(_: &str) -> Token { Token } }\n",
    )
    .write(
        "src/parser.rs",
        "//! Build a syntax tree from tokens.\n\
         use crate::lexer;\n\n\
         pub struct Parser;\n\n\
         impl Parser {\n\
             pub fn parse(&self, input: &str) -> Ast {\n\
                 let _tokens = lexer::tokenize(input);\n\
                 Ast\n\
             }\n\
         }\n\
         pub struct Ast;\n",
    )
    .write(
        "src/main.rs",
        "mod lexer;\nmod parser;\nfn main() {\n    let p = parser::Parser;\n    let _ = p.parse(\"hi\");\n}\n",
    )
    .write(
        "docs/design.md",
        "# Design\n\nThe lexer tokenizes; the parser builds an abstract syntax tree.\n",
    );

    git(ws.path(), &["init", "-q"]);
    git(ws.path(), &["add", "-A"]);
    git(
        ws.path(),
        &["commit", "-q", "-m", "add lexer and parser skeleton"],
    );
    ws
}

struct Harness {
    engine: SearchEngine,
}

impl Harness {
    async fn build(ws: &TempWorkspace, with_embeddings: bool) -> Self {
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

        if with_embeddings {
            let embed_pipeline = EmbedPipeline::new(
                ws.path().to_path_buf(),
                LanguageRegistry::with_builtin_languages().unwrap(),
                Arc::new(HashingEmbedder::default()),
                embed.clone(),
            );
            embed_pipeline
                .bootstrap(&index, delta.generation)
                .await
                .unwrap();
        }

        Self {
            engine: SearchEngine::new(
                ws.path().to_path_buf(),
                index,
                graph,
                embed,
                Arc::new(HashingEmbedder::default()),
                LanguageRegistry::with_builtin_languages().unwrap(),
            ),
        }
    }
}

#[tokio::test]
async fn lexical_search_finds_the_file_that_contains_the_phrase() {
    let ws = fixture();
    let h = Harness::build(&ws, true).await;

    let results = h
        .engine
        .search(&SearchQuery::new("tokenize the input characters").mode(SearchMode::Lexical))
        .await
        .unwrap();

    assert_eq!(
        results.hits[0].path, "src/lexer.rs",
        "results: {:#?}",
        results.hits
    );
}

#[tokio::test]
async fn symbol_search_finds_a_function_by_name() {
    let ws = fixture();
    let h = Harness::build(&ws, true).await;

    let results = h
        .engine
        .search(&SearchQuery::new("tokenize").mode(SearchMode::Symbol))
        .await
        .unwrap();

    let top = &results.hits[0];
    assert_eq!(top.path, "src/lexer.rs");
    assert_eq!(top.symbol_path.as_deref(), Some("tokenize"));
}

#[tokio::test]
async fn semantic_search_contributes_when_embeddings_exist() {
    let ws = fixture();
    let h = Harness::build(&ws, true).await;

    let results = h
        .engine
        .search(&SearchQuery::new("split text into lexical tokens").mode(SearchMode::Semantic))
        .await
        .unwrap();

    assert!(!results.hits.is_empty());
    assert!(
        results.degraded.iter().all(|d| !d.contains("semantic")),
        "semantic should not have degraded: {:?}",
        results.degraded
    );
}

#[tokio::test]
async fn search_works_fully_with_embeddings_disabled() {
    let ws = fixture();
    let h = Harness::build(&ws, false).await;

    let results = h
        .engine
        .search(&SearchQuery::new("tokenize the parser input"))
        .await
        .unwrap();

    // Semantic stepped aside, with a reason...
    assert!(
        results.degraded.iter().any(|d| d.contains("semantic")),
        "expected a semantic degradation note, got {:?}",
        results.degraded
    );
    // ...but the search still produced ranked, explained hits.
    assert!(!results.hits.is_empty());
    assert!(results.hits.iter().all(|h| h.explanation.is_complete()));
    assert!(results
        .hits
        .iter()
        .any(|h| h.path == "src/lexer.rs" || h.path == "src/parser.rs"));
}

#[tokio::test]
async fn every_hit_carries_a_complete_and_self_consistent_explanation() {
    let ws = fixture();
    let h = Harness::build(&ws, true).await;

    let results = h
        .engine
        .search(&SearchQuery::new("parse tokens into a syntax tree").anchor("src/parser.rs"))
        .await
        .unwrap();

    assert!(!results.hits.is_empty());
    for hit in &results.hits {
        assert!(
            hit.explanation.is_complete(),
            "incomplete explanation: {hit:#?}"
        );
        assert!(
            (hit.score - hit.explanation.recompute()).abs() < 1e-9,
            "score {} disagrees with its explanation {} for {}",
            hit.score,
            hit.explanation.recompute(),
            hit.path
        );
        let provenance = hit.provenance();
        assert!(!provenance.retrieval_path.is_empty());
        assert_eq!(provenance.score, Some(hit.score));
        // stage_scores name only modes that actually ran
        for stage in &hit.explanation.stage_scores {
            assert!(results.modes_run.contains(&stage.mode));
        }
    }
}

#[tokio::test]
async fn regex_mode_matches_a_pattern_and_rejects_a_broken_one() {
    let ws = fixture();
    let h = Harness::build(&ws, true).await;

    let ok = h
        .engine
        .search(&SearchQuery::new(r"fn \w+\(").mode(SearchMode::Regex))
        .await
        .unwrap();
    assert!(!ok.hits.is_empty());

    let err = h
        .engine
        .search(&SearchQuery::new(r"fn (unclosed").mode(SearchMode::Regex))
        .await
        .unwrap_err();
    assert!(matches!(err, SearchError::BadPattern { kind: "regex", .. }));
}

#[tokio::test]
async fn ast_mode_runs_a_tree_sitter_pattern() {
    let ws = fixture();
    let h = Harness::build(&ws, true).await;

    let results = h
        .engine
        .search(&SearchQuery::new("(function_item) @fn").mode(SearchMode::Ast))
        .await
        .unwrap();

    assert!(results.hits.iter().any(|h| h.path == "src/lexer.rs"));
}

#[tokio::test]
async fn dependency_mode_walks_the_graph_from_the_anchors() {
    let ws = fixture();
    let h = Harness::build(&ws, true).await;

    let results = h
        .engine
        .search(
            &SearchQuery::new("anything")
                .mode(SearchMode::Dependency)
                .anchor("src/parser.rs"),
        )
        .await
        .unwrap();

    // parser.rs imports lexer.rs and main.rs imports parser.rs — both
    // are within a couple of hops.
    let paths: Vec<&str> = results.hits.iter().map(|h| h.path.as_str()).collect();
    assert!(
        paths.contains(&"src/lexer.rs") || paths.contains(&"src/main.rs"),
        "dependency traversal found {paths:?}"
    );
}

#[tokio::test]
async fn git_mode_matches_a_commit_message() {
    let ws = fixture();
    let h = Harness::build(&ws, true).await;

    let results = h
        .engine
        .search(&SearchQuery::new("lexer skeleton").mode(SearchMode::Git))
        .await
        .unwrap();

    assert!(!results.hits.is_empty());
    assert!(results.degraded.iter().all(|d| !d.contains("git")));
}

#[tokio::test]
async fn searching_an_unindexed_workspace_is_a_typed_error() {
    let ws = TempWorkspace::new();
    ws.write("src/lib.rs", "pub fn f() {}\n");
    let store = Arc::new(Store::open_in_memory(&migrations()).unwrap());
    let engine = SearchEngine::new(
        ws.path().to_path_buf(),
        IndexStore::new(store.clone()),
        GraphStore::new(store.clone()),
        EmbedStore::new(store.clone()),
        Arc::new(HashingEmbedder::default()),
        LanguageRegistry::with_builtin_languages().unwrap(),
    );

    let err = engine.search(&SearchQuery::new("f")).await.unwrap_err();
    assert!(matches!(err, SearchError::NotIndexed));
}
