//! `Runtime`: wires every already-built subsystem into one embedded agent
//! runtime for a single workspace (§4.1, §4.23). This is the composition
//! root — the one place in the whole workspace allowed to know about every
//! layer at once, so that `valyria-cli` never has to.

use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::OptionalExtension;
use valyria_agent::{AgentDriver, PlanningMode};
use valyria_context::ContextAssembler;
use valyria_events::{EventBus, EventKind, NewEvent};
use valyria_index::IndexStore;
use valyria_ledger::Ledger;
use valyria_memory::{MemoryStore, RetrievalRequest};
use valyria_model_registry::{score_card_for_role, CardScore, Catalog, ModelCard, ModelRole};
use valyria_model_store::{HttpFetcher, ModelStore, NullProber};
use valyria_orchestrator::{Orchestrator, Role};
use valyria_permissions::PermissionEngine;
use valyria_plan::{PlanRevision, PlanStore, RollbackError, RollbackReport};
use valyria_runtime_fake::{FakeModelRuntime, Scenario};
use valyria_sandbox::{detect_platform_launcher, ProcessLauncher, SandboxProfile};
use valyria_store::Store;
use valyria_task::{Budget, Task, TaskManager};
use valyria_tools::ToolRuntime;
use valyria_types::{AgentState, CheckpointId, ErrorCode, PermissionMode, TaskId, WorkspaceId};
use valyria_util::{CancellationToken, Clock, ContentHash, SystemClock};
use valyria_verify::{CompletionReport, VerificationLog};
use valyria_vfs::WorkspaceRoot;

use crate::doctor::{Doctor, DoctorReport};
use crate::error::{AppError, Result};
use crate::global::GlobalStore;
use crate::migrations::workspace_migrations;
use crate::storage::{PurgeOutcome, PurgeScope, StorageInspector, StorageReport};

/// Which Core-owned config file a [`Runtime::config_set`] write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigWriteScope {
    /// `<repo>/.valyria/config.toml`.
    Workspace,
    /// `~/.valyria/config.toml`.
    User,
}

impl ConfigWriteScope {
    /// Parse the wire string (`"workspace"` | `"user"`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "workspace" => Some(Self::Workspace),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// Working-tree status as [`Runtime::git_status`] returns it.
#[derive(Debug, Clone)]
pub struct GitStatusView {
    pub branch: Option<String>,
    pub detached: bool,
    pub head_commit: Option<String>,
    pub files: Vec<valyria_git::FileStatus>,
}

/// One agent-touched file and how it is currently classified, as
/// [`Runtime::ledger_changes`] returns it (§15, §16, G8).
#[derive(Debug, Clone)]
pub struct LedgerChangeView {
    pub path: String,
    /// `agent_authored` | `pre_existing` | `concurrent_user_modification`
    /// | `unknown`.
    pub classification: &'static str,
    /// `write` | `delete` — the agent's most recent action on the path.
    pub kind: &'static str,
    pub task_id: String,
    pub step_id: String,
    pub tool_invocation_id: Option<String>,
}

/// Model detail as [`Runtime::model_inspect`] returns it.
#[derive(Debug, Clone)]
pub struct ModelInspectView {
    pub card: ModelCard,
    pub installed: bool,
    pub installed_at_ms: Option<i64>,
    pub probe_tokens_per_sec: Option<f64>,
    pub active_roles: Vec<String>,
}

/// Loads a scenario TOML file into a `Scenario` `RuntimeConfig` can be
/// built with, without the caller needing to depend on
/// `valyria-runtime-fake` directly — kept here specifically so
/// `valyria-cli` (which per D11 may depend only on this crate and
/// `valyria-protocol`) can offer a `--scenario <file>` flag while its own
/// `Cargo.toml` never lists an agent-internals crate.
pub fn load_scenario(path: &std::path::Path) -> Result<Scenario> {
    Ok(Scenario::load_toml(path)?)
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub workspace_path: PathBuf,
    /// Defaults to `<workspace_path>/.valyria` (§4.1's per-workspace
    /// layout) if left as `None` by `RuntimeConfig::new`.
    pub data_dir: PathBuf,
    pub permission_mode: PermissionMode,
    pub scenario: Scenario,
    /// Whether `Planning` asks the model for a plan (Phase 8) or is the
    /// Phase 3 pass-through. Defaults to pass-through.
    pub planning_mode: PlanningMode,
    /// `~/.valyria` (or `$VALYRIA_HOME`) — home of `global.db`, the model
    /// store, and logs (§4.1). Defaults to [`GlobalStore::default_root`];
    /// tests point it at a tempdir.
    pub global_dir: PathBuf,
}

impl RuntimeConfig {
    pub fn new(workspace_path: impl Into<PathBuf>) -> Self {
        let workspace_path = workspace_path.into();
        let data_dir = workspace_path.join(".valyria");
        Self {
            workspace_path,
            data_dir,
            permission_mode: PermissionMode::default(),
            scenario: Scenario::default_walking_skeleton(),
            planning_mode: PlanningMode::default(),
            global_dir: GlobalStore::default_root(),
        }
    }

