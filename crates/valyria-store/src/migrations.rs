//! Forward-only, versioned schema migrations (§48). Each migration is a
//! single SQL script applied inside its own transaction; the applied
//! version is recorded in `schema_migrations` so re-running `migrate` is
//! idempotent.

use rusqlite::Connection;

use crate::error::{Result, StoreError};

pub struct Migration {
    pub version: i64,
    pub description: &'static str,
    pub sql: &'static str,
}

pub fn run_migrations(conn: &mut Connection, migrations: &[Migration]) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            description TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL
        );",
    )?;

    let mut sorted: Vec<&Migration> = migrations.iter().collect();
    sorted.sort_by_key(|m| m.version);

    for migration in sorted {
        let already_applied: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [migration.version],
            |row| row.get(0),
        )?;
        if already_applied {
            continue;
        }

        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)
            .map_err(|e| StoreError::Migration {
                version: migration.version,
                source: e,
            })?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        tx.execute(
            "INSERT INTO schema_migrations (version, description, applied_at_ms) VALUES (?1, ?2, ?3)",
            rusqlite::params![migration.version, migration.description, now_ms],
        )
        .map_err(|e| StoreError::Migration {
            version: migration.version,
            source: e,
        })?;
        tx.commit()?;
        tracing::info!(
            version = migration.version,
            description = migration.description,
            "applied migration"
        );
    }

    Ok(())
}

pub fn applied_versions(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
    let versions = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_migrations() -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                description: "create widgets",
                sql: "CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            },
            Migration {
                version: 2,
                description: "add widgets.color",
                sql: "ALTER TABLE widgets ADD COLUMN color TEXT;",
            },
        ]
    }

    #[test]
    fn applies_migrations_in_order() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn, &sample_migrations()).unwrap();
        assert_eq!(applied_versions(&conn).unwrap(), vec![1, 2]);

        conn.execute(
            "INSERT INTO widgets (name, color) VALUES ('gizmo', 'red')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn, &sample_migrations()).unwrap();
        // Running again must not error (e.g. re-adding the column).
        run_migrations(&mut conn, &sample_migrations()).unwrap();
        assert_eq!(applied_versions(&conn).unwrap(), vec![1, 2]);
    }

    #[test]
    fn out_of_order_declaration_still_applies_by_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        let mut migrations = sample_migrations();
        migrations.reverse();
        run_migrations(&mut conn, &migrations).unwrap();
        assert_eq!(applied_versions(&conn).unwrap(), vec![1, 2]);
    }

    #[test]
    fn failing_migration_does_not_get_marked_applied() {
        let mut conn = Connection::open_in_memory().unwrap();
        let bad = vec![Migration {
            version: 1,
            description: "broken",
            sql: "THIS IS NOT VALID SQL;",
        }];
        assert!(run_migrations(&mut conn, &bad).is_err());
        assert!(applied_versions(&conn).unwrap().is_empty());
    }
}
