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
    Migration {
        version: 902,
        description: "record license acceptance on installed models",
        sql: "ALTER TABLE installed_model ADD COLUMN license_accepted_at_ms INTEGER;",
    },
    Migration {
        version: 903,
        description: "create model_endpoint table",
        sql: "CREATE TABLE model_endpoint (
        id TEXT PRIMARY KEY,
        base_url TEXT NOT NULL,
        display_name TEXT NOT NULL,
        remote_model_name TEXT NOT NULL,
        context_length INTEGER NOT NULL,
        supports_native_tools INTEGER NOT NULL,
        supports_grammar INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL
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
    /// Unix ms at which the user accepted `license_name` (mirrors the
    /// model's `manifest.json`). `None` for installs with no distinct
    /// license-acceptance step.
    pub license_accepted_at_ms: Option<i64>,
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
        let license_accepted_at_ms = manifest.license_accepted_at_ms;
        let probe_json = manifest
            .probe
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO installed_model
                     (id, weights_file, content_hash, size_bytes, license_name, installed_at_ms, license_accepted_at_ms, probe_json)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        id,
                        weights_file,
                        content_hash,
                        size_bytes,
                        license_name,
                        installed_at_ms,
                        license_accepted_at_ms,
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
                        "SELECT id, weights_file, content_hash, size_bytes, license_name, installed_at_ms, license_accepted_at_ms, probe_json
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
                    "SELECT id, weights_file, content_hash, size_bytes, license_name, installed_at_ms, license_accepted_at_ms, probe_json
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

    // --- external endpoints (§ M6: a registered, already-running
    // OpenAI-compatible server Core neither downloads nor supervises) ---

    /// Register (or replace) an external endpoint.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_endpoint(
        &self,
        id: &str,
        base_url: &str,
        display_name: &str,
        remote_model_name: &str,
        context_length: u32,
        supports_native_tools: bool,
        supports_grammar: bool,
        created_at_ms: i64,
    ) -> Result<()> {
        let (id, base_url, display_name, remote_model_name) = (
            id.to_string(),
            base_url.to_string(),
            display_name.to_string(),
            remote_model_name.to_string(),
        );
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO model_endpoint
                     (id, base_url, display_name, remote_model_name, context_length, supports_native_tools, supports_grammar, created_at_ms)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        id,
                        base_url,
                        display_name,
                        remote_model_name,
                        context_length,
                        supports_native_tools,
                        supports_grammar,
                        created_at_ms
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn endpoint(&self, id: &str) -> Result<Option<EndpointRow>> {
        let id = id.to_string();
        let row = self
            .store
            .call(move |conn| {
                let row = conn
                    .query_row(
                        "SELECT id, base_url, display_name, remote_model_name, context_length, supports_native_tools, supports_grammar, created_at_ms
                         FROM model_endpoint WHERE id = ?1",
                        params![id],
                        map_endpoint_row,
                    )
                    .optional()?;
                Ok(row)
            })
            .await?;
        Ok(row)
    }

    pub async fn endpoints(&self) -> Result<Vec<EndpointRow>> {
        let rows = self
            .store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, base_url, display_name, remote_model_name, context_length, supports_native_tools, supports_grammar, created_at_ms
                     FROM model_endpoint ORDER BY id",
                )?;
                let rows = stmt
                    .query_map([], map_endpoint_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(rows)
    }

    /// Returns whether a row was actually removed.
    pub async fn remove_endpoint(&self, id: &str) -> Result<bool> {
        let id = id.to_string();
        let removed = self
            .store
            .call(move |conn| {
                let n = conn.execute("DELETE FROM model_endpoint WHERE id = ?1", params![id])?;
                Ok(n > 0)
            })
            .await?;
        Ok(removed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointRow {
    pub id: String,
    pub base_url: String,
    pub display_name: String,
    pub remote_model_name: String,
    pub context_length: u32,
    pub supports_native_tools: bool,
    pub supports_grammar: bool,
    pub created_at_ms: i64,
}

fn map_endpoint_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EndpointRow> {
    Ok(EndpointRow {
        id: row.get(0)?,
        base_url: row.get(1)?,
        display_name: row.get(2)?,
        remote_model_name: row.get(3)?,
        context_length: row.get::<_, i64>(4)? as u32,
        supports_native_tools: row.get(5)?,
        supports_grammar: row.get(6)?,
        created_at_ms: row.get(7)?,
    })
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InstalledModelRow> {
    Ok(InstalledModelRow {
        id: row.get(0)?,
        weights_file: row.get(1)?,
        content_hash: row.get(2)?,
        size_bytes: row.get::<_, i64>(3)? as u64,
        license_name: row.get(4)?,
        installed_at_ms: row.get(5)?,
        license_accepted_at_ms: row.get(6)?,
        probe_json: row.get(7)?,
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
        assert!(applied.contains(&902));
        assert!(applied.contains(&903));
    }

    #[test]
    fn migration_block_is_in_the_900s_and_ordered() {
        let versions: Vec<i64> = MIGRATIONS.iter().map(|m| m.version).collect();
        let mut sorted = versions.clone();
        sorted.sort_unstable();
        assert_eq!(versions, sorted, "migrations must be version-ordered");
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

    #[tokio::test]
    async fn endpoints_round_trip_replace_and_remove() {
        let store = Arc::new(Store::open_in_memory(MIGRATIONS).unwrap());
        let db = InstalledModelStore::new(store);

        assert_eq!(db.endpoint("ollama-local").await.unwrap(), None);
        assert!(db.endpoints().await.unwrap().is_empty());

        db.add_endpoint(
            "ollama-local",
            "http://127.0.0.1:11434/v1",
            "My Ollama",
            "qwen2.5-coder:7b",
            32768,
            true,
            false,
            100,
        )
        .await
        .unwrap();

        let row = db.endpoint("ollama-local").await.unwrap().unwrap();
        assert_eq!(row.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(row.display_name, "My Ollama");
        assert_eq!(row.remote_model_name, "qwen2.5-coder:7b");
        assert_eq!(row.context_length, 32768);
        assert!(row.supports_native_tools);
        assert!(!row.supports_grammar);
        assert_eq!(db.endpoints().await.unwrap().len(), 1);

        // Re-adding the same id replaces rather than erroring or duplicating.
        db.add_endpoint(
            "ollama-local",
            "http://127.0.0.1:11434/v1",
            "My Ollama (renamed)",
            "qwen2.5-coder:7b",
            32768,
            true,
            false,
            101,
        )
        .await
        .unwrap();
        assert_eq!(db.endpoints().await.unwrap().len(), 1);
        assert_eq!(
            db.endpoint("ollama-local")
                .await
                .unwrap()
                .unwrap()
                .display_name,
            "My Ollama (renamed)"
        );

        assert!(db.remove_endpoint("ollama-local").await.unwrap());
        assert_eq!(db.endpoint("ollama-local").await.unwrap(), None);
        // Removing something already gone is a clean `false`, not an error.
        assert!(!db.remove_endpoint("ollama-local").await.unwrap());
    }
}
