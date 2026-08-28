//! The knowledge graph end to end: index a real fixture repository,
//! derive the graph from it, and ask the four questions the query API
//! exists to answer.

use std::sync::Arc;

use valyria_graph::{Direction, EdgeKind, GraphStore, NodeId, NodeKind};
use valyria_index::{IndexPipeline, IndexStore};
use valyria_lang::LanguageRegistry;
use valyria_store::{Migration, Store};
use valyria_testkit::TempWorkspace;
use valyria_types::Generation;

fn migrations() -> Vec<Migration> {
    let mut all: Vec<Migration> = valyria_index::MIGRATIONS.to_vec();
    all.extend(valyria_graph::MIGRATIONS.iter().copied());
    all
}

struct Harness {
    pipeline: IndexPipeline,
    graph: GraphStore,
}

impl Harness {
    fn new(ws: &TempWorkspace) -> Self {
        let store = Arc::new(Store::open_in_memory(&migrations()).unwrap());
        Self {
            pipeline: IndexPipeline::new(
                ws.path().to_path_buf(),
                LanguageRegistry::with_builtin_languages().unwrap(),
                IndexStore::new(store.clone()),
            ),
            graph: GraphStore::new(store),
        }
    }

    async fn index_and_build(&self) -> Generation {
        let delta = self.pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();
        self.graph
            .build_for(self.pipeline.store(), delta.generation)
            .await
            .unwrap();
        delta.generation
    }
}

/// A small Rust repository with a real dependency chain:
/// `main -> parser -> lexer`, plus a test that exercises the parser.
fn rust_fixture() -> TempWorkspace {
    let ws = TempWorkspace::new();
    ws.write(
        "src/lexer.rs",
        "pub fn tokenize(input: &str) -> usize {\n    input.len()\n}\n",
    )
    .write(
        "src/parser.rs",
        "use crate::lexer;\nuse serde::Deserialize;\n\npub struct Parser;\n\nimpl Parser {\n    pub fn parse(&self, input: &str) -> usize {\n        lexer::tokenize(input)\n    }\n}\n",
    )
    .write(
        "src/main.rs",
        "use crate::parser::Parser;\n\nfn main() {\n    let p = Parser;\n    p.parse(\"x\");\n}\n",
    )
    .write(
        "src/parser_test.rs",
        "use crate::parser::Parser;\n\n#[test]\nfn parses_input() {\n    Parser.parse(\"x\");\n}\n",
    );
    ws
}

#[tokio::test]
async fn files_symbols_and_modules_all_become_nodes() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    for id in [
        NodeId::file("src/parser.rs"),
        NodeId::symbol("src/parser.rs", "Parser::parse"),
        NodeId::module("src"),
    ] {
        let node = harness.graph.node(g, &id).await.unwrap();
        assert!(node.is_some(), "missing node {id}");
    }

    let parse = harness
        .graph
        .node(g, &NodeId::symbol("src/parser.rs", "Parser::parse"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parse.kind, NodeKind::Symbol);
    assert_eq!(parse.name, "parse");
    assert_eq!(parse.language.as_deref(), Some("rust"));
}

