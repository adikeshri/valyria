//! The default decision table (§22): what each permission mode does with
//! each category/action combination, before any grant or explicit rule is
//! consulted. This is the concrete encoding of the three modes' prose
//! descriptions in the PRD.

use valyria_types::{PermissionCategory, PermissionMode};

use crate::request::{ActionKind, PermissionRequest, RiskLevel};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultDecision {
    Allow,
    Ask,
    /// Denied regardless of mode — today only git history modification,
    /// matching §24's "Dangerous operations require permission" read at
    /// its strictest: rewriting history isn't just "needs permission",
    /// it's off by default in every mode including Autonomous, pending an
    /// explicit per-workspace override this crate doesn't implement yet
    /// (see [`crate::engine::PermissionEngine`] docs).
    Deny,
}

pub fn default_decision(mode: PermissionMode, req: &PermissionRequest) -> DefaultDecision {
    use PermissionCategory::*;

    match req.category {
        GitHistoryModification => DefaultDecision::Deny,

        SecretAccess
        | DependencyInstallation
        | DestructiveCommands
        | OutsideWorkspaceAccess
        | PlanScopeExpansion
        | Network => DefaultDecision::Ask,

        Filesystem => filesystem_decision(mode, req),
        Shell => shell_decision(mode, req),
    }
}

fn filesystem_decision(mode: PermissionMode, req: &PermissionRequest) -> DefaultDecision {
    if req.action == ActionKind::Read {
        return DefaultDecision::Allow;
    }
    match mode {
        PermissionMode::Manual => DefaultDecision::Ask,
        PermissionMode::Assisted | PermissionMode::Autonomous => {
            if req.in_plan_scope {
                DefaultDecision::Allow
            } else {
                DefaultDecision::Ask
            }
        }
    }
}

