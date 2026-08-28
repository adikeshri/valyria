//! `valyria doctor` (§4.28): a battery of environment checks, each
//! returning a status, a plain-language detail, and — when it is not a
//! pass — a concrete remediation.
//!
//! Every check is a free function taking only what it needs, so the
//! "diagnoses a battery of deliberately broken environments" exit
//! criterion is tested by handing each function a broken input directly.
//! [`Doctor::run`] is the composition that the protocol / CLI call.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use valyria_hardware::HardwareReport;
use valyria_index::IndexStore;
use valyria_model_store::InstalledModelStore;

/// Ordered worst-to-best so `max` gives the overall summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckStatus::Pass => "pass",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub remediation: Option<String>,
}

impl DoctorCheck {
    fn pass(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Pass,
            detail: detail.into(),
            remediation: None,
        }
    }
    fn warn(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warn,
            detail: detail.into(),
            remediation: Some(fix.into()),
        }
    }
    fn fail(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Fail,
            detail: detail.into(),
            remediation: Some(fix.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn summary(&self) -> CheckStatus {
        self.checks
            .iter()
            .map(|c| c.status)
            .max()
            .unwrap_or(CheckStatus::Pass)
    }
}

// --- individual checks ------------------------------------------------------

pub fn check_runtime() -> DoctorCheck {
    DoctorCheck::pass(
        "runtime",
        format!(
            "valyria {}, protocol {}",
            env!("CARGO_PKG_VERSION"),
            valyria_protocol::PROTOCOL_VERSION
        ),
    )
}

pub fn check_data_dir(data_dir: &Path) -> DoctorCheck {
    if !data_dir.exists() {
        return DoctorCheck::fail(
            "data_dir",
            format!("{} does not exist", data_dir.display()),
            "run any `valyria` command in the workspace to create it, or check the --workspace path",
        );
    }
    let probe = data_dir.join(".doctor-write-probe");
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            DoctorCheck::pass("data_dir", format!("{} is writable", data_dir.display()))
        }
        Err(e) => DoctorCheck::fail(
            "data_dir",
            format!("{} is not writable: {e}", data_dir.display()),
            "fix the directory's permissions (the runtime needs to write workspace.db and blobs here)",
        ),
    }
}

pub fn check_workspace_db(data_dir: &Path) -> DoctorCheck {
    let path = data_dir.join("workspace.db");
    if !path.exists() {
        return DoctorCheck::warn(
            "workspace_db",
            "no workspace.db yet",
            "run `valyria run \"...\"` once to initialise the workspace database",
        );
    }
    let conn = match rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            return DoctorCheck::fail(
                "workspace_db",
                format!("cannot open {}: {e}", path.display()),
                "the database file is unreadable — restore it from a backup or remove `.valyria/` to start fresh",
            )
        }
    };
    match conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)) {
        Ok(s) if s == "ok" => DoctorCheck::pass("workspace_db", "integrity_check: ok"),
        Ok(s) => DoctorCheck::fail(
            "workspace_db",
            format!("integrity_check reported: {s}"),
            "the database is corrupt — `valyria task` history may be lost; remove `.valyria/workspace.db` to start fresh",
        ),
        Err(e) => DoctorCheck::fail(
            "workspace_db",
            format!("integrity_check failed: {e}"),
            "the file is not a valid SQLite database — remove `.valyria/workspace.db` to start fresh",
        ),
    }
}

/// < 100 MiB free ⇒ fail, < 1 GiB ⇒ warn.
pub fn classify_disk(available_bytes: u64) -> CheckStatus {
    const MIB: u64 = 1024 * 1024;
    if available_bytes < 100 * MIB {
        CheckStatus::Fail
    } else if available_bytes < 1024 * MIB {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    }
}

