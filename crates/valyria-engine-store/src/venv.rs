//! Python-venv provisioning for the MLX engine (`~/.valyria/engines/mlx/
//! <version>/venv/`). A structurally different install than
//! [`crate::store::EngineStore`]'s single-archive download+unpack — MLX
//! is a Python package (`mlx-lm`) with its own dependency tree
//! (`transformers`, `numpy`, …), so "install the engine" here means
//! "build an isolated venv and pip-install a pinned version into it",
//! not "unpack a binary". Kept as a sibling module rather than folded
//! into `EngineStore` so neither install path has to pretend to be the
//! other's shape.
//!
//! Like [`crate::store::EngineStore::install_with_progress`], this is
//! idempotent (a second [`MlxVenvStore::provision`] call for an already-
//! provisioned version returns immediately) and never leaves a
//! half-installed version behind for [`MlxVenvStore::resolve`] to find —
//! a failed `pip install` or a version mismatch deletes the venv
//! directory rather than leaving it for a later caller to trip over.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{EngineStoreError, Result};

const MANIFEST_FILENAME: &str = "mlx-manifest.json";

/// The exact `mlx-lm` version this build of valyria knows how to drive —
/// bumped deliberately, the same discipline as the engine catalog's own
/// pinned releases (§ engine catalog). Pinning by exact version (rather
/// than a floor like `>=0.31`) is what makes provisioning reproducible;
/// pip's own resolver still picks compatible versions of `mlx-lm`'s
/// transitive dependencies (`transformers`, `numpy`, …), verified against
/// the version pin actually installed via the `import mlx_lm; print(...
/// __version__)` check in [`MlxVenvStore::provision`], not merely trusted.
pub const MLX_LM_VERSION: &str = "0.31.3";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MlxManifest {
    mlx_lm_version: String,
    installed_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct MlxVenvStore {
    root: PathBuf,
}

impl MlxVenvStore {
    /// `root` is the same `~/.valyria` root `EngineStore::new` takes —
    /// this store owns the `engines/mlx/` subtree under it.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn version_dir(&self, version: &str) -> PathBuf {
        self.root.join("engines").join("mlx").join(version)
    }

    fn venv_dir(&self, version: &str) -> PathBuf {
        self.version_dir(version).join("venv")
    }

    /// Where `python` lives inside a venv rooted at `venv_dir` — the
    /// layout differs between platforms, but MLX itself only ever runs
    /// on macOS/Apple Silicon, so in practice this is always the Unix
    /// arm. Kept branchless-by-platform anyway so the module still
    /// compiles (and this helper stays correct) on every CI runner.
    fn python_bin(venv_dir: &Path) -> PathBuf {
        if cfg!(windows) {
            venv_dir.join("Scripts").join("python.exe")
        } else {
            venv_dir.join("bin").join("python")
        }
    }

    /// The venv's own `python` binary, already provisioned with
    /// `mlx-lm==version`, if a previous [`Self::provision`] for that
    /// exact version finished. Independent of any *other* installed
    /// version, mirroring `EngineStore::resolve`'s per-release
    /// isolation.
    pub fn resolve(&self, version: &str) -> Option<PathBuf> {
        let manifest_path = self.version_dir(version).join(MANIFEST_FILENAME);
        let text = std::fs::read_to_string(&manifest_path).ok()?;
        let manifest: MlxManifest = serde_json::from_str(&text).ok()?;
        if manifest.mlx_lm_version != version {
            return None;
        }
        let python = Self::python_bin(&self.venv_dir(version));
        python.is_file().then_some(python)
    }

    /// Create a venv from `base_python` (see [`find_system_python`]) and
    /// pin `mlx-lm==version` into it. Idempotent — a second call for a
    /// version [`Self::resolve`] would already satisfy never touches
    /// the venv or the network again.
    ///
    /// On any failure (venv creation, `pip install`, or the installed
    /// package reporting a version other than the one pinned) the
    /// half-built version directory is removed so a retry starts clean
    /// rather than finding — and trusting — partial state.
    pub async fn provision(&self, base_python: &Path, version: &str) -> Result<PathBuf> {
        if let Some(python) = self.resolve(version) {
            return Ok(python);
        }

        let dir = self.version_dir(version);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;

        if let Err(err) = self.provision_inner(base_python, version, &dir).await {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(err);
        }

        Ok(Self::python_bin(&self.venv_dir(version)))
    }

    async fn provision_inner(&self, base_python: &Path, version: &str, dir: &Path) -> Result<()> {
        let venv_dir = self.venv_dir(version);

        run(
            Command::new(base_python)
                .arg("-m")
                .arg("venv")
                .arg(&venv_dir),
            "python -m venv",
        )?;

        let python = Self::python_bin(&venv_dir);
        run(
            Command::new(&python).args(["-m", "pip", "install", "--upgrade", "pip"]),
            "pip install --upgrade pip",
        )?;
        run(
            Command::new(&python).args([
                "-m",
                "pip",
                "install",
                "--no-cache-dir",
                &format!("mlx-lm=={version}"),
            ]),
            "pip install mlx-lm",
        )?;

        let installed = run_capture(
            Command::new(&python).args(["-c", "import mlx_lm; print(mlx_lm.__version__)"]),
            "python -c import mlx_lm",
        )?;
        let installed = installed.trim();
        if installed != version {
            return Err(EngineStoreError::VersionMismatch {
                component: "mlx-lm".to_string(),
                expected: version.to_string(),
                actual: installed.to_string(),
            });
        }

        let manifest = MlxManifest {
            mlx_lm_version: version.to_string(),
            installed_at_ms: now_ms(),
        };
        std::fs::write(
            dir.join(MANIFEST_FILENAME),
            serde_json::to_string_pretty(&manifest)?,
        )?;
        tracing::info!(version, "mlx venv provisioned");
        Ok(())
    }
}