fn shell_decision(mode: PermissionMode, req: &PermissionRequest) -> DefaultDecision {
    match mode {
        PermissionMode::Manual => DefaultDecision::Ask,
        PermissionMode::Assisted => {
            if req.risk == RiskLevel::Safe {
                DefaultDecision::Allow
            } else {
                DefaultDecision::Ask
            }
        }
        PermissionMode::Autonomous => {
            if req.risk.auto_allowable() {
                DefaultDecision::Allow
            } else {
                DefaultDecision::Ask
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_types::{StepId, TaskId};
    use valyria_util::ContentHash;

    fn base_req(category: PermissionCategory, action: ActionKind) -> PermissionRequest {
        PermissionRequest {
            task_id: TaskId::new(),
            step_id: StepId::new(),
            tool: "test_tool",
            category,
            action,
            risk: RiskLevel::Unknown,
            input_hash: ContentHash::of_bytes(b"x"),
            target: "target".into(),
            in_plan_scope: false,
        }
    }

    #[test]
    fn git_history_modification_always_denied() {
        for mode in [
            PermissionMode::Manual,
            PermissionMode::Assisted,
            PermissionMode::Autonomous,
        ] {
            let req = base_req(
                PermissionCategory::GitHistoryModification,
                ActionKind::Write,
            );
            assert_eq!(
                default_decision(mode, &req),
                DefaultDecision::Deny,
                "mode={mode:?}"
            );
        }
    }

    #[test]
    fn filesystem_reads_always_allowed() {
        for mode in [
            PermissionMode::Manual,
            PermissionMode::Assisted,
            PermissionMode::Autonomous,
        ] {
            let req = base_req(PermissionCategory::Filesystem, ActionKind::Read);
            assert_eq!(
                default_decision(mode, &req),
                DefaultDecision::Allow,
                "mode={mode:?}"
            );
        }
    }

    #[test]
    fn manual_mode_asks_for_every_mutating_filesystem_write() {
        let mut req = base_req(PermissionCategory::Filesystem, ActionKind::Write);
        req.in_plan_scope = true;
        assert_eq!(
            default_decision(PermissionMode::Manual, &req),
            DefaultDecision::Ask
        );
    }

    #[test]
    fn assisted_and_autonomous_allow_in_scope_writes() {
        let mut req = base_req(PermissionCategory::Filesystem, ActionKind::Write);
        req.in_plan_scope = true;
        assert_eq!(
            default_decision(PermissionMode::Assisted, &req),
            DefaultDecision::Allow
        );
        assert_eq!(
            default_decision(PermissionMode::Autonomous, &req),
            DefaultDecision::Allow
        );
    }

    #[test]
    fn assisted_and_autonomous_ask_for_out_of_scope_writes() {
        let mut req = base_req(PermissionCategory::Filesystem, ActionKind::Write);
        req.in_plan_scope = false;
        assert_eq!(
            default_decision(PermissionMode::Assisted, &req),
            DefaultDecision::Ask
        );
        assert_eq!(
            default_decision(PermissionMode::Autonomous, &req),
            DefaultDecision::Ask
        );
    }

    #[test]
    fn shell_manual_always_asks_regardless_of_risk() {
        let mut req = base_req(PermissionCategory::Shell, ActionKind::Execute);
        req.risk = RiskLevel::Safe;
        assert_eq!(
            default_decision(PermissionMode::Manual, &req),
            DefaultDecision::Ask
        );
    }

    #[test]
    fn shell_assisted_allows_only_safe_risk() {
        let mut req = base_req(PermissionCategory::Shell, ActionKind::Execute);
        req.risk = RiskLevel::Safe;
        assert_eq!(
            default_decision(PermissionMode::Assisted, &req),
            DefaultDecision::Allow
        );

        req.risk = RiskLevel::Controlled;
        assert_eq!(
            default_decision(PermissionMode::Assisted, &req),
            DefaultDecision::Ask
        );
    }

    #[test]
    fn shell_autonomous_allows_safe_and_controlled_but_not_unknown_or_destructive() {
        let mut req = base_req(PermissionCategory::Shell, ActionKind::Execute);
        for allowed in [RiskLevel::Safe, RiskLevel::Controlled] {
            req.risk = allowed;
            assert_eq!(
                default_decision(PermissionMode::Autonomous, &req),
                DefaultDecision::Allow,
                "{allowed:?}"
            );
        }
        for asked in [RiskLevel::Unknown, RiskLevel::Destructive] {
            req.risk = asked;
            assert_eq!(
                default_decision(PermissionMode::Autonomous, &req),
                DefaultDecision::Ask,
                "{asked:?}"
            );
        }
    }

    #[test]
    fn network_always_asks_in_every_mode() {
        for mode in [
            PermissionMode::Manual,
            PermissionMode::Assisted,
            PermissionMode::Autonomous,
        ] {
            let req = base_req(PermissionCategory::Network, ActionKind::Execute);
            assert_eq!(
                default_decision(mode, &req),
                DefaultDecision::Ask,
                "mode={mode:?}"
            );
        }
    }

    #[test]
    fn secret_access_always_asks_never_silently_allowed() {
        for mode in [
            PermissionMode::Manual,
            PermissionMode::Assisted,
            PermissionMode::Autonomous,
        ] {
            let req = base_req(PermissionCategory::SecretAccess, ActionKind::Read);
            assert_eq!(
                default_decision(mode, &req),
                DefaultDecision::Ask,
                "mode={mode:?}"
            );
        }
    }

    #[test]
    fn destructive_commands_and_dependency_installation_always_ask() {
        for category in [
            PermissionCategory::DestructiveCommands,
            PermissionCategory::DependencyInstallation,
        ] {
            for mode in [
                PermissionMode::Manual,
                PermissionMode::Assisted,
                PermissionMode::Autonomous,
            ] {
                let req = base_req(category, ActionKind::Execute);
                assert_eq!(
                    default_decision(mode, &req),
                    DefaultDecision::Ask,
                    "{category:?} mode={mode:?}"
                );
            }
        }
    }
}
