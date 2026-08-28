//! End-to-end indexing: bootstrap, generational reads, the incremental
//! pipeline, and the drift check — against real files on disk and a real
//! SQLite database.

use std::sync::Arc;

use valyria_index::{
    verify_index, GenerationStage, IndexPipeline, IndexProgress, IndexStore, ScanOptions,
};
use valyria_lang::{LanguageRegistry, SymbolKind};
use valyria_store::Store;
use valyria_testkit::TempWorkspace;
use valyria_types::Generation;

fn pipeline_for(ws: &TempWorkspace) -> IndexPipeline {
    let store = Store::open_in_memory(valyria_index::MIGRATIONS).unwrap();
    IndexPipeline::new(
        ws.path().to_path_buf(),
        LanguageRegistry::with_builtin_languages().unwrap(),
        IndexStore::new(Arc::new(store)),
    )
}

fn fixture() -> TempWorkspace {
    let ws = TempWorkspace::new();
    ws.write(
        "src/parser.rs",
        "pub struct Parser;\n\nimpl Parser {\n    pub fn parse(&self) -> bool { helper() }\n}\n\nfn helper() -> bool { true }\n",
    )
    .write("src/lib.rs", "pub mod parser;\n")
    .write("README.md", "# Fixture\n");
    ws
}

#[tokio::test]
async fn bootstrap_indexes_files_and_symbols() {
    let ws = fixture();
    let pipeline = pipeline_for(&ws);

    let delta = pipeline.bootstrap(&|_| {}).await.unwrap();
    let store = pipeline.store();

    let files = store.files(delta.generation).await.unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["README.md", "src/lib.rs", "src/parser.rs"]);

    let symbols = store
        .symbols_in(delta.generation, "src/parser.rs")
        .await
        .unwrap();
    let names: Vec<&str> = symbols.iter().map(|s| s.symbol_path.as_str()).collect();
    assert_eq!(names, ["Parser", "Parser::parse", "helper"]);
}

#[tokio::test]
async fn bootstrap_publishes_a_files_only_generation_before_symbols() {
    // §4.14: a large repository must be usable before the whole pipeline
    // finishes. The staged generation is what makes that true, so its
    // existence is asserted rather than assumed.
    let ws = fixture();
    let pipeline = pipeline_for(&ws);

    let seen = std::sync::Mutex::new(Vec::new());
    pipeline
        .bootstrap(&|p| {
            if !matches!(p, IndexProgress::Scanning(_)) {
                seen.lock().unwrap().push(p);
            }
        })
        .await
        .unwrap();

    let seen = seen.into_inner().unwrap();
    assert!(matches!(seen[0], IndexProgress::Staged { .. }));
    assert!(matches!(seen[1], IndexProgress::Complete { .. }));

    let generations = pipeline.store().generations().await.unwrap();
    assert_eq!(generations.len(), 2);
    assert_eq!(generations[0].stage, GenerationStage::FilesOnly);
    assert_eq!(generations[0].symbol_count, 0);
    assert_eq!(generations[1].stage, GenerationStage::Complete);
    assert!(generations[1].symbol_count > 0);
    // The file list is complete at the staged generation — that is the
    // point of publishing it early.
    assert_eq!(generations[0].file_count, generations[1].file_count);
}

#[tokio::test]
async fn an_older_generation_still_sees_the_repository_as_it_was() {
    // D8, the property the whole versioned schema exists for: a step that
    // planned against generation N keeps seeing N's repository however far
    // the index moves on.
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    let first = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    ws.write(
        "src/parser.rs",
        "pub struct Parser;\n\nimpl Parser {\n    pub fn parse(&self) -> bool { true }\n    pub fn reset(&mut self) {}\n}\n",
    );
    let second = pipeline
        .apply_paths(&["src/parser.rs".to_string()])
        .await
        .unwrap();
    assert!(second.generation > first.generation);

    let old: Vec<String> = pipeline
        .store()
        .symbols_in(first.generation, "src/parser.rs")
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.symbol_path)
        .collect();
    let new: Vec<String> = pipeline
        .store()
        .symbols_in(second.generation, "src/parser.rs")
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.symbol_path)
        .collect();

    assert_eq!(old, ["Parser", "Parser::parse", "helper"]);
    assert_eq!(new, ["Parser", "Parser::parse", "Parser::reset"]);
}

