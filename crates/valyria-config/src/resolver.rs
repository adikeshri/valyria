//! Ties the layers together in the resolution order the build plan
//! specifies (§4.3): compiled defaults -> global -> workspace -> env ->
//! per-task overrides, later winning, every leaf's origin recorded.

use std::path::{Path, PathBuf};

use crate::env_layer::env_layer;
use crate::error::{ConfigError, Result};
use crate::floor::{validate_floor, PolicyFloor};
use crate::merge::LayeredConfig;
use crate::origin::{ConfigOrigin, OriginMap};
use crate::settings::Settings;

#[derive(Debug)]
pub struct Resolved {
    pub settings: Settings,
    origins: OriginMap,
}

impl Resolved {
    /// Where did the effective value at `dotted_path` (e.g.
    /// `"network.internet"`) come from?
    pub fn origin_of(&self, dotted_path: &str) -> Option<ConfigOrigin> {
        self.origins.get(dotted_path).copied()
    }
}

#[derive(Default)]
pub struct ConfigResolver {
    global_path: Option<PathBuf>,
    workspace_path: Option<PathBuf>,
    env_vars: Option<Vec<(String, String)>>,
    override_toml: Option<String>,
    floor: Option<PolicyFloor>,
}

impl ConfigResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn global_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.global_path = Some(path.into());
        self
    }

    pub fn workspace_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.workspace_path = Some(path.into());
        self
    }

    /// Inject env vars explicitly (tests use this instead of touching real
    /// process env, which is global mutable state and flaky under parallel
    /// test execution). Omitted in production use, where [`Self::resolve`]
    /// reads `std::env::vars()`.
    pub fn env_vars(mut self, vars: Vec<(String, String)>) -> Self {
        self.env_vars = Some(vars);
        self
    }

    pub fn override_toml(mut self, toml_str: impl Into<String>) -> Self {
        self.override_toml = Some(toml_str.into());
        self
    }

    pub fn floor(mut self, floor: PolicyFloor) -> Self {
        self.floor = Some(floor);
        self
    }

    pub fn resolve(&self) -> Result<Resolved> {
        let mut layered = LayeredConfig::new();

        let default_toml = toml::Value::try_from(Settings::default())
            .expect("Settings::default() always serializes");
        layered.apply_layer(default_toml, ConfigOrigin::Default);

        if let Some(path) = &self.global_path {
            apply_file_layer(&mut layered, path, ConfigOrigin::Global)?;
        }
        if let Some(path) = &self.workspace_path {
            apply_file_layer(&mut layered, path, ConfigOrigin::Workspace)?;
        }

        let env_vars = self
            .env_vars
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        layered.apply_layer(env_layer(env_vars), ConfigOrigin::Env);

        if let Some(raw) = &self.override_toml {
            let value: toml::Value = toml::from_str(raw).map_err(|source| ConfigError::Toml {
                path: "<override>".to_string(),
                source,
            })?;
            layered.apply_layer(value, ConfigOrigin::Override);
        }

        // Round-trip through a TOML string rather than relying on
        // `Value -> T` deserialization plumbing directly: it's the same
        // path `toml::from_str` uses elsewhere in this crate, so behavior
        // (e.g. `#[serde(default)]` handling) stays consistent everywhere.
        let merged_str = toml::to_string(&layered.merged).expect("merged value always serializes");
        let settings: Settings =
            toml::from_str(&merged_str).map_err(|source| ConfigError::Deserialize {
                path: "<merged>",
                source,
            })?;

        let floor = self.floor.as_ref();
        let default_floor = PolicyFloor::default();
        validate_floor(&settings, floor.unwrap_or(&default_floor))?;

        Ok(Resolved {
            settings,
            origins: layered.origins,
        })
    }
}

fn apply_file_layer(layered: &mut LayeredConfig, path: &Path, source: ConfigOrigin) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let value: toml::Value = toml::from_str(&text).map_err(|e| ConfigError::Toml {
        path: path.display().to_string(),
        source: e,
    })?;
    layered.apply_layer(value, source);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_types::{Access, PermissionMode};

    #[test]
    fn resolves_to_defaults_with_no_layers() {
        let resolved = ConfigResolver::new().env_vars(vec![]).resolve().unwrap();
        assert_eq!(resolved.settings, Settings::default());
        assert_eq!(
            resolved.origin_of("permission.mode"),
            Some(ConfigOrigin::Default)
        );
    }

    #[test]
    fn workspace_file_overrides_global_file() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.toml");
        let workspace = dir.path().join("workspace.toml");
        std::fs::write(&global, "[permission]\nmode = \"manual\"\n").unwrap();
        std::fs::write(&workspace, "[permission]\nmode = \"autonomous\"\n").unwrap();

        let resolved = ConfigResolver::new()
            .global_path(&global)
            .workspace_path(&workspace)
            .env_vars(vec![])
            .resolve()
            .unwrap();

        assert_eq!(
            resolved.settings.permission.mode,
            PermissionMode::Autonomous
        );
        assert_eq!(
            resolved.origin_of("permission.mode"),
            Some(ConfigOrigin::Workspace)
        );
    }

    #[test]
    fn env_overrides_files() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.toml");
        std::fs::write(&workspace, "[permission]\nmode = \"autonomous\"\n").unwrap();

        let resolved = ConfigResolver::new()
            .workspace_path(&workspace)
            .env_vars(vec![(
                "VALYRIA_PERMISSION_MODE".to_string(),
                "manual".to_string(),
            )])
            .resolve()
            .unwrap();

        assert_eq!(resolved.settings.permission.mode, PermissionMode::Manual);
        assert_eq!(
            resolved.origin_of("permission.mode"),
            Some(ConfigOrigin::Env)
        );
    }

    #[test]
    fn per_task_override_wins_over_everything() {
        let resolved = ConfigResolver::new()
            .env_vars(vec![(
                "VALYRIA_PERMISSION_MODE".to_string(),
                "manual".to_string(),
            )])
            .override_toml("[permission]\nmode = \"autonomous\"\n")
            .resolve()
            .unwrap();

        assert_eq!(
            resolved.settings.permission.mode,
            PermissionMode::Autonomous
        );
        assert_eq!(
            resolved.origin_of("permission.mode"),
            Some(ConfigOrigin::Override)
        );
    }

    #[test]
    fn missing_files_are_not_an_error() {
        let resolved = ConfigResolver::new()
            .global_path("/nonexistent/global.toml")
            .workspace_path("/nonexistent/workspace.toml")
            .env_vars(vec![])
            .resolve()
            .unwrap();
        assert_eq!(resolved.settings, Settings::default());
    }

    #[test]
    fn policy_floor_rejects_config_that_exposes_credentials() {
        let result = ConfigResolver::new()
            .env_vars(vec![(
                "VALYRIA_NETWORK_CREDENTIALS".to_string(),
                "allowed".to_string(),
            )])
            .resolve();
        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::PolicyFloorViolation { .. }));
    }

    #[test]
    fn unrelated_sibling_settings_survive_a_narrow_override() {
        let resolved = ConfigResolver::new()
            .env_vars(vec![(
                "VALYRIA_PERMISSION_MODE".to_string(),
                "manual".to_string(),
            )])
            .resolve()
            .unwrap();
        // Only permission.mode was overridden; network policy must still
        // be exactly the compiled default.
        assert_eq!(
            resolved.settings.network,
            valyria_types::NetworkPolicy::default()
        );
        let _ = Access::Denied;
    }
}