    pub fn with_planning_mode(mut self, mode: PlanningMode) -> Self {
        self.planning_mode = mode;
        self
    }

    pub fn with_global_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.global_dir = dir.into();
        self
    }

    /// Redirect the per-workspace data directory. Also redirects
    /// `global_dir` to `<data_dir>/global` — tests and sandboxed runs
    /// override `data_dir` precisely so they touch nothing outside their
    /// tempdir, and a shared real `~/.valyria/global.db` would defeat
    /// that. Call [`Self::with_global_dir`] afterward to point at a real
    /// global store anyway.
    pub fn with_data_dir(mut self, data_dir: impl Into<PathBuf>) -> Self {
        self.data_dir = data_dir.into();
        self.global_dir = self.data_dir.join("global");
        self
    }

    pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }

    pub fn with_scenario(mut self, scenario: Scenario) -> Self {
        self.scenario = scenario;
        self
    }
}

pub struct Runtime {
    events: Arc<EventBus>,
    tasks: Arc<TaskManager>,
    driver: Arc<AgentDriver>,
    plan_store: Arc<PlanStore>,
    ledger: Arc<Ledger>,
    workspace_id: WorkspaceId,
    workspace_path: PathBuf,
    data_dir: PathBuf,
    store: Arc<Store>,
    index: Arc<IndexStore>,
    verification_log: Arc<VerificationLog>,
    memory: Arc<MemoryStore>,
    global: Arc<GlobalStore>,
    engine: Arc<PermissionEngine>,
    permission_mode: PermissionMode,
}

impl Runtime {
    /// Opens (creating if absent) the workspace's `.valyria` data
    /// directory and applies every crate's migrations to one shared
    /// `workspace.db`.
    ///
    /// Deliberately does **not** run workspace-wide crash recovery here,
    /// even though §4.23 describes recovery as a startup step — in Phase
    /// 3's embedded, no-daemon model, *every* CLI invocation calls
    /// `open()`, including ones with no intent to drive anything (`task
    /// status`, `task pause`) and ones actively driving a *different*
    /// task in the same workspace. There is no per-process liveness
    /// tracking to tell "this task's driver crashed" apart from "this
    /// task's driver is alive right now, in another process" — a blanket
    /// scan would force-pause a task another live process is mid-step on
    /// out from under it. Recovery instead happens narrowly, only inside
    /// `resume_task`, scoped to the one task id being resumed — see
    /// `valyria_task::TaskManager::recover_task_if_active`'s docs for the
    /// full reasoning.
    pub async fn open(config: RuntimeConfig) -> Result<Self> {
        let workspace_root = WorkspaceRoot::new(&config.workspace_path).map_err(AppError::Vfs)?;
        std::fs::create_dir_all(&config.data_dir).map_err(|e| {
            AppError::Vfs(valyria_vfs::VfsError::Io {
                path: config.data_dir.display().to_string(),
                source: e,
            })
        })?;

        let store = Arc::new(Store::open(
            &config.data_dir.join("workspace.db"),
            &workspace_migrations(),
        )?);
        let events = Arc::new(EventBus::new(store.clone()));
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);

        let workspace_id = load_or_create_workspace_id(&store).await?;

        let ledger = Arc::new(Ledger::new(config.data_dir.join("blobs"))?);
        let engine = Arc::new(PermissionEngine::new(config.permission_mode, clock.clone()));
        let tool_runtime = Arc::new(ToolRuntime::new(
            valyria_tools::all_tools(),
            engine.clone(),
            clock.clone(),
        ));
        let engine_handle = engine.clone();

        let mut orchestrator = Orchestrator::new();
        orchestrator.bind(
            Role::PrimaryCoder,
            Arc::new(FakeModelRuntime::from_scenario(config.scenario)),
        );
        let orchestrator = Arc::new(orchestrator);

        let context = Arc::new(ContextAssembler::new(tool_runtime.clone()));
        let verification_log = Arc::new(VerificationLog::new(store.clone()));
        let index = Arc::new(IndexStore::new(store.clone()));
        let memory = Arc::new(MemoryStore::new(store.clone()));
        let plan_store = Arc::new(PlanStore::new(store.clone()));