#[tokio::test]
async fn an_incremental_update_leaves_untouched_files_alone() {
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    ws.write("src/lib.rs", "pub mod parser;\npub mod extra;\n");
    let delta = pipeline
        .apply_paths(&["src/lib.rs".to_string(), "src/parser.rs".to_string()])
        .await
        .unwrap();

    // `src/parser.rs` was offered but is byte-identical, so it is not
    // reported as modified: the content hash, not the caller's claim,
    // decides what changed.
    assert_eq!(delta.modified, ["src/lib.rs"]);
    assert!(delta.added.is_empty());
    assert!(delta.removed.is_empty());
}

#[tokio::test]
async fn a_new_file_is_added_and_a_deleted_one_is_removed() {
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    ws.write("src/extra.rs", "pub fn extra() {}\n");
    ws.remove("README.md");

    let delta = pipeline
        .apply_paths(&["src/extra.rs".to_string(), "README.md".to_string()])
        .await
        .unwrap();

    assert_eq!(delta.added, ["src/extra.rs"]);
    assert_eq!(delta.removed, ["README.md"]);

    let files = pipeline.store().files(delta.generation).await.unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["src/extra.rs", "src/lib.rs", "src/parser.rs"]);
}

#[tokio::test]
async fn an_update_that_changes_nothing_does_not_mint_a_generation() {
    // Otherwise a noisy watcher would invalidate every in-flight step's
    // snapshot for no reason at all.
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    let first = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    let delta = pipeline
        .apply_paths(&["src/parser.rs".to_string()])
        .await
        .unwrap();

    assert!(delta.is_empty());
    assert_eq!(delta.generation, first.generation);
    assert_eq!(pipeline.store().generations().await.unwrap().len(), 1);
}

#[tokio::test]
async fn resync_recovers_from_bulk_changes_the_watcher_never_reported() {
    // §4.15: a branch switch changes thousands of files at once. Resync
    // reconciles from what is actually on disk rather than from a replay
    // of events that may never have arrived.
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    ws.remove("src/parser.rs");
    ws.write("src/other.rs", "pub fn other() {}\n");
    ws.write("src/lib.rs", "pub mod other;\n");

    let delta = pipeline.resync().await.unwrap();
    assert_eq!(delta.added, ["src/other.rs"]);
    assert_eq!(delta.modified, ["src/lib.rs"]);
    assert_eq!(delta.removed, ["src/parser.rs"]);

    let drift = verify_index(&pipeline, delta.generation).await.unwrap();
    assert!(drift.is_clean(), "{drift:?}");
}

#[tokio::test]
async fn the_drift_check_is_clean_after_a_long_run_of_incremental_edits() {
    // The Phase 4 exit criterion: `verify-index` shows zero drift after a
    // fuzz of edits, renames and deletes. This is the deterministic
    // version of that, run on every CI job.
    let ws = TempWorkspace::new();
    let pipeline = pipeline_for(&ws);
    for i in 0..10 {
        ws.write(format!("src/m{i}.rs"), format!("pub fn f{i}() {{}}\n"));
    }
    let mut generation = pipeline
        .bootstrap_unstaged(&|_| {})
        .await
        .unwrap()
        .generation;

    for round in 0..10 {
        let mut touched = Vec::new();

        // Edit.
        let edited = format!("src/m{}.rs", round % 10);
        ws.write(
            &edited,
            format!("pub fn f{round}() {{}}\npub struct S{round};\n"),
        );
        touched.push(edited);

        // Create.
        let created = format!("src/new{round}.rs");
        ws.write(&created, format!("pub fn created{round}() {{}}\n"));
        touched.push(created);

        // Rename: a delete plus a create, which is what a watcher reports.
        if round > 0 {
            let from = format!("src/new{}.rs", round - 1);
            let to = format!("src/renamed{}.rs", round - 1);
            let content = ws.read(&from);
            ws.write(&to, content);
            ws.remove(&from);
            touched.push(from);
            touched.push(to);
        }

        // Delete.
        if round >= 5 {
            let gone = format!("src/m{round}.rs");
            if ws.exists(&gone) {
                ws.remove(&gone);
                touched.push(gone);
            }
        }

        generation = pipeline.apply_paths(&touched).await.unwrap().generation;
    }

    let drift = verify_index(&pipeline, generation).await.unwrap();
    assert!(
        drift.is_clean(),
        "incremental indexing drifted from a full rebuild: {drift:#?}"
    );
}

