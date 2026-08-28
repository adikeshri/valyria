//! The embedding pipeline end to end: index a fixture repository, embed a
//! generation, search it, and check the two things that have no symptom
//! of their own when they break — approximate-vs-exact agreement, and
//! chunk-level reuse across a re-embed.

use std::sync::Arc;

use valyria_embed::{EmbedError, EmbedPipeline, EmbedStore, HashingEmbedder};
use valyria_index::{IndexPipeline, IndexStore};
use valyria_lang::LanguageRegistry;
use valyria_store::{Migration, Store};
use valyria_testkit::TempWorkspace;
use valyria_types::Generation;

fn migrations() -> Vec<Migration> {
    let mut all: Vec<Migration> = valyria_index::MIGRATIONS.to_vec();
    all.extend(valyria_embed::MIGRATIONS.iter().copied());
    all
}

struct Harness {
    index: IndexPipeline,
    embed: EmbedPipeline,
}

impl Harness {
    fn new(ws: &TempWorkspace) -> Self {
        let store = Arc::new(Store::open_in_memory(&migrations()).unwrap());
        let registry = LanguageRegistry::with_builtin_languages().unwrap();
        Self {
            index: IndexPipeline::new(
                ws.path().to_path_buf(),
                LanguageRegistry::with_builtin_languages().unwrap(),
                IndexStore::new(store.clone()),
            ),
            embed: EmbedPipeline::new(
                ws.path().to_path_buf(),
                registry,
                Arc::new(HashingEmbedder::default()),
                EmbedStore::new(store),
            ),
        }
    }

    async fn index_and_embed(&self) -> Generation {
        let delta = self.index.bootstrap_unstaged(&|_| {}).await.unwrap();
        self.embed
            .bootstrap(self.index.store(), delta.generation)
            .await
            .unwrap();
        delta.generation
    }
}

fn fixture() -> TempWorkspace {
    let ws = TempWorkspace::new();
    ws.write(
        "src/parser.rs",
        "//! Parse a token stream into a syntax tree.\n\
         pub struct Parser;\n\n\
         impl Parser {\n\
             pub fn parse(&self, tokens: &[Token]) -> Ast {\n\
                 // walk the tokens and build abstract syntax tree nodes\n\
                 Ast::default()\n\
             }\n\
         }\n",
    )
    .write(
        "src/net.rs",
        "//! Network client: retry, backoff, timeouts.\n\
         pub struct Client;\n\n\
         impl Client {\n\
             pub fn request(&self, url: &str) -> Response {\n\
                 // open a socket, send the http request, retry on failure\n\
                 Response::default()\n\
             }\n\
         }\n",
    )
    .write(
        "README.md",
        "# Fixture\n\nA small repository about parsing and networking.\n",
    );
    ws
}

#[tokio::test]
async fn a_semantic_query_finds_the_relevant_file() {
    let ws = fixture();
    let h = Harness::new(&ws);
    let gen = h.index_and_embed().await;

    let embedder = HashingEmbedder::default();
    let query = valyria_embed::Embedder::embed(
        &embedder,
        "build an abstract syntax tree by walking parser tokens",
    );

    let hits = h.embed.store().search(gen, &query, 3).await.unwrap();

    assert!(!hits.is_empty(), "semantic search returned nothing");
    assert_eq!(
        hits[0].path, "src/parser.rs",
        "the parsing query should rank the parser file first, got {hits:?}"
    );
}

#[tokio::test]
async fn approximate_and_exact_search_agree_on_the_top_result() {
    let ws = fixture();
    let h = Harness::new(&ws);
    let gen = h.index_and_embed().await;

    let embedder = HashingEmbedder::default();
    for q in [
        "parse tokens into a syntax tree",
        "retry a network request with backoff",
        "a small repository",
    ] {
        let query = valyria_embed::Embedder::embed(&embedder, q);
        let approx = h.embed.store().search(gen, &query, 3).await.unwrap();
        let exact = h.embed.store().search_exact(gen, &query, 3).await.unwrap();
        assert_eq!(
            approx.first().map(|h| &h.path),
            exact.first().map(|h| &h.path),
            "hnsw and brute force disagree on the nearest chunk for {q:?}"
        );
    }
}

#[tokio::test]
async fn re_embedding_reuses_the_vectors_of_unchanged_chunks() {
    let ws = fixture();
    let h = Harness::new(&ws);
    let gen1 = h.index_and_embed().await;
    let built1 = h.embed.store().stats(gen1).await.unwrap();
    assert_eq!(built1.reused, 0, "the first build has nothing to reuse");

    // Change one file; the other two are untouched.
    ws.write(
        "src/parser.rs",
        "//! Parse a token stream into a syntax tree, now with error recovery.\n\
         pub struct Parser;\n",
    );
    let delta = h
        .index
        .apply_paths(&["src/parser.rs".to_string()])
        .await
        .unwrap();
    assert_ne!(delta.generation, gen1);

    let stats = h
        .embed
        .reembed(h.index.store(), delta.generation, gen1)
        .await
        .unwrap();

    assert!(
        stats.reused > 0,
        "chunks from net.rs and README.md should have been reused, none were"
    );
    assert!(
        stats.reused < stats.chunks,
        "the changed file must be re-embedded"
    );
}

#[tokio::test]
async fn searching_an_unembedded_generation_is_a_typed_error() {
    let ws = fixture();
    let h = Harness::new(&ws);
    let delta = h.index.bootstrap_unstaged(&|_| {}).await.unwrap();

    let query = valyria_embed::Embedder::embed(&HashingEmbedder::default(), "anything");
    let err = h
        .embed
        .store()
        .search(delta.generation, &query, 5)
        .await
        .unwrap_err();
    assert!(matches!(err, EmbedError::NotBuilt(g) if g == delta.generation));
}
