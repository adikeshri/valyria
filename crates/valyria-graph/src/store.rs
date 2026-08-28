//! Persisting the graph and querying it.
//!
//! The query surface is deliberately small and typed — `neighbors`,
//! `paths`, `subgraph_around`, `impact_of` — rather than a general query
//! language (§4.14). Four operations cover what the context engine, the
//! verification strategy, and `--explain` actually ask for, and each can
//! be given a sensible bound.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use rusqlite::{params, Row};
use valyria_index::IndexStore;
use valyria_store::Store;
use valyria_types::Generation;

use crate::build::{self, BuiltGraph, GraphInput};
use crate::error::{GraphError, Result};
use crate::model::{
    Confidence, Direction, Edge, EdgeKind, GraphStats, ImpactSet, Node, NodeId, NodeKind, Subgraph,
    UnresolvedRef,
};

/// How far `subgraph_around` and `impact_of` walk by default. Two hops
/// reaches "the callers of my callers" — far enough to be useful, close
/// enough that the result is still about the thing you asked about.
pub const DEFAULT_DEPTH: usize = 2;

#[derive(Clone)]
pub struct GraphStore {
    store: Arc<Store>,
}

impl std::fmt::Debug for GraphStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphStore")
            .field("db", &self.store.db_path())
            .finish()
    }
}

impl GraphStore {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Derive and store the graph for one index generation, replacing any
    /// previous build of the same generation.
    pub async fn build_for(
        &self,
        index: &IndexStore,
        generation: Generation,
    ) -> Result<GraphStats> {
        let files = index.files(generation).await?;
        let symbols = index.all_symbols(generation).await?;
        let imports = index.all_imports(generation).await?;
        let calls = index.all_calls(generation).await?;
        let tests = index.tests(generation).await?;

        let graph = build::build(GraphInput {
            files: &files,
            symbols: &symbols,
            imports: &imports,
            calls: &calls,
            tests: &tests,
        });

        self.write(generation, graph).await
    }

    async fn write(&self, generation: Generation, graph: BuiltGraph) -> Result<GraphStats> {
        let g = generation.0 as i64;
        self.store
            .call(move |conn| {
                let tx = conn.transaction()?;
                // Replace rather than merge: the graph is a pure function
                // of its generation, so a rebuild must not be able to
                // leave a stale edge behind.
                for table in [
                    "graph_node",
                    "graph_edge",
                    "graph_unresolved",
                    "graph_build",
                ] {
                    tx.execute(&format!("DELETE FROM {table} WHERE generation = ?1"), [g])?;
                }

                for node in &graph.nodes {
                    tx.execute(
                        "INSERT INTO graph_node
                            (generation, id, kind, name, path, symbol_path, language, start_line)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            g,
                            node.id.as_str(),
                            node.kind.as_str(),
                            node.name,
                            node.path,
                            node.symbol_path,
                            node.language,
                            node.start_line.map(|l| l as i64),
                        ],
                    )?;
                }

                for edge in &graph.edges {
                    // `OR IGNORE`: two call sites in the same function to
                    // the same callee are one edge, and resolution can
                    // legitimately produce the pair twice.
                    tx.execute(
                        "INSERT OR IGNORE INTO graph_edge
                            (generation, from_id, to_id, kind, confidence)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            g,
                            edge.from.as_str(),
                            edge.to.as_str(),
                            edge.kind.as_str(),
                            edge.confidence.as_str(),
                        ],
                    )?;
                }

                for unresolved in &graph.unresolved {
                    tx.execute(
                        "INSERT OR IGNORE INTO graph_unresolved (generation, from_id, kind, target)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![
                            g,
                            unresolved.from.as_str(),
                            unresolved.kind.as_str(),
                            unresolved.target,
                        ],
                    )?;
                }