#[tokio::test]
async fn the_drift_check_reports_an_index_that_missed_a_change() {
    // The check has to be able to fail, or asserting it passes proves
    // nothing. Editing a file without telling the index simulates exactly
    // the watcher-missed-an-event bug it exists to catch.
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    let delta = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    ws.write("src/parser.rs", "pub fn totally_different() {}\n");
    ws.write("src/unseen.rs", "pub fn unseen() {}\n");
    ws.remove("README.md");

    let drift = verify_index(&pipeline, delta.generation).await.unwrap();
    assert!(!drift.is_clean());
    assert_eq!(drift.stale_content, ["src/parser.rs"]);
    assert_eq!(drift.missing_files, ["src/unseen.rs"]);
    assert_eq!(drift.stale_files, ["README.md"]);
}

#[tokio::test]
async fn reading_at_a_generation_that_was_never_published_is_an_error() {
    // Silently answering from the current generation would defeat D8: a
    // step planned against a vanished snapshot must be told so.
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    let err = pipeline.store().files(Generation(9999)).await.unwrap_err();
    assert!(matches!(
        err,
        valyria_index::IndexError::UnknownGeneration(_)
    ));
}

#[tokio::test]
async fn pruning_drops_history_but_never_the_current_generation() {
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    let first = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    ws.write("src/parser.rs", "pub fn only_this() {}\n");
    let second = pipeline
        .apply_paths(&["src/parser.rs".to_string()])
        .await
        .unwrap();

    pipeline
        .store()
        .prune_before(second.generation)
        .await
        .unwrap();

    // The old snapshot is gone and says so.
    assert!(matches!(
        pipeline.store().files(first.generation).await.unwrap_err(),
        valyria_index::IndexError::UnknownGeneration(_)
    ));
    // The current one is intact.
    let symbols = pipeline
        .store()
        .symbols_in(second.generation, "src/parser.rs")
        .await
        .unwrap();
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "only_this");
}

