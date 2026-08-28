//! Deriving the graph from the index.
//!
//! Building is a pure function of one index generation: same generation
//! in, same graph out. That is what makes the graph safe to throw away and
//! recompute, and what keeps it from becoming a second source of truth
//! that can disagree with the index.

use std::collections::{BTreeMap, BTreeSet};

use valyria_index::{CallRecord, FileRecord, ImportRecord, SymbolRecord, TestRecord};
use valyria_lang::SymbolKind;

use crate::model::{Confidence, Edge, EdgeKind, Node, NodeId, NodeKind, UnresolvedRef};
use crate::resolve::{self, FileLookup, Resolution, SymbolLookup};

/// Everything one build produces, before it is written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub unresolved: Vec<UnresolvedRef>,
}

/// The index rows one build reads. Taken as plain slices rather than a
/// store handle so the derivation is testable without a database.
#[derive(Debug, Clone, Copy)]
pub struct GraphInput<'a> {
    pub files: &'a [FileRecord],
    pub symbols: &'a [SymbolRecord],
    pub imports: &'a [ImportRecord],
    pub calls: &'a [CallRecord],
    pub tests: &'a [TestRecord],
}

pub fn build(input: GraphInput<'_>) -> BuiltGraph {
    let mut graph = BuiltGraph::default();

    let test_paths: BTreeSet<(&str, &str)> = input
        .tests
        .iter()
        .map(|t| (t.path.as_str(), t.symbol_path.as_str()))
        .collect();

    add_files_and_modules(&mut graph, input.files);
    add_symbols(&mut graph, input.symbols, input.files, &test_paths);
    add_symbol_nesting(&mut graph, input.symbols);

    let file_lookup = FileLookup::new(input.files.iter().map(|f| f.path.clone()));
    let imports_by_file = group_by(input.imports, |i| i.path.as_str());
    let resolved_imports = add_imports(&mut graph, &imports_by_file, &file_lookup);

    let symbol_lookup = SymbolLookup::new(input.symbols);
    let calls_by_file = group_by(input.calls, |c| c.path.as_str());
    add_calls(
        &mut graph,
        &calls_by_file,
        &symbol_lookup,
        &resolved_imports,
        &test_paths,
    );

    // Deterministic output: the graph is compared across rebuilds by the
    // same drift-style reasoning the index uses, and a `BTreeMap`
    // iteration order is not enough on its own once resolution starts
    // producing several edges per call site.
    graph.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    graph.nodes.dedup_by(|a, b| a.id == b.id);
    graph
        .edges
        .sort_by(|a, b| (&a.from, &a.to, a.kind).cmp(&(&b.from, &b.to, b.kind)));
    graph
        .edges
        .dedup_by(|a, b| (&a.from, &a.to, a.kind) == (&b.from, &b.to, b.kind));
    graph
        .unresolved
        .sort_by(|a, b| (&a.from, a.kind, &a.target).cmp(&(&b.from, b.kind, &b.target)));
    graph.unresolved.dedup();

    graph
}

