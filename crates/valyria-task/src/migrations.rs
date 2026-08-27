//! `valyria-task`'s SQLite schema. Versions 100-199 are reserved for this
//! crate; `valyria-events` owns 1-99. Every future layer-5+ crate wanting
//! tables in the shared `workspace.db` should reserve the next hundred
//! block and document it here, since `Migration.version` is a *global*
//! dedup key once every crate's slice is concatenated by `valyria-app`.

use valyria_store::Migration;

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 100,
        description: "create tasks table",
        sql: "CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            parent_task TEXT,
            objective TEXT NOT NULL,
            state TEXT NOT NULL,
            paused_from TEXT,
            plan_scope TEXT NOT NULL DEFAULT '[]',
            budget_max_steps INTEGER,
            budget_max_wall_ms INTEGER,
            budget_max_tokens INTEGER,
            index_generation_at_start INTEGER,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            completed_at_ms INTEGER,
            recovery_note TEXT,
            pending_signal TEXT
        );
        CREATE INDEX tasks_state ON tasks(state);",
    },
    Migration {
        version: 101,
        description: "create task_journal table",
        sql: "CREATE TABLE task_journal (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            effect_id TEXT,
            payload TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX task_journal_task_id ON task_journal(task_id, seq);",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_collide_with_events_migration_versions() {
        let task_versions: Vec<i64> = MIGRATIONS.iter().map(|m| m.version).collect();
        let event_versions: Vec<i64> = valyria_events::MIGRATIONS
            .iter()
            .map(|m| m.version)
            .collect();
        for v in &task_versions {
            assert!(
                !event_versions.contains(v),
                "version {v} collides between valyria-task and valyria-events"
            );
        }
    }

    #[test]
    fn both_slices_apply_cleanly_to_one_store() {
        let mut combined: Vec<Migration> = valyria_events::MIGRATIONS.to_vec();
        combined.extend(MIGRATIONS.iter().copied());
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut conn = conn;
        valyria_store::run_migrations(&mut conn, &combined).unwrap();
        let applied = valyria_store::applied_versions(&conn).unwrap();
        assert!(applied.contains(&1));
        assert!(applied.contains(&100));
        assert!(applied.contains(&101));
    }
}