#[tokio::test]
async fn pruning_past_the_current_generation_is_clamped() {
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    let delta = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    pipeline
        .store()
        .prune_before(Generation(9999))
        .await
        .unwrap();

    assert!(!pipeline
        .store()
        .files(delta.generation)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn symbols_can_be_looked_up_by_name_and_by_path() {
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    let delta = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();
    let store = pipeline.store();

    let by_name = store
        .symbols_named(delta.generation, "parse")
        .await
        .unwrap();
    assert_eq!(by_name.len(), 1);
    assert_eq!(by_name[0].kind, SymbolKind::Method);
    assert_eq!(by_name[0].qualified_name(), "src/parser.rs#Parser::parse");

    let by_path = store
        .symbols_by_path(delta.generation, "Parser::parse", None)
        .await
        .unwrap();
    assert_eq!(by_path.len(), 1);

    let scoped = store
        .symbols_by_path(delta.generation, "Parser::parse", Some("src/lib.rs"))
        .await
        .unwrap();
    assert!(scoped.is_empty());
}

#[tokio::test]
async fn full_text_symbol_search_matches_on_a_prefix() {
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    let hits = pipeline.store().search_symbols("par", 10).await.unwrap();
    let names: Vec<&str> = hits.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"Parser"), "{names:?}");
    assert!(names.contains(&"parse"), "{names:?}");

    assert!(pipeline
        .store()
        .search_symbols("nothingmatchesthis", 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn full_text_search_forgets_symbols_from_deleted_files() {
    let ws = fixture();
    let pipeline = pipeline_for(&ws);
    pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();
    assert!(!pipeline
        .store()
        .search_symbols("helper", 10)
        .await
        .unwrap()
        .is_empty());

    ws.remove("src/parser.rs");
    pipeline
        .apply_paths(&["src/parser.rs".to_string()])
        .await
        .unwrap();

    assert!(pipeline
        .store()
        .search_symbols("helper", 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn imports_calls_and_tests_are_all_indexed() {
    let ws = TempWorkspace::new();
    ws.write(
        "src/lib.rs",
        "use std::fmt::Debug;\n\npub fn run() { helper(); }\nfn helper() {}\n\n#[test]\nfn works() { run(); }\n",
    );
    let pipeline = pipeline_for(&ws);
    let delta = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();
    let store = pipeline.store();
    let g = delta.generation;

    let imports = store.imports_of(g, "src/lib.rs").await.unwrap();
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].raw_path, "std::fmt::Debug");

    let calls = store.calls_in(g, "src/lib.rs").await.unwrap();
    let helper_call = calls.iter().find(|c| c.name == "helper").unwrap();
    assert_eq!(helper_call.enclosing_symbol_path.as_deref(), Some("run"));

    let tests = store.tests(g).await.unwrap();
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0].name, "works");
}

#[tokio::test]
async fn stats_describe_the_generation_they_are_asked_about() {
    let ws = fixture();
    ws.write("data.bin", [0u8, 1, 2, 3]);
    let pipeline = pipeline_for(&ws);
    let delta = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    let stats = pipeline.store().stats(delta.generation).await.unwrap();
    assert_eq!(stats.files, 4);
    assert!(stats.symbols > 0);
    assert_eq!(stats.files_with_parse_errors, 0);
    // `README.md` and `data.bin`: neither has a grammar in this build.
    assert_eq!(stats.files_without_language, 2);
}

#[tokio::test]
async fn a_file_that_stops_parsing_is_still_indexed_and_flagged() {
    let ws = TempWorkspace::new();
    ws.write("src/lib.rs", "pub fn good() {}\n");
    let pipeline = pipeline_for(&ws);
    pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    ws.write("src/lib.rs", "pub fn good() {}\npub fn broken( {\n");
    let delta = pipeline
        .apply_paths(&["src/lib.rs".to_string()])
        .await
        .unwrap();

    let file = pipeline
        .store()
        .file(delta.generation, "src/lib.rs")
        .await
        .unwrap()
        .unwrap();
    assert!(file.has_parse_errors);

    let stats = pipeline.store().stats(delta.generation).await.unwrap();
    assert_eq!(stats.files_with_parse_errors, 1);

    // Partial facts beat no facts: `good` is still findable.
    let symbols = pipeline
        .store()
        .symbols_in(delta.generation, "src/lib.rs")
        .await
        .unwrap();
    assert!(symbols.iter().any(|s| s.name == "good"));
}

#[tokio::test]
async fn the_index_survives_a_reopen_of_the_database() {
    // Persistence, not just correctness in memory: the whole point of
    // indexing is not doing it again on the next invocation.
    let ws = fixture();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("workspace.db");

    let generation = {
        let store = Store::open(&db, valyria_index::MIGRATIONS).unwrap();
        let pipeline = IndexPipeline::new(
            ws.path().to_path_buf(),
            LanguageRegistry::with_builtin_languages().unwrap(),
            IndexStore::new(Arc::new(store)),
        );
        pipeline
            .bootstrap_unstaged(&|_| {})
            .await
            .unwrap()
            .generation
    };

    let store = Store::open(&db, valyria_index::MIGRATIONS).unwrap();
    let index = IndexStore::new(Arc::new(store));
    assert_eq!(index.current_generation().await.unwrap(), generation);
    let symbols = index.symbols_in(generation, "src/parser.rs").await.unwrap();
    assert_eq!(symbols.len(), 3);
}

#[tokio::test]
async fn an_unindexed_workspace_says_so_rather_than_answering_emptily() {
    let store = Store::open_in_memory(valyria_index::MIGRATIONS).unwrap();
    let index = IndexStore::new(Arc::new(store));

    assert!(index.current().await.unwrap().is_none());
    assert!(matches!(
        index.current_generation().await.unwrap_err(),
        valyria_index::IndexError::NotIndexed
    ));
    assert!(index
        .search_symbols("anything", 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn an_empty_workspace_still_gets_a_generation_to_read_at() {
    let ws = TempWorkspace::new();
    let pipeline = pipeline_for(&ws);
    let delta = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    assert_eq!(delta.generation, Generation(1));
    assert!(pipeline
        .store()
        .files(delta.generation)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn oversized_files_are_listed_but_contribute_no_symbols() {
    let ws = TempWorkspace::new();
    ws.write("src/generated.rs", "pub fn f() {}\n".repeat(1000));
    let pipeline = pipeline_for(&ws).with_options(ScanOptions {
        max_parse_bytes: 100,
    });
    let delta = pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    let files = pipeline.store().files(delta.generation).await.unwrap();
    assert_eq!(files.len(), 1);
    assert!(pipeline
        .store()
        .symbols_in(delta.generation, "src/generated.rs")
        .await
        .unwrap()
        .is_empty());
}
