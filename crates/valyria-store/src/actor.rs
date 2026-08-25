//! The single-writer SQLite actor (D7).
//!
//! `rusqlite::Connection` is `Send` but not `Sync`, and SQLite's own
//! locking makes concurrent writers from multiple threads a source of
//! `SQLITE_BUSY` contention. Rather than fight that, exactly one dedicated
//! OS thread owns the connection for the lifetime of the [`Store`]; every
//! other caller submits a closure and awaits its result over a channel.
//! This gives every access — read or write — a consistent, serialized view
//! without needing a connection pool or lock dance, which is the right
//! trade for a local, single-process, single-writer workload.

use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;

use rusqlite::Connection;

use crate::error::{Result, StoreError};
use crate::migrations::{run_migrations, Migration};

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

pub struct Store {
    // `Option` (rather than a bare `Sender`) so `Drop` can explicitly close
    // the channel — by dropping the sender — *before* joining the actor
    // thread. Field drop order in Rust runs after an explicit `Drop::drop`
    // body finishes, so without this, `handle.join()` would wait forever
    // for a thread whose exit condition (`for job in rx` ending) can only
    // be satisfied by dropping `tx`, which hasn't happened yet.
    tx: Option<std_mpsc::Sender<Job>>,
    db_path: PathBuf,
    handle: Option<JoinHandle<()>>,
}

impl Store {
    /// Open (creating if absent) the database at `path`, apply `migrations`,
    /// and start the owning actor thread. Migration failures surface here,
    /// synchronously, before any caller can observe a half-migrated store.
    pub fn open(path: &Path, migrations: &[Migration]) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        run_migrations(&mut conn, migrations)?;

        let (tx, rx) = std_mpsc::channel::<Job>();
        let handle = std::thread::Builder::new()
            .name(format!("valyria-store:{}", path.display()))
            .spawn(move || {
                let mut conn = conn;
                for job in rx {
                    job(&mut conn);
                }
            })
            .expect("failed to spawn store actor thread");

        Ok(Self {
            tx: Some(tx),
            db_path: path.to_path_buf(),
            handle: Some(handle),
        })
    }

    /// Open an in-memory database — used by tests and by short-lived
    /// scratch stores that never need to persist.
    pub fn open_in_memory(migrations: &[Migration]) -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        run_migrations(&mut conn, migrations)?;

        let (tx, rx) = std_mpsc::channel::<Job>();
        let handle = std::thread::Builder::new()
            .name("valyria-store:memory".to_string())
            .spawn(move || {
                let mut conn = conn;
                for job in rx {
                    job(&mut conn);
                }
            })
            .expect("failed to spawn store actor thread");

        Ok(Self {
            tx: Some(tx),
            db_path: PathBuf::from(":memory:"),
            handle: Some(handle),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Run `f` against the connection on the actor thread and await its
    /// result. `f` may open its own `conn.transaction()` for multi-statement
    /// atomicity.
    pub async fn call<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let result = f(conn);
            let _ = reply_tx.send(result);
        });
        self.tx
            .as_ref()
            .ok_or(StoreError::ActorShutDown)?
            .send(job)
            .map_err(|_| StoreError::ActorShutDown)?;
        reply_rx.await.map_err(|_| StoreError::ActorShutDown)?
    }

    /// On-disk size of the database file(s), for `storage.inspect` (§48).
    /// Best-effort: sums the main db file plus WAL/SHM sidecars if present.
    pub fn size_bytes(&self) -> u64 {
        if self.db_path == Path::new(":memory:") {
            return 0;
        }
        [
            self.db_path.clone(),
            append_suffix(&self.db_path, "-wal"),
            append_suffix(&self.db_path, "-shm"),
        ]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

impl Drop for Store {
    fn drop(&mut self) {
        // Explicitly close the channel *first* by dropping the sender.
        // Only then does the actor thread's `for job in rx {}` loop see
        // the channel disconnect and exit, which is the precondition for
        // `join()` below ever returning.
        self.tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_migrations() -> Vec<Migration> {
        vec![Migration {
            version: 1,
            description: "create kv",
            sql: "CREATE TABLE kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);",
        }]
    }

    #[tokio::test]
    async fn call_executes_on_actor_thread_and_returns_result() {
        let store = Store::open_in_memory(&no_migrations()).unwrap();
        store
            .call(|conn| {
                conn.execute("INSERT INTO kv (k, v) VALUES ('a', '1')", [])?;
                Ok(())
            })
            .await
            .unwrap();

        let value: String = store
            .call(
                |conn| Ok(conn.query_row("SELECT v FROM kv WHERE k = 'a'", [], |row| row.get(0))?),
            )
            .await
            .unwrap();
        assert_eq!(value, "1");
    }

    #[tokio::test]
    async fn transaction_rolls_back_on_error() {
        let store = Store::open_in_memory(&no_migrations()).unwrap();
        let result: Result<()> = store
            .call(|conn| {
                let tx = conn.transaction()?;
                tx.execute("INSERT INTO kv (k, v) VALUES ('x', '1')", [])?;
                // Violates the primary key -> should fail and roll back.
                tx.execute("INSERT INTO kv (k, v) VALUES ('x', '2')", [])?;
                tx.commit()?;
                Ok(())
            })
            .await;
        assert!(result.is_err());

        let count: i64 = store
            .call(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM kv", [], |row| row.get(0))?))
            .await
            .unwrap();
        assert_eq!(count, 0, "partial transaction must not be visible");
    }

    #[tokio::test]
    async fn many_concurrent_callers_are_serialized_without_busy_errors() {
        let store = std::sync::Arc::new(Store::open_in_memory(&no_migrations()).unwrap());
        let mut handles = Vec::new();
        for i in 0..50 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .call(move |conn| {
                        conn.execute(
                            "INSERT INTO kv (k, v) VALUES (?1, ?2)",
                            rusqlite::params![format!("k{i}"), i.to_string()],
                        )?;
                        Ok(())
                    })
                    .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }

        let count: i64 = store
            .call(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM kv", [], |row| row.get(0))?))
            .await
            .unwrap();
        assert_eq!(count, 50);
    }

    #[tokio::test]
    async fn persists_to_disk_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("workspace.db");

        {
            let store = Store::open(&db_path, &no_migrations()).unwrap();
            store
                .call(|conn| {
                    conn.execute("INSERT INTO kv (k, v) VALUES ('persisted', 'yes')", [])?;
                    Ok(())
                })
                .await
                .unwrap();
        } // store dropped, actor thread joined, connection closed

        let store = Store::open(&db_path, &no_migrations()).unwrap();
        let value: String = store
            .call(|conn| {
                Ok(
                    conn.query_row("SELECT v FROM kv WHERE k = 'persisted'", [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(value, "yes");
    }

    #[test]
    fn size_bytes_is_zero_for_in_memory() {
        let store = Store::open_in_memory(&no_migrations()).unwrap();
        assert_eq!(store.size_bytes(), 0);
    }

    #[tokio::test]
    async fn size_bytes_grows_after_writes_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("sized.db");
        let store = Store::open(&db_path, &no_migrations()).unwrap();
        let before = store.size_bytes();
        for i in 0..500 {
            store
                .call(move |conn| {
                    conn.execute(
                        "INSERT INTO kv (k, v) VALUES (?1, ?2)",
                        rusqlite::params![format!("row{i}"), "x".repeat(200)],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        // Force a WAL checkpoint so the growth is reflected on disk
        // immediately rather than sitting only in memory.
        store
            .call(|conn| {
                conn.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(store.size_bytes() > before);
    }
}