        let global = Arc::new(GlobalStore::open(&config.global_dir).await?);
        global
            .register_workspace(
                workspace_id,
                &config.workspace_path,
                clock.now().as_millis() as i64,
            )
            .await?;
        let hash_cache = Arc::new(valyria_vfs::HashCache::new());
        let launcher: Arc<dyn ProcessLauncher> = Arc::from(detect_platform_launcher());
        let sandbox_profile = SandboxProfile::new().allow_write(workspace_root.as_path());

        let tasks = Arc::new(TaskManager::new(
            store.clone(),
            events.clone(),
            clock.clone(),
        ));

        let driver = Arc::new(
            AgentDriver::new(
                tasks.clone(),
                tool_runtime,
                orchestrator,
                context,
                ledger.clone(),
                engine,
                verification_log.clone(),
                plan_store.clone(),
                workspace_root,
                hash_cache,
                clock,
                launcher,
                sandbox_profile,
            )
            .with_planning_mode(config.planning_mode),
        );

        Ok(Self {
            events,
            tasks,
            driver,
            plan_store,
            ledger,
            workspace_id,
            workspace_path: config.workspace_path.clone(),
            data_dir: config.data_dir.clone(),
            store,
            index,
            verification_log,
            memory,
            global,
            engine: engine_handle,
            permission_mode: config.permission_mode,
        })
    }

    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    pub fn events(&self) -> Arc<EventBus> {
        self.events.clone()
    }

    pub async fn create_and_start_task(&self, objective: String) -> Result<TaskId> {
        self.create_and_start_task_with_mode(objective, None).await
    }

    /// Create a task, optionally pinned to a per-task autonomy mode (§25,
    /// G1). With `None` the task runs at the daemon's start-time mode,
    /// exactly as before. The override is dropped when the task terminates
    /// (see [`Self::spawn_driver`]).
    pub async fn create_and_start_task_with_mode(
        &self,
        objective: String,
        permission_mode: Option<PermissionMode>,
    ) -> Result<TaskId> {
        let task = self
            .tasks
            .create(self.workspace_id, objective, Budget::default())
            .await?;
        if let Some(mode) = permission_mode {
            self.engine.set_task_mode(task.id, mode);
        }
        self.spawn_driver(task.id);
        Ok(task.id)
    }

    /// Resumes a task: first, narrowly recovers *this one task* if it
    /// looks like a crash left it mid-step (see
    /// `valyria_task::TaskManager::recover_task_if_active`'s docs for why
    /// this is scoped to the single task being resumed, not a
    /// workspace-wide scan) — this is what makes `valyria task resume
    /// <id>` correct after a real `kill -9` of whatever process was
    /// driving it, without that recovery ever able to disturb an
    /// unrelated task a *different*, still-alive process is mid-step on.
    /// Then transitions it back to the exact state it was paused from
    /// (state-based, no special-casing — legal for any `Paused` task
    /// regardless of *why* it was paused) and spawns a fresh driver loop
    /// to continue it.
    pub async fn resume_task(&self, task_id: TaskId) -> Result<()> {
        self.tasks.recover_task_if_active(task_id).await?;

        let task = self.tasks.get(task_id).await?;
        if task.state == AgentState::Paused {
            let target = task.paused_from.ok_or(AppError::NotPaused(task_id))?;
            self.tasks.transition(task_id, target).await?;
        }
        if !task.state.is_terminal() {
            self.spawn_driver(task_id);
        }
        Ok(())
    }

    /// Durable, cross-process pause: writes the request to the task's row
    /// so it's observed by whichever process (this one or another) is
    /// actually driving it — see `Task::pending_signal`'s docs.
    pub async fn pause_task(&self, task_id: TaskId) -> Result<()> {
        Ok(self.tasks.request_pause(task_id).await?)
    }

    pub async fn cancel_task(&self, task_id: TaskId) -> Result<()> {
        Ok(self.tasks.request_cancel(task_id).await?)
    }

    /// Resolves an outstanding `WAITING_FOR_PERMISSION` decision and, if
    /// that leaves the task in a live (non-terminal, non-waiting) state,
    /// spawns a fresh driver to keep it running — `AgentDriver::
    /// resolve_permission` only performs the one resolution step, it does
    /// not itself loop.
    pub async fn resolve_permission(&self, task_id: TaskId, approve: bool) -> Result<()> {
        let decision = if approve {
            valyria_agent::ApprovalDecision::Once
        } else {
            valyria_agent::ApprovalDecision::Deny
        };
        self.resolve_permission_scoped(task_id, None, decision)
            .await
    }

    /// [`Self::resolve_permission`] with an optional `request_id` to assert
    /// against the current pending request (returns `approval.superseded`
    /// on a mismatch) and a `decision` of once / task / deny (§13, G2).
    pub async fn resolve_permission_scoped(
        &self,
        task_id: TaskId,
        request_id: Option<String>,
        decision: valyria_agent::ApprovalDecision,
    ) -> Result<()> {
        self.driver
            .resolve_permission_scoped(task_id, request_id, decision)
            .await?;
        let task = self.tasks.get(task_id).await?;
        if !task.state.is_terminal()
            && task.state != AgentState::WaitingForPermission
            && task.state != AgentState::WaitingForUser
        {
            self.spawn_driver(task_id);
        }
        Ok(())
    }

    pub async fn task_status(&self, task_id: TaskId) -> Result<Task> {
        Ok(self.tasks.get(task_id).await?)
    }

    /// The latest accepted plan revision for a task, if it has one (Phase
    /// 8). `None` for a task the driver ran as a Phase-3 pass-through.
    pub async fn plan(&self, task_id: TaskId) -> Result<Option<PlanRevision>> {
        self.plan_store
            .latest_revision(task_id)
            .await
            .map_err(|e| AppError::Plan(e.to_string()))
    }

    /// `(plan_step_id, checkpoint_id)` for every checkpoint recorded for a
    /// task — the ids `task_rollback` expects (§16, G13).
    pub async fn plan_checkpoints(&self, task_id: TaskId) -> Result<Vec<(String, String)>> {
        Ok(self
            .plan_store
            .checkpoints_for_task(task_id)
            .await
            .map_err(|e| AppError::Plan(e.to_string()))?
            .into_iter()
            .map(|c| (c.step_id.to_string(), c.id.to_string()))
            .collect())
    }

    /// Roll a task's workspace back to a checkpoint taken at a plan step
    /// boundary. Restores the checkpointed files exactly; refuses on any
    /// file touched since (§4.25).
    pub async fn rollback_to_checkpoint(
        &self,
        task_id: TaskId,
        checkpoint_id: CheckpointId,
    ) -> std::result::Result<RollbackReport, RollbackError> {
        self.driver
            .rollback_to_checkpoint(task_id, checkpoint_id)
            .await
    }

    // --- Phase 10: read-only introspection surface -----------------------

    pub fn workspace_path(&self) -> &std::path::Path {
        &self.workspace_path
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    pub fn global(&self) -> &Arc<GlobalStore> {
        &self.global
    }

    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_mode
    }

    pub async fn list_tasks(&self) -> Result<Vec<Task>> {
        Ok(self.tasks.list(self.workspace_id).await?)
    }

    /// The completion report (§15, D4) — assembled *only* from persisted
    /// verification runs, so an unbacked "tests pass" never appears as a
    /// verified fact.
    pub async fn completion_report(&self, task_id: TaskId) -> Result<CompletionReport> {
        let runs = self.verification_log.list_for_task(task_id).await?;
        Ok(CompletionReport::from_runs(task_id, &runs, &[]))
    }

    pub async fn current_index_generation(&self) -> Result<Option<u64>> {
        Ok(self
            .index
            .current()
            .await
            .ok()
            .flatten()
            .map(|g| g.generation.0))
    }

    pub async fn doctor(&self) -> DoctorReport {
        Doctor {
            data_dir: self.data_dir.clone(),
            workspace_path: self.workspace_path.clone(),
            global_root: self.global.root().to_path_buf(),
            index: self.index.clone(),
            models: self.global.models().clone(),
        }
        .run()
        .await
    }

    fn storage_inspector(&self) -> StorageInspector {
        StorageInspector::new(
            self.data_dir.clone(),
            self.global.root().to_path_buf(),
            self.memory.clone(),
            self.global.user_memory().clone(),
        )
    }

    pub fn storage_inspect(&self) -> StorageReport {
        self.storage_inspector().inspect()
    }

    pub async fn storage_purge(&self, scope: PurgeScope, dry_run: bool) -> Result<PurgeOutcome> {
        self.storage_inspector().purge(scope, dry_run).await
    }

    /// `(key, value, origin)` for every effective config leaf `valyria
    /// config` shows (§4.3). Every key here is round-trippable through
    /// [`Self::config_set`] (G6) — the `network` policy is reported as its
    /// five individual leaves rather than one debug blob so a write and a
    /// re-read line up.
    pub fn config_show(&self) -> Result<Vec<(String, String, String)>> {
        let resolved = valyria_config::ConfigResolver::new()
            .global_path(self.global.root().join("config.toml"))
            .workspace_path(self.data_dir.join("config.toml"))
            .env_vars(std::env::vars().collect())
            .resolve()?;

        let origin = |key: &str| {
            resolved
                .origin_of(key)
                .map(|o| format!("{o:?}").to_lowercase())
                .unwrap_or_else(|| "default".to_string())
        };
        let net = &resolved.settings.network;
        let access = |a: valyria_types::Access| format!("{a:?}").to_lowercase();
        Ok(vec![
            (
                "permission.mode".to_string(),
                format!("{:?}", resolved.settings.permission.mode).to_lowercase(),
                origin("permission.mode"),
            ),
            (
                "log.format".to_string(),
                format!("{:?}", resolved.settings.log.format).to_lowercase(),
                origin("log.format"),
            ),
            (
                "network.repository".to_string(),
                access(net.repository),
                origin("network.repository"),
            ),
            (
                "network.workspace_filesystem".to_string(),
                access(net.workspace_filesystem),
                origin("network.workspace_filesystem"),
            ),
            (
                "network.local_commands".to_string(),
                access(net.local_commands),
                origin("network.local_commands"),
            ),
            (
                "network.internet".to_string(),
                access(net.internet),
                origin("network.internet"),
            ),
            (
                "network.credentials".to_string(),
                access(net.credentials),
                origin("network.credentials"),
            ),
        ])
    }

    /// Write one config leaf to a Core-owned file, then return the
    /// re-resolved [`Self::config_show`] view (§24, G6). `scope` is
    /// `workspace` (→ `<repo>/.valyria/config.toml`) or `user`
    /// (→ `~/.valyria/config.toml`). The write is policy-floor validated
    /// before it touches disk; on any error nothing is written.
    pub fn config_set(
        &self,
        key: &str,
        value: &str,
        scope: ConfigWriteScope,
    ) -> Result<Vec<(String, String, String)>> {
        let path = match scope {
            ConfigWriteScope::Workspace => self.data_dir.join("config.toml"),
            ConfigWriteScope::User => self.global.root().join("config.toml"),
        };
        valyria_config::write_key(&path, key, value).map_err(AppError::ConfigWrite)?;
        self.config_show()
    }

    // --- repository read surface (§7, §14, §17, §33; capability `repo`) ---

    /// Largest `git_diff` payload Core will return before truncating.
    const GIT_DIFF_CAP: usize = 512 * 1024;

    fn git_repo(&self) -> Result<valyria_git::Repo> {
        Ok(valyria_git::Repo::open(&self.workspace_path)?)
    }

    /// Working-tree status: branch/HEAD plus per-file staged/unstaged
    /// changes. Read-only — git *writes* stay Core-internal (§17).
    pub fn git_status(&self) -> Result<GitStatusView> {
        let repo = self.git_repo()?;
        let head = repo.head_info()?;
        Ok(GitStatusView {
            branch: head.branch,
            detached: head.detached,
            head_commit: head.commit,
            files: repo.status()?.files,
        })
    }

    /// Unified-diff text for the working tree. `staged == false` is
    /// worktree-vs-index; `staged == true` is index-vs-HEAD. `path`
    /// restricts to one repo-relative file.
    pub fn git_diff(&self, path: Option<&str>, staged: bool) -> Result<valyria_git::WorktreeDiff> {
        Ok(self
            .git_repo()?
            .worktree_diff(path, staged, Self::GIT_DIFF_CAP)?)
    }

    /// Newest-first commits from HEAD (at most `limit`, capped at 500).
    /// An unborn HEAD yields an empty list rather than an error.
    pub fn git_log(&self, limit: usize) -> Result<Vec<valyria_git::CommitInfo>> {
        match self.git_repo()?.log(limit.min(500)) {
            Ok(commits) => Ok(commits),
            Err(valyria_git::GitError::UnbornHead) => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn git_branches(&self) -> Result<Vec<valyria_git::BranchInfo>> {
        Ok(self.git_repo()?.branches()?)
    }

    /// Agent-touched files for `task_id`, each with the ledger's current
    /// classification (agent-authored / pre-existing / concurrent user
    /// modification) computed against the file's on-disk state now
    /// (§15, §16, G8). One row per path — the agent's most recent entry
    /// for it.
    pub fn ledger_changes(&self, task_id: TaskId) -> Vec<LedgerChangeView> {
        use std::collections::BTreeMap;
        use valyria_ledger::ChangeClassification;

        // Most recent ledger entry per path (entries are append-order).
        let mut latest: BTreeMap<std::path::PathBuf, valyria_ledger::LedgerEntry> = BTreeMap::new();
        for entry in self.ledger.entries_for_task(task_id) {
            latest.insert(entry.path.clone(), entry);
        }

        latest
            .into_values()
            .map(|entry| {
                let abs = self.workspace_path.join(&entry.path);
                let observed = std::fs::read(&abs).ok().map(|b| ContentHash::of_bytes(&b));
                let classification = match self.ledger.classify(&entry.path, observed) {
                    ChangeClassification::AgentAuthored => "agent_authored",
                    ChangeClassification::PreExisting => "pre_existing",
                    ChangeClassification::ConcurrentUserModification => {
                        "concurrent_user_modification"
                    }
                    ChangeClassification::Unknown => "unknown",
                };
                let kind = if entry.after_hash.is_none() {
                    "delete"
                } else {
                    "write"
                };
                LedgerChangeView {
                    path: entry.path.display().to_string(),
                    classification,
                    kind,
                    task_id: entry.task_id.to_string(),
                    step_id: entry.step_id.to_string(),
                    tool_invocation_id: entry.tool_invocation_id.map(|t| t.to_string()),
                }
            })
            .collect()
    }

    /// The newest published index generation, if any (§4.30).
    pub async fn index_status(&self) -> Result<Option<valyria_index::GenerationInfo>> {
        Ok(self.index.current().await?)
    }

    /// Index the whole workspace as one generation and build the
    /// import/call graph over it, so `search_query` and `index_status`
    /// have something to serve. Returns the new generation's info.
    ///
    /// Indexing is otherwise an internal, task-driven concern; this is the
    /// explicit entry point the desktop client's "build index" action and
    /// the first-run flow call.
    pub async fn reindex(&self) -> Result<valyria_index::GenerationInfo> {
        let registry = valyria_lang::LanguageRegistry::with_builtin_languages()
            .map_err(|e| AppError::Repo(format!("language registry: {e}")))?;
        let pipeline = valyria_index::IndexPipeline::new(
            self.workspace_path.clone(),
            registry,
            (*self.index).clone(),
        );
        let delta = pipeline.bootstrap_unstaged(&|_| {}).await?;
        valyria_graph::GraphStore::new(self.store.clone())
            .build_for(&self.index, delta.generation)
            .await
            .map_err(|e| AppError::Repo(format!("graph build: {e}")))?;
        self.index
            .current()
            .await?
            .ok_or_else(|| AppError::Repo("index generation vanished after publish".into()))
    }

    /// Run the fused code search (§4.16) and return the ranked, explained
    /// hits verbatim.
    ///
    /// `SearchEngine::search` returns a `!Send` future (it holds a `gix`
    /// handle across `.await`), so it cannot be awaited inside the `Send`
    /// `Client::call`. It runs on a dedicated current-thread runtime on a
    /// scoped OS thread; only the plain-data `SearchResults` crosses back.
    /// This mirrors `valyria_context::retrieve::SearchRetriever`.
    pub fn search(
        &self,
        query: &valyria_search::SearchQuery,
    ) -> Result<valyria_search::SearchResults> {
        let root = self.workspace_path.clone();
        let index = (*self.index).clone();
        let store = self.store.clone();

        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| AppError::Repo(format!("search runtime: {e}")))?;
                    let registry = valyria_lang::LanguageRegistry::with_builtin_languages()
                        .map_err(|e| AppError::Repo(format!("language registry: {e}")))?;
                    let engine = valyria_search::SearchEngine::new(
                        root,
                        index,
                        valyria_graph::GraphStore::new(store.clone()),
                        valyria_embed::EmbedStore::new(store),
                        std::sync::Arc::new(valyria_embed::HashingEmbedder::default()),
                        registry,
                    );
                    rt.block_on(engine.search(query)).map_err(AppError::Search)
                })
                .join()
                .map_err(|_| AppError::Repo("search thread panicked".into()))?
        })
    }

    /// Relevance-ranked memory entries for `query` (§4.19). With no query,
    /// returns nothing — a browse-all surface is a follow-up.
    pub async fn memory_list(
        &self,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<valyria_memory::MemoryEntry>> {
        let Some(query) = query else {
            return Ok(vec![]);
        };
        let now = SystemClock.now().as_millis() as i64;
        let retrieved = self
            .memory
            .retrieve(RetrievalRequest::new(query, now).limit(limit))
            .await?;
        Ok(retrieved
            .pinned
            .into_iter()
            .chain(retrieved.ranked.into_iter().map(|s| s.entry))
            .collect())
    }

    /// The catalog, each card tagged with whether its weights are
    /// installed in the global store (§4.21).
    pub async fn model_list(&self) -> Result<Vec<(valyria_model_registry::ModelCard, bool)>> {
        let catalog = Catalog::embedded().map_err(|e| AppError::Plan(e.to_string()))?;
        let installed: std::collections::BTreeSet<String> = self
            .global
            .models()
            .list()
            .await?
            .into_iter()
            .map(|r| r.id)
            .collect();
        Ok(catalog
            .cards()
            .iter()
            .cloned()
            .map(|c| {
                let is_installed = installed.contains(&c.id);
                (c, is_installed)
            })
            .collect())
    }

    // --- hardware & model management (§20, §21, §22, §37;
    // capabilities `hardware`, `model_manage`) ---

    fn model_store(&self) -> ModelStore {
        ModelStore::new(self.global.root())
    }

    /// A structured hardware report (§37) — the source the first-run
    /// wizard's Hardware View and model recommendation are built on.
    pub fn hardware_probe(&self) -> valyria_hardware::HardwareReport {
        valyria_hardware::probe()
    }

    /// Score every catalog candidate for `role` against measured hardware
    /// (§22, §41). Returns `(recommended, all_candidates_best_first)`.
    /// A non-fitting card is still listed, with `score: None`. The
    /// recommendation is Core's `fit()` scoring, not an app heuristic.
    pub async fn model_recommend(
        &self,
        role: ModelRole,
    ) -> Result<(
        Option<(ModelCard, CardScore)>,
        Vec<(ModelCard, Option<CardScore>, bool)>,
    )> {
        let catalog = Catalog::embedded().map_err(|e| AppError::Plan(e.to_string()))?;
        let hw = self.hardware_probe();
        let installed: std::collections::BTreeSet<String> = self
            .global
            .models()
            .list()
            .await?
            .into_iter()
            .map(|r| r.id)
            .collect();

        let mut scored: Vec<(ModelCard, Option<CardScore>, bool)> = catalog
            .candidates_for_role(role)
            .into_iter()
            .map(|card| {
                let score = score_card_for_role(card, role, &hw);
                (card.clone(), score, installed.contains(&card.id))
            })
            .collect();
        // Fitting candidates first, best adjusted score first; non-fitting last.
        scored.sort_by(|a, b| match (&a.1, &b.1) {
            (Some(x), Some(y)) => y
                .adjusted
                .partial_cmp(&x.adjusted)
                .unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.0.id.cmp(&b.0.id),
        });

        let recommended = scored
            .iter()
            .find_map(|(c, s, _)| s.map(|s| (c.clone(), s)));
        Ok((recommended, scored))
    }

    /// Begin installing catalog model `id`. Returns immediately; the
    /// download runs on a background task and reports
    /// `model_install_progress` / `_completed` / `_failed` on the event
    /// stream (§20, §21). The weights land in `~/.valyria/models/<id>/`,
    /// Core-owned; nothing else fetches them.
    pub async fn model_install(&self, id: &str) -> Result<()> {
        self.model_install_with(id, HttpFetcher::new()?).await
    }

    /// [`Self::model_install`] with an injected [`Fetcher`](valyria_model_store::Fetcher)
    /// — production passes an `HttpFetcher`; tests pass an in-memory one.
    pub async fn model_install_with<F>(&self, id: &str, fetcher: F) -> Result<()>
    where
        F: valyria_model_store::Fetcher + Send + Sync + 'static,
    {
        let catalog = Catalog::embedded().map_err(|e| AppError::Plan(e.to_string()))?;
        let card = catalog
            .get(id)
            .ok_or_else(|| AppError::Repo(format!("no catalog model `{id}`")))?
            .clone();
        let store = self.model_store();
        if store.is_installed(id) {
            return Err(AppError::ModelStore(
                valyria_model_store::ModelStoreError::AlreadyInstalled { id: id.to_string() },
            ));
        }
        let hw = self.hardware_probe();
        let plan = store.plan_install(&card, &hw).confirm();

        let events = self.events.clone();
        let installed_index = self.global.models().clone();
        let id_owned = id.to_string();

        tokio::spawn(async move {
            let cancel = CancellationToken::new();

            // The progress callback is synchronous; funnel its updates
            // through a channel that a concurrent task turns into events.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let pid = id_owned.clone();
            let progress = move |p: valyria_model_store::InstallProgress| {
                let _ = tx.send(p);
            };

            let drain_events = events.clone();
            let drain_id = id_owned.clone();
            let drainer = tokio::spawn(async move {
                while let Some(p) = rx.recv().await {
                    let _ = drain_events
                        .append(NewEvent::new(
                            EventKind::ModelInstallProgress,
                            serde_json::json!({
                                "id": drain_id,
                                "phase": p.phase.as_str(),
                                "downloaded_bytes": p.downloaded_bytes,
                                "total_bytes": p.total_bytes,
                            }),
                        ))
                        .await;
                }
            });

            let outcome = store
                .install_with_progress(&plan, &fetcher, &NullProber, &cancel, &progress)
                .await;
            drop(progress); // close tx so the drainer finishes
            let _ = drainer.await;

            match outcome {
                Ok(manifest) => {
                    let _ = installed_index.record(&manifest).await;
                    let _ = events
                        .append(NewEvent::new(
                            EventKind::ModelInstallCompleted,
                            serde_json::json!({
                                "id": pid,
                                "size_bytes": manifest.size_bytes,
                            }),
                        ))
                        .await;
                }
                Err(e) => {
                    let _ = events
                        .append(NewEvent::new(
                            EventKind::ModelInstallFailed,
                            serde_json::json!({
                                "id": pid,
                                "code": ErrorCode::code(&e),
                                "message": e.to_string(),
                            }),
                        ))
                        .await;
                }
            }
        });
        Ok(())
    }

    /// Remove installed model `id`, dropping any role bindings that named
    /// it. Returns bytes reclaimed.
    pub async fn model_remove(&self, id: &str) -> Result<u64> {
        let freed = self.model_store().remove(id)?;
        let _ = self.global.models().delete(id).await;
        let _ = self.global.models().clear_bindings_for(id).await;
        Ok(freed)
    }

    /// Bind installed model `id` to `role` (§38). Persisted in `global.db`.
    pub async fn model_activate(&self, id: &str, role: ModelRole) -> Result<()> {
        if !self.model_store().is_installed(id) {
            return Err(AppError::ModelStore(
                valyria_model_store::ModelStoreError::NotInstalled { id: id.to_string() },
            ));
        }
        let now = SystemClock.now().as_millis() as i64;
        self.global
            .models()
            .set_role_binding(role.as_str(), id, now)
            .await?;
        Ok(())
    }

    /// Full detail for model `id`: its catalog card, its manifest when
    /// installed, and the roles it is bound to.
    pub async fn model_inspect(&self, id: &str) -> Result<ModelInspectView> {
        let catalog = Catalog::embedded().map_err(|e| AppError::Plan(e.to_string()))?;
        let card = catalog
            .get(id)
            .ok_or_else(|| AppError::Repo(format!("no catalog model `{id}`")))?
            .clone();
        let store = self.model_store();
        let installed = store.is_installed(id);
        let manifest = if installed {
            store.manifest(id).ok()
        } else {
            None
        };
        let active_roles: Vec<String> = self
            .global
            .models()
            .role_bindings()
            .await?
            .into_iter()
            .filter(|(_, m)| m == id)
            .map(|(role, _)| role)
            .collect();
        Ok(ModelInspectView {
            card,
            installed,
            installed_at_ms: manifest.as_ref().map(|m| m.installed_at_ms),
            probe_tokens_per_sec: manifest
                .as_ref()
                .and_then(|m| m.probe.as_ref())
                .map(|p| p.tokens_per_sec as f64),
            active_roles,
        })
    }

    fn spawn_driver(&self, task_id: TaskId) {
        let driver = self.driver.clone();
        let tasks = self.tasks.clone();
        let engine = self.engine.clone();
        tokio::spawn(async move {
            if let Err(error) = driver.run(task_id, CancellationToken::new()).await {
                tracing::error!(%task_id, %error, "agent driver exited with an error");
            }
            // Release any per-task autonomy override once the task is
            // *terminal* — not on a mere pause / waiting-for-permission
            // yield, which also returns from `driver.run` (§25, G1).
            if let Ok(task) = tasks.get(task_id).await {
                if task.state.is_terminal() {
                    engine.clear_task_mode(task_id);
                }
            }
        });
    }
}

async fn load_or_create_workspace_id(store: &Store) -> Result<WorkspaceId> {
    let existing = store
        .call(|conn| {
            conn.query_row(
                "SELECT value FROM workspace_meta WHERE key = 'workspace_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(valyria_store::StoreError::from)
        })
        .await?;

    if let Some(id_str) = existing {
        return id_str
            .parse()
            .map_err(|_| AppError::CorruptWorkspaceId(id_str));
    }

    let id = WorkspaceId::new();
    let id_str = id.to_string();
    store
        .call(move |conn| {
            conn.execute(
                "INSERT INTO workspace_meta (key, value) VALUES ('workspace_id', ?1)",
                [&id_str],
            )?;
            Ok(())
        })
        .await?;
    Ok(id)
}