                let stats = GraphStats {
                    nodes: count(&tx, "graph_node", g)?,
                    edges: count(&tx, "graph_edge", g)?,
                    unresolved: count(&tx, "graph_unresolved", g)?,
                };

                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                tx.execute(
                    "INSERT INTO graph_build
                        (generation, built_at_ms, node_count, edge_count, unresolved_count)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        g,
                        now_ms,
                        stats.nodes as i64,
                        stats.edges as i64,
                        stats.unresolved as i64
                    ],
                )?;

                tx.commit()?;
                Ok(stats)
            })
            .await
            .map_err(GraphError::from)
    }

    pub async fn stats(&self, generation: Generation) -> Result<GraphStats> {
        let g = generation.0 as i64;
        let stats = self
            .store
            .call(move |conn| {
                conn.query_row(
                    "SELECT node_count, edge_count, unresolved_count
                     FROM graph_build WHERE generation = ?1",
                    [g],
                    |row| {
                        Ok(GraphStats {
                            nodes: row.get::<_, i64>(0)? as u64,
                            edges: row.get::<_, i64>(1)? as u64,
                            unresolved: row.get::<_, i64>(2)? as u64,
                        })
                    },
                )
                .ok()
                .map(Ok)
                .unwrap_or(Ok(GraphStats::default()))
            })
            .await
            .map_err(GraphError::from)?;
        self.ensure_built(generation).await?;
        Ok(stats)
    }

    pub async fn node(&self, generation: Generation, id: &NodeId) -> Result<Option<Node>> {
        self.ensure_built(generation).await?;
        let g = generation.0 as i64;
        let id = id.0.clone();
        self.store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, kind, name, path, symbol_path, language, start_line
                     FROM graph_node WHERE generation = ?1 AND id = ?2",
                )?;
                let mut rows = stmt.query_map(params![g, id], node_from_row)?;
                Ok(rows.next().transpose()?)
            })
            .await
            .map_err(GraphError::from)
    }

    /// Edges touching `id`. `kinds` empty means every kind.
    pub async fn neighbors(
        &self,
        generation: Generation,
        id: &NodeId,
        direction: Direction,
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        self.ensure_built(generation).await?;
        let edges = self
            .edges_for(generation, std::slice::from_ref(id), direction)
            .await?;
        Ok(if kinds.is_empty() {
            edges
        } else {
            edges
                .into_iter()
                .filter(|edge| kinds.contains(&edge.kind))
                .collect()
        })
    }

    /// Everything within `depth` hops of `id`, as a self-contained
    /// subgraph a client can render or a context engine can rank over.
    pub async fn subgraph_around(
        &self,
        generation: Generation,
        id: &NodeId,
        depth: usize,
        kinds: &[EdgeKind],
    ) -> Result<Subgraph> {
        self.ensure_built(generation).await?;

        let mut seen: BTreeSet<NodeId> = BTreeSet::from([id.clone()]);
        let mut frontier = vec![id.clone()];
        let mut edges: Vec<Edge> = Vec::new();

        for _ in 0..depth {
            if frontier.is_empty() {
                break;
            }
            let mut found = self
                .edges_for(generation, &frontier, Direction::Both)
                .await?;
            if !kinds.is_empty() {
                found.retain(|edge| kinds.contains(&edge.kind));
            }

            let mut next = Vec::new();
            for edge in &found {
                for endpoint in [&edge.from, &edge.to] {
                    if seen.insert(endpoint.clone()) {
                        next.push(endpoint.clone());
                    }
                }
            }
            edges.extend(found);
            frontier = next;
        }

        edges.sort_by(|a, b| (&a.from, &a.to, a.kind).cmp(&(&b.from, &b.to, b.kind)));
        edges.dedup_by(|a, b| (&a.from, &a.to, a.kind) == (&b.from, &b.to, b.kind));

        let nodes = self
            .nodes_by_id(generation, &seen.iter().cloned().collect::<Vec<_>>())
            .await?;
        Ok(Subgraph { nodes, edges })
    }

    /// Every simple path from `from` to `to` no longer than `max_depth`
    /// edges, shortest first.
    ///
    /// Bounded by construction: an unbounded path search on a real
    /// repository's call graph does not terminate in any useful time, so
    /// the depth limit is a parameter rather than an optimization.
    pub async fn paths(
        &self,
        generation: Generation,
        from: &NodeId,
        to: &NodeId,
        max_depth: usize,
    ) -> Result<Vec<Vec<NodeId>>> {
        self.ensure_built(generation).await?;
        if from == to {
            return Ok(vec![vec![from.clone()]]);
        }

        let mut found = Vec::new();
        let mut queue: VecDeque<Vec<NodeId>> = VecDeque::from([vec![from.clone()]]);

        while let Some(path) = queue.pop_front() {
            // `max_depth` counts *edges*, so a path of n nodes has already
            // used n-1 of the budget and can only be extended while that
            // is below the limit.
            if path.len() > max_depth {
                continue;
            }
            let tail = path.last().expect("paths are never empty").clone();
            let edges = self
                .edges_for(generation, std::slice::from_ref(&tail), Direction::Outgoing)
                .await?;

            for edge in edges {
                if path.contains(&edge.to) {
                    continue; // simple paths only: no revisiting
                }
                let mut extended = path.clone();
                extended.push(edge.to.clone());
                if edge.to == *to {
                    found.push(extended);
                } else {
                    queue.push_back(extended);
                }
            }
        }

        found.sort_by_key(|path| (path.len(), path.first().cloned()));
        Ok(found)
    }

    /// What a change to `path` could affect, and which tests to run
    /// (§4.14's `impact_of`).
    ///
    /// Walks *incoming* edges: the question is not "what does this file
    /// use" but "what depends on it", which is the direction that answers
    /// "what might I have broken?".
    pub async fn impact_of(
        &self,
        generation: Generation,
        path: &str,
        depth: usize,
    ) -> Result<ImpactSet> {
        self.ensure_built(generation).await?;

        let mut affected: Vec<String> = Vec::new();
        let mut covering: BTreeSet<NodeId> = BTreeSet::new();
        let mut seen: BTreeSet<NodeId> = BTreeSet::new();

        // Seed with the file itself and everything it defines: a caller
        // depends on a symbol, not on the file, so starting from the file
        // node alone would find nothing.
        let mut frontier = vec![NodeId::file(path)];
        frontier.extend(
            self.nodes_in_file(generation, path)
                .await?
                .into_iter()
                .map(|node| node.id),
        );
        seen.extend(frontier.iter().cloned());

        for _ in 0..depth {
            if frontier.is_empty() {
                break;
            }
            let edges = self
                .edges_for(generation, &frontier, Direction::Incoming)
                .await?;

            let mut next = Vec::new();
            for edge in edges {
                // `Contains` runs from a directory to its files and from a
                // symbol to its nested symbols; following it backwards
                // would sweep in every sibling in the repository, which is
                // not impact.
                if edge.kind == EdgeKind::Contains {
                    continue;
                }
                if edge.from.kind() == Some(NodeKind::Test) {
                    covering.insert(edge.from.clone());
                }
                if let Some(file) = edge.from.file_path() {
                    if file != path && !affected.iter().any(|f| f == file) {
                        affected.push(file.to_string());
                    }
                }
                if seen.insert(edge.from.clone()) {
                    next.push(edge.from);
                }
            }
            frontier = next;
        }

        Ok(ImpactSet {
            origin: path.to_string(),
            affected_files: affected,
            covering_tests: covering.into_iter().collect(),
        })
    }

    /// References that point outside the repository — third-party imports
    /// and standard-library calls. Real facts about the code that simply
    /// have no node on the other end.
    pub async fn unresolved(&self, generation: Generation) -> Result<Vec<UnresolvedRef>> {
        self.ensure_built(generation).await?;
        let g = generation.0 as i64;
        self.store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT from_id, kind, target FROM graph_unresolved
                     WHERE generation = ?1 ORDER BY from_id, kind, target",
                )?;
                let rows = stmt
                    .query_map([g], |row| {
                        let kind: String = row.get(1)?;
                        Ok(UnresolvedRef {
                            from: NodeId(row.get(0)?),
                            kind: EdgeKind::parse(&kind).unwrap_or(EdgeKind::Imports),
                            target: row.get(2)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .map_err(GraphError::from)
    }

    /// Drop every graph built for a generation below `keep_from`.
    pub async fn prune_before(&self, keep_from: Generation) -> Result<u64> {
        let g = keep_from.0 as i64;
        self.store
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut removed = 0u64;
                for table in [
                    "graph_node",
                    "graph_edge",
                    "graph_unresolved",
                    "graph_build",
                ] {
                    removed += tx
                        .execute(&format!("DELETE FROM {table} WHERE generation < ?1"), [g])?
                        as u64;
                }
                tx.commit()?;
                Ok(removed)
            })
            .await
            .map_err(GraphError::from)
    }

    /// Whether a graph exists for this index generation. Never errors on
    /// "not built" — it is the check a caller uses to decide whether to
    /// run a graph query at all, or to fall back to something else.
    pub async fn is_built(&self, generation: Generation) -> Result<bool> {
        let g = generation.0 as i64;
        self.store
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM graph_build WHERE generation = ?1)",
                    [g],
                    |row| row.get(0),
                )?)
            })
            .await
            .map_err(GraphError::from)
    }

    async fn ensure_built(&self, generation: Generation) -> Result<()> {
        if self.is_built(generation).await? {
            Ok(())
        } else {
            Err(GraphError::NotBuilt(generation))
        }
    }

    async fn edges_for(
        &self,
        generation: Generation,
        ids: &[NodeId],
        direction: Direction,
    ) -> Result<Vec<Edge>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let g = generation.0 as i64;
        let ids: Vec<String> = ids.iter().map(|id| id.0.clone()).collect();

        self.store
            .call(move |conn| {
                let placeholders = std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let predicate = match direction {
                    Direction::Outgoing => format!("from_id IN ({placeholders})"),
                    Direction::Incoming => format!("to_id IN ({placeholders})"),
                    Direction::Both => {
                        format!("from_id IN ({placeholders}) OR to_id IN ({placeholders})")
                    }
                };
                let sql = format!(
                    "SELECT from_id, to_id, kind, confidence FROM graph_edge
                     WHERE generation = ? AND ({predicate})
                     ORDER BY from_id, to_id, kind"
                );

                let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(g)];
                let repeats = if matches!(direction, Direction::Both) {
                    2
                } else {
                    1
                };
                for _ in 0..repeats {
                    for id in &ids {
                        values.push(Box::new(id.clone()));
                    }
                }

                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(values.iter()), edge_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .map_err(GraphError::from)
    }

    async fn nodes_by_id(&self, generation: Generation, ids: &[NodeId]) -> Result<Vec<Node>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let g = generation.0 as i64;
        let ids: Vec<String> = ids.iter().map(|id| id.0.clone()).collect();

        self.store
            .call(move |conn| {
                let placeholders = std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT id, kind, name, path, symbol_path, language, start_line
                     FROM graph_node WHERE generation = ? AND id IN ({placeholders})
                     ORDER BY id"
                );
                let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(g)];
                for id in &ids {
                    values.push(Box::new(id.clone()));
                }

                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(values.iter()), node_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .map_err(GraphError::from)
    }

    async fn nodes_in_file(&self, generation: Generation, path: &str) -> Result<Vec<Node>> {
        let g = generation.0 as i64;
        let path = path.to_string();
        self.store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, kind, name, path, symbol_path, language, start_line
                     FROM graph_node
                     WHERE generation = ?1 AND path = ?2 AND kind IN ('sym', 'test')
                     ORDER BY id",
                )?;
                let rows = stmt
                    .query_map(params![g, path], node_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .map_err(GraphError::from)
    }
}

