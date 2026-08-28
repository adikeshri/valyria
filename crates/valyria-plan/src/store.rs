//! Persistence for plans, checkpoints and role artifacts (§4.25, §4.1).
//!
//! Every method is `async` and funnels through `Store::call`, matching
//! `valyria_verify::VerificationLog`. Nothing here is process-local: a
//! resumed task rebuilds its plan and checkpoints from these rows.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::params;
use valyria_store::Store;
use valyria_types::{CheckpointId, TaskId, Timestamp};

use crate::checkpoint::Checkpoint;
use crate::error::Result;
use crate::model::{Plan, PlanRevision, PlanStepId};
use crate::roles::{AgentRole, Artifact, StoredArtifact};

pub struct PlanStore {
    store: Arc<Store>,
}

impl std::fmt::Debug for PlanStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PlanStore")
    }
}

impl PlanStore {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    // --- plan revisions -------------------------------------------------

    /// Persist one accepted revision. Idempotent on `(task_id, revision)`.
    pub async fn save_revision(&self, task_id: TaskId, rev: &PlanRevision) -> Result<()> {
        let task = task_id.to_string();
        let revision = rev.revision as i64;
        let parent = rev.parent_hash.clone();
        let rationale = rev.rationale.clone();
        let plan_json = serde_json::to_string(&rev.plan)?;
        let created = rev.created_at.as_millis() as i64;
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO plan_revision
                     (task_id, revision, parent_hash, rationale, plan_json, created_at_ms)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![task, revision, parent, rationale, plan_json, created],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// The highest-numbered revision for a task, or `None` if it has no
    /// plan — the flag the driver uses to decide "plan-driven or not".
    pub async fn latest_revision(&self, task_id: TaskId) -> Result<Option<PlanRevision>> {
        let task = task_id.to_string();
        let row: Option<(i64, Option<String>, String, String, i64)> = self
            .store
            .call(move |conn| {
                conn.query_row(
                    "SELECT revision, parent_hash, rationale, plan_json, created_at_ms
                     FROM plan_revision WHERE task_id = ?1
                     ORDER BY revision DESC LIMIT 1",
                    params![task],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(valyria_store::StoreError::from(other)),
                })
            })
            .await?;
        row.map(row_to_revision).transpose()
    }

