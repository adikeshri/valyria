//! Persisting verification runs (§27: "every run persists command, env,
//! exit code, duration, parsed results, raw output blob, and the
//! changeset it applied to").
//!
//! Migration block **700-799** in the shared `workspace.db`. The table is
//! append-only — a run, once recorded, is immutable evidence. The
//! completion report ([`crate::report`]) is built from these rows and
//! nothing else (D4).

use std::sync::Arc;

use rusqlite::params;
use valyria_store::{Migration, Store};
use valyria_types::{TaskId, Timestamp, VerificationRunId};

use crate::error::Result;
use crate::parse::Failure;
use crate::run::{VerificationOutcome, VerificationRun};

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 700,
    description: "create verification_run table",
    sql: "CREATE TABLE verification_run (
        id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL,
        command_kind TEXT NOT NULL,
        command_display TEXT NOT NULL,
        tier TEXT,
        outcome TEXT NOT NULL,
        exit_code INTEGER,
        duration_ms INTEGER NOT NULL,
        changeset_hash TEXT,
        failures_json TEXT NOT NULL,
        stdout TEXT NOT NULL,
        stderr TEXT NOT NULL,
        truncated INTEGER NOT NULL DEFAULT 0,
        captured_at_ms INTEGER NOT NULL,
        seq INTEGER NOT NULL
    );
    CREATE INDEX verification_run_task ON verification_run(task_id, seq);",
}];

/// A verification run as read back from the store.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationRunRecord {
    pub id: VerificationRunId,
    pub task_id: TaskId,
    pub command_kind: String,
    pub command_display: String,
    pub tier: Option<String>,
    pub outcome: VerificationOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub changeset_hash: Option<String>,
    pub failures: Vec<Failure>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub captured_at: Timestamp,
    pub seq: u64,
}

impl VerificationRunRecord {
    pub fn passed(&self) -> bool {
        self.outcome == VerificationOutcome::Passed
    }
}

/// Append-only store of verification runs, one logical log per task.
pub struct VerificationLog {
    store: Arc<Store>,
}

impl std::fmt::Debug for VerificationLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VerificationLog")
    }
}

