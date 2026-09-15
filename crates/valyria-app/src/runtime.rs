//! `Runtime`: wires every already-built subsystem into one embedded agent
//! runtime for a single workspace (§4.1, §4.23). This is the composition
//! root — the one place in the whole workspace allowed to know about every
//! layer at once, so that `valyria-cli` never has to.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::OptionalExtension;
use valyria_agent::{AgentDriver, PlanningMode};
use valyria_context::ContextAssembler;
use valyria_events::{EventBus, EventKind, NewEvent};
use valyria_index::IndexStore;
use valyria_ledger::Ledger;
use valyria_memory::{MemoryStore, RetrievalRequest};
use valyria_model::{GenerateRequest, Message, ModelRuntime, SamplingParams};
use valyria_model_registry::{score_card_for_role, CardScore, Catalog, ModelCard, ModelRole};
use valyria_model_store::{HttpFetcher, ModelStore, NullProber};
use valyria_orchestrator::{NoModelRuntime, Role, RoleRouter};
use valyria_permissions::PermissionEngine;
use valyria_plan::{PlanRevision, PlanStore, RollbackError, RollbackReport};
use valyria_runtime_fake::{FakeModelRuntime, Scenario};
use valyria_runtime_llamacpp::{LlamaServerRuntime, LocalModelServer};
use valyria_sandbox::{detect_platform_launcher, ProcessLauncher, SandboxProfile};
use valyria_store::Store;
use valyria_task::{Budget, ControlSignal, Task, TaskManager};
use valyria_tools::ToolRuntime;
use valyria_types::{AgentState, CheckpointId, ErrorCode, PermissionMode, TaskId, WorkspaceId};
use valyria_util::{CancellationToken, Clock, ContentHash, SystemClock};
use valyria_verify::{CompletionReport, VerificationLog};
use valyria_vfs::WorkspaceRoot;

use crate::doctor::{Doctor, DoctorReport};
use crate::error::{AppError, Result};
use crate::global::GlobalStore;
use crate::migrations::workspace_migrations;
use crate::model_runtimes::ModelRuntimeRegistry;
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
    /// Unix ms the user accepted `card.license_name`, from the install
    /// manifest. `None` when not installed or installed pre-acceptance.
    pub license_accepted_at_ms: Option<i64>,
    pub probe_tokens_per_sec: Option<f64>,
    pub active_roles: Vec<String>,
}

/// One row of [`Runtime::model_list`] — a catalog card with local state.
#[derive(Debug, Clone)]
pub struct ModelListEntryView {
    pub card: ModelCard,
    pub installed: bool,
    /// `ModelRole` names this model is bound to, sorted.
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

/// Which `ModelRuntime` backs `Role::PrimaryCoder` (and, once more than
/// one role is bound, every other role too). `Fake` is what the whole
/// existing test suite, the walking-skeleton demo, and `--scenario` run
/// against — a `RuntimeConfig` defaults to it so nothing that doesn't ask
/// for `Local` ever touches a subprocess or the network. `Local` is real
/// inference: `Runtime::open` reads `model_role_binding` and boots a
/// managed `llama-server` per installed, bound model.
#[derive(Debug, Clone)]
pub enum ModelBackend {
    Fake(Scenario),
    Local,
}

impl ModelBackend {
    pub fn is_fake(&self) -> bool {
        matches!(self, ModelBackend::Fake(_))
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub workspace_path: PathBuf,
    /// Defaults to `<workspace_path>/.valyria` (§4.1's per-workspace
    /// layout) if left as `None` by `RuntimeConfig::new`.
    pub data_dir: PathBuf,
    pub permission_mode: PermissionMode,
    pub model_backend: ModelBackend,
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
            model_backend: ModelBackend::Fake(Scenario::default_walking_skeleton()),
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
        self.model_backend = ModelBackend::Fake(scenario);
        self
    }

