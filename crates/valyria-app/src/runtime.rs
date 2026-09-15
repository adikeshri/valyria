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
use valyria_model::{
    Capabilities, GenerateRequest, LocalModelServer, Message, ModelRuntime, SamplingParams,
};
use valyria_model_registry::{
    parse_public_key_hex, score_card_for_role, CardScore, Catalog, EngineKind, ModelCard,
    ModelRole, RoleBinding, CATALOG_PUBLIC_KEY_HEX,
};
use valyria_model_store::{EndpointRow, Fetcher, HttpFetcher, ModelStore, NullProber};
use valyria_orchestrator::{
    EvictReason, ModelPool, NoModelRuntime, PoolError, PoolEvent, Role, RoleRouter,
};
use valyria_permissions::PermissionEngine;
use valyria_plan::{PlanRevision, PlanStore, RollbackError, RollbackReport, StoredArtifact};
use valyria_runtime_fake::{FakeModelRuntime, Scenario};
use valyria_runtime_llamacpp::LlamaServerRuntime;
use valyria_runtime_mlx::MlxServerRuntime;
use valyria_runtime_openai_compat::{OpenAiCompatRuntime, ReqwestTransport};
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

/// One row of [`Runtime::model_endpoint_list`] — a registered external
/// endpoint with its currently-bound roles.
#[derive(Debug, Clone)]
pub struct ModelEndpointView {
    pub row: EndpointRow,
    /// `ModelRole` names this endpoint is bound to, sorted.
    pub active_roles: Vec<String>,
}

/// The optional fields of [`Runtime::model_endpoint_add`], bundled so the
/// method stays a handful of parameters instead of eight independent
/// ones. Every field defaults sensibly when omitted — see the field docs
/// on [`valyria_protocol::ModelEndpointAddRequest`], which this mirrors.
#[derive(Debug, Clone, Default)]
pub struct ModelEndpointOptions<'a> {
    pub display_name: Option<&'a str>,
    pub remote_model_name: Option<&'a str>,
    pub context_length: Option<u32>,
    pub supports_native_tools: Option<bool>,
    pub supports_grammar: Option<bool>,
}

