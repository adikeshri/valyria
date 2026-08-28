//! `valyria-index`'s slice of `workspace.db`. Versions 300-399 are
//! reserved for this crate (`valyria-events` owns 1-99, `valyria-task`
//! 100-199, `valyria-app` 200-299, `valyria-graph` 400-499).
//!
//! ## Why every table is versioned rather than mutated in place
//!
//! D8 requires that a long agent step never sees the index change
//! underneath it. Rather than locking, every row carries the generation
//! range it is valid for: `valid_from` is the generation that introduced
//! it and `valid_to` is the generation that retired it (`NULL` while the
//! row is current). A read at generation `g` selects
//! `valid_from <= g AND (valid_to IS NULL OR valid_to > g)`, which is
//! snapshot isolation with no reader-writer coordination at all — and it
//! makes "what did the index look like when this step was planned?"
//! answerable after the fact, which is what §8's stale-context detection
//! needs.
//!
//! Rows are joined on `path` rather than a surrogate file id: a file row
//! is itself versioned, so a surrogate id would change every time a file
//! changed, forcing every one of its symbols to be rewritten even when
//! the symbol itself did not move.

use valyria_store::Migration;

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 300,
        description: "create index_generation table",
        sql: "CREATE TABLE index_generation (
            generation INTEGER PRIMARY KEY,
            created_at_ms INTEGER NOT NULL,
            stage TEXT NOT NULL,
            file_count INTEGER NOT NULL,
            symbol_count INTEGER NOT NULL
        );",
    },
    Migration {
        version: 301,
        description: "create index_file table",
        sql: "CREATE TABLE index_file (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL,
            language TEXT,
            content_hash TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            line_count INTEGER NOT NULL,
            is_binary INTEGER NOT NULL,
            has_parse_errors INTEGER NOT NULL DEFAULT 0,
            valid_from INTEGER NOT NULL,
            valid_to INTEGER
        );
        CREATE INDEX index_file_path ON index_file(path, valid_from);
        CREATE INDEX index_file_live ON index_file(valid_to) WHERE valid_to IS NULL;",
    },
    Migration {
        version: 302,
        description: "create index_symbol table",
        sql: "CREATE TABLE index_symbol (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            symbol_path TEXT NOT NULL,
            start_byte INTEGER NOT NULL,
            end_byte INTEGER NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            name_start_byte INTEGER NOT NULL,
            name_end_byte INTEGER NOT NULL,
            name_start_line INTEGER NOT NULL,
            signature TEXT NOT NULL,
            doc TEXT,
            valid_from INTEGER NOT NULL,
            valid_to INTEGER
        );
        CREATE INDEX index_symbol_path ON index_symbol(path, valid_from);
        CREATE INDEX index_symbol_name ON index_symbol(name, valid_from);
        CREATE INDEX index_symbol_symbol_path ON index_symbol(symbol_path, valid_from);",
    },
    Migration {
        version: 303,
        description: "create index_import, index_call and index_test tables",
        sql: "CREATE TABLE index_import (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL,
            raw_path TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            valid_from INTEGER NOT NULL,
            valid_to INTEGER
        );
        CREATE INDEX index_import_path ON index_import(path, valid_from);
        CREATE INDEX index_import_raw ON index_import(raw_path, valid_from);

        CREATE TABLE index_call (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL,
            name TEXT NOT NULL,
            enclosing_symbol_path TEXT,
            start_line INTEGER NOT NULL,
            valid_from INTEGER NOT NULL,
            valid_to INTEGER
        );
        CREATE INDEX index_call_path ON index_call(path, valid_from);
        CREATE INDEX index_call_name ON index_call(name, valid_from);

        CREATE TABLE index_test (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL,
            name TEXT NOT NULL,
            symbol_path TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            valid_from INTEGER NOT NULL,
            valid_to INTEGER
        );
        CREATE INDEX index_test_path ON index_test(path, valid_from);",
    },
    Migration {
        version: 304,
        description: "create index_symbol_fts full-text index",
        // FTS5 covers only *live* symbols: it is the lexical entry point
        // for search against the current generation (§4.14), and keeping
        // historical rows in it would make every query filter them back
        // out. Point-in-time reads use `index_symbol` directly.
        sql: "CREATE VIRTUAL TABLE index_symbol_fts USING fts5(
            name,
            symbol_path,
            path UNINDEXED,
            kind UNINDEXED,
            tokenize = 'unicode61 remove_diacritics 2'
        );",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_version_is_inside_this_crates_reserved_block() {
        for migration in MIGRATIONS {
            assert!(
                (300..400).contains(&migration.version),
                "version {} is outside valyria-index's 300-399 block",
                migration.version
            );
        }
    }

    #[test]
    fn applies_cleanly_including_the_fts5_virtual_table() {
        // Also a build check: `rusqlite`'s bundled SQLite must have been
        // compiled with FTS5, or this migration fails here rather than at
        // a user's first search.
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        valyria_store::run_migrations(&mut conn, MIGRATIONS).unwrap();
        let applied = valyria_store::applied_versions(&conn).unwrap();
        assert_eq!(applied, vec![300, 301, 302, 303, 304]);

        conn.execute(
            "INSERT INTO index_symbol_fts (name, symbol_path, path, kind)
             VALUES ('parse', 'Parser::parse', 'src/p.rs', 'method')",
            [],
        )
        .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM index_symbol_fts WHERE index_symbol_fts MATCH 'parse'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }
}
