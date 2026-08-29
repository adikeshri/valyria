//! The runner: materialize the repo, open a real [`Runtime`] bound to the
//! task's fake-model scenario, drive the task to a terminal state, then
//! grade it with the oracle and project the journal into [`BenchMetrics`].
//!
//! Every run is fully hermetic — a throwaway workspace *and* a throwaway
//! `~/.valyria` (`with_data_dir` redirects the global dir too) — so the
//! suite is safe to run anywhere, including offline CI.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use valyria_app::{Runtime, RuntimeConfig};
use valyria_events::Seq;
use valyria_types::PermissionMode;

use crate::error::{BenchError, Result};
use crate::metrics::BenchMetrics;
use crate::oracle::{OracleContext, OracleVerdict};
use crate::report::BenchReport;
use crate::task::{BenchTask, TaskCategory};

/// The outcome of one graded benchmark run. Serializable so a whole
/// [`BenchReport`] is a diffable artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchOutcome {
    pub id: String,
    pub category: TaskCategory,
    pub objective: String,
    pub final_state: String,
    pub report_status: String,
    pub oracle: String,
    pub oracle_detail: String,
    /// The single bottom line: did the executable oracle pass?
    pub passed: bool,
    pub metrics: BenchMetrics,
}

pub struct BenchRunner {
    timeout: Duration,
}

impl Default for BenchRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

impl BenchRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run and grade one task.
    pub async fn run(&self, task: &BenchTask) -> Result<BenchOutcome> {
        let workspace = tempfile::tempdir().map_err(|source| BenchError::Io {
            path: "tempdir".into(),
            source,
        })?;
        let data_dir = tempfile::tempdir().map_err(|source| BenchError::Io {
            path: "tempdir".into(),
            source,
        })?;
        task.repo.materialize(workspace.path())?;

        let before = hash_tree(workspace.path());

        let mut config = RuntimeConfig::new(workspace.path())
            .with_data_dir(data_dir.path().join("data"))
            .with_permission_mode(PermissionMode::Assisted)
            .with_scenario(task.scenario.clone());
        if task.model_authored_plan {
            config = config.with_planning_mode(valyria_app::PlanningMode::ModelAuthored);
        }

        let runtime = Runtime::open(config)
            .await
            .map_err(|e| BenchError::runtime(&task.id, e))?;

        let started = Instant::now();
        let task_id = runtime
            .create_and_start_task(task.objective.clone())
            .await
            .map_err(|e| BenchError::runtime(&task.id, e))?;

        let (final_state, reached_terminal) = loop {
            let status = runtime
                .task_status(task_id)
                .await
                .map_err(|e| BenchError::runtime(&task.id, e))?;
            if status.state.is_terminal() {
                break (status.state, true);
            }
            if started.elapsed() > self.timeout {
                break (status.state, false);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let wall_ms = started.elapsed().as_millis() as u64;

        let report = runtime
            .completion_report(task_id)
            .await
            .map_err(|e| BenchError::runtime(&task.id, e))?;

        let events = runtime
            .events()
            .replay_since(Seq(0))
            .await
            .map_err(|e| BenchError::Events {
                task: task.id.clone(),
                detail: e.to_string(),
            })?
            .into_iter()
            .filter(|e| e.task_id == Some(task_id))
            .collect::<Vec<_>>();

        let after = hash_tree(workspace.path());
        let files_changed = diff_trees(&before, &after);
        let mut metrics = BenchMetrics::from_events(&events, wall_ms, reached_terminal);
        // The journal's `FileChanged` projection isn't task-scoped in
        // every path; the on-disk diff is the authoritative count.
        metrics.files_changed = files_changed.len() as u32;

        let verdict: OracleVerdict = task.oracle.check(&OracleContext {
            workspace: workspace.path(),
            report: &report,
            final_state,
            files_changed: &files_changed,
        });

        Ok(BenchOutcome {
            id: task.id.clone(),
            category: task.category,
            objective: task.objective.clone(),
            final_state: final_state.to_string(),
            report_status: format!("{:?}", report.status),
            oracle: task.oracle.name(),
            oracle_detail: verdict.detail,
            passed: verdict.passed,
            metrics,
        })
    }

    /// Run every task and collect a [`BenchReport`]. Tasks run
    /// sequentially — each stands up its own tokio-driven runtime, and
    /// serial execution keeps the numbers stable.
    pub async fn run_suite(&self, tasks: &[BenchTask]) -> Result<BenchReport> {
        let mut runs = Vec::with_capacity(tasks.len());
        for task in tasks {
            runs.push(self.run(task).await?);
        }
        Ok(BenchReport::new(runs))
    }
}

/// blake3 of every regular file under `root`, keyed by workspace-relative
/// path, skipping the runtime's own `.valyria` data dir and any `.git`.
fn hash_tree(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".valyria" && name != ".git"
        })
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        if let Ok(bytes) = std::fs::read(entry.path()) {
            out.insert(
                rel.to_string_lossy().replace('\\', "/"),
                blake3::hash(&bytes).to_hex().to_string(),
            );
        }
    }
    out
}

fn diff_trees(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<String> {
    let mut changed = Vec::new();
    for (path, hash) in after {
        if before.get(path) != Some(hash) {
            changed.push(path.clone());
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            changed.push(path.clone());
        }
    }
    changed.sort();
    changed.dedup();
    changed
}