#[tokio::test]
async fn a_test_function_becomes_a_test_node_not_a_plain_symbol() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let test = harness
        .graph
        .node(g, &NodeId::test("src/parser_test.rs", "parses_input"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(test.kind, NodeKind::Test);

    // And is not *also* present as an ordinary symbol.
    assert!(harness
        .graph
        .node(g, &NodeId::symbol("src/parser_test.rs", "parses_input"))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn imports_between_files_become_edges() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let imports = harness
        .graph
        .neighbors(
            g,
            &NodeId::file("src/parser.rs"),
            Direction::Outgoing,
            &[EdgeKind::Imports],
        )
        .await
        .unwrap();
    let targets: Vec<&str> = imports.iter().map(|e| e.to.as_str()).collect();
    assert_eq!(targets, ["file:src/lexer.rs"]);
}

#[tokio::test]
async fn an_import_of_a_third_party_crate_is_recorded_as_unresolved_not_dropped() {
    // "this file depends on serde" is a real fact even though serde has
    // no node.
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let unresolved = harness.graph.unresolved(g).await.unwrap();
    assert!(
        unresolved
            .iter()
            .any(|u| u.kind == EdgeKind::Imports && u.target.contains("serde")),
        "{unresolved:#?}"
    );
}

#[tokio::test]
async fn who_calls_this_and_what_does_this_call_are_both_answerable() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let parse = NodeId::symbol("src/parser.rs", "Parser::parse");

    let outgoing = harness
        .graph
        .neighbors(g, &parse, Direction::Outgoing, &[EdgeKind::Calls])
        .await
        .unwrap();
    let callees: Vec<&str> = outgoing.iter().map(|e| e.to.as_str()).collect();
    assert_eq!(callees, ["sym:src/lexer.rs#tokenize"]);

    let incoming = harness
        .graph
        .neighbors(g, &parse, Direction::Incoming, &[EdgeKind::Calls])
        .await
        .unwrap();
    let callers: Vec<&str> = incoming.iter().map(|e| e.from.as_str()).collect();
    assert!(callers.contains(&"sym:src/main.rs#main"), "{callers:?}");
    assert!(
        callers.contains(&"test:src/parser_test.rs#parses_input"),
        "{callers:?}"
    );
}

#[tokio::test]
async fn a_test_that_calls_a_symbol_gets_a_tests_edge_as_well_as_a_calls_edge() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let tested = harness
        .graph
        .neighbors(
            g,
            &NodeId::test("src/parser_test.rs", "parses_input"),
            Direction::Outgoing,
            &[EdgeKind::Tests],
        )
        .await
        .unwrap();
    let targets: Vec<&str> = tested.iter().map(|e| e.to.as_str()).collect();
    assert_eq!(targets, ["sym:src/parser.rs#Parser::parse"]);
}

#[tokio::test]
async fn a_call_into_the_standard_library_is_unresolved_rather_than_mis_bound() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let unresolved = harness.graph.unresolved(g).await.unwrap();
    assert!(
        unresolved
            .iter()
            .any(|u| u.kind == EdgeKind::Calls && u.target == "len"),
        "{unresolved:#?}"
    );
}

#[tokio::test]
async fn nested_symbols_are_contained_by_their_parent() {
    let ws = TempWorkspace::new();
    ws.write(
        "app/models.py",
        "class Parser:\n    def parse(self):\n        return 1\n",
    );
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let contained = harness
        .graph
        .neighbors(
            g,
            &NodeId::symbol("app/models.py", "Parser"),
            Direction::Outgoing,
            &[EdgeKind::Contains],
        )
        .await
        .unwrap();
    let targets: Vec<&str> = contained.iter().map(|e| e.to.as_str()).collect();
    assert_eq!(targets, ["sym:app/models.py#Parser.parse"]);
}

#[tokio::test]
async fn a_module_contains_its_files_and_its_subdirectories() {
    let ws = TempWorkspace::new();
    ws.write("src/a.rs", "pub fn a() {}")
        .write("src/deep/b.rs", "pub fn b() {}");
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let children = harness
        .graph
        .neighbors(
            g,
            &NodeId::module("src"),
            Direction::Outgoing,
            &[EdgeKind::Contains],
        )
        .await
        .unwrap();
    let targets: Vec<&str> = children.iter().map(|e| e.to.as_str()).collect();
    assert!(targets.contains(&"file:src/a.rs"), "{targets:?}");
    assert!(targets.contains(&"mod:src/deep"), "{targets:?}");
}

#[tokio::test]
async fn impact_of_a_change_finds_dependents_and_the_tests_that_cover_them() {
    // The question §4.26's verification strategy asks: I changed the
    // lexer — what might break, and what should I run?
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let impact = harness.graph.impact_of(g, "src/lexer.rs", 3).await.unwrap();
    assert_eq!(impact.origin, "src/lexer.rs");
    assert!(
        impact.affected_files.contains(&"src/parser.rs".to_string()),
        "{impact:#?}"
    );
    assert!(
        impact.affected_files.contains(&"src/main.rs".to_string()),
        "a two-hop dependent should be reachable: {impact:#?}"
    );
    assert_eq!(
        impact.covering_tests,
        [NodeId::test("src/parser_test.rs", "parses_input")]
    );
}

#[tokio::test]
async fn impact_stays_within_its_depth_limit() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    // One hop from the lexer reaches the parser but not the parser's own
    // callers.
    let shallow = harness.graph.impact_of(g, "src/lexer.rs", 1).await.unwrap();
    assert_eq!(shallow.affected_files, ["src/parser.rs"]);
}