pub fn check_disk(hw: &HardwareReport) -> DoctorCheck {
    let avail = hw.disk.available_bytes;
    let gib = avail as f64 / (1024.0 * 1024.0 * 1024.0);
    match classify_disk(avail) {
        CheckStatus::Pass => DoctorCheck::pass("disk_space", format!("{gib:.1} GiB free")),
        CheckStatus::Warn => DoctorCheck::warn(
            "disk_space",
            format!("only {gib:.1} GiB free"),
            "free space before installing a model or indexing a large repository",
        ),
        CheckStatus::Fail => DoctorCheck::fail(
            "disk_space",
            format!("only {gib:.2} GiB free"),
            "free disk space — the runtime cannot safely write the index or the change ledger",
        ),
    }
}

pub fn check_git(workspace: &Path) -> DoctorCheck {
    match valyria_git::Repo::open(workspace) {
        Ok(repo) => match repo.head_info() {
            Ok(head) => DoctorCheck::pass(
                "git",
                format!(
                    "on {}",
                    head.branch.as_deref().unwrap_or("a detached HEAD")
                ),
            ),
            Err(e) => DoctorCheck::warn(
                "git",
                format!("repository present but HEAD is unresolvable: {e}"),
                "make an initial commit, or check for an interrupted rebase/merge",
            ),
        },
        Err(_) => DoctorCheck::warn(
            "git",
            "not a git repository",
            "run `git init` — history-aware search ranking and change classification are limited without it",
        ),
    }
}

pub fn check_sandbox() -> DoctorCheck {
    if cfg!(target_os = "macos") {
        DoctorCheck::pass(
            "sandbox",
            "Seatbelt available (filesystem + network confinement for spawned commands)",
        )
    } else {
        DoctorCheck::warn(
            "sandbox",
            "no OS-level confinement on this platform — commands run under PermissiveSandbox",
            "Linux/Windows confinement is not implemented yet; run in a container or VM for isolation",
        )
    }
}

/// Linux inotify watch ceiling: < 8192 is too low for a large repo.
pub fn classify_watch_limit(limit: u64) -> CheckStatus {
    if limit < 8_192 {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    }
}

pub fn check_watcher_limits() -> DoctorCheck {
    let path = "/proc/sys/fs/inotify/max_user_watches";
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let limit: u64 = raw.trim().parse().unwrap_or(0);
            match classify_watch_limit(limit) {
                CheckStatus::Pass => DoctorCheck::pass(
                    "watcher_limits",
                    format!("inotify max_user_watches = {limit}"),
                ),
                _ => DoctorCheck::warn(
                    "watcher_limits",
                    format!("inotify max_user_watches = {limit} — low for large repositories"),
                    "raise it: `sudo sysctl fs.inotify.max_user_watches=524288`",
                ),
            }
        }
        // Not Linux (or the file is unavailable): incremental indexing
        // falls back to polling, which works, just less promptly.
        Err(_) => DoctorCheck::pass(
            "watcher_limits",
            "not applicable on this platform (filesystem polling fallback)",
        ),
    }
}

pub fn check_permission_config(
    global_cfg: Option<&Path>,
    workspace_cfg: Option<&Path>,
) -> DoctorCheck {
    let mut resolver = valyria_config::ConfigResolver::new().env_vars(vec![]);
    if let Some(p) = global_cfg.filter(|p| p.exists()) {
        resolver = resolver.global_path(p);
    }
    if let Some(p) = workspace_cfg.filter(|p| p.exists()) {
        resolver = resolver.workspace_path(p);
    }
    match resolver.resolve() {
        Ok(resolved) => DoctorCheck::pass(
            "permission_config",
            format!(
                "permission mode: {:?} (within the policy floor)",
                resolved.settings.permission.mode
            ),
        ),
        Err(e) => DoctorCheck::fail(
            "permission_config",
            format!("config does not satisfy the policy floor: {e}"),
            "edit `.valyria/config.toml` — config can tighten access below the floor, never loosen past it",
        ),
    }
}