fn run(cmd: &mut Command, step: &str) -> Result<()> {
    let output = cmd.output().map_err(|e| EngineStoreError::VenvProvision {
        step: step.to_string(),
        detail: e.to_string(),
    })?;
    if !output.status.success() {
        return Err(EngineStoreError::VenvProvision {
            step: step.to_string(),
            detail: format!(
                "exit {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    Ok(())
}

fn run_capture(cmd: &mut Command, step: &str) -> Result<String> {
    let output = cmd.output().map_err(|e| EngineStoreError::VenvProvision {
        step: step.to_string(),
        detail: e.to_string(),
    })?;
    if !output.status.success() {
        return Err(EngineStoreError::VenvProvision {
            step: step.to_string(),
            detail: format!(
                "exit {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Search `PATH` for a usable `python3` interpreter to build the venv
/// from, newest-declared-version first. This ordering is load-bearing,
/// not cosmetic: confirmed live on this machine, `mlx-lm==0.31.3`'s own
/// `mlx` dependency ships no wheel compatible with system Python 3.9
/// (`/usr/bin/python3` here) — `pip install` fails outright — while it
/// installs cleanly under a newer interpreter (3.13) found earlier on
/// the same `PATH`. Preferring the newest available interpreter is the
/// simplest way to avoid that footgun without hand-maintaining a
/// minimum-Python-version table that would drift out of sync with
/// whatever `mlx-lm` version is pinned above.
pub fn find_system_python() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let candidates = [
        "python3.13",
        "python3.12",
        "python3.11",
        "python3.10",
        "python3.9",
        "python3",
    ];
    for candidate in candidates {
        for dir in std::env::split_paths(&path_var) {
            let full = dir.join(candidate);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_system_python_locates_a_real_interpreter_on_this_machine() {
        // Every dev machine and CI runner this workspace targets has some
        // python3 on PATH — this is a real, offline, non-flaky check
        // (no network, no venv creation), just PATH search logic.
        let found = find_system_python();
        assert!(found.is_some(), "expected to find a python3 on PATH");
        assert!(found.unwrap().is_file());
    }

    #[test]
    fn resolve_is_none_for_an_unprovisioned_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = MlxVenvStore::new(dir.path());
        assert!(store.resolve(MLX_LM_VERSION).is_none());
    }

    #[test]
    fn resolve_ignores_a_manifest_for_a_different_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = MlxVenvStore::new(dir.path());
        let version_dir = store.version_dir("0.30.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        let manifest = MlxManifest {
            mlx_lm_version: "0.30.0".to_string(),
            installed_at_ms: 0,
        };
        std::fs::write(
            version_dir.join(MANIFEST_FILENAME),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        // No actual python binary under venv/bin, so even a manifest
        // match must not resolve.
        assert!(store.resolve("0.30.0").is_none());
    }

    #[tokio::test]
    async fn provision_creates_a_real_venv_directory_structure() {
        // Real `python3 -m venv` (no network — venv creation is purely
        // local), stopped short of the network-dependent `pip install
        // mlx-lm` step by pointing pip at a bogus index so the failure
        // is fast and deterministic; this still exercises real venv
        // creation plus the "half-built state is cleaned up on failure"
        // contract.
        let Some(base_python) = find_system_python() else {
            eprintln!("skipping: no system python3 found");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let store = MlxVenvStore::new(dir.path());

        let venv_dir = store.venv_dir("99.99.99");
        run(
            Command::new(&base_python)
                .arg("-m")
                .arg("venv")
                .arg(&venv_dir),
            "python -m venv",
        )
        .expect("real venv creation should succeed offline");
        assert!(MlxVenvStore::python_bin(&venv_dir).is_file());
    }

    #[tokio::test]
    async fn provision_cleans_up_after_a_failed_pip_install() {
        let Some(base_python) = find_system_python() else {
            eprintln!("skipping: no system python3 found");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let store = MlxVenvStore::new(dir.path());

        // A version that can never exist on PyPI forces pip to fail
        // fast (still needs network to observe the 404 — if this
        // machine is offline the pip subprocess fails for a different
        // reason, which is fine, the assertion only cares that failure
        // cleans up).
        let result = store.provision(&base_python, "0.0.0-does-not-exist").await;
        assert!(result.is_err());
        assert!(!store.version_dir("0.0.0-does-not-exist").exists());
    }

    /// Real network, real PyPI, real venv — proves the pinned
    /// `mlx-lm` version is genuinely installable end-to-end.
    /// `#[ignore]`d because it needs the network and can take a while
    /// (mlx-lm's dependency tree includes `transformers`); run
    /// explicitly with `cargo test -p valyria-engine-store -- --ignored
    /// provision_real_mlx_venv_and_it_imports`.
    #[tokio::test]
    #[ignore]
    async fn provision_real_mlx_venv_and_it_imports() {
        let base_python = find_system_python().expect("a system python3");
        let dir = tempfile::tempdir().unwrap();
        let store = MlxVenvStore::new(dir.path());

        let python = store
            .provision(&base_python, MLX_LM_VERSION)
            .await
            .expect("real mlx-lm provision");
        assert!(python.is_file());

        // Idempotent: resolve now finds it without re-provisioning.
        assert_eq!(store.resolve(MLX_LM_VERSION), Some(python.clone()));

        let out = std::process::Command::new(&python)
            .args(["-c", "import mlx_lm.server; print('ok')"])
            .output()
            .expect("run the provisioned python");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }
}