    /// Drive real local inference instead of the fake: `Runtime::open`
    /// reads `model_role_binding` and boots a managed `llama-server` per
    /// installed, bound model, fetching the engine itself the first time.
    pub fn with_local_models(mut self) -> Self {
        self.model_backend = ModelBackend::Local;
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
    /// Cancel handles for `model_install` downloads currently in flight,
    /// keyed by catalog id. An entry exists only while the background task
    /// runs; `model_install_cancel` fires the token and the task's next
    /// checkpoint stops it.
    installs: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// The same `Arc<RoleRouter>` the driver holds — `model_activate` /
    /// `model_remove` rebind through this handle, which is why it must be
    /// the *same* `Arc`, not a fresh `RoleRouter`. Every role is currently
    /// bound as a length-1 chain (`bind_single`): real fallback chains
    /// across multiple installed models for one role are
    /// `docs/COMPLETION-PLAN.md` M6 (catalog-backed multi-model role
    /// selection), not wired up here yet.
    orchestrator: Arc<RoleRouter>,
    /// The live `llama-server` handles this `Runtime` has started. Empty
    /// (and untouched) when `model_backend` is `Fake`.
    model_runtimes: Arc<ModelRuntimeRegistry>,
    /// `true` when `model_backend` was `Fake` — `model_activate` /
    /// `model_remove` skip all server lifecycle work and stay pure DB
    /// writes, matching the pre-Phase-9 behaviour every existing test
    /// depends on.
    use_fake_model: bool,
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

        let use_fake_model = config.model_backend.is_fake();
        let orchestrator = Arc::new(RoleRouter::new());
        if let ModelBackend::Fake(scenario) = &config.model_backend {
            orchestrator.bind_single(
                Role::PrimaryCoder,
                "fake",
                Arc::new(FakeModelRuntime::from_scenario(scenario.clone())),
            );
        }

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

        let model_runtimes = Arc::new(ModelRuntimeRegistry::new());
        if matches!(config.model_backend, ModelBackend::Local) {
            let model_store = ModelStore::new(global.root());
            let bindings = global.models().role_bindings().await?;
            let mut primary_bound = false;
            for (role_str, model_id) in bindings {
                let Ok(role) = role_str.parse::<ModelRole>() else {
                    tracing::warn!(role = %role_str, "unknown role in model_role_binding, skipping");
                    continue;
                };
                if !model_store.is_installed(&model_id) {
                    continue;
                }
                if role == Role::PrimaryCoder {
                    primary_bound = true;
                }
                orchestrator.bind_single(
                    role,
                    model_id.clone(),
                    Arc::new(NoModelRuntime::starting(&model_id)),
                );
                spawn_model_boot(
                    role,
                    model_id,
                    global.root().to_path_buf(),
                    model_store.clone(),
                    orchestrator.clone(),
                    model_runtimes.clone(),
                    events.clone(),
                );
            }
            if !primary_bound {
                orchestrator.bind_single(
                    Role::PrimaryCoder,
                    "none",
                    Arc::new(NoModelRuntime::none_bound()),
                );
            }
        }

        let hash_cache = Arc::new(valyria_vfs::HashCache::new());
        let launcher: Arc<dyn ProcessLauncher> = Arc::from(detect_platform_launcher());
        let sandbox_profile = SandboxProfile::new().allow_write(workspace_root.as_path());

        let tasks = Arc::new(TaskManager::new(
            store.clone(),
            events.clone(),
            clock.clone(),
        ));

        // M2: a real local-model install has real content worth retrieving
        // for, so `open` bootstraps the index synchronously and wires a
        // `LiveRetriever::Search` into the driver — the Fake backend keeps
        // `LiveRetriever::empty()` (every pre-M2 fake-model scenario's
        // exact behaviour, and every timing-sensitive CLI kill/resume test
        // stays unaffected by index-bootstrap latency it never needed).
        // Synchronous and blocking here rather than the staged, non-
        // blocking background bootstrap the design calls for
        // (docs/COMPLETION-PLAN.md's own M2 write-up) — a scoped-down
        // first cut; failure degrades to no retrieval rather than failing
        // `open` outright, matching how a missing LSP server or sandbox
        // mechanism degrades elsewhere rather than erroring.
        let retriever = if use_fake_model {
            valyria_context::LiveRetriever::empty()
        } else {
            match bootstrap_index_for_retrieval(config.workspace_path.clone(), &index, &store).await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "index bootstrap for retrieval failed; continuing with no repository retrieval");
                    valyria_context::LiveRetriever::empty()
                }
            }
        };

        let driver = Arc::new(
            AgentDriver::new(
                tasks.clone(),
                tool_runtime,
                orchestrator.clone(),
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
            .with_planning_mode(config.planning_mode)
            .with_retriever(retriever)
            .with_store(store.clone()),
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
            installs: Arc::new(Mutex::new(HashMap::new())),
            orchestrator,
            model_runtimes,
            use_fake_model,
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
        // `transition` unconditionally clears `pending_signal` — a cancel
        // requested against this task while its driver was dead (nothing
        // running anywhere to notice it) must survive the Paused ->
        // paused_from transition below, or resuming a task silently drops
        // a pending cancel and just continues running it. `PauseRequested`
        // is deliberately NOT carried the same way: unlike a cancel, a
        // stale-or-racing pause has no reason to outlive an explicit
        // resume — re-arming it here would make the driver's very first
        // pending-signal check (`AgentDriver::run`) re-pause before the
        // resumed step does any work, so a pause that merely raced with
        // this resume call would silently turn "resume" into a no-op
        // instead of continuing the task.
        let pending_signal = task.pending_signal;
        if task.state == AgentState::Paused {
            let target = task.paused_from.ok_or(AppError::NotPaused(task_id))?;
            self.tasks.transition(task_id, target).await?;
        }
        if pending_signal == Some(ControlSignal::CancelRequested) {
            self.tasks.request_cancel(task_id).await?;
        }
        let task = self.tasks.get(task_id).await?;
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
    /// installed in the global store and which roles it is bound to
    /// (§4.21). This is the full "what can I run" surface — the catalog
    /// ships embedded, so a clean machine still lists every model.
    pub async fn model_list(&self) -> Result<Vec<ModelListEntryView>> {
        let catalog = Catalog::embedded().map_err(|e| AppError::Plan(e.to_string()))?;
        let installed: std::collections::BTreeSet<String> = self
            .global
            .models()
            .list()
            .await?
            .into_iter()
            .map(|r| r.id)
            .collect();
        let mut roles_by_model: HashMap<String, Vec<String>> = HashMap::new();
        for (role, model_id) in self.global.models().role_bindings().await? {
            roles_by_model.entry(model_id).or_default().push(role);
        }
        for roles in roles_by_model.values_mut() {
            roles.sort();
        }
        Ok(catalog
            .cards()
            .iter()
            .cloned()
            .map(|c| {
                let installed = installed.contains(&c.id);
                let active_roles = roles_by_model.get(&c.id).cloned().unwrap_or_default();
                ModelListEntryView {
                    card: c,
                    installed,
                    active_roles,
                }
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
    ///
    /// `accept_license` is the caller's record of the user having accepted
    /// the model's license (its text is on [`Self::model_inspect`]). Core
    /// refuses the install with `model.license_not_accepted` when it is
    /// `false` — no weights are ever fetched without acknowledgement.
    pub async fn model_install(&self, id: &str, accept_license: bool) -> Result<()> {
        self.model_install_with(id, accept_license, HttpFetcher::new()?)
            .await
    }

    /// [`Self::model_install`] with an injected [`Fetcher`](valyria_model_store::Fetcher)
    /// — production passes an `HttpFetcher`; tests pass an in-memory one.
    pub async fn model_install_with<F>(
        &self,
        id: &str,
        accept_license: bool,
        fetcher: F,
    ) -> Result<()>
    where
        F: valyria_model_store::Fetcher + Send + Sync + 'static,
    {
        if !accept_license {
            return Err(AppError::LicenseNotAccepted(id.to_string()));
        }
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

        let cancel = CancellationToken::new();
        {
            let mut installs = self.installs.lock().expect("installs mutex");
            if installs.contains_key(id) {
                return Err(AppError::InstallInFlight(id.to_string()));
            }
            installs.insert(id.to_string(), cancel.clone());
        }

        let now_ms = SystemClock.now().as_millis() as i64;
        let hw = self.hardware_probe();
        let plan = store.plan_install(&card, &hw).accept_license(now_ms);

        let events = self.events.clone();
        let installed_index = self.global.models().clone();
        let installs = self.installs.clone();
        let id_owned = id.to_string();
        let use_fake_model = self.use_fake_model;
        let global_root = self.global.root().to_path_buf();

        tokio::spawn(async move {
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

            // The fake backend never has a real engine to probe with (and
            // must stay network-free for its own test suite); real local
            // inference gets the genuine load-and-generate check so a
            // download that hashes correctly but can't actually load a
            // model is still caught (§4.21's "never partial-on-success").
            let outcome = if use_fake_model {
                store
                    .install_with_progress(&plan, &fetcher, &NullProber, &cancel, &progress)
                    .await
            } else {
                let prober = ServerProber {
                    global_root,
                    events: events.clone(),
                };
                store
                    .install_with_progress(&plan, &fetcher, &prober, &cancel, &progress)
                    .await
            };
            drop(progress); // close tx so the drainer finishes
            let _ = drainer.await;

            installs.lock().expect("installs mutex").remove(&id_owned);

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

    /// Cancel an in-flight [`Self::model_install`] for `id`. Fires the
    /// download's cancellation token; the background task stops at its next
    /// chunk boundary and emits `model_install_failed` with code
    /// `model_store.cancelled`, leaving a `.part` file so a later install
    /// resumes. A no-op (still `Ok`) when nothing is installing `id`.
    pub async fn model_install_cancel(&self, id: &str) -> Result<()> {
        if let Some(token) = self.installs.lock().expect("installs mutex").get(id) {
            token.cancel();
        }
        Ok(())
    }

    /// Ids of every model whose install is currently in flight.
    pub fn installs_in_flight(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .installs
            .lock()
            .expect("installs mutex")
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    /// Remove installed model `id`, dropping any role bindings that named
    /// it. Stops any server currently serving it *before* the weights are
    /// deleted out from under it. Returns bytes reclaimed.
    pub async fn model_remove(&self, id: &str) -> Result<u64> {
        if !self.use_fake_model {
            for role in self.model_runtimes.roles_for_model(id).await {
                self.orchestrator
                    .bind_single(role, "none", Arc::new(NoModelRuntime::none_bound()));
                if let Some(old) = self.model_runtimes.take(role).await {
                    old.shutdown().await; // awaited: files are about to go away
                }
                let _ = self
                    .events
                    .append(NewEvent::new(
                        EventKind::ModelServerStopped,
                        serde_json::json!({
                            "role": role.as_str(),
                            "id": id,
                            "reason": "model_removed",
                        }),
                    ))
                    .await;
            }
        }

        let freed = self.model_store().remove(id)?;
        let _ = self.global.models().delete(id).await;
        let _ = self.global.models().clear_bindings_for(id).await;
        Ok(freed)
    }

    /// Bind installed model `id` to `role` (§38), persisted in `global.db`
    /// **unconditionally and first** — a boot failure below still leaves a
    /// binding `Runtime::open` retries on the next process, and the DB
    /// write is declarative intent, not a liveness claim. With the fake
    /// backend that persistence is the whole operation, matching every
    /// existing test's expectations. With real inference, this then starts
    /// (or re-points) the server for `role` and waits for it to answer
    /// `/health` before returning — an explicit user action, so unlike the
    /// background boot loop, blocking here is the right trade.
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

        if self.use_fake_model {
            return Ok(());
        }

        let _ = self
            .events
            .append(NewEvent::new(
                EventKind::ModelServerStarting,
                serde_json::json!({ "role": role.as_str(), "id": id }),
            ))
            .await;

        let model_store = self.model_store();
        match boot_model_server(self.global.root(), id, &model_store, &self.events).await {
            Ok(handle) => {
                let port = handle.port();
                self.orchestrator.bind_single(
                    role,
                    id.to_string(),
                    handle.clone() as Arc<dyn ModelRuntime>,
                );
                if let Some(old) = self.model_runtimes.swap(role, id.to_string(), handle).await {
                    tokio::spawn(async move { old.shutdown().await });
                }
                let _ = self
                    .events
                    .append(NewEvent::new(
                        EventKind::ModelServerReady,
                        serde_json::json!({ "role": role.as_str(), "id": id, "port": port }),
                    ))
                    .await;
                Ok(())
            }
            Err(e) => {
                self.orchestrator.bind_single(
                    role,
                    id.to_string(),
                    Arc::new(NoModelRuntime::failed(id, &e.to_string())),
                );
                let _ = self
                    .events
                    .append(NewEvent::new(
                        EventKind::ModelServerFailed,
                        serde_json::json!({
                            "role": role.as_str(),
                            "id": id,
                            "code": ErrorCode::code(&e),
                            "message": e.to_string(),
                        }),
                    ))
                    .await;
                Err(AppError::ModelServerStart {
                    id: id.to_string(),
                    role: role.as_str().to_string(),
                    source: match e {
                        AppError::LlamaCpp(inner) => inner,
                        other => valyria_runtime_llamacpp::LlamaError::EngineUnavailable(
                            other.to_string(),
                        ),
                    },
                })
            }
        }
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
        let mut active_roles: Vec<String> = self
            .global
            .models()
            .role_bindings()
            .await?
            .into_iter()
            .filter(|(_, m)| m == id)
            .map(|(role, _)| role)
            .collect();
        active_roles.sort();
        Ok(ModelInspectView {
            card,
            installed,
            installed_at_ms: manifest.as_ref().map(|m| m.installed_at_ms),
            license_accepted_at_ms: manifest.as_ref().and_then(|m| m.license_accepted_at_ms),
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
                // `driver.run`'s own `?`-propagation only ever transitions
                // a task on the *happy* exits it recognizes (repair
                // give-up, approval denial, ...); an error it doesn't
                // otherwise handle — a model that's `Unavailable` because
                // nothing is activated chief among them — must still not
                // leave the task silently wedged in whatever step it was
                // mid-way through forever. Answer §36 here if nothing else
                // already did.
                if let Ok(task) = tasks.get(task_id).await {
                    if !task.state.is_terminal() {
                        let _ = tasks
                            .append_journal(
                                task_id,
                                valyria_task::JournalEntryKind::RecoveryNote {
                                    note: format!("agent driver exited with an error: {error}"),
                                },
                            )
                            .await;
                        let _ = tasks.transition(task_id, AgentState::Failed).await;
                    }
                }
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

// --- real local inference (Phase 9 follow-up): boot a `llama-server` per
// installed, bound model. Free functions (not `impl Runtime` methods)
// because the background boot loop in `open()` needs to run before `Self`
// exists, and `model_activate` reuses the same two building blocks. ---

/// A real load-and-generate post-install probe (replaces `NullProber`
/// for the `Local` backend): spin the just-downloaded weights up on a
/// managed `llama-server`, ask for one short completion, and tear it
/// down. If the *engine* itself can't be resolved (offline, first run
/// with no network), that is treated as **skip, not fail** — the model
/// still installs, just unverified, exactly like `NullProber` would have
/// left it; a corrupted or unloadable *model* is still a hard failure per
/// `ModelStore::install_with_progress`'s existing contract.
struct ServerProber {
    global_root: PathBuf,
    events: Arc<EventBus>,
}

#[async_trait::async_trait]
impl valyria_model_store::Prober for ServerProber {
    async fn probe(
        &self,
        weights: &std::path::Path,
        card: &ModelCard,
    ) -> valyria_model_store::Result<valyria_model_store::ProbeResult> {
        let binary = match resolve_or_install_engine(&self.global_root, &self.events).await {
            Ok(bin) => bin,
            Err(_) => {
                return Ok(valyria_model_store::ProbeResult {
                    loads: true,
                    working_transport: card.transport_preference,
                    tokens_per_sec: 0.0,
                    measured_ram_bytes: card.requirement.min_ram_bytes,
                });
            }
        };

        let log_path = self
            .global_root
            .join("logs")
            .join(format!("llama-probe-{}.log", card.id));
        let to_probe_err = |detail: String| valyria_model_store::ModelStoreError::Probe {
            id: card.id.clone(),
            detail,
        };

        let rt = LlamaServerRuntime::start_with_timeout(
            binary,
            weights.to_path_buf(),
            card,
            log_path,
            Duration::from_secs(180),
        )
        .await
        .map_err(|e| to_probe_err(e.to_string()))?;

        // Embedder/reranker models have no chat-completion path at all —
        // llama-server never answers a `/v1/chat/completions` request for
        // one, so asking would just hang (found live: nomic-embed-text-v1.5
        // sat past 200s). The server having loaded and answered `/health`
        // (above) is already the meaningful integrity signal for those; a
        // chat probe is only appropriate for a card that actually declares
        // a chat-capable role.
        let chat_capable = card
            .role_suitability
            .keys()
            .any(|r| !matches!(r, ModelRole::Embedder | ModelRole::Reranker));
        if !chat_capable {
            rt.shutdown().await;
            return Ok(valyria_model_store::ProbeResult {
                loads: true,
                working_transport: card.transport_preference,
                tokens_per_sec: 0.0,
                measured_ram_bytes: card.requirement.min_ram_bytes,
            });
        }

        // Defense in depth for chat-capable models too: even a model that
        // *should* answer must never be able to hang the install forever —
        // bound the single probe generation.
        const PROBE_GENERATE_TIMEOUT: Duration = Duration::from_secs(60);
        let req = GenerateRequest::new(vec![Message::user("Reply with one short word.")])
            .with_sampling(SamplingParams {
                temperature: 0.0,
                top_p: 1.0,
                max_tokens: Some(8),
                stop: Vec::new(),
            });
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            PROBE_GENERATE_TIMEOUT,
            rt.generate(req, CancellationToken::new()),
        )
        .await;
        let elapsed = started.elapsed();
        rt.shutdown().await;

        let completion = match result {
            Ok(inner) => inner.map_err(|e| to_probe_err(e.to_string()))?,
            Err(_) => {
                return Err(to_probe_err(format!(
                    "model did not answer a trivial prompt within {}s",
                    PROBE_GENERATE_TIMEOUT.as_secs()
                )))
            }
        };
        if completion.text.trim().is_empty() && completion.tool_calls.is_empty() {
            return Err(to_probe_err(
                "model started but produced no output for a trivial prompt".into(),
            ));
        }
        let tokens_per_sec = if elapsed.as_secs_f32() > 0.0 {
            completion.usage.completion_tokens as f32 / elapsed.as_secs_f32()
        } else {
            0.0
        };

        Ok(valyria_model_store::ProbeResult {
            loads: true,
            working_transport: card.transport_preference,
            tokens_per_sec,
            measured_ram_bytes: card.requirement.min_ram_bytes,
        })
    }
}

/// Resolve the inference engine, downloading and unpacking it the first
/// time (emitting `engine_install_progress` / `_completed` / `_failed`) if
/// [`valyria_engine_store::EngineStore::resolve`] comes back empty.
async fn resolve_or_install_engine(
    global_root: &std::path::Path,
    events: &Arc<EventBus>,
) -> Result<PathBuf> {
    let engine_store = valyria_engine_store::EngineStore::new(global_root);
    if let Some(bin) = engine_store.resolve("llama.cpp") {
        return Ok(bin);
    }

    let catalog = valyria_engine_store::Catalog::embedded()?;
    let release = catalog
        .entry("llama.cpp")
        .map(|e| e.release.clone())
        .unwrap_or_default();
    let fetcher = valyria_engine_store::HttpFetcher::new()?;
    let cancel = CancellationToken::new();

    // Same synchronous-callback-to-event-stream pattern as
    // `model_install_with`'s download progress.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let progress = move |p: valyria_engine_store::InstallProgress| {
        let _ = tx.send(p);
    };
    let drain_events = events.clone();
    let drain_release = release.clone();
    let drainer = tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let _ = drain_events
                .append(NewEvent::new(
                    EventKind::EngineInstallProgress,
                    serde_json::json!({
                        "component": "llama.cpp",
                        "version": drain_release.clone(),
                        "phase": p.phase.as_str(),
                        "downloaded_bytes": p.downloaded_bytes,
                        "total_bytes": p.total_bytes,
                    }),
                ))
                .await;
        }
    });

    let outcome = engine_store
        .install_with_progress(&catalog, "llama.cpp", &fetcher, &cancel, &progress)
        .await;
    drop(progress); // close tx so the drainer finishes
    let _ = drainer.await;

    match outcome {
        Ok(bin) => {
            let _ = events
                .append(NewEvent::new(
                    EventKind::EngineInstallCompleted,
                    serde_json::json!({ "component": "llama.cpp", "version": release }),
                ))
                .await;
            Ok(bin)
        }
        Err(e) => {
            let _ = events
                .append(NewEvent::new(
                    EventKind::EngineInstallFailed,
                    serde_json::json!({
                        "component": "llama.cpp",
                        "version": release,
                        "code": e.code(),
                        "message": e.to_string(),
                    }),
                ))
                .await;
            Err(AppError::EngineStore(e))
        }
    }
}

/// `Runtime::open`'s M2 bootstrap: build (or catch up) the file/symbol
/// index and its import/call graph, then wrap it as a
/// `LiveRetriever::Search` the driver can query every turn. Mirrors
/// `Runtime::reindex` exactly (same pipeline, same steps) — kept as its
/// own free function because `reindex` needs `&self` for its `Result`
/// error path and event-free contract, while this one runs *during*
/// `open`, before `Self` exists, and its errors are caught and degraded
/// rather than propagated (see the call site's comment).
async fn bootstrap_index_for_retrieval(
    workspace_path: PathBuf,
    index: &IndexStore,
    store: &Arc<Store>,
) -> Result<valyria_context::LiveRetriever> {
    let registry = valyria_lang::LanguageRegistry::with_builtin_languages()
        .map_err(|e| AppError::Repo(format!("language registry: {e}")))?;
    let pipeline =
        valyria_index::IndexPipeline::new(workspace_path.clone(), registry.clone(), index.clone());
    let delta = pipeline.bootstrap_unstaged(&|_| {}).await?;
    valyria_graph::GraphStore::new(store.clone())
        .build_for(index, delta.generation)
        .await
        .map_err(|e| AppError::Repo(format!("graph build: {e}")))?;

    let embedder: Arc<dyn valyria_embed::Embedder> =
        Arc::new(valyria_embed::HashingEmbedder::default());
    let embed = valyria_embed::EmbedStore::new(store.clone());
    let embed_pipeline = valyria_embed::EmbedPipeline::new(
        workspace_path.clone(),
        registry.clone(),
        embedder.clone(),
        embed.clone(),
    );
    // Embeddings are the slowest stage and the least essential — lexical
    // and symbol search (and therefore retrieval) already work without
    // them (§9.4.15's staged-availability design). A failure here
    // degrades to search without semantic ranking rather than losing
    // retrieval entirely.
    if let Err(e) = embed_pipeline.bootstrap(index, delta.generation).await {
        tracing::warn!(error = %e, "embedding bootstrap failed; retrieval continues without semantic ranking");
    }

    let engine = valyria_search::SearchEngine::new(
        workspace_path,
        index.clone(),
        valyria_graph::GraphStore::new(store.clone()),
        embed,
        embedder,
        registry,
    );
    Ok(valyria_context::LiveRetriever::Search(
        valyria_context::SearchRetriever::new(engine, index.clone()),
    ))
}

/// Resolve the engine (installing it if needed) and boot a `llama-server`
/// for `model_id`, waiting until it answers `/health`. Touches neither the
/// orchestrator nor the registry — the caller (the background boot loop,
/// or `Runtime::model_activate`) decides what "ready" means for it.
async fn boot_model_server(
    global_root: &std::path::Path,
    model_id: &str,
    model_store: &ModelStore,
    events: &Arc<EventBus>,
) -> Result<Arc<dyn LocalModelServer>> {
    let catalog = Catalog::embedded().map_err(|e| AppError::Plan(e.to_string()))?;
    let card = catalog
        .get(model_id)
        .ok_or_else(|| AppError::Repo(format!("no catalog model `{model_id}`")))?
        .clone();
    let weights = model_store.weights_path(model_id)?;
    let binary = resolve_or_install_engine(global_root, events).await?;
    let log_path = global_root
        .join("logs")
        .join(format!("llama-{model_id}.log"));

    let rt = LlamaServerRuntime::start(binary, weights, &card, log_path)
        .await
        .map_err(AppError::LlamaCpp)?;
    Ok(Arc::new(rt))
}

/// The background half of booting a model at `Runtime::open` time:
/// `model_server_starting` immediately, then `boot_model_server`, then
/// `rebind` the orchestrator to the result (a real server on success, a
/// `NoModelRuntime::failed` on failure) and emit `model_server_ready` /
/// `_failed`. `open()` never awaits this — it returns as soon as the task
/// is spawned, with the role already pointed at `NoModelRuntime::starting`.
fn spawn_model_boot(
    role: Role,
    model_id: String,
    global_root: PathBuf,
    model_store: ModelStore,
    orchestrator: Arc<RoleRouter>,
    model_runtimes: Arc<ModelRuntimeRegistry>,
    events: Arc<EventBus>,
) {
    tokio::spawn(async move {
        let _ = events
            .append(NewEvent::new(
                EventKind::ModelServerStarting,
                serde_json::json!({ "role": role.as_str(), "id": model_id }),
            ))
            .await;

        match boot_model_server(&global_root, &model_id, &model_store, &events).await {
            Ok(handle) => {
                let port = handle.port();
                orchestrator.bind_single(
                    role,
                    model_id.clone(),
                    handle.clone() as Arc<dyn ModelRuntime>,
                );
                if let Some(old) = model_runtimes.swap(role, model_id.clone(), handle).await {
                    // Drain in place rather than block this task: an
                    // in-flight generate against the old server still
                    // holds its own `Arc` clone and finishes regardless.
                    tokio::spawn(async move { old.shutdown().await });
                }
                let _ = events
                    .append(NewEvent::new(
                        EventKind::ModelServerReady,
                        serde_json::json!({ "role": role.as_str(), "id": model_id, "port": port }),
                    ))
                    .await;
            }
            Err(e) => {
                orchestrator.bind_single(
                    role,
                    model_id.clone(),
                    Arc::new(NoModelRuntime::failed(&model_id, &e.to_string())),
                );
                let _ = events
                    .append(NewEvent::new(
                        EventKind::ModelServerFailed,
                        serde_json::json!({
                            "role": role.as_str(),
                            "id": model_id,
                            "code": ErrorCode::code(&e),
                            "message": e.to_string(),
                        }),
                    ))
                    .await;
            }
        }
    });
}