    pub async fn all_revisions(&self, task_id: TaskId) -> Result<Vec<PlanRevision>> {
        let task = task_id.to_string();
        let rows: Vec<(i64, Option<String>, String, String, i64)> = self
            .store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT revision, parent_hash, rationale, plan_json, created_at_ms
                     FROM plan_revision WHERE task_id = ?1 ORDER BY revision",
                )?;
                let out = stmt
                    .query_map(params![task], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(out)
            })
            .await?;
        rows.into_iter().map(row_to_revision).collect()
    }

    // --- checkpoints --------------------------------------------------

    pub async fn save_checkpoint(&self, cp: &Checkpoint) -> Result<()> {
        let id = cp.id.to_string();
        let task = cp.task_id.to_string();
        let step = cp.step_id.to_string();
        let files_json = serde_json::to_string(&cp.files)?;
        let watermark = cp.ledger_watermark as i64;
        let created = cp.created_at.as_millis() as i64;
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO plan_checkpoint
                     (id, task_id, step_id, files_json, ledger_watermark, created_at_ms)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![id, task, step, files_json, watermark, created],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn checkpoint(&self, id: CheckpointId) -> Result<Option<Checkpoint>> {
        let id_s = id.to_string();
        let row: Option<(String, String, String, String, i64, i64)> = self
            .store
            .call(move |conn| {
                conn.query_row(
                    "SELECT id, task_id, step_id, files_json, ledger_watermark, created_at_ms
                     FROM plan_checkpoint WHERE id = ?1",
                    params![id_s],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(valyria_store::StoreError::from(other)),
                })
            })
            .await?;
        row.map(row_to_checkpoint).transpose()
    }

    pub async fn checkpoints_for_task(&self, task_id: TaskId) -> Result<Vec<Checkpoint>> {
        let task = task_id.to_string();
        let rows: Vec<(String, String, String, String, i64, i64)> = self
            .store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, task_id, step_id, files_json, ledger_watermark, created_at_ms
                     FROM plan_checkpoint WHERE task_id = ?1 ORDER BY created_at_ms",
                )?;
                let out = stmt
                    .query_map(params![task], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(out)
            })
            .await?;
        rows.into_iter().map(row_to_checkpoint).collect()
    }

    // --- artifacts --------------------------------------------------

    pub async fn save_artifact(&self, art: &StoredArtifact) -> Result<()> {
        let id = valyria_types::EffectId::new().to_string();
        let task = art.task_id.to_string();
        let kind = art.artifact.kind().as_str().to_string();
        let role = art.produced_by.as_str().to_string();
        let json = serde_json::to_string(&art.artifact)?;
        let created = art.created_at.as_millis() as i64;
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO task_artifact
                     (id, task_id, kind, produced_by_role, artifact_json, created_at_ms)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![id, task, kind, role, json, created],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn artifacts_for_task(&self, task_id: TaskId) -> Result<Vec<StoredArtifact>> {
        let task = task_id.to_string();
        let rows: Vec<(String, String, String, i64)> = self
            .store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT produced_by_role, kind, artifact_json, created_at_ms
                     FROM task_artifact WHERE task_id = ?1 ORDER BY created_at_ms",
                )?;
                let out = stmt
                    .query_map(params![task], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(out)
            })
            .await?;
        let mut artifacts = Vec::new();
        for (role, _kind, json, created) in rows {
            let artifact: Artifact = serde_json::from_str(&json)?;
            let produced_by = parse_role(&role);
            artifacts.push(StoredArtifact {
                task_id,
                produced_by,
                artifact,
                created_at: Timestamp::from_millis(created as u128),
            });
        }
        Ok(artifacts)
    }

    /// Backs `valyria clean` scoped to a task.
    pub async fn purge_task(&self, task_id: TaskId) -> Result<u64> {
        let task = task_id.to_string();
        let n = self
            .store
            .call(move |conn| {
                let a = conn.execute(
                    "DELETE FROM plan_revision WHERE task_id = ?1",
                    params![task],
                )?;
                let b = conn.execute(
                    "DELETE FROM plan_checkpoint WHERE task_id = ?1",
                    params![task],
                )?;
                let c = conn.execute(
                    "DELETE FROM task_artifact WHERE task_id = ?1",
                    params![task],
                )?;
                Ok((a + b + c) as u64)
            })
            .await?;
        Ok(n)
    }
}

fn row_to_revision(row: (i64, Option<String>, String, String, i64)) -> Result<PlanRevision> {
    let (revision, parent_hash, rationale, plan_json, created_at_ms) = row;
    let plan: Plan = serde_json::from_str(&plan_json)?;
    Ok(PlanRevision {
        revision: revision as u32,
        parent_hash,
        rationale,
        plan,
        created_at: Timestamp::from_millis(created_at_ms as u128),
    })
}

fn row_to_checkpoint(row: (String, String, String, String, i64, i64)) -> Result<Checkpoint> {
    let (id, task_id, step_id, files_json, watermark, created) = row;
    let files: BTreeMap<PathBuf, Option<String>> = serde_json::from_str(&files_json)?;
    Ok(Checkpoint {
        id: id.parse().unwrap_or_else(|_| CheckpointId::new()),
        task_id: task_id.parse().unwrap_or_else(|_| TaskId::new()),
        step_id: PlanStepId::new(step_id).unwrap_or_else(|_| PlanStepId::new("unknown").unwrap()),
        files,
        ledger_watermark: watermark as usize,
        created_at: Timestamp::from_millis(created as u128),
    })
}