#[tokio::test]
async fn impact_does_not_sweep_in_every_sibling_via_containment() {
    // Following `Contains` backwards would reach the directory and then
    // every other file in it, which is not impact at all.
    let ws = TempWorkspace::new();
    ws.write("src/lonely.rs", "pub fn lonely() {}")
        .write("src/unrelated_a.rs", "pub fn a() {}")
        .write("src/unrelated_b.rs", "pub fn b() {}");
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let impact = harness
        .graph
        .impact_of(g, "src/lonely.rs", 3)
        .await
        .unwrap();
    assert!(impact.affected_files.is_empty(), "{impact:#?}");
}

#[tokio::test]
async fn paths_finds_the_call_chain_between_two_symbols() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let paths = harness
        .graph
        .paths(
            g,
            &NodeId::symbol("src/main.rs", "main"),
            &NodeId::symbol("src/lexer.rs", "tokenize"),
            4,
        )
        .await
        .unwrap();

    assert!(!paths.is_empty(), "expected a path from main to tokenize");
    let shortest: Vec<&str> = paths[0].iter().map(|n| n.as_str()).collect();
    assert_eq!(
        shortest,
        [
            "sym:src/main.rs#main",
            "sym:src/parser.rs#Parser::parse",
            "sym:src/lexer.rs#tokenize"
        ]
    );
}

#[tokio::test]
async fn paths_respects_its_depth_bound() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    // `max_depth` counts edges: `main -> parse -> tokenize` is two of
    // them, so a budget of one must find nothing.
    let paths = harness
        .graph
        .paths(
            g,
            &NodeId::symbol("src/main.rs", "main"),
            &NodeId::symbol("src/lexer.rs", "tokenize"),
            1,
        )
        .await
        .unwrap();
    assert!(
        paths.is_empty(),
        "a two-edge path must not be found at depth 1"
    );
}

#[tokio::test]
async fn subgraph_around_returns_a_self_contained_neighborhood() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let subgraph = harness
        .graph
        .subgraph_around(
            g,
            &NodeId::symbol("src/parser.rs", "Parser::parse"),
            1,
            &[EdgeKind::Calls],
        )
        .await
        .unwrap();

    // Every endpoint mentioned by an edge is also present as a node —
    // that is what makes the result renderable on its own.
    let ids: Vec<&str> = subgraph.nodes.iter().map(|n| n.id.as_str()).collect();
    for edge in &subgraph.edges {
        assert!(ids.contains(&edge.from.as_str()), "{:?}", edge.from);
        assert!(ids.contains(&edge.to.as_str()), "{:?}", edge.to);
    }
    assert!(ids.contains(&"sym:src/lexer.rs#tokenize"));
}

#[tokio::test]
async fn a_javascript_repository_resolves_relative_imports() {
    let ws = TempWorkspace::new();
    ws.write(
        "src/util.js",
        "export function helper(x) {\n  return x;\n}\n",
    )
    .write(
        "src/main.js",
        "import { helper } from './util';\n\nexport function run() {\n  return helper(1);\n}\n",
    );
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let imports = harness
        .graph
        .neighbors(
            g,
            &NodeId::file("src/main.js"),
            Direction::Outgoing,
            &[EdgeKind::Imports],
        )
        .await
        .unwrap();
    assert_eq!(imports[0].to.as_str(), "file:src/util.js");

    let calls = harness
        .graph
        .neighbors(
            g,
            &NodeId::symbol("src/main.js", "run"),
            Direction::Outgoing,
            &[EdgeKind::Calls],
        )
        .await
        .unwrap();
    assert_eq!(calls[0].to.as_str(), "sym:src/util.js#helper");
}

#[tokio::test]
async fn a_go_repository_resolves_package_imports_by_suffix() {
    let ws = TempWorkspace::new();
    ws.write(
        "internal/parse/parse.go",
        "package parse\n\nfunc Tokenize(s string) int { return len(s) }\n",
    )
    .write(
        "cmd/main.go",
        "package main\n\nimport (\n    \"github.com/org/repo/internal/parse\"\n)\n\nfunc main() {\n    parse.Tokenize(\"x\")\n}\n",
    );
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let imports = harness
        .graph
        .neighbors(
            g,
            &NodeId::file("cmd/main.go"),
            Direction::Outgoing,
            &[EdgeKind::Imports],
        )
        .await
        .unwrap();
    assert_eq!(imports[0].to.as_str(), "file:internal/parse/parse.go");
}