fn add_files_and_modules(graph: &mut BuiltGraph, files: &[FileRecord]) {
    let mut directories: BTreeSet<String> = BTreeSet::new();

    for file in files {
        graph.nodes.push(Node {
            id: NodeId::file(&file.path),
            kind: NodeKind::File,
            name: base_name(&file.path).to_string(),
            path: file.path.clone(),
            symbol_path: None,
            language: file.language.clone(),
            start_line: None,
        });

        let dir = parent_dir(&file.path);
        directories.insert(dir.to_string());
        graph.edges.push(Edge {
            from: NodeId::module(dir),
            to: NodeId::file(&file.path),
            kind: EdgeKind::Contains,
            confidence: Confidence::Exact,
        });
    }

    // Every ancestor directory too, so `impact_of` and subgraph queries
    // can walk up from a file to the area of the repository it lives in.
    let mut all_dirs = directories.clone();
    for dir in &directories {
        let mut current = dir.as_str();
        while let Some((parent, _)) = current.rsplit_once('/') {
            all_dirs.insert(parent.to_string());
            graph.edges.push(Edge {
                from: NodeId::module(parent),
                to: NodeId::module(current),
                kind: EdgeKind::Contains,
                confidence: Confidence::Exact,
            });
            current = parent;
        }
        if !current.is_empty() {
            all_dirs.insert(String::new());
            graph.edges.push(Edge {
                from: NodeId::module(""),
                to: NodeId::module(current),
                kind: EdgeKind::Contains,
                confidence: Confidence::Exact,
            });
        }
    }

    for dir in all_dirs {
        graph.nodes.push(Node {
            id: NodeId::module(&dir),
            kind: NodeKind::Module,
            name: if dir.is_empty() {
                "/".to_string()
            } else {
                base_name(&dir).to_string()
            },
            path: dir,
            symbol_path: None,
            language: None,
            start_line: None,
        });
    }
}

fn add_symbols(
    graph: &mut BuiltGraph,
    symbols: &[SymbolRecord],
    files: &[FileRecord],
    test_paths: &BTreeSet<(&str, &str)>,
) {
    let languages: BTreeMap<&str, Option<&str>> = files
        .iter()
        .map(|f| (f.path.as_str(), f.language.as_deref()))
        .collect();

    for symbol in symbols {
        let is_test = test_paths.contains(&(symbol.path.as_str(), symbol.symbol_path.as_str()));
        let id = if is_test {
            NodeId::test(&symbol.path, &symbol.symbol_path)
        } else {
            NodeId::symbol(&symbol.path, &symbol.symbol_path)
        };

        graph.nodes.push(Node {
            id: id.clone(),
            kind: if is_test {
                NodeKind::Test
            } else {
                NodeKind::Symbol
            },
            name: symbol.name.clone(),
            path: symbol.path.clone(),
            symbol_path: Some(symbol.symbol_path.clone()),
            language: languages
                .get(symbol.path.as_str())
                .copied()
                .flatten()
                .map(|s| s.to_string()),
            start_line: Some(symbol.span.start_line),
        });

        graph.edges.push(Edge {
            from: NodeId::file(&symbol.path),
            to: id,
            kind: EdgeKind::Defines,
            confidence: Confidence::Exact,
        });
    }
}

/// `Contains` edges between a symbol and the symbols nested inside it,
/// derived from spans rather than from symbol-path string prefixes: a path
/// prefix would wrongly nest `Parser` inside `Parse` in a language whose
/// separator is a single character.
fn add_symbol_nesting(graph: &mut BuiltGraph, symbols: &[SymbolRecord]) {
    let by_file = group_by(symbols, |s| s.path.as_str());

    for (path, file_symbols) in by_file {
        for symbol in &file_symbols {
            let parent = file_symbols
                .iter()
                .filter(|other| other.span.strictly_contains(&symbol.span))
                .min_by_key(|other| other.span.len_bytes());
            let Some(parent) = parent else { continue };

            graph.edges.push(Edge {
                from: NodeId::symbol(path, &parent.symbol_path),
                to: NodeId::symbol(path, &symbol.symbol_path),
                kind: EdgeKind::Contains,
                confidence: Confidence::Exact,
            });
        }
    }
}

