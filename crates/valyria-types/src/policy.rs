//! Shared permission/policy vocabulary (§22, §23). Lives at the foundation
//! layer because both `valyria-config` (which stores the configured
//! defaults) and `valyria-permissions` (which enforces them, layer 3) need
//! the same enums, and neither should own the other's copy.

use serde::{Deserialize, Serialize};

/// How much autonomy the agent has before it must stop and ask (§22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Ask for every mutating action.
    Manual,
    /// Auto-allow reads and workspace-scoped safe commands; ask for writes
    /// outside the declared plan scope, installs, network, destructive
    /// operations.
    #[default]
    Assisted,
    /// Auto-allow within the workspace, the declared plan scope, and
    /// verified-safe command classes; still ask for destructive operations,
    /// network, history rewrites, and out-of-workspace access.
    Autonomous,
}

/// The categories a permission request can fall into (§22), plus the two
/// the build plan adds beyond the PRD baseline: `SecretAccess` and
/// `PlanScopeExpansion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionCategory {
    Filesystem,
    Shell,
    Network,
    DependencyInstallation,
    DestructiveCommands,
    GitHistoryModification,
    OutsideWorkspaceAccess,
    SecretAccess,
    PlanScopeExpansion,
}

/// How permissive a given axis of behavior is allowed to be. Ordered from
/// least to most permissive so a "ceiling" (the policy floor) can be
/// expressed as a simple `<=` comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Denied,
    Controlled,
    Allowed,
}

/// The default network policy (§23): repository and workspace filesystem
/// access are allowed outright, local commands are controlled (subject to
/// the permission engine), and internet/credentials are denied unless a
/// specific request is explicitly authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicy {
    pub repository: Access,
    pub workspace_filesystem: Access,
    pub local_commands: Access,
    pub internet: Access,
    pub credentials: Access,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            repository: Access::Allowed,
            workspace_filesystem: Access::Allowed,
            local_commands: Access::Controlled,
            internet: Access::Denied,
            credentials: Access::Denied,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_orders_from_least_to_most_permissive() {
        assert!(Access::Denied < Access::Controlled);
        assert!(Access::Controlled < Access::Allowed);
    }

    #[test]
    fn default_network_policy_matches_prd_defaults() {
        let policy = NetworkPolicy::default();
        assert_eq!(policy.repository, Access::Allowed);
        assert_eq!(policy.workspace_filesystem, Access::Allowed);
        assert_eq!(policy.local_commands, Access::Controlled);
        assert_eq!(policy.internet, Access::Denied);
        assert_eq!(policy.credentials, Access::Denied);
    }

    #[test]
    fn default_permission_mode_is_assisted() {
        assert_eq!(PermissionMode::default(), PermissionMode::Assisted);
    }

    #[test]
    fn permission_mode_serializes_snake_case() {
        let json = serde_json::to_string(&PermissionMode::Autonomous).unwrap();
        assert_eq!(json, "\"autonomous\"");
    }

    #[test]
    fn permission_category_serializes_snake_case() {
        let json = serde_json::to_string(&PermissionCategory::GitHistoryModification).unwrap();
        assert_eq!(json, "\"git_history_modification\"");
    }
}
