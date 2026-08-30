//! `valyria-model-store`'s slice of the **global** `global.db` (§4.1:
//! "one global database for models/user memory/config state"). Versions
//! **900-999** are reserved for this crate, continuing the hundred-block
//! convention. The rows here are a fast index over the on-disk
//! `manifest.json` files and are fully rebuildable by rescanning the
//! store — the filesystem is the source of truth.

use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use valyria_store::{Migration, Store};

use crate::error::Result;
use crate::manifest::Manifest;

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 900,
        description: "create installed_model table",
        sql: "CREATE TABLE installed_model (
        id TEXT PRIMARY KEY,
        weights_file TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        size_bytes INTEGER NOT NULL,
        license_name TEXT NOT NULL,
        installed_at_ms INTEGER NOT NULL,
        probe_json TEXT
    );",
    },
    Migration {
        version: 901,
        description: "create model_role_binding table",
        sql: "CREATE TABLE model_role_binding (
        role TEXT PRIMARY KEY,
        model_id TEXT NOT NULL,
        bound_at_ms INTEGER NOT NULL
    );",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledModelRow {
    pub id: String,
    pub weights_file: String,
    pub content_hash: String,
    pub size_bytes: u64,
    pub license_name: String,
    pub installed_at_ms: i64,
    pub probe_json: Option<String>,
}

pub struct InstalledModelStore {
    store: Arc<Store>,
}

impl std::fmt::Debug for InstalledModelStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InstalledModelStore")
    }
}

impl InstalledModelStore {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Upsert the index row for a freshly-installed model.
    pub async fn record(&self, manifest: &Manifest) -> Result<()> {
        let id = manifest.card.id.clone();
        let weights_file = manifest.weights_file.clone();
        let content_hash = manifest.content_hash.clone();
        let size_bytes = manifest.size_bytes as i64;
        let license_name = manifest.card.license_name.clone();
        let installed_at_ms = manifest.installed_at_ms;
        let probe_json = manifest
            .probe
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO installed_model
                     (id, weights_file, content_hash, size_bytes, license_name, installed_at_ms, probe_json)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        id,
                        weights_file,
                        content_hash,
                        size_bytes,
                        license_name,
                        installed_at_ms,
                        probe_json
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn get(&self, id: &str) -> Result<Option<InstalledModelRow>> {
        let id = id.to_string();
        let row = self
            .store
            .call(move |conn| {
                let row = conn
                    .query_row(
                        "SELECT id, weights_file, content_hash, size_bytes, license_name, installed_at_ms, probe_json
                         FROM installed_model WHERE id = ?1",
                        params![id],
                        map_row,
                    )
                    .optional()?;
                Ok(row)
            })
            .await?;
        Ok(row)
    }

    pub async fn list(&self) -> Result<Vec<InstalledModelRow>> {
        let rows = self
            .store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, weights_file, content_hash, size_bytes, license_name, installed_at_ms, probe_json
                     FROM installed_model ORDER BY id",
                )?;
                let rows = stmt
                    .query_map([], map_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(rows)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.store
            .call(move |conn| {
                conn.execute("DELETE FROM installed_model WHERE id = ?1", params![id])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    // --- role bindings (§38: which installed model serves which role) ---

    /// Bind `role` (a `ModelRole` display string, e.g. `primary_coder`) to
    /// installed model `model_id`. Replaces any existing binding.
    pub async fn set_role_binding(&self, role: &str, model_id: &str, at_ms: i64) -> Result<()> {
        let (role, model_id) = (role.to_string(), model_id.to_string());
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO model_role_binding (role, model_id, bound_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![role, model_id, at_ms],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// The model bound to `role`, if any.
    pub async fn role_binding(&self, role: &str) -> Result<Option<String>> {
        let role = role.to_string();
        let id = self
            .store
            .call(move |conn| {
                let id = conn
                    .query_row(
                        "SELECT model_id FROM model_role_binding WHERE role = ?1",
                        params![role],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?;
                Ok(id)
            })
            .await?;
        Ok(id)
    }

    /// Every `(role, model_id)` binding, ordered by role.
    pub async fn role_bindings(&self) -> Result<Vec<(String, String)>> {
        let rows = self
            .store
            .call(move |conn| {
                let mut stmt =
                    conn.prepare("SELECT role, model_id FROM model_role_binding ORDER BY role")?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(rows)
    }

    /// Drop every binding that points at `model_id` (called when it is
    /// removed).
    pub async fn clear_bindings_for(&self, model_id: &str) -> Result<()> {
        let model_id = model_id.to_string();
        self.store
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM model_role_binding WHERE model_id = ?1",
                    params![model_id],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InstalledModelRow> {
    Ok(InstalledModelRow {
        id: row.get(0)?,
        weights_file: row.get(1)?,
        content_hash: row.get(2)?,
        size_bytes: row.get::<_, i64>(3)? as u64,
        license_name: row.get(4)?,
        installed_at_ms: row.get(5)?,
        probe_json: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_block_is_in_the_900s() {
        assert!(MIGRATIONS.iter().all(|m| (900..1000).contains(&m.version)));
    }

    #[test]
    fn applies_cleanly() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        valyria_store::run_migrations(&mut conn, MIGRATIONS).unwrap();
        let applied = valyria_store::applied_versions(&conn).unwrap();
        assert!(applied.contains(&900));
        assert!(applied.contains(&901));
    }

    #[tokio::test]
    async fn role_bindings_round_trip_and_clear() {
        let store = Arc::new(Store::open_in_memory(MIGRATIONS).unwrap());
        let db = InstalledModelStore::new(store);

        assert_eq!(db.role_binding("primary_coder").await.unwrap(), None);
        db.set_role_binding("primary_coder", "qwen-x", 10)
            .await
            .unwrap();
        db.set_role_binding("planner", "qwen-x", 11).await.unwrap();
        assert_eq!(
            db.role_binding("primary_coder").await.unwrap().as_deref(),
            Some("qwen-x")
        );
        assert_eq!(db.role_bindings().await.unwrap().len(), 2);

        // Rebinding replaces.
        db.set_role_binding("primary_coder", "llama-y", 12)
            .await
            .unwrap();
        assert_eq!(
            db.role_binding("primary_coder").await.unwrap().as_deref(),
            Some("llama-y")
        );

        // Removing a model clears every binding that named it.
        db.clear_bindings_for("qwen-x").await.unwrap();
        assert_eq!(db.role_binding("planner").await.unwrap(), None);
        assert_eq!(
            db.role_binding("primary_coder").await.unwrap().as_deref(),
            Some("llama-y")
        );
    }
}
