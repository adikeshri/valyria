//! SQLite text encoding for `AgentState`. Reuses `AgentState`'s `Display`
//! (SCREAMING_SNAKE_CASE, matching PRD naming) for the forward direction so
//! stored values are human-readable directly in the `.db` file; the
//! reverse direction is necessarily a manual match since `AgentState` has
//! no `FromStr` of its own (nothing outside this crate needs one).

use valyria_types::AgentState;

use crate::types::ControlSignal;

pub(crate) fn state_to_text(state: AgentState) -> String {
    state.to_string()
}

pub(crate) fn signal_to_text(signal: ControlSignal) -> &'static str {
    match signal {
        ControlSignal::PauseRequested => "PAUSE",
        ControlSignal::CancelRequested => "CANCEL",
    }
}

pub(crate) fn signal_from_text(s: &str) -> Option<ControlSignal> {
    Some(match s {
        "PAUSE" => ControlSignal::PauseRequested,
        "CANCEL" => ControlSignal::CancelRequested,
        _ => return None,
    })
}

pub(crate) fn state_from_text(s: &str) -> Option<AgentState> {
    Some(match s {
        "IDLE" => AgentState::Idle,
        "UNDERSTANDING" => AgentState::Understanding,
        "DISCOVERY" => AgentState::Discovery,
        "PLANNING" => AgentState::Planning,
        "IMPLEMENTING" => AgentState::Implementing,
        "VERIFYING" => AgentState::Verifying,
        "DIAGNOSING" => AgentState::Diagnosing,
        "REPAIRING" => AgentState::Repairing,
        "WAITING_FOR_PERMISSION" => AgentState::WaitingForPermission,
        "WAITING_FOR_USER" => AgentState::WaitingForUser,
        "PAUSED" => AgentState::Paused,
        "COMPLETED" => AgentState::Completed,
        "FAILED" => AgentState::Failed,
        "CANCELLED" => AgentState::Cancelled,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_round_trips() {
        for state in AgentState::ALL {
            let text = state_to_text(state);
            assert_eq!(state_from_text(&text), Some(state), "{text}");
        }
    }

    #[test]
    fn unknown_text_is_none() {
        assert_eq!(state_from_text("NOT_A_STATE"), None);
    }
}
