//! The benchmark task model: `{ repo, objective, setup, oracle }` from
//! §4.30, plus the fake-model [`Scenario`] that stands in for "what the
//! agent decides to do" so the run is deterministic and offline.

use std::path::Path;

use serde::{Deserialize, Serialize};
use valyria_runtime_fake::Scenario;

use crate::error::{BenchError, Result};
use crate::oracle::Oracle;

/// The task categories §4.30 calls for. Reporting groups pass-rate by
/// this so a regression in, say, refactoring is visible even if the
/// overall number holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskCategory {
    BugFix,
    Feature,
    Refactor,
    TestCreation,
    DependencyWork,
    Debugging,
    Exploration,
}

impl TaskCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskCategory::BugFix => "bug_fix",
            TaskCategory::Feature => "feature",
            TaskCategory::Refactor => "refactor",
            TaskCategory::TestCreation => "test_creation",
            TaskCategory::DependencyWork => "dependency_work",
            TaskCategory::Debugging => "debugging",
            TaskCategory::Exploration => "exploration",
        }
    }
}

/// The repository a task runs against: a set of files laid down into a
/// fresh temp directory. A pinned-commit real repo would be another
/// `RepoSpec` variant; the offline CI suite only needs fixtures.
#[derive(Debug, Clone, Default)]
pub struct RepoSpec {
    files: Vec<(String, String)>,
}

impl RepoSpec {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a file (relative path, contents). Chainable.
    pub fn file(mut self, rel: impl Into<String>, contents: impl Into<String>) -> Self {
        self.files.push((rel.into(), contents.into()));
        self
    }

    pub fn files(&self) -> &[(String, String)] {
        &self.files
    }

    /// Write every file into `root`, creating parent directories. Also
    /// marks any `*.sh` file executable so a discovered `verify.sh`
    /// convention command actually launches.
    pub fn materialize(&self, root: &Path) -> Result<()> {
        for (rel, contents) in &self.files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| BenchError::Io {
                    path: parent.display().to_string(),
                    source,
                })?;
            }
            std::fs::write(&path, contents).map_err(|source| BenchError::Io {
                path: path.display().to_string(),
                source,
            })?;
            #[cfg(unix)]
            if rel.ends_with(".sh") {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&path)
                    .map_err(|source| BenchError::Io {
                        path: path.display().to_string(),
                        source,
                    })?
                    .permissions();
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }
        Ok(())
    }
}

/// One benchmark task. `scenario` is the fake model's script; `oracle`
/// is the executable pass/fail check run against the workspace after the
/// task reaches a terminal state.
pub struct BenchTask {
    pub id: String,
    pub category: TaskCategory,
    pub objective: String,
    pub repo: RepoSpec,
    pub scenario: Scenario,
    pub oracle: Box<dyn Oracle>,
    /// Run `Planning` as a model-authored, validated plan (Phase 8)
    /// rather than the default pass-through.
    pub model_authored_plan: bool,
}

impl BenchTask {
    pub fn new(
        id: impl Into<String>,
        category: TaskCategory,
        objective: impl Into<String>,
        repo: RepoSpec,
        scenario: Scenario,
        oracle: impl Oracle + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            category,
            objective: objective.into(),
            repo,
            scenario,
            oracle: Box::new(oracle),
            model_authored_plan: false,
        }
    }
}

impl std::fmt::Debug for BenchTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BenchTask")
            .field("id", &self.id)
            .field("category", &self.category)
            .field("objective", &self.objective)
            .field("oracle", &self.oracle.name())
            .finish()
    }
}
