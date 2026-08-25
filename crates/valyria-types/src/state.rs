//! The agent state machine (§7): the fourteen required states and the
//! transition table between them.
//!
//! This module defines the *abstract legality* of a transition — is this
//! ever a valid edge in the graph — as a pure, exhaustively-tested function.
//! It intentionally does not model the extra invariant that resuming from
//! [`AgentState::Paused`] must return to the exact state the task was
//! paused from: that requires knowing what state was paused (data the task
//! record holds, not the state enum itself), so it's enforced by
//! `valyria-task`/`valyria-agent` in Phase 3 using this table as the coarse
//! first check.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentState {
    Idle,
    Understanding,
    Discovery,
    Planning,
    Implementing,
    Verifying,
    Diagnosing,
    Repairing,
    WaitingForPermission,
    WaitingForUser,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl AgentState {
    pub const ALL: [AgentState; 14] = [
        AgentState::Idle,
        AgentState::Understanding,
        AgentState::Discovery,
        AgentState::Planning,
        AgentState::Implementing,
        AgentState::Verifying,
        AgentState::Diagnosing,
        AgentState::Repairing,
        AgentState::WaitingForPermission,
        AgentState::WaitingForUser,
        AgentState::Paused,
        AgentState::Completed,
        AgentState::Failed,
        AgentState::Cancelled,
    ];

    /// Terminal states have no outgoing transitions: once reached, a task's
    /// state never changes again (a new task must be created instead).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AgentState::Completed | AgentState::Failed | AgentState::Cancelled
        )
    }

    /// Every non-terminal state's specific "happy path plus recognized
    /// recovery" edges. Universal interrupts (pause/cancel/fail) are added
    /// on top of this in [`AgentState::can_transition_to`], not listed here,
    /// so this table stays readable as "what does progress look like".
    fn domain_edges(self) -> &'static [AgentState] {
        use AgentState::*;
        match self {
            Idle => &[Understanding],
            Understanding => &[Discovery, WaitingForUser],
            Discovery => &[Planning, Understanding, WaitingForUser],
            Planning => &[Implementing, WaitingForPermission],
            Implementing => &[Verifying, WaitingForPermission, WaitingForUser],
            Verifying => &[Completed, Diagnosing],
            Diagnosing => &[Repairing, WaitingForUser, Failed],
            Repairing => &[Verifying, WaitingForUser],
            WaitingForPermission => &[Implementing, Planning, Failed],
            WaitingForUser => &[Understanding, Implementing, Diagnosing, Repairing],
            // Paused may resume into any non-terminal, non-Paused state;
            // the exact target is validated against task metadata by the
            // caller (see module docs).
            Paused => &[
                Idle,
                Understanding,
                Discovery,
                Planning,
                Implementing,
                Verifying,
                Diagnosing,
                Repairing,
                WaitingForPermission,
                WaitingForUser,
            ],
            Completed | Failed | Cancelled => &[],
        }
    }

    /// Whether `self -> other` is ever a legal transition in the abstract
    /// state graph. Terminal states accept nothing; every non-terminal
    /// state additionally accepts the universal interrupts `Paused` and
    /// `Cancelled` (an agent can always be interrupted), and every
    /// non-terminal, non-Paused state additionally accepts `Failed` (an
    /// unrecoverable error can surface almost anywhere in the loop).
    pub fn can_transition_to(self, other: AgentState) -> bool {
        if self.is_terminal() {
            return false;
        }
        if self.domain_edges().contains(&other) {
            return true;
        }
        // No universal self-loops: pausing an already-paused task (or
        // cancelling an already-cancelled one, etc.) is a no-op, not a
        // transition, so it's excluded before the universal-interrupt
        // rules below would otherwise allow it.
        if other == self {
            return false;
        }
        if matches!(other, AgentState::Paused | AgentState::Cancelled) {
            return true;
        }
        if matches!(other, AgentState::Failed) && self != AgentState::Paused {
            return true;
        }
        false
    }
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AgentState::Idle => "IDLE",
            AgentState::Understanding => "UNDERSTANDING",
            AgentState::Discovery => "DISCOVERY",
            AgentState::Planning => "PLANNING",
            AgentState::Implementing => "IMPLEMENTING",
            AgentState::Verifying => "VERIFYING",
            AgentState::Diagnosing => "DIAGNOSING",
            AgentState::Repairing => "REPAIRING",
            AgentState::WaitingForPermission => "WAITING_FOR_PERMISSION",
            AgentState::WaitingForUser => "WAITING_FOR_USER",
            AgentState::Paused => "PAUSED",
            AgentState::Completed => "COMPLETED",
            AgentState::Failed => "FAILED",
            AgentState::Cancelled => "CANCELLED",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_accept_nothing() {
        for terminal in [
            AgentState::Completed,
            AgentState::Failed,
            AgentState::Cancelled,
        ] {
            for target in AgentState::ALL {
                assert!(
                    !terminal.can_transition_to(target),
                    "{terminal} should not transition to {target}"
                );
            }
        }
    }

    #[test]
    fn every_non_terminal_state_can_reach_a_terminal_state() {
        // Guards against a state the agent could get permanently stuck in.
        for state in AgentState::ALL {
            if state.is_terminal() {
                continue;
            }
            let reaches_terminal = AgentState::ALL
                .iter()
                .any(|&t| t.is_terminal() && state.can_transition_to(t));
            assert!(
                reaches_terminal,
                "{state} cannot reach any terminal state directly"
            );
        }
    }

    #[test]
    fn happy_path_is_connected() {
        use AgentState::*;
        let path = [
            Idle,
            Understanding,
            Discovery,
            Planning,
            Implementing,
            Verifying,
            Completed,
        ];
        for pair in path.windows(2) {
            assert!(
                pair[0].can_transition_to(pair[1]),
                "{} -> {} should be legal",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn repair_loop_is_connected() {
        use AgentState::*;
        assert!(Verifying.can_transition_to(Diagnosing));
        assert!(Diagnosing.can_transition_to(Repairing));
        assert!(Repairing.can_transition_to(Verifying));
    }

    #[test]
    fn universal_interrupts_available_from_every_non_terminal_state() {
        for state in AgentState::ALL {
            if state.is_terminal() {
                continue;
            }
            if state != AgentState::Paused {
                assert!(
                    state.can_transition_to(AgentState::Paused),
                    "{state} -> PAUSED"
                );
            }
            assert!(
                state.can_transition_to(AgentState::Cancelled),
                "{state} -> CANCELLED"
            );
        }
    }

    #[test]
    fn paused_cannot_pause_itself_or_go_straight_to_failed() {
        // Paused resumes to a working state or is cancelled; a "failure
        // while paused" isn't a real event, so it's excluded from the
        // universal Failed-from-anywhere rule.
        assert!(!AgentState::Paused.can_transition_to(AgentState::Paused));
        assert!(!AgentState::Paused.can_transition_to(AgentState::Failed));
    }

    #[test]
    fn no_self_loops_outside_paused_resume_semantics() {
        for state in AgentState::ALL {
            if state.is_terminal() {
                continue;
            }
            if state == AgentState::Paused {
                continue; // resume target is validated by the caller
            }
            assert!(
                !state.can_transition_to(state),
                "{state} should not transition to itself"
            );
        }
    }

    #[test]
    fn display_matches_prd_naming() {
        assert_eq!(
            AgentState::WaitingForPermission.to_string(),
            "WAITING_FOR_PERMISSION"
        );
        assert_eq!(AgentState::WaitingForUser.to_string(), "WAITING_FOR_USER");
    }
}
