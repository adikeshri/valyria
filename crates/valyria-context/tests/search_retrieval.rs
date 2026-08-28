//! `intelligence` feature: `SearchRetriever` turns `valyria-search` hits
//! into context candidates, with symbol structure pulled from the index so
//! the compressor can shed a file symbol-by-symbol — never mid-body.

#![cfg(feature = "intelligence")]

use std::sync::Arc;

use valyria_context::{
    CandidateContent, ContextEngine, EngineInput, PromptAssembler, RetrievalQuery, Retriever,
    SearchRetriever,
};
use valyria_embed::{EmbedPipeline, EmbedStore, HashingEmbedder};
use valyria_graph::GraphStore;
use valyria_index::{IndexPipeline, IndexStore};
use valyria_lang::LanguageRegistry;
use valyria_search::SearchEngine;
use valyria_store::{Migration, Store};
use valyria_testkit::TempWorkspace;
use valyria_util::DeterministicRng;

fn migrations() -> Vec<Migration> {
    let mut all: Vec<Migration> = valyria_index::MIGRATIONS.to_vec();
    all.extend(valyria_graph::MIGRATIONS.iter().copied());
    all.extend(valyria_embed::MIGRATIONS.iter().copied());
    all
}

fn fixture() -> TempWorkspace {
    let ws = TempWorkspace::new();
    ws.write(
        "src/lexer.rs",
        "//! Tokenizer: scan source text into tokens.\n\
         \n\
         /// Split `src` into whitespace-delimited tokens.\n\
         pub fn tokenize(src: &str) -> Vec<Token> {\n\
         \x20   let mut out = Vec::new();\n\
         \x20   for (i, word) in src.split_whitespace().enumerate() {\n\
         \x20       out.push(Token { text: word.to_string(), offset: i });\n\
         \x20   }\n\
         \x20   out\n\
         }\n\
         \n\
         /// One lexical token and where it started.\n\
         pub struct Token {\n\
         \x20   pub text: String,\n\
         \x20   pub offset: usize,\n\
         }\n",
    )
    .write(
        "src/parser.rs",
        "//! Recursive-descent parser.\n\
         use crate::lexer::{tokenize, Token};\n\
         \n\
         /// Parse `src` into an AST.\n\
         pub fn parse(src: &str) -> Ast {\n\
         \x20   let tokens = tokenize(src);\n\
         \x20   Ast { node_count: tokens.len() }\n\
         }\n\
         \n\
         pub struct Ast {\n\
         \x20   pub node_count: usize,\n\
         }\n",
    );
    ws
}

async fn build(ws: &TempWorkspace) -> SearchRetriever {
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

    let engine = SearchEngine::new(
        ws.path().to_path_buf(),
        index.clone(),
        graph,
        embed,
        Arc::new(HashingEmbedder::default()),
        LanguageRegistry::with_builtin_languages().unwrap(),
    );
    SearchRetriever::new(engine, index)
}

#[tokio::test]
async fn hits_become_source_candidates_with_verbatim_symbol_bodies() {
    let ws = fixture();
    let retriever = build(&ws).await;

    let candidates = retriever
        .retrieve(&RetrievalQuery::new("tokenize the source text into tokens"))
        .await
        .unwrap();

    assert!(!candidates.is_empty(), "expected at least one candidate");

    // At least one candidate is structured source with symbols.
    let source = candidates
        .iter()
        .find_map(|c| match &c.content {
            CandidateContent::Source { path, symbols, .. } if !symbols.is_empty() => {
                Some((path.clone(), symbols.clone()))
            }
            _ => None,
        })
        .expect("a Source candidate with symbols");

    let (path, symbols) = source;
    let disk = std::fs::read_to_string(ws.path().join(&path)).unwrap();
    for s in &symbols {
        assert!(
            disk.contains(s.body.trim_end()),
            "symbol {} body is not a verbatim slice of {path}",
            s.symbol_path
        );
        assert!(disk.contains(s.signature.trim_end()));
    }

    // Provenance carries the search explanation.
    let lexer_cand = candidates
        .iter()
        .find(|c| c.label().contains("lexer.rs"))
        .expect("lexer.rs among the hits");
    assert!(!lexer_cand.provenance.retrieval_path.is_empty());
    assert!(lexer_cand.provenance.score.is_some());
}

#[tokio::test]
async fn context_engine_over_search_stays_within_budget_and_replays() {
    let ws = fixture();
    let retriever = build(&ws).await;
    let engine = ContextEngine::new(retriever).with_assembler(
        PromptAssembler::new()
            .with_policy("POLICY")
            .with_rng(Arc::new(DeterministicRng::from_seed(7))),
    );

    let input = EngineInput::new("add a comment to the tokenize function", 2_000)
        .with_query(RetrievalQuery::new("tokenize function in the lexer").anchor("src/lexer.rs"));
    let out = engine.build(input).await.unwrap();

    assert!(
        out.within_budget(),
        "assembled {} tokens, budget {}",
        out.total_tokens,
        out.allocation.available
    );

    let json = serde_json::to_string(&out.snapshot).unwrap();
    let back: valyria_context::ContextSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back.render(), out.messages);
}

#[tokio::test]
async fn explicit_paths_are_included_even_when_search_misses_them() {
    let ws = fixture();
    let retriever = build(&ws).await;

    let mut q = RetrievalQuery::new("something unrelated to any file content xyzzy");
    q.explicit_paths = vec!["src/parser.rs".to_string()];
    let candidates = retriever.retrieve(&q).await.unwrap();

    assert!(candidates.iter().any(|c| c.label().contains("parser.rs")));
}