/// Returns, per importing file, the set of repository files it imports —
/// which call resolution then uses as its scoping evidence.
fn add_imports<'a>(
    graph: &mut BuiltGraph,
    imports_by_file: &BTreeMap<&'a str, Vec<&ImportRecord>>,
    files: &FileLookup,
) -> BTreeMap<&'a str, BTreeSet<String>> {
    let mut resolved_by_file = BTreeMap::new();

    for (path, imports) in imports_by_file {
        let mut targets = BTreeSet::new();
        for import in imports {
            match resolve::resolve_import(path, &import.raw_path, files) {
                Resolution::Resolved {
                    targets: hits,
                    confidence,
                } => {
                    for hit in hits {
                        graph.edges.push(Edge {
                            from: NodeId::file(path),
                            to: NodeId::file(&hit),
                            kind: EdgeKind::Imports,
                            confidence,
                        });
                        targets.insert(hit);
                    }
                }
                Resolution::External => graph.unresolved.push(UnresolvedRef {
                    from: NodeId::file(path),
                    kind: EdgeKind::Imports,
                    target: import.raw_path.clone(),
                }),
            }
        }
        resolved_by_file.insert(*path, targets);
    }

    resolved_by_file
}

fn add_calls(
    graph: &mut BuiltGraph,
    calls_by_file: &BTreeMap<&str, Vec<&CallRecord>>,
    symbols: &SymbolLookup,
    imports: &BTreeMap<&str, BTreeSet<String>>,
    test_paths: &BTreeSet<(&str, &str)>,
) {
    let empty = BTreeSet::new();

    for (path, calls) in calls_by_file {
        let imported = imports.get(path).unwrap_or(&empty);

        for call in calls {
            // A call outside any function has no caller node to attach to.
            // It is real (module-level initialization runs), but there is
            // nothing to draw the edge from.
            let Some(caller_symbol) = call.enclosing_symbol_path.as_deref() else {
                continue;
            };
            let caller = node_id_for(path, caller_symbol, test_paths);

            match resolve::resolve_call(path, call, symbols, imported) {
                Resolution::Resolved {
                    targets,
                    confidence,
                } => {
                    for target in targets {
                        let callee = node_id_for(&target.path, &target.symbol_path, test_paths);
                        if callee == caller {
                            continue; // direct recursion adds no reachability
                        }
                        graph.edges.push(Edge {
                            from: caller.clone(),
                            to: callee.clone(),
                            kind: EdgeKind::Calls,
                            confidence,
                        });

                        // A test that calls a symbol exercises it. This is
                        // the edge §4.26 walks to answer "which tests
                        // cover this change?" without running anything.
                        if caller.kind() == Some(NodeKind::Test) {
                            graph.edges.push(Edge {
                                from: caller.clone(),
                                to: callee,
                                kind: EdgeKind::Tests,
                                confidence,
                            });
                        }
                    }
                }
                Resolution::External => graph.unresolved.push(UnresolvedRef {
                    from: caller,
                    kind: EdgeKind::Calls,
                    target: call.name.clone(),
                }),
            }
        }
    }
}

fn node_id_for(path: &str, symbol_path: &str, test_paths: &BTreeSet<(&str, &str)>) -> NodeId {
    if test_paths.contains(&(path, symbol_path)) {
        NodeId::test(path, symbol_path)
    } else {
        NodeId::symbol(path, symbol_path)
    }
}

fn group_by<'a, T, F>(items: &'a [T], key: F) -> BTreeMap<&'a str, Vec<&'a T>>
where
    F: Fn(&'a T) -> &'a str,
{
    let mut out: BTreeMap<&'a str, Vec<&'a T>> = BTreeMap::new();
    for item in items {
        out.entry(key(item)).or_default().push(item);
    }
    out
}

fn base_name(path: &str) -> &str {
    path.rsplit_once('/').map(|(_, name)| name).unwrap_or(path)
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

/// Symbol kinds worth surfacing as graph nodes on their own. Everything
/// else is still indexed; it just does not clutter a subgraph view.
pub fn is_graph_worthy(kind: SymbolKind) -> bool {
    !matches!(kind, SymbolKind::Variable | SymbolKind::Field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_name_and_parent_dir_handle_root_level_files() {
        assert_eq!(base_name("src/parser.rs"), "parser.rs");
        assert_eq!(base_name("README.md"), "README.md");
        assert_eq!(parent_dir("src/a/b.rs"), "src/a");
        assert_eq!(parent_dir("README.md"), "");
    }
}