#[tokio::test]
async fn rebuilding_the_same_generation_replaces_rather_than_duplicates() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;
    let first = harness.graph.stats(g).await.unwrap();

    harness
        .graph
        .build_for(harness.pipeline.store(), g)
        .await
        .unwrap();
    let second = harness.graph.stats(g).await.unwrap();

    assert_eq!(first, second);
}

#[tokio::test]
async fn a_graph_that_was_never_built_says_so_rather_than_looking_empty() {
    // "nothing relates to anything" and "nobody has computed the
    // relationships" are different answers, and only one is worth acting
    // on.
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let delta = harness.pipeline.bootstrap_unstaged(&|_| {}).await.unwrap();

    let err = harness
        .graph
        .impact_of(delta.generation, "src/lexer.rs", 2)
        .await
        .unwrap_err();
    assert!(matches!(err, valyria_graph::GraphError::NotBuilt(_)));
}

#[tokio::test]
async fn each_generation_keeps_its_own_graph() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let first = harness.index_and_build().await;

    ws.write(
        "src/lexer.rs",
        "pub fn tokenize(_input: &str) -> usize { 0 }\npub fn extra() {}\n",
    );
    let delta = harness
        .pipeline
        .apply_paths(&["src/lexer.rs".to_string()])
        .await
        .unwrap();
    harness
        .graph
        .build_for(harness.pipeline.store(), delta.generation)
        .await
        .unwrap();

    // The old graph is untouched; the new one has the added symbol.
    assert!(harness
        .graph
        .node(first, &NodeId::symbol("src/lexer.rs", "extra"))
        .await
        .unwrap()
        .is_none());
    assert!(harness
        .graph
        .node(delta.generation, &NodeId::symbol("src/lexer.rs", "extra"))
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn pruning_drops_graphs_for_generations_no_longer_needed() {
    let ws = rust_fixture();
    let harness = Harness::new(&ws);
    let first = harness.index_and_build().await;

    ws.write(
        "src/lexer.rs",
        "pub fn tokenize(_input: &str) -> usize { 0 }\n",
    );
    let delta = harness
        .pipeline
        .apply_paths(&["src/lexer.rs".to_string()])
        .await
        .unwrap();
    harness
        .graph
        .build_for(harness.pipeline.store(), delta.generation)
        .await
        .unwrap();

    harness.graph.prune_before(delta.generation).await.unwrap();

    assert!(matches!(
        harness.graph.stats(first).await.unwrap_err(),
        valyria_graph::GraphError::NotBuilt(_)
    ));
    assert!(harness.graph.stats(delta.generation).await.unwrap().nodes > 0);
}

#[tokio::test]
async fn an_ambiguous_call_records_every_candidate_rather_than_picking_one() {
    let ws = TempWorkspace::new();
    ws.write("src/a.rs", "pub fn helper() {}\n")
        .write("src/b.rs", "pub fn helper() {}\n")
        .write("src/caller.rs", "pub fn run() { helper(); }\n");
    let harness = Harness::new(&ws);
    let g = harness.index_and_build().await;

    let calls = harness
        .graph
        .neighbors(
            g,
            &NodeId::symbol("src/caller.rs", "run"),
            Direction::Outgoing,
            &[EdgeKind::Calls],
        )
        .await
        .unwrap();

    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|e| e.confidence == valyria_graph::Confidence::Ambiguous));
}

#[tokio::test]
async fn building_a_graph_is_deterministic() {
    let ws = rust_fixture();

    let a = Harness::new(&ws);
    let ga = a.index_and_build().await;
    let b = Harness::new(&ws);
    let gb = b.index_and_build().await;

    let sub_a = a
        .graph
        .subgraph_around(ga, &NodeId::file("src/parser.rs"), 3, &[])
        .await
        .unwrap();
    let sub_b = b
        .graph
        .subgraph_around(gb, &NodeId::file("src/parser.rs"), 3, &[])
        .await
        .unwrap();
    assert_eq!(sub_a, sub_b);
}
