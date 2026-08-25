//! Origin tracking: every effective config value records which layer
//! produced it, so `valyria config` can answer "where did this come from?"
//! (§4.3).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ConfigOrigin {
    /// Compiled-in defaults (`Settings::default()`).
    Default,
    /// `~/.valyria/config.toml`.
    Global,
    /// `<repo>/.valyria/config.toml`.
    Workspace,
    /// `VALYRIA_*` environment variables.
    Env,
    /// Per-task programmatic overrides.
    Override,
}

impl std::fmt::Display for ConfigOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ConfigOrigin::Default => "default",
            ConfigOrigin::Global => "global",
            ConfigOrigin::Workspace => "workspace",
            ConfigOrigin::Env => "env",
            ConfigOrigin::Override => "override",
        };
        f.write_str(s)
    }
}

/// Dotted config path (e.g. `"network.internet"`) -> the layer that set it.
pub type OriginMap = BTreeMap<String, ConfigOrigin>;
