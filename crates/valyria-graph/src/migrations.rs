//! `valyria-graph`'s slice of `workspace.db`. Versions 400-499 are
//! reserved for this crate.
//!
//! Unlike the index (which versions rows so old generations stay
//! readable), the graph stamps every row with the single index generation
//! it was derived from. The graph is a pure function of the index, so
//! there is nothing to preserve that the index does not already hold: a
//! generation's graph is either built or it is not.

use valyria_store::Migration;

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 400,
        description: "create graph_build, graph_node and graph_edge tables",
        sql: "CREATE TABLE graph_build (
            generation INTEGER PRIMARY KEY,
            built_at_ms INTEGER NOT NULL,
            node_count INTEGER NOT NULL,
            edge_count INTEGER NOT NULL,
            unresolved_count INTEGER NOT NULL
        );

        CREATE TABLE graph_node (
            generation INTEGER NOT NULL,
            id TEXT NOT NULL,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            symbol_path TEXT,
            language TEXT,
            start_line INTEGER,
            PRIMARY KEY (generation, id)
        );
        CREATE INDEX graph_node_path ON graph_node(generation, path);

        CREATE TABLE graph_edge (
            generation INTEGER NOT NULL,
            from_id TEXT NOT NULL,
            to_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            confidence TEXT NOT NULL,
            PRIMARY KEY (generation, from_id, to_id, kind)
        );
        -- Both directions get an index, which is what makes
        -- `what does X call?` and `who calls X?` equally cheap (§4.14's
        -- 'both directions materialized').
        CREATE INDEX graph_edge_out ON graph_edge(generation, from_id, kind);
        CREATE INDEX graph_edge_in ON graph_edge(generation, to_id, kind);",
    },
    Migration {
        version: 401,
        description: "create graph_unresolved table",
        sql: "CREATE TABLE graph_unresolved (
            generation INTEGER NOT NULL,
            from_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            target TEXT NOT NULL,
            PRIMARY KEY (generation, from_id, kind, target)
        );
        CREATE INDEX graph_unresolved_target ON graph_unresolved(generation, target);",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_version_is_inside_this_crates_reserved_block() {
        for migration in MIGRATIONS {
            assert!(
                (400..500).contains(&migration.version),
                "version {} is outside valyria-graph's 400-499 block",
                migration.version
            );
        }
    }

    #[test]
    fn applies_cleanly_alongside_the_index_schema() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut all: Vec<Migration> = valyria_index::MIGRATIONS.to_vec();
        all.extend(MIGRATIONS.iter().copied());
        valyria_store::run_migrations(&mut conn, &all).unwrap();

        let applied = valyria_store::applied_versions(&conn).unwrap();
        assert!(applied.contains(&300));
        assert!(applied.contains(&400));
        assert!(applied.contains(&401));
    }
}