fn parse_role(s: &str) -> AgentRole {
    match s {
        "researcher" => AgentRole::Researcher,
        "planner" => AgentRole::Planner,
        "tester" => AgentRole::Tester,
        "reviewer" => AgentRole::Reviewer,
        _ => AgentRole::Implementer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EstimatedScope, PlanStep, VerificationRequirement};
    use std::collections::BTreeMap;

    async fn store() -> Arc<Store> {
        Arc::new(Store::open_in_memory(crate::migrations::MIGRATIONS).unwrap())
    }

    fn sample_plan() -> Plan {
        Plan {
            plan_scope: vec!["src/".into()],
            steps: vec![PlanStep {
                id: PlanStepId::new("s1").unwrap(),
                intent: "do the thing".into(),
                targets: vec!["src/lib.rs".into()],
                depends_on: vec![],
                parallelizable: false,
                checkpoint: true,
                verification: VerificationRequirement::Inherit,
                rollback_boundary: true,
                approval_required: false,
                estimated_scope: EstimatedScope::default(),
            }],
        }
    }

    #[tokio::test]
    async fn revision_round_trip_and_latest() {
        let ps = PlanStore::new(store().await);
        let task = TaskId::new();
        let r1 = PlanRevision::first(sample_plan(), "initial", Timestamp::from_millis(1));
        ps.save_revision(task, &r1).await.unwrap();
        let mut p2 = sample_plan();
        p2.steps[0].intent = "revised".into();
        let r2 = r1.revise(p2, "tweak", Timestamp::from_millis(2));
        ps.save_revision(task, &r2).await.unwrap();

        let latest = ps.latest_revision(task).await.unwrap().unwrap();
        assert_eq!(latest.revision, 2);
        assert_eq!(latest.plan.steps[0].intent, "revised");
        assert_eq!(ps.all_revisions(task).await.unwrap().len(), 2);
        assert!(ps.latest_revision(TaskId::new()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn checkpoint_round_trip() {
        let ps = PlanStore::new(store().await);
        let task = TaskId::new();
        let mut files = BTreeMap::new();
        files.insert(PathBuf::from("a.txt"), Some("de".repeat(32)));
        files.insert(PathBuf::from("gone.txt"), None);
        let cp = Checkpoint {
            id: CheckpointId::new(),
            task_id: task,
            step_id: PlanStepId::new("s1").unwrap(),
            files,
            ledger_watermark: 3,
            created_at: Timestamp::from_millis(9),
        };
        ps.save_checkpoint(&cp).await.unwrap();
        let back = ps.checkpoint(cp.id).await.unwrap().unwrap();
        assert_eq!(back, cp);
        assert_eq!(ps.checkpoints_for_task(task).await.unwrap(), vec![cp]);
    }

    #[tokio::test]
    async fn artifact_round_trip() {
        let ps = PlanStore::new(store().await);
        let task = TaskId::new();
        let art = StoredArtifact {
            task_id: task,
            produced_by: AgentRole::Tester,
            artifact: Artifact::VerificationReport {
                passed: true,
                commands_run: vec!["cargo test".into()],
                failures: vec![],
            },
            created_at: Timestamp::from_millis(4),
        };
        ps.save_artifact(&art).await.unwrap();
        let back = ps.artifacts_for_task(task).await.unwrap();
        assert_eq!(back, vec![art]);
    }

    #[tokio::test]
    async fn purge_removes_everything_for_a_task() {
        let ps = PlanStore::new(store().await);
        let task = TaskId::new();
        ps.save_revision(
            task,
            &PlanRevision::first(sample_plan(), "x", Timestamp::from_millis(1)),
        )
        .await
        .unwrap();
        assert!(ps.purge_task(task).await.unwrap() >= 1);
        assert!(ps.latest_revision(task).await.unwrap().is_none());
    }
}
