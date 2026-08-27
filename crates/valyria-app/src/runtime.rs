//! `Runtime`: wires every already-built subsystem into one embedded agent
//! runtime for a single workspace (§4.1, §4.23). This is the composition
//! root — the one place in the whole workspace allowed to know about every
//! layer at once, so that `valyria-cli` never has to.

use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::OptionalExtension;
use valyria_agent::AgentDriver;
use valyria_context::ContextAssembler;
use valyria_events::EventBus;
use valyria_ledger::Ledger;
use valyria_orchestrator::{Orchestrator, Role};
use valyria_permissions::PermissionEngine;
use valyria_runtime_fake::{FakeModelRuntime, Scenario};
use valyria_sandbox::{detect_platform_launcher, ProcessLauncher, SandboxProfile};
use valyria_store::Store;
use valyria_task::{Budget, Task, TaskManager};
use valyria_tools::ToolRuntime;
use valyria_types::{AgentState, PermissionMode, TaskId, WorkspaceId};
use valyria_util::{CancellationToken, Clock, SystemClock};
use valyria_vfs::WorkspaceRoot;

use crate::error::{AppError, Result};
use crate::migrations::workspace_migrations;

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
        }
    }

    pub fn with_data_dir(mut self, data_dir: impl Into<PathBuf>) -> Self {
        self.data_dir = data_dir.into();
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
    workspace_id: WorkspaceId,
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

        let mut orchestrator = Orchestrator::new();
        orchestrator.bind(
            Role::PrimaryCoder,
            Arc::new(FakeModelRuntime::from_scenario(config.scenario)),
        );
        let orchestrator = Arc::new(orchestrator);

        let context = Arc::new(ContextAssembler::new(tool_runtime.clone()));
        let hash_cache = Arc::new(valyria_vfs::HashCache::new());
        let launcher: Arc<dyn ProcessLauncher> = Arc::from(detect_platform_launcher());
        let sandbox_profile = SandboxProfile::new().allow_write(workspace_root.as_path());

        let tasks = Arc::new(TaskManager::new(
            store.clone(),
            events.clone(),
            clock.clone(),
        ));

        let driver = Arc::new(AgentDriver::new(
            tasks.clone(),
            tool_runtime,
            orchestrator,
            context,
            ledger,
            engine,
            workspace_root,
            hash_cache,
            clock,
            launcher,
            sandbox_profile,
        ));

        Ok(Self {
            events,
            tasks,
            driver,
            workspace_id,
        })
    }

    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    pub fn events(&self) -> Arc<EventBus> {
        self.events.clone()
    }

    pub async fn create_and_start_task(&self, objective: String) -> Result<TaskId> {
        let task = self
            .tasks
            .create(self.workspace_id, objective, Budget::default())
            .await?;
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
        self.driver.resolve_permission(task_id, approve).await?;
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

    fn spawn_driver(&self, task_id: TaskId) {
        let driver = self.driver.clone();
        tokio::spawn(async move {
            if let Err(error) = driver.run(task_id, CancellationToken::new()).await {
                tracing::error!(%task_id, %error, "agent driver exited with an error");
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