fn count(tx: &rusqlite::Transaction<'_>, table: &str, g: i64) -> rusqlite::Result<u64> {
    tx.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE generation = ?1"),
        [g],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n as u64)
}

fn node_from_row(row: &Row<'_>) -> rusqlite::Result<Node> {
    let kind: String = row.get(1)?;
    Ok(Node {
        id: NodeId(row.get(0)?),
        kind: NodeKind::parse(&kind).unwrap_or(NodeKind::File),
        name: row.get(2)?,
        path: row.get(3)?,
        symbol_path: row.get(4)?,
        language: row.get(5)?,
        start_line: row.get::<_, Option<i64>>(6)?.map(|l| l as u32),
    })
}

fn edge_from_row(row: &Row<'_>) -> rusqlite::Result<Edge> {
    let kind: String = row.get(2)?;
    let confidence: String = row.get(3)?;
    Ok(Edge {
        from: NodeId(row.get(0)?),
        to: NodeId(row.get(1)?),
        kind: EdgeKind::parse(&kind).unwrap_or(EdgeKind::Contains),
        confidence: Confidence::parse(&confidence).unwrap_or(Confidence::Ambiguous),
    })
}

/// Group edges by the node they leave, for callers that want an adjacency
/// view rather than a flat list.
pub fn adjacency(edges: &[Edge]) -> BTreeMap<&NodeId, Vec<&Edge>> {
    let mut out: BTreeMap<&NodeId, Vec<&Edge>> = BTreeMap::new();
    for edge in edges {
        out.entry(&edge.from).or_default().push(edge);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacency_groups_edges_by_source() {
        let edges = vec![
            Edge {
                from: NodeId::file("a.rs"),
                to: NodeId::file("b.rs"),
                kind: EdgeKind::Imports,
                confidence: Confidence::Likely,
            },
            Edge {
                from: NodeId::file("a.rs"),
                to: NodeId::file("c.rs"),
                kind: EdgeKind::Imports,
                confidence: Confidence::Likely,
            },
            Edge {
                from: NodeId::file("b.rs"),
                to: NodeId::file("c.rs"),
                kind: EdgeKind::Imports,
                confidence: Confidence::Likely,
            },
        ];

        let adjacency = adjacency(&edges);
        assert_eq!(adjacency[&NodeId::file("a.rs")].len(), 2);
        assert_eq!(adjacency[&NodeId::file("b.rs")].len(), 1);
    }
}