/// The result of a successful [`Runtime::catalog_refresh`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogRefreshOutcome {
    pub previous_version: u32,
    pub new_version: u32,
    pub model_count: usize,
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
    /// Hex-encoded ed25519 public key `catalog_refresh` (and every later
    /// re-load of a persisted refresh) trusts. `None` uses this build's
    /// compiled-in [`valyria_model_registry::CATALOG_PUBLIC_KEY_HEX`] —
    /// the only sensible choice in production. Exists as a config knob
    /// purely so a test can exercise the *real* `catalog_refresh` →
    /// persist → `effective_catalog` re-load round trip with a throwaway
    /// keypair it actually holds the private half of, since the
    /// compiled-in key's private half deliberately exists nowhere.
    pub catalog_trusted_key_hex: Option<String>,
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
            catalog_trusted_key_hex: None,
        }
    }

    /// Test-only in practice (see the field's own doc comment) — trust a
    /// different ed25519 public key for `catalog_refresh` than this
    /// build's compiled-in one.
    pub fn with_catalog_trusted_key_hex(mut self, hex: impl Into<String>) -> Self {
        self.catalog_trusted_key_hex = Some(hex.into());
        self
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
    /// M6: the memory budget every real model activation is admitted
    /// against before its server boots — `None` for the `Fake` backend
    /// (matching `model_runtimes`, nothing here is ever touched). Sized
    /// once, at `open()`, from `valyria_hardware::probe()`'s measured
    /// available RAM; not re-probed live (a machine's available RAM
    /// shifts constantly from unrelated processes, and re-sizing the
    /// budget under an admission already in flight would make eviction
    /// decisions non-reproducible).
    pool: Option<Arc<tokio::sync::Mutex<ModelPool>>>,
    /// The ed25519 public key `catalog_refresh` and every catalog read
    /// (via [`effective_catalog`]) trust — this build's compiled-in
    /// [`CATALOG_PUBLIC_KEY_HEX`] unless overridden by [`RuntimeConfig::
    /// catalog_trusted_key_hex`] (test-only in practice).
    catalog_trusted_key: valyria_model_registry::VerifyingKey,
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
        let catalog_trusted_key = parse_public_key_hex(
            config
                .catalog_trusted_key_hex
                .as_deref()
                .unwrap_or(CATALOG_PUBLIC_KEY_HEX),
        )
        .map_err(|e| AppError::Repo(format!("catalog trusted key: {e}")))?;
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
        let mut pool: Option<Arc<tokio::sync::Mutex<ModelPool>>> = None;
        if matches!(config.model_backend, ModelBackend::Local) {
            // M6: sized from measured available RAM, reserving headroom for
            // the OS and this process's own working set rather than
            // claiming every last byte as loadable — 80% is a documented,
            // simple heuristic, not a hardware-verified ceiling.
            let hw = valyria_hardware::probe();
            let budget_bytes = (hw.ram_available_bytes as f64 * 0.8) as u64;
            let model_pool = Arc::new(tokio::sync::Mutex::new(ModelPool::new(budget_bytes)));
            pool = Some(model_pool.clone());

            let model_store = ModelStore::new(global.root());
            let bindings = global.models().role_bindings().await?;
            let mut bound_roles: std::collections::HashSet<Role> = std::collections::HashSet::new();
            for (role_str, model_id) in bindings {
                let Ok(role) = role_str.parse::<ModelRole>() else {
                    tracing::warn!(role = %role_str, "unknown role in model_role_binding, skipping");
                    continue;
                };
                // A persisted binding can name an external endpoint
                // instead of an installed catalog model — cheap and
                // synchronous to re-point (no process to spawn or wait
                // on), unlike the local-server boot below.
                if let Some(row) = global.models().endpoint(&model_id).await? {
                    match Runtime::endpoint_runtime(&row) {
                        Ok(rt) => {
                            bound_roles.insert(role);
                            orchestrator.bind_single(role, model_id.clone(), rt);
                        }
                        Err(e) => {
                            tracing::warn!(role = %role_str, endpoint = %model_id, error = %e, "could not rebuild endpoint runtime at startup");
                        }
                    }
                    continue;
                }
                if !model_store.is_installed(&model_id) {
                    continue;
                }
                bound_roles.insert(role);
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
                    ModelBootHandles {
                        orchestrator: orchestrator.clone(),
                        model_runtimes: model_runtimes.clone(),
                        pool: model_pool.clone(),
                        events: events.clone(),
                        catalog_trusted_key,
                    },
                );
            }

            // M6: every role a user has never explicitly activated (no
            // persisted `model_role_binding` row) gets a *best-effort*
            // auto-derived choice instead of sitting unbound —
            // `RoleBinding::derive` picks the best-fitting installed model
            // for the role, scored against this machine's real, just-
            // measured hardware. Deliberately never persisted: an explicit
            // `model_activate` writes a real row and, from then on, wins
            // on every subsequent `open()` without any extra bookkeeping
            // to distinguish "auto" from "override" rows — this recomputes
            // from scratch each time instead, so it also tracks newly
            // installed/removed models automatically. Only `.primary` is
            // used, not `.fallbacks` — multi-model fallback chains are a
            // distinct, not-yet-wired piece of M6 (see the `orchestrator`
            // field's own doc comment).
            let installed = model_store.installed().unwrap_or_default();
            // `RoleBinding::derive`/`select_for_role` treat an *empty*
            // `available` list as "consider the whole catalog" — the
            // right behavior for `model_recommend`'s "what would I need
            // to install" question, but wrong here: with nothing
            // installed at all, that would auto-select a catalog entry
            // whose weights don't actually exist on disk, and
            // `spawn_model_boot` would fail trying to load them. Guard it
            // explicitly rather than relying on every call site to know
            // that distinction.
            if !installed.is_empty() {
                let catalog = effective_catalog(global.root(), &catalog_trusted_key);
                for role in ModelRole::ALL {
                    if bound_roles.contains(&role) {
                        continue;
                    }
                    let Ok(derived) = RoleBinding::derive(&catalog, role, &hw, &installed) else {
                        continue;
                    };
                    bound_roles.insert(role);
                    orchestrator.bind_single(
                        role,
                        derived.primary.clone(),
                        Arc::new(NoModelRuntime::starting(&derived.primary)),
                    );
                    spawn_model_boot(
                        role,
                        derived.primary,
                        global.root().to_path_buf(),
                        model_store.clone(),
                        ModelBootHandles {
                            orchestrator: orchestrator.clone(),
                            model_runtimes: model_runtimes.clone(),
                            pool: model_pool.clone(),
                            events: events.clone(),
                            catalog_trusted_key,
                        },
                    );
                }
            }

            if !bound_roles.contains(&Role::PrimaryCoder) {
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
            .with_store(store.clone())
            .with_memory(memory.clone()),
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
            pool,
            catalog_trusted_key,
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

    /// Answer a task parked in `WAITING_FOR_USER` (M4) and, if that leaves
    /// it live, spawn a fresh driver to keep it running — mirrors
    /// `resolve_permission_scoped` exactly: `AgentDriver::respond_to_user`
    /// only performs the one resolution step.
    pub async fn respond_to_user(&self, task_id: TaskId, answer: String) -> Result<()> {
        self.driver.respond_to_user(task_id, answer).await?;
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

    /// M5, protocol 1.13.0: every direct child of a task, oldest first.
    pub async fn task_children(&self, task_id: TaskId) -> Result<Vec<Task>> {
        Ok(self.tasks.children_of(task_id).await?)
    }

    /// Every role-pipeline artifact produced against a task, oldest first.
    pub async fn task_artifacts(&self, task_id: TaskId) -> Result<Vec<StoredArtifact>> {
        self.plan_store
            .artifacts_for_task(task_id)
            .await
            .map_err(|e| AppError::Plan(e.to_string()))
    }

    /// Every plan revision for a task, oldest first.
    pub async fn plan_revisions(&self, task_id: TaskId) -> Result<Vec<PlanRevision>> {
        self.plan_store
            .all_revisions(task_id)
            .await
            .map_err(|e| AppError::Plan(e.to_string()))
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
        let catalog = effective_catalog(self.global.root(), &self.catalog_trusted_key);
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
        let catalog = effective_catalog(self.global.root(), &self.catalog_trusted_key);
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
        let catalog = effective_catalog(self.global.root(), &self.catalog_trusted_key);
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
        if let Some(row) = self.global.models().endpoint(id).await? {
            return self.activate_endpoint(&row, role).await;
        }
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

        let model_store = self.model_store();
        let Some(pool) = &self.pool else {
            // Invariant: `pool` is `Some` exactly when `!use_fake_model`
            // (both follow `config.model_backend == Local`), and the
            // fake-backend case already returned above.
            return Err(AppError::ModelPool(
                "missing for a real-backend runtime".into(),
            ));
        };
        let footprint_bytes = model_store.manifest(id)?.size_bytes;
        if let Err(e) = admit_to_pool(
            pool,
            &self.model_runtimes,
            &self.orchestrator,
            &self.events,
            id,
            role,
            footprint_bytes,
        )
        .await
        {
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
                        "code": "model_pool.wont_fit",
                        "message": e.to_string(),
                    }),
                ))
                .await;
            return Err(AppError::ModelPool(e.to_string()));
        }

        let _ = self
            .events
            .append(NewEvent::new(
                EventKind::ModelServerStarting,
                serde_json::json!({ "role": role.as_str(), "id": id }),
            ))
            .await;

        match boot_model_server(
            self.global.root(),
            id,
            &model_store,
            &self.events,
            &self.catalog_trusted_key,
        )
        .await
        {
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
                    retryable: e.retryable(),
                    message: e.to_string(),
                })
            }
        }
    }

    /// Full detail for model `id`: its catalog card, its manifest when
    /// installed, and the roles it is bound to.
    pub async fn model_inspect(&self, id: &str) -> Result<ModelInspectView> {
        let catalog = effective_catalog(self.global.root(), &self.catalog_trusted_key);
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

    /// Register (or replace) an already-running external OpenAI-
    /// compatible server (Ollama, LM Studio, vLLM, …). Core neither
    /// downloads nor supervises its process — [`Self::model_activate`]
    /// just points a client at `base_url` once this exists. `id` must
    /// not collide with an embedded-catalog model id, so `model_activate`
    /// never has to guess which of two same-named things a caller meant.
    pub async fn model_endpoint_add(
        &self,
        id: &str,
        base_url: &str,
        opts: ModelEndpointOptions<'_>,
    ) -> Result<()> {
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(AppError::Repo(format!(
                "endpoint base_url must start with http:// or https://, got {base_url:?}"
            )));
        }
        let scheme_len = base_url.find("://").unwrap() + 3;
        if base_url[scheme_len..].trim_start_matches('/').is_empty() {
            return Err(AppError::Repo(format!(
                "endpoint base_url {base_url:?} has no host"
            )));
        }
        let catalog = effective_catalog(self.global.root(), &self.catalog_trusted_key);
        if catalog.get(id).is_some() {
            return Err(AppError::Repo(format!(
                "`{id}` is already an embedded catalog model id; pick a different endpoint id"
            )));
        }
        let now = SystemClock.now().as_millis() as i64;
        self.global
            .models()
            .add_endpoint(
                id,
                base_url,
                opts.display_name.unwrap_or(id),
                opts.remote_model_name.unwrap_or(id),
                opts.context_length.unwrap_or(8192),
                opts.supports_native_tools.unwrap_or(true),
                opts.supports_grammar.unwrap_or(false),
                now,
            )
            .await?;
        Ok(())
    }

    /// Unregister endpoint `id`. Any role currently bound to it is
    /// rebound to [`NoModelRuntime::none_bound`] first — mirrors
    /// [`Self::model_remove`]'s "never leave a role pointing at
    /// something that no longer exists" contract.
    pub async fn model_endpoint_remove(&self, id: &str) -> Result<()> {
        if !self.use_fake_model {
            for (role_str, bound_id) in self.global.models().role_bindings().await? {
                if bound_id != id {
                    continue;
                }
                let Ok(role) = role_str.parse::<ModelRole>() else {
                    continue;
                };
                self.orchestrator
                    .bind_single(role, "none", Arc::new(NoModelRuntime::none_bound()));
            }
        }
        let removed = self.global.models().remove_endpoint(id).await?;
        if !removed {
            return Err(AppError::Repo(format!("no such model endpoint `{id}`")));
        }
        let _ = self.global.models().clear_bindings_for(id).await;
        Ok(())
    }

    /// Every registered external endpoint, with the roles each currently
    /// serves.
    pub async fn model_endpoint_list(&self) -> Result<Vec<ModelEndpointView>> {
        let bindings = self.global.models().role_bindings().await?;
        let mut out = Vec::new();
        for row in self.global.models().endpoints().await? {
            let mut active_roles: Vec<String> = bindings
                .iter()
                .filter(|(_, m)| m == &row.id)
                .map(|(role, _)| role.clone())
                .collect();
            active_roles.sort();
            out.push(ModelEndpointView { row, active_roles });
        }
        Ok(out)
    }

    /// [`Self::model_activate`]'s endpoint branch: no process to spawn or
    /// wait on, so this is synchronous relative to the local-model path —
    /// the RPC's own success/failure already tells the caller everything
    /// `model_server_starting`/`_ready`/`_failed` exist to report for a
    /// managed local server, so this deliberately emits none of them.
    async fn activate_endpoint(&self, row: &EndpointRow, role: ModelRole) -> Result<()> {
        let now = SystemClock.now().as_millis() as i64;
        self.global
            .models()
            .set_role_binding(role.as_str(), &row.id, now)
            .await?;

        if self.use_fake_model {
            return Ok(());
        }

        // A role previously served by a *locally managed* server must not
        // leak its process just because the role now points elsewhere —
        // `model_runtimes` only ever replaces an entry via `swap`, which
        // nothing calls on this path since there is no new local handle
        // to register in its place.
        if let Some(old) = self.model_runtimes.take(role).await {
            tokio::spawn(async move { old.shutdown().await });
        }

        let runtime = Self::endpoint_runtime(row)?;
        self.orchestrator.bind_single(role, row.id.clone(), runtime);
        Ok(())
    }

    /// Build the (unstarted-by-us, already-running) [`ModelRuntime`] for
    /// endpoint `row` — a thin `OpenAiCompatRuntime` pointed at its
    /// `base_url`, using `remote_model_name` (not `row.id`) as the wire
    /// `"model"` field. The same real bug confirmed live against the
    /// managed MLX adapter — a server that treats a *mismatched* `"model"`
    /// field as a load target rather than ignoring it — applies here too,
    /// for any endpoint whose server enforces it; getting this field right
    /// is the caller's job via `remote_model_name` at `model_endpoint_add`
    /// time, not something Core can discover on its own.
    fn endpoint_runtime(row: &EndpointRow) -> Result<Arc<dyn ModelRuntime>> {
        let transport = ReqwestTransport::new(row.base_url.clone())
            .map_err(|e| AppError::Repo(format!("endpoint `{}`: {e}", row.id)))?;
        let capabilities = Capabilities {
            context_length: row.context_length,
            supports_native_tools: row.supports_native_tools,
            supports_grammar: row.supports_grammar,
            supports_streaming: true,
        };
        let rt = OpenAiCompatRuntime::new(transport, row.remote_model_name.clone(), capabilities);
        Ok(Arc::new(rt))
    }

    /// Fetch a candidate catalog + its detached signature from
    /// `catalog_url`/`signature_url`, verify the signature against
    /// [`Self::catalog_trusted_key`] (this build's compiled-in
    /// [`CATALOG_PUBLIC_KEY_HEX`] unless a test overrode it via
    /// [`RuntimeConfig::catalog_trusted_key_hex`]), refuse it if its
    /// `version` isn't strictly newer than what's currently in effect
    /// (anti-rollback — see [`valyria_model_registry::Catalog::
    /// verify_and_parse_signed`]), and only then persist it durably
    /// (atomic write) as the catalog every other `Runtime` method reads
    /// through [`effective_catalog`]. Nothing is written, and nothing
    /// already cached is disturbed, on any failure — signature, parse, or
    /// rollback alike.
    pub async fn catalog_refresh(
        &self,
        catalog_url: &str,
        signature_url: &str,
    ) -> Result<CatalogRefreshOutcome> {
        self.catalog_refresh_with(catalog_url, signature_url, &CatalogHttpFetcher::new()?)
            .await
    }

    /// [`Self::catalog_refresh`] with an injected [`Fetcher`] — production
    /// passes a real `HttpFetcher`; tests pass an in-memory one, or a
    /// real one pointed at a real local HTTP server, and use
    /// [`RuntimeConfig::catalog_trusted_key_hex`] to trust a throwaway
    /// keypair they actually hold the private half of, since this
    /// build's compiled-in key's private half deliberately exists
    /// nowhere (see [`CATALOG_PUBLIC_KEY_HEX`]'s own doc comment).
    pub async fn catalog_refresh_with<F: Fetcher>(
        &self,
        catalog_url: &str,
        signature_url: &str,
        fetcher: &F,
    ) -> Result<CatalogRefreshOutcome> {
        let current_version =
            effective_catalog(self.global.root(), &self.catalog_trusted_key).version();

        let catalog_bytes = fetch_whole(fetcher, catalog_url).await?;
        let sig_bytes = fetch_whole(fetcher, signature_url).await?;
        let signature_hex = String::from_utf8(sig_bytes)
            .map_err(|e| AppError::Repo(format!("signature file is not valid UTF-8: {e}")))?;

        let refreshed = Catalog::verify_and_parse_signed(
            &catalog_bytes,
            signature_hex.trim(),
            &self.catalog_trusted_key,
            current_version,
        )
        .map_err(|e| AppError::Repo(e.to_string()))?;

        let dir = catalog_dir(self.global.root());
        let io_err = |e: std::io::Error| AppError::Repo(format!("catalog refresh: {e}"));
        std::fs::create_dir_all(&dir).map_err(io_err)?;
        write_atomic(&dir.join(REFRESHED_CATALOG_JSON), &catalog_bytes).map_err(io_err)?;
        write_atomic(
            &dir.join(REFRESHED_CATALOG_SIG),
            signature_hex.trim().as_bytes(),
        )
        .map_err(io_err)?;

        Ok(CatalogRefreshOutcome {
            previous_version: current_version,
            new_version: refreshed.version(),
            model_count: refreshed.cards().len(),
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

fn unverified_probe_result(card: &ModelCard) -> valyria_model_store::ProbeResult {
    valyria_model_store::ProbeResult {
        loads: true,
        working_transport: card.transport_preference,
        tokens_per_sec: 0.0,
        measured_ram_bytes: card.requirement.min_ram_bytes,
    }
}

fn to_probe_err(id: &str, detail: String) -> valyria_model_store::ModelStoreError {
    valyria_model_store::ModelStoreError::Probe {
        id: id.to_string(),
        detail,
    }
}

/// The engine-agnostic half of a post-install probe, shared by every
/// `LocalModelServer` (`LlamaServerRuntime`, `MlxServerRuntime`, ...):
/// skip the chat round trip for a card with no chat-capable role (an
/// embedder/reranker never answers `/v1/chat/completions` — asking would
/// just hang, found live against `nomic-embed-text-v1.5`), otherwise send
/// one bounded trivial prompt and measure it. Always shuts the server
/// down before returning, success or failure.
///
/// `generate_timeout` is a caller-supplied bound rather than a shared
/// constant because it means two very different things per engine.
/// Confirmed live against a real, cold (never-before-fetched) `mlx_lm.
/// server`: its `/health` answers 200 as soon as the HTTP listener is up
/// — *not* once the model has actually finished downloading and
/// loading — so for a fresh MLX install the real wait happens inside
/// this generate call itself (the server queues the request until the
/// model is ready), observed taking several minutes for a multi-GB
/// model on a real network. For llama.cpp the weights are already local
/// disk by the time this runs (`ModelStore` downloaded them first), so a
/// short bound is the correct defense-in-depth there.
async fn run_probe_generate(
    rt: &dyn LocalModelServer,
    card: &ModelCard,
    generate_timeout: Duration,
) -> valyria_model_store::Result<valyria_model_store::ProbeResult> {
    let chat_capable = card
        .role_suitability
        .keys()
        .any(|r| !matches!(r, ModelRole::Embedder | ModelRole::Reranker));
    if !chat_capable {
        rt.shutdown().await;
        return Ok(unverified_probe_result(card));
    }

    let req = GenerateRequest::new(vec![Message::user("Reply with one short word.")])
        .with_sampling(SamplingParams {
            temperature: 0.0,
            top_p: 1.0,
            max_tokens: Some(8),
            stop: Vec::new(),
        });
    let started = std::time::Instant::now();
    let result =
        tokio::time::timeout(generate_timeout, rt.generate(req, CancellationToken::new())).await;
    // For a cold MLX install this `elapsed` includes the one-time
    // download+load wait, not just generation — so `tokens_per_sec`
    // below is a real but pessimistic lower bound for that case, not a
    // steady-state throughput figure. Left as-is rather than adding a
    // separate warm-up call: it is an honest measurement of what this
    // particular call actually took, and every later boot of the same
    // model is fast (see `MLX_PROBE_READY_TIMEOUT`'s doc comment).
    let elapsed = started.elapsed();
    rt.shutdown().await;

    let completion = match result {
        Ok(inner) => inner.map_err(|e| to_probe_err(&card.id, e.to_string()))?,
        Err(_) => {
            return Err(to_probe_err(
                &card.id,
                format!(
                    "model did not answer a trivial prompt within {}s",
                    generate_timeout.as_secs()
                ),
            ))
        }
    };
    if completion.text.trim().is_empty() && completion.tool_calls.is_empty() {
        return Err(to_probe_err(
            &card.id,
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

#[async_trait::async_trait]
impl valyria_model_store::Prober for ServerProber {
    async fn probe(
        &self,
        weights: &std::path::Path,
        card: &ModelCard,
    ) -> valyria_model_store::Result<valyria_model_store::ProbeResult> {
        match card.engine {
            EngineKind::LlamaCpp => {
                let binary = match resolve_or_install_engine(&self.global_root, &self.events).await
                {
                    Ok(bin) => bin,
                    Err(_) => return Ok(unverified_probe_result(card)),
                };
                let log_path = self
                    .global_root
                    .join("logs")
                    .join(format!("llama-probe-{}.log", card.id));
                let rt = LlamaServerRuntime::start_with_timeout(
                    binary,
                    weights.to_path_buf(),
                    card,
                    log_path,
                    Duration::from_secs(180),
                )
                .await
                .map_err(|e| to_probe_err(&card.id, e.to_string()))?;
                run_probe_generate(&rt, card, Duration::from_secs(60)).await
            }
            EngineKind::Mlx => {
                let python =
                    match resolve_or_install_mlx_engine(&self.global_root, &self.events).await {
                        Ok(p) => p,
                        Err(_) => return Ok(unverified_probe_result(card)),
                    };
                let log_path = self
                    .global_root
                    .join("logs")
                    .join(format!("mlx-probe-{}.log", card.id));
                // Unlike the llama.cpp branch above, this is the *first*
                // time this model's weights are fetched at all. Confirmed
                // live against a real, cold `mlx_lm.server`: its `/health`
                // answers 200 as soon as the HTTP listener is up, well
                // *before* the multi-GB Hugging Face download it still
                // has to do finishes — so `await_ready` below returns
                // quickly regardless, and the real wait happens inside
                // the *generate* call afterward, which the server queues
                // until the model actually finishes downloading and
                // loading (observed: several minutes for a ~4.3 GB model
                // on a real network, real `/v1/chat/completions` request
                // held open the whole time rather than erroring). Both
                // timeouts below are generously sized for that reason —
                // `await_ready`'s mainly as defense in depth against a
                // genuinely stuck process. Every later boot of the same
                // model (`model_activate`, restarts) hits `mlx_lm`/
                // `huggingface_hub`'s own on-disk cache and is fast; this
                // generous budget is install-time-only.
                const MLX_PROBE_READY_TIMEOUT: Duration = Duration::from_secs(30 * 60);
                const MLX_PROBE_GENERATE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
                let rt = MlxServerRuntime::start_with_timeout(
                    python,
                    weights.to_path_buf(),
                    card,
                    log_path,
                    MLX_PROBE_READY_TIMEOUT,
                )
                .await
                .map_err(|e| to_probe_err(&card.id, e.to_string()))?;
                run_probe_generate(&rt, card, MLX_PROBE_GENERATE_TIMEOUT).await
            }
        }
    }
}

/// M6: admits `id`/`role`/`footprint_bytes` into `pool`'s memory budget
/// and projects whatever the pool decided — a `resource_pressure` notice,
/// one `model_evicted` per victim (actually shutting down that victim's
/// live server and rebinding its role[s] to `NoModelRuntime::evicted`
/// first — this function's caller, in turn, boots the *new* server only
/// after this returns `Ok`, so a role is never left pointing at a server
/// that already stopped answering), and a final `model_loaded` — as real
/// protocol events. `Err(PoolError::WontFit)` means the caller must not
/// boot the server at all; nothing is evicted or rebound in that case
/// (`ModelPool::admit` never partially applies).
async fn admit_to_pool(
    pool: &tokio::sync::Mutex<ModelPool>,
    model_runtimes: &ModelRuntimeRegistry,
    orchestrator: &RoleRouter,
    events: &EventBus,
    id: &str,
    role: Role,
    footprint_bytes: u64,
) -> std::result::Result<(), PoolError> {
    let pool_events = pool.lock().await.admit(id, role, footprint_bytes)?;
    for ev in pool_events {
        match ev {
            PoolEvent::ResourcePressure {
                requested_bytes,
                budget_bytes,
            } => {
                let _ = events
                    .append(NewEvent::new(
                        EventKind::ResourcePressure,
                        serde_json::json!({
                            "requested_bytes": requested_bytes,
                            "budget_bytes": budget_bytes,
                        }),
                    ))
                    .await;
            }
            PoolEvent::Evicted {
                id: victim_id,
                reason,
            } => {
                let reason_str = match reason {
                    EvictReason::MemoryPressure => "memory_pressure",
                    EvictReason::Manual => "manual",
                };
                for victim_role in model_runtimes.roles_for_model(&victim_id).await {
                    if let Some(handle) = model_runtimes.take(victim_role).await {
                        orchestrator.bind_single(
                            victim_role,
                            victim_id.clone(),
                            Arc::new(NoModelRuntime::evicted(&victim_id)),
                        );
                        tokio::spawn(async move { handle.shutdown().await });
                    }
                }
                let _ = events
                    .append(NewEvent::new(
                        EventKind::ModelEvicted,
                        serde_json::json!({ "id": victim_id, "reason": reason_str }),
                    ))
                    .await;
            }
            PoolEvent::Loaded {
                id,
                footprint_bytes,
            } => {
                let _ = events
                    .append(NewEvent::new(
                        EventKind::ModelLoaded,
                        serde_json::json!({ "id": id, "footprint_bytes": footprint_bytes }),
                    ))
                    .await;
            }
        }
    }
    Ok(())
}

const REFRESHED_CATALOG_JSON: &str = "refreshed.json";
const REFRESHED_CATALOG_SIG: &str = "refreshed.json.sig";

fn catalog_dir(global_root: &std::path::Path) -> PathBuf {
    global_root.join("catalog")
}

/// The catalog actually in effect: a signed refresh persisted by a
/// previous [`Runtime::catalog_refresh`] if one exists and still
/// verifies, falling back to the embedded baseline otherwise (no refresh
/// has ever happened, or a previously-accepted one somehow no longer
/// verifies — e.g. this build's compiled-in trusted key rotated since it
/// was written). Never trusts the bytes on disk on their own: re-verifies
/// the signature every time, since that check is cheap and the
/// alternative is trusting unauthenticated state on every single catalog
/// read.
fn effective_catalog(
    global_root: &std::path::Path,
    trusted_key: &valyria_model_registry::VerifyingKey,
) -> Catalog {
    match load_refreshed_catalog(global_root, trusted_key) {
        Some(catalog) => catalog,
        None => embedded_or_empty(),
    }
}

fn embedded_or_empty() -> Catalog {
    Catalog::embedded().unwrap_or_else(|e| {
        tracing::error!(error = %e, "embedded catalog.json failed to parse");
        Catalog::from_cards(Vec::new())
    })
}

fn load_refreshed_catalog(
    global_root: &std::path::Path,
    trusted_key: &valyria_model_registry::VerifyingKey,
) -> Option<Catalog> {
    let dir = catalog_dir(global_root);
    let bytes = std::fs::read(dir.join(REFRESHED_CATALOG_JSON)).ok()?;
    let sig = std::fs::read_to_string(dir.join(REFRESHED_CATALOG_SIG)).ok()?;
    match Catalog::verify_and_parse(&bytes, sig.trim(), trusted_key) {
        Ok(catalog) => Some(catalog),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "persisted catalog refresh no longer verifies; falling back to the embedded catalog"
            );
            None
        }
    }
}

/// A [`Fetcher`] for `catalog_refresh` specifically — deliberately its
/// own type rather than a reuse of `valyria_model_store::HttpFetcher`.
/// That fetcher forces `https_only(true)`, the right call for weights
/// (a large binary blob whose only integrity check is a blake3 hash
/// *from the very catalog being fetched over that connection*, so the
/// transport is meaningfully part of the trust chain). A catalog refresh
/// is different: every byte is independently ed25519-verified against a
/// key baked into this binary, not learned from the connection at all —
/// transport security here is real defense in depth, not the trust
/// anchor, so requiring HTTPS specifically would rule out legitimate
/// internal/self-hosted catalog mirrors (and, confirmed live while
/// building this, even a plain local HTTP server used for testing)
/// without buying back any actual authenticity guarantee the signature
/// doesn't already provide.
#[derive(Debug, Clone)]
struct CatalogHttpFetcher {
    client: reqwest::Client,
}

impl CatalogHttpFetcher {
    fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .user_agent(concat!("valyria-app/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| {
                AppError::Repo(format!("building the catalog-refresh HTTP client: {e}"))
            })?;
        Ok(Self { client })
    }
}

#[async_trait::async_trait]
impl Fetcher for CatalogHttpFetcher {
    async fn head(
        &self,
        url: &str,
    ) -> valyria_model_store::Result<valyria_model_store::RemoteObject> {
        let resp = self
            .client
            .head(url)
            .send()
            .await
            .map_err(|e| catalog_fetch_err(url, e))?
            .error_for_status()
            .map_err(|e| catalog_fetch_err(url, e))?;
        let headers = resp.headers();
        let len = headers
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| catalog_fetch_err(url, "HEAD response had no usable Content-Length"))?;
        let etag = headers
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let supports_ranges = headers
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("bytes"))
            .unwrap_or(false);
        Ok(valyria_model_store::RemoteObject {
            len,
            etag,
            supports_ranges,
        })
    }

    async fn get_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> valyria_model_store::Result<Vec<u8>> {
        let last = end.saturating_sub(1).max(start);
        let resp = self
            .client
            .get(url)
            .header(reqwest::header::RANGE, format!("bytes={start}-{last}"))
            .send()
            .await
            .map_err(|e| catalog_fetch_err(url, e))?
            .error_for_status()
            .map_err(|e| catalog_fetch_err(url, e))?;
        let bytes = resp.bytes().await.map_err(|e| catalog_fetch_err(url, e))?;
        Ok(bytes.to_vec())
    }
}

fn catalog_fetch_err(url: &str, e: impl std::fmt::Display) -> valyria_model_store::ModelStoreError {
    valyria_model_store::ModelStoreError::Download {
        id: url.to_string(),
        detail: e.to_string(),
    }
}

/// Fetch the whole (small — a catalog and its signature are at most a
/// few KB) object at `url` via the same [`Fetcher`] seam `model_install`
/// uses for weights — `head` for the length, one `get_range` for
/// everything, no chunking or resume machinery needed at this size.
async fn fetch_whole<F: Fetcher>(fetcher: &F, url: &str) -> Result<Vec<u8>> {
    let head = fetcher
        .head(url)
        .await
        .map_err(|e| AppError::Repo(format!("HEAD {url} failed: {e}")))?;
    fetcher
        .get_range(url, 0, head.len)
        .await
        .map_err(|e| AppError::Repo(format!("GET {url} failed: {e}")))
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
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

/// The MLX sibling of [`resolve_or_install_engine`]: resolve (or, the
/// first time, provision) the `mlx-lm` venv under `~/.valyria/engines/
/// mlx/`, emitting the same `engine_install_progress`/`_completed`/
/// `_failed` events `llama.cpp` does. Provisioning a Python venv is a
/// handful of discrete blocking steps (`python -m venv`, two `pip`
/// invocations, an import check), not a byte-tracked download, so unlike
/// the llama.cpp path there is exactly one `engine_install_progress`
/// event (phase `"provisioning"`, no meaningful byte counts) rather than
/// a stream of them.
async fn resolve_or_install_mlx_engine(
    global_root: &std::path::Path,
    events: &Arc<EventBus>,
) -> Result<PathBuf> {
    let store = valyria_engine_store::MlxVenvStore::new(global_root);
    let version = valyria_engine_store::MLX_LM_VERSION;
    if let Some(python) = store.resolve(version) {
        return Ok(python);
    }

    let base_python = valyria_engine_store::find_system_python().ok_or_else(|| {
        AppError::Mlx(valyria_runtime_mlx::MlxError::EngineUnavailable(
            "no system python3 found on PATH to build the mlx venv from".into(),
        ))
    })?;

    let _ = events
        .append(NewEvent::new(
            EventKind::EngineInstallProgress,
            serde_json::json!({
                "component": "mlx-lm",
                "version": version,
                "phase": "provisioning",
                "downloaded_bytes": 0,
                "total_bytes": 0,
            }),
        ))
        .await;

    match store.provision(&base_python, version).await {
        Ok(python) => {
            let _ = events
                .append(NewEvent::new(
                    EventKind::EngineInstallCompleted,
                    serde_json::json!({ "component": "mlx-lm", "version": version }),
                ))
                .await;
            Ok(python)
        }
        Err(e) => {
            let _ = events
                .append(NewEvent::new(
                    EventKind::EngineInstallFailed,
                    serde_json::json!({
                        "component": "mlx-lm",
                        "version": version,
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
    catalog_trusted_key: &valyria_model_registry::VerifyingKey,
) -> Result<Arc<dyn LocalModelServer>> {
    let catalog = effective_catalog(global_root, catalog_trusted_key);
    let card = catalog
        .get(model_id)
        .ok_or_else(|| AppError::Repo(format!("no catalog model `{model_id}`")))?
        .clone();

    match card.engine {
        EngineKind::LlamaCpp => {
            let weights = model_store.weights_path(model_id)?;
            let binary = resolve_or_install_engine(global_root, events).await?;
            let log_path = global_root
                .join("logs")
                .join(format!("llama-{model_id}.log"));
            let rt = LlamaServerRuntime::start(binary, weights, &card, log_path)
                .await
                .map_err(AppError::LlamaCpp)?;
            Ok(Arc::new(rt) as Arc<dyn LocalModelServer>)
        }
        EngineKind::Mlx => {
            // `weights_file` here is the upstream HF repo id, not a path
            // under this model's on-disk directory — see `EngineKind`'s
            // and `MLX_LAZY_DOWNLOAD_SENTINEL`'s doc comments. Read
            // straight from the manifest rather than through
            // `ModelStore::weights_path`, which would (wrongly) resolve
            // it against the model's local directory.
            let repo_id = PathBuf::from(model_store.manifest(model_id)?.weights_file);
            let python = resolve_or_install_mlx_engine(global_root, events).await?;
            let log_path = global_root.join("logs").join(format!("mlx-{model_id}.log"));
            let rt = MlxServerRuntime::start(python, repo_id, &card, log_path)
                .await
                .map_err(AppError::Mlx)?;
            Ok(Arc::new(rt) as Arc<dyn LocalModelServer>)
        }
    }
}

/// The background half of booting a model at `Runtime::open` time:
/// `model_server_starting` immediately, then `boot_model_server`, then
/// `rebind` the orchestrator to the result (a real server on success, a
/// `NoModelRuntime::failed` on failure) and emit `model_server_ready` /
/// `_failed`. `open()` never awaits this — it returns as soon as the task
/// is spawned, with the role already pointed at `NoModelRuntime::starting`.
/// The shared handles every model-boot path (`spawn_model_boot`,
/// `model_activate`) rebinds/notifies through — bundled into one struct so
/// passing them around stays a single argument rather than growing the
/// parameter list every time a new piece of shared state joins them.
#[derive(Clone)]
struct ModelBootHandles {
    orchestrator: Arc<RoleRouter>,
    model_runtimes: Arc<ModelRuntimeRegistry>,
    pool: Arc<tokio::sync::Mutex<ModelPool>>,
    events: Arc<EventBus>,
    catalog_trusted_key: valyria_model_registry::VerifyingKey,
}

fn spawn_model_boot(
    role: Role,
    model_id: String,
    global_root: PathBuf,
    model_store: ModelStore,
    handles: ModelBootHandles,
) {
    let ModelBootHandles {
        orchestrator,
        model_runtimes,
        pool,
        events,
        catalog_trusted_key,
    } = handles;
    tokio::spawn(async move {
        let footprint_bytes = match model_store.manifest(&model_id) {
            Ok(m) => m.size_bytes,
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
                return;
            }
        };
        if let Err(e) = admit_to_pool(
            &pool,
            &model_runtimes,
            &orchestrator,
            &events,
            &model_id,
            role,
            footprint_bytes,
        )
        .await
        {
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
                        "code": "model_pool.wont_fit",
                        "message": e.to_string(),
                    }),
                ))
                .await;
            return;
        }

        let _ = events
            .append(NewEvent::new(
                EventKind::ModelServerStarting,
                serde_json::json!({ "role": role.as_str(), "id": model_id }),
            ))
            .await;

        match boot_model_server(
            &global_root,
            &model_id,
            &model_store,
            &events,
            &catalog_trusted_key,
        )
        .await
        {
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

#[cfg(test)]
mod admit_to_pool_tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::{self, BoxStream, StreamExt};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use valyria_model::{Capabilities, Chunk, Completion, Health, ModelError};

    struct FakeServer {
        id: &'static str,
        shutdowns: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelRuntime for FakeServer {
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                context_length: 4096,
                supports_native_tools: true,
                supports_grammar: false,
                supports_streaming: false,
            }
        }
        async fn health(&self) -> Health {
            Health::Healthy
        }
        fn count_tokens(&self, text: &str) -> usize {
            text.len()
        }
        async fn generate(
            &self,
            _req: GenerateRequest,
            _cancel: CancellationToken,
        ) -> std::result::Result<Completion, ModelError> {
            unimplemented!()
        }
        fn stream(
            &self,
            _req: GenerateRequest,
            _cancel: CancellationToken,
        ) -> BoxStream<'static, std::result::Result<Chunk, ModelError>> {
            stream::empty().boxed()
        }
    }

    #[async_trait]
    impl LocalModelServer for FakeServer {
        async fn shutdown(&self) {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
        }
        fn model_id(&self) -> &str {
            self.id
        }
        fn port(&self) -> u16 {
            0
        }
    }

    fn bus() -> Arc<EventBus> {
        let store = Arc::new(Store::open_in_memory(valyria_events::MIGRATIONS).unwrap());
        Arc::new(EventBus::new(store))
    }

    const GB: u64 = 1_000_000_000;

    /// The core M6 exit criterion at the wiring level (`ModelPool` itself
    /// already proves the algorithm; this proves the *plumbing* around
    /// it): admitting a model that doesn't fit alongside an already-
    /// resident higher-priority one evicts the resident's *real* server —
    /// `shutdown` actually gets called, the role is rebound away from the
    /// dead handle, and both a `resource_pressure` and a `model_evicted`
    /// event land on the real event stream — not just a bookkeeping
    /// change inside `ModelPool`.
    #[tokio::test]
    async fn eviction_shuts_down_the_real_server_and_rebinds_the_role() {
        let pool = tokio::sync::Mutex::new(ModelPool::new(8 * GB));
        let registry = ModelRuntimeRegistry::new();
        let orchestrator = RoleRouter::new();
        let events = bus();

        let embed_shutdowns = Arc::new(AtomicUsize::new(0));
        let embed_handle = Arc::new(FakeServer {
            id: "embed",
            shutdowns: embed_shutdowns.clone(),
        });
        orchestrator.bind_single(Role::Embedder, "embed".to_string(), embed_handle.clone());
        registry
            .swap(Role::Embedder, "embed".to_string(), embed_handle)
            .await;
        admit_to_pool(
            &pool,
            &registry,
            &orchestrator,
            &events,
            "embed",
            Role::Embedder,
            6 * GB,
        )
        .await
        .unwrap();
        assert!(registry
            .roles_for_model("embed")
            .await
            .contains(&Role::Embedder));

        // A coder needs 6 GB too; used is 6, budget 8 → the embedder (lower
        // priority) must go to make room.
        admit_to_pool(
            &pool,
            &registry,
            &orchestrator,
            &events,
            "coder",
            Role::PrimaryCoder,
            6 * GB,
        )
        .await
        .unwrap();

        // The real server was actually told to shut down — not just
        // dropped from ModelPool's own bookkeeping.
        assert_eq!(embed_shutdowns.load(Ordering::SeqCst), 1);
        // The registry no longer thinks the embedder role is served by it.
        assert!(registry.roles_for_model("embed").await.is_empty());
        // The orchestrator's binding for the evicted role was replaced —
        // health now honestly reports unavailable rather than pointing at
        // a server that already stopped listening.
        let health = orchestrator
            .generate(
                Role::Embedder,
                GenerateRequest::new(vec![]),
                CancellationToken::new(),
            )
            .await;
        assert!(health.is_err(), "{health:?}");

        let recorded = events
            .replay_since(valyria_events::Seq::ZERO)
            .await
            .unwrap();
        let kinds: Vec<_> = recorded.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EventKind::ResourcePressure), "{kinds:?}");
        assert!(kinds.contains(&EventKind::ModelEvicted), "{kinds:?}");
        assert!(kinds.contains(&EventKind::ModelLoaded), "{kinds:?}");

        let evicted_payload = recorded
            .iter()
            .find(|e| e.kind == EventKind::ModelEvicted)
            .unwrap();
        assert_eq!(evicted_payload.payload["id"], "embed");
        assert_eq!(evicted_payload.payload["reason"], "memory_pressure");
    }

    /// A model too big for the whole budget is refused *before* anything
    /// is evicted or rebound — the caller never boots a server for it.
    #[tokio::test]
    async fn wont_fit_leaves_residents_and_bindings_untouched() {
        let pool = tokio::sync::Mutex::new(ModelPool::new(4 * GB));
        let registry = ModelRuntimeRegistry::new();
        let orchestrator = RoleRouter::new();
        let events = bus();

        let coder_shutdowns = Arc::new(AtomicUsize::new(0));
        let coder_handle = Arc::new(FakeServer {
            id: "coder",
            shutdowns: coder_shutdowns.clone(),
        });
        orchestrator.bind_single(
            Role::PrimaryCoder,
            "coder".to_string(),
            coder_handle.clone(),
        );
        registry
            .swap(Role::PrimaryCoder, "coder".to_string(), coder_handle)
            .await;
        admit_to_pool(
            &pool,
            &registry,
            &orchestrator,
            &events,
            "coder",
            Role::PrimaryCoder,
            3 * GB,
        )
        .await
        .unwrap();

        let err = admit_to_pool(
            &pool,
            &registry,
            &orchestrator,
            &events,
            "huge",
            Role::Embedder,
            10 * GB,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, PoolError::WontFit { .. }));

        assert_eq!(coder_shutdowns.load(Ordering::SeqCst), 0);
        assert!(registry
            .roles_for_model("coder")
            .await
            .contains(&Role::PrimaryCoder));
    }
}
