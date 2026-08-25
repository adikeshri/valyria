//! The effective settings shape. Kept intentionally small for Phase 0 —
//! more sections (model role bindings, sandbox profiles, ...) get added as
//! their owning subsystems land, following the same layered-resolution and
//! policy-floor mechanics established here.

use serde::{Deserialize, Serialize};
use valyria_types::{NetworkPolicy, PermissionMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    #[default]
    Pretty,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PermissionSettings {
    pub mode: PermissionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LogSettings {
    pub format: LogFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub permission: PermissionSettings,
    pub network: NetworkPolicy,
    pub log: LogSettings,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let settings = Settings::default();
        let s = toml::to_string(&settings).unwrap();
        let back: Settings = toml::from_str(&s).unwrap();
        assert_eq!(settings, back);
    }

    #[test]
    fn partial_toml_fills_in_defaults() {
        let partial: Settings = toml::from_str("[permission]\nmode = \"manual\"\n").unwrap();
        assert_eq!(partial.permission.mode, PermissionMode::Manual);
        assert_eq!(partial.network, NetworkPolicy::default());
    }
}
