//! `valyria-embed`'s slice of `workspace.db`. Versions 500-599 are
//! reserved for this crate (`valyria-events` owns 1-99, `valyria-task`
//! 100-199, `valyria-app` 200-299, `valyria-index` 300-399,
//! `valyria-graph` 400-499).
//!
//! Like the graph, the embedding store stamps every row with the single
//! index generation it was derived from rather than versioning rows: a
//! generation's vectors are either built or they are not, and the index
//! itself is the thing that preserves history. The `chunk_hash` index is
//! what makes a rebuild for a new generation cheap — unchanged chunks
//! carry the same hash, so their vectors are copied forward instead of
//! recomputed.

use valyria_store::Migration;

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 500,
    description: "create embed_build and embed_chunk tables",
    sql: "CREATE TABLE embed_build (
        generation INTEGER PRIMARY KEY,
        built_at_ms INTEGER NOT NULL,
        embedder_id TEXT NOT NULL,
        dim INTEGER NOT NULL,
        chunk_count INTEGER NOT NULL,
        reused_count INTEGER NOT NULL
    );

    CREATE TABLE embed_chunk (
        generation INTEGER NOT NULL,
        path TEXT NOT NULL,
        symbol_path TEXT,
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        start_line INTEGER NOT NULL,
        end_line INTEGER NOT NULL,
        chunk_hash TEXT NOT NULL,
        vector BLOB NOT NULL,
        PRIMARY KEY (generation, path, start_byte)
    );
    -- Look up a chunk's vector by its content hash, across generations,
    -- so a rebuild reuses unchanged chunks instead of re-embedding them.
    CREATE INDEX embed_chunk_hash ON embed_chunk(chunk_hash);
    CREATE INDEX embed_chunk_path ON embed_chunk(generation, path);",
}];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_version_is_inside_this_crates_reserved_block() {
        for migration in MIGRATIONS {
            assert!(
                (500..600).contains(&migration.version),
                "version {} is outside valyria-embed's 500-599 block",
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
        assert!(applied.contains(&500));
    }
}