impl VerificationLog {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Record one run against `task_id`. `seq` is assigned as
    /// `max(seq)+1` for the task so the log has a stable order
    /// independent of `captured_at` clock resolution.
    pub async fn record(&self, task_id: TaskId, run: &VerificationRun) -> Result<()> {
        let id = run.id.to_string();
        let task = task_id.to_string();
        let kind = run.command.kind.as_str().to_string();
        let display = run.command.display();
        let tier = run.tier.map(|t| format!("{t:?}"));
        let outcome = outcome_str(run.outcome).to_string();
        let exit_code = run.exit_code;
        let duration = run.duration_ms as i64;
        let cs_hash = run.changeset_hash.map(|h| h.to_hex());
        let failures_json = serde_json::to_string(&run.failures)?;
        let stdout = run.stdout.clone();
        let stderr = run.stderr.clone();
        let truncated = run.truncated as i64;
        let captured = run.captured_at.as_millis() as i64;

        self.store
            .call(move |conn| {
                let next_seq: i64 = conn
                    .query_row(
                        "SELECT COALESCE(MAX(seq), 0) + 1 FROM verification_run WHERE task_id = ?1",
                        params![task],
                        |r| r.get(0),
                    )
                    .unwrap_or(1);
                conn.execute(
                    "INSERT INTO verification_run
                     (id, task_id, command_kind, command_display, tier, outcome, exit_code,
                      duration_ms, changeset_hash, failures_json, stdout, stderr, truncated,
                      captured_at_ms, seq)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                    params![
                        id,
                        task,
                        kind,
                        display,
                        tier,
                        outcome,
                        exit_code,
                        duration,
                        cs_hash,
                        failures_json,
                        stdout,
                        stderr,
                        truncated,
                        captured,
                        next_seq,
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn list_for_task(&self, task_id: TaskId) -> Result<Vec<VerificationRunRecord>> {
        let task = task_id.to_string();
        let rows = self
            .store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, task_id, command_kind, command_display, tier, outcome, exit_code,
                            duration_ms, changeset_hash, failures_json, stdout, stderr, truncated,
                            captured_at_ms, seq
                     FROM verification_run WHERE task_id = ?1 ORDER BY seq",
                )?;
                let mapped = stmt
                    .query_map(params![task], |row| {
                        Ok(RawRow {
                            id: row.get(0)?,
                            task_id: row.get(1)?,
                            command_kind: row.get(2)?,
                            command_display: row.get(3)?,
                            tier: row.get(4)?,
                            outcome: row.get(5)?,
                            exit_code: row.get(6)?,
                            duration_ms: row.get(7)?,
                            changeset_hash: row.get(8)?,
                            failures_json: row.get(9)?,
                            stdout: row.get(10)?,
                            stderr: row.get(11)?,
                            truncated: row.get(12)?,
                            captured_at_ms: row.get(13)?,
                            seq: row.get(14)?,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(mapped)
            })
            .await?;
        Ok(rows.into_iter().map(RawRow::into_record).collect())
    }

    pub async fn latest_for_task(&self, task_id: TaskId) -> Result<Option<VerificationRunRecord>> {
        Ok(self.list_for_task(task_id).await?.into_iter().next_back())
    }

    /// Delete every run for a task — backs `valyria clean` scoped to a
    /// task's artifacts.
    pub async fn purge_task(&self, task_id: TaskId) -> Result<u64> {
        let task = task_id.to_string();
        let n = self
            .store
            .call(move |conn| {
                let n = conn.execute(
                    "DELETE FROM verification_run WHERE task_id = ?1",
                    params![task],
                )?;
                Ok(n as u64)
            })
            .await?;
        Ok(n)
    }
}

struct RawRow {
    id: String,
    task_id: String,
    command_kind: String,
    command_display: String,
    tier: Option<String>,
    outcome: String,
    exit_code: Option<i64>,
    duration_ms: i64,
    changeset_hash: Option<String>,
    failures_json: String,
    stdout: String,
    stderr: String,
    truncated: i64,
    captured_at_ms: i64,
    seq: i64,
}

impl RawRow {
    fn into_record(self) -> VerificationRunRecord {
        VerificationRunRecord {
            id: self.id.parse().unwrap_or_else(|_| VerificationRunId::new()),
            task_id: self.task_id.parse().unwrap_or_else(|_| TaskId::new()),
            command_kind: self.command_kind,
            command_display: self.command_display,
            tier: self.tier,
            outcome: outcome_from_str(&self.outcome),
            exit_code: self.exit_code.map(|c| c as i32),
            duration_ms: self.duration_ms as u64,
            changeset_hash: self.changeset_hash,
            failures: serde_json::from_str(&self.failures_json).unwrap_or_default(),
            stdout: self.stdout,
            stderr: self.stderr,
            truncated: self.truncated != 0,
            captured_at: Timestamp::from_millis(self.captured_at_ms as u128),
            seq: self.seq as u64,
        }
    }
}

fn outcome_str(o: VerificationOutcome) -> &'static str {
    match o {
        VerificationOutcome::Passed => "passed",
        VerificationOutcome::Failed => "failed",
        VerificationOutcome::Errored => "errored",
        VerificationOutcome::TimedOut => "timed_out",
    }
}

fn outcome_from_str(s: &str) -> VerificationOutcome {
    match s {
        "passed" => VerificationOutcome::Passed,
        "timed_out" => VerificationOutcome::TimedOut,
        "errored" => VerificationOutcome::Errored,
        _ => VerificationOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandKind, CommandSource, VerifyCommand};
    use crate::run::run_from_captured;
    use crate::strategy::Tier;

    async fn store() -> Arc<Store> {
        Arc::new(Store::open_in_memory(MIGRATIONS).unwrap())
    }

    fn cmd() -> VerifyCommand {
        VerifyCommand::new(
            CommandKind::Test,
            "cargo",
            ["test"],
            CommandSource::Manifest {
                file: "Cargo.toml".into(),
            },
        )
    }

    #[tokio::test]
    async fn round_trips_a_run() {
        let log = VerificationLog::new(store().await);
        let task = TaskId::new();
        let run = run_from_captured(
            &cmd(),
            Some(Tier::Full),
            "test tests::x ... FAILED\ntest result: FAILED. 0 passed; 1 failed",
            "",
            Some(101),
            Timestamp::from_millis(42),
        );
        log.record(task, &run).await.unwrap();

        let back = log.list_for_task(task).await.unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, run.id);
        assert_eq!(back[0].outcome, VerificationOutcome::Failed);
        assert_eq!(back[0].failures.len(), 1);
        assert_eq!(back[0].tier.as_deref(), Some("Full"));
        assert_eq!(back[0].seq, 1);
    }

    #[tokio::test]
    async fn seq_increments_per_task() {
        let log = VerificationLog::new(store().await);
        let task = TaskId::new();
        for _ in 0..3 {
            let run = run_from_captured(&cmd(), None, "ok", "", Some(0), Timestamp::from_millis(1));
            log.record(task, &run).await.unwrap();
        }
        let back = log.list_for_task(task).await.unwrap();
        assert_eq!(
            back.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(log.latest_for_task(task).await.unwrap().unwrap().seq == 3);
    }

    #[tokio::test]
    async fn purge_removes_a_tasks_runs() {
        let log = VerificationLog::new(store().await);
        let task = TaskId::new();
        let run = run_from_captured(&cmd(), None, "ok", "", Some(0), Timestamp::from_millis(1));
        log.record(task, &run).await.unwrap();
        assert_eq!(log.purge_task(task).await.unwrap(), 1);
        assert!(log.list_for_task(task).await.unwrap().is_empty());
    }

    #[test]
    fn migration_block_is_in_the_700s() {
        assert!(MIGRATIONS.iter().all(|m| (700..800).contains(&m.version)));
    }
}