pub async fn check_index(index: &IndexStore) -> DoctorCheck {
    match index.current().await {
        Ok(Some(gen)) => DoctorCheck::pass(
            "index",
            format!("generation {} present", gen.generation),
        ),
        Ok(None) => DoctorCheck::warn(
            "index",
            "no repository index has been built",
            "the agent loop bootstraps this on first run; nothing to do if you have not run a task yet",
        ),
        Err(e) => DoctorCheck::fail(
            "index",
            format!("index metadata is unreadable: {e}"),
            "remove `.valyria/index/` and re-run — it will be rebuilt",
        ),
    }
}

pub async fn check_models(models: &InstalledModelStore) -> DoctorCheck {
    match models.list().await {
        Ok(rows) if rows.is_empty() => DoctorCheck::warn(
            "models",
            "no models installed",
            "run `valyria model install <id>` — the agent loop needs a local model (the fake model is test-only)",
        ),
        Ok(rows) => DoctorCheck::pass(
            "models",
            format!("{} model(s) installed", rows.len()),
        ),
        Err(e) => DoctorCheck::fail(
            "models",
            format!("installed-model index is unreadable: {e}"),
            "remove `~/.valyria/global.db` (it is a rebuildable index over the manifests) and re-run",
        ),
    }
}

// --- composition ----------------------------------------------------------

pub struct Doctor {
    pub data_dir: PathBuf,
    pub workspace_path: PathBuf,
    pub global_root: PathBuf,
    pub index: Arc<IndexStore>,
    pub models: Arc<InstalledModelStore>,
}

impl std::fmt::Debug for Doctor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Doctor")
            .field("workspace_path", &self.workspace_path)
            .finish_non_exhaustive()
    }
}

impl Doctor {
    pub async fn run(&self) -> DoctorReport {
        let hw = valyria_hardware::probe();
        let checks = vec![
            check_runtime(),
            check_data_dir(&self.data_dir),
            check_workspace_db(&self.data_dir),
            check_disk(&hw),
            check_git(&self.workspace_path),
            check_sandbox(),
            check_watcher_limits(),
            check_permission_config(
                Some(&self.global_root.join("config.toml")),
                Some(&self.data_dir.join("config.toml")),
            ),
            check_index(&self.index).await,
            check_models(&self.models).await,
        ];
        DoctorReport { checks }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_thresholds() {
        assert_eq!(classify_disk(50 * 1024 * 1024), CheckStatus::Fail);
        assert_eq!(classify_disk(500 * 1024 * 1024), CheckStatus::Warn);
        assert_eq!(classify_disk(8 * 1024 * 1024 * 1024), CheckStatus::Pass);
    }

    #[test]
    fn watch_limit_thresholds() {
        assert_eq!(classify_watch_limit(1024), CheckStatus::Warn);
        assert_eq!(classify_watch_limit(524_288), CheckStatus::Pass);
    }

    #[test]
    fn summary_is_the_worst_status() {
        let report = DoctorReport {
            checks: vec![
                DoctorCheck::pass("a", "ok"),
                DoctorCheck::warn("b", "hm", "fix"),
                DoctorCheck::pass("c", "ok"),
            ],
        };
        assert_eq!(report.summary(), CheckStatus::Warn);
    }

    #[test]
    fn missing_data_dir_fails() {
        let c = check_data_dir(Path::new("/no/such/valyria/dir"));
        assert_eq!(c.status, CheckStatus::Fail);
    }

    #[test]
    fn corrupt_workspace_db_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("workspace.db"), b"this is not sqlite").unwrap();
        let c = check_workspace_db(dir.path());
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.remediation.is_some());
    }

    #[test]
    fn absent_workspace_db_only_warns() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(check_workspace_db(dir.path()).status, CheckStatus::Warn);
    }

    #[test]
    fn non_git_workspace_warns_not_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(check_git(dir.path()).status, CheckStatus::Warn);
    }
}
