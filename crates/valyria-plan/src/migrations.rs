//! `valyria-plan`'s slice of the shared `workspace.db`. Versions **800-899**
//! are reserved for this crate, continuing the hundred-block convention
//! (`valyria-events` 1-99, `valyria-task` 100-199, `valyria-app` 200-299,
//! `valyria-index` 300-399, `valyria-graph` 400-499, `valyria-embed`
//! 500-599, `valyria-memory` 600-699, `valyria-verify` 700-799).
//!
//! `plan_revision` is append-only and keyed `(task_id, revision)` — plans
//! are living documents and every accepted revision is kept. `plan_checkpoint`
//! and `task_artifact` are likewise append-only.

use valyria_store::Migration;

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 800,
        description: "create plan_revision table",
        sql: "CREATE TABLE plan_revision (
            task_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            parent_hash TEXT,
            rationale TEXT NOT NULL,
            plan_json TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY (task_id, revision)
        );",
    },
    Migration {
        version: 801,
        description: "create plan_checkpoint table",
        sql: "CREATE TABLE plan_checkpoint (
            id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL,
            step_id TEXT NOT NULL,
            files_json TEXT NOT NULL,
            ledger_watermark INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX plan_checkpoint_task ON plan_checkpoint(task_id, created_at_ms);",
    },
    Migration {
        version: 802,
        description: "create task_artifact table",
        sql: "CREATE TABLE task_artifact (
            id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            produced_by_role TEXT NOT NULL,
            artifact_json TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX task_artifact_task ON task_artifact(task_id, created_at_ms);",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_block_is_in_the_800s() {
        assert!(MIGRATIONS.iter().all(|m| (800..900).contains(&m.version)));
    }

    #[test]
    fn applies_cleanly() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        valyria_store::run_migrations(&mut conn, MIGRATIONS).unwrap();
        let applied = valyria_store::applied_versions(&conn).unwrap();
        assert!(applied.contains(&800));
        assert!(applied.contains(&801));
        assert!(applied.contains(&802));
    }
}
