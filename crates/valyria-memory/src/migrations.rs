//! `valyria-memory`'s slice of `workspace.db`. Versions 600-699 are
//! reserved for this crate (`valyria-events` owns 1-99, `valyria-task`
//! 100-199, `valyria-app` 200-299, `valyria-index` 300-399,
//! `valyria-graph` 400-499, `valyria-embed` 500-599).
//!
//! User-tier memory is conceptually global (§48 puts it in `global.db`),
//! but Phase 6 keeps every tier in the one workspace database: the
//! `MemoryStore` API is identical either way, and routing the `User`
//! scope to a second connection is a wiring change for whichever phase
//! stands up `global.db` as a first-class store.

use valyria_store::Migration;

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 600,
    description: "create memory_entry table",
    sql: "CREATE TABLE memory_entry (
        id TEXT PRIMARY KEY,
        scope_kind TEXT NOT NULL,
        scope_id TEXT,
        author TEXT NOT NULL,
        kind TEXT NOT NULL,
        text TEXT NOT NULL,
        provenance TEXT NOT NULL,
        confidence REAL NOT NULL,
        created_ms INTEGER NOT NULL,
        last_seen_ms INTEGER NOT NULL,
        uses INTEGER NOT NULL DEFAULT 0,
        retired INTEGER NOT NULL DEFAULT 0,
        retired_reason TEXT
    );
    CREATE INDEX memory_entry_scope ON memory_entry(scope_kind, scope_id, retired);
    CREATE INDEX memory_entry_live ON memory_entry(retired) WHERE retired = 0;",
}];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_version_is_inside_this_crates_reserved_block() {
        for m in MIGRATIONS {
            assert!(
                (600..700).contains(&m.version),
                "version {} is outside valyria-memory's 600-699 block",
                m.version
            );
        }
    }

    #[test]
    fn applies_cleanly() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        valyria_store::run_migrations(&mut conn, MIGRATIONS).unwrap();
        assert_eq!(valyria_store::applied_versions(&conn).unwrap(), vec![600]);
        conn.execute(
            "INSERT INTO memory_entry
             (id, scope_kind, scope_id, author, kind, text, provenance, confidence, created_ms, last_seen_ms)
             VALUES ('mem_1','repository',NULL,'agent','command','cargo test','t',0.6,0,0)",
            [],
        )
        .unwrap();
    }
}
