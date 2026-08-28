//! The repair loop's bookkeeping (§30): one [`RepairAttempt`] per
//! diagnose → edit → re-verify cycle, and a [`RepairLedger`] that decides
//! what to do when a cycle does not fix things — escalate the
//! verification strategy, switch the model role, ask the user, or give up
//! with a diagnosis.
//!
//! The actual "make an edit" step is the model's; this module only tracks
//! whether the loop is converging and enforces the bounds that stop it
//! running forever (§8 "infinite retries").

use crate::loop_detect::LoopFinding;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairOutcome {
    /// Re-verification passed — the loop is done.
    Fixed,
    /// Still failing, but fewer failures / a further frontier than before.
    Improved,
    /// Still failing, no measurable change.
    NoChange,
    /// Still failing, and worse than before (new failures introduced).
    Regressed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairAttempt {
    pub attempt: u32,
    /// The diagnosis this attempt was reacting to (its fingerprint).
    pub diagnosis_fingerprint: String,
    /// A short description of the edit the model made.
    pub edit_summary: String,
    pub outcome: RepairOutcome,
}

/// What the driver should do next after a repair attempt did not fix the
/// failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairDecision {
    /// Try another edit against the same diagnosis.
    Continue,
    /// Re-run verification with a broader strategy before repairing
    /// again (maybe the targeted test is hiding the real breakage).
    EscalateStrategy,
    /// Hand the repair to a stronger model role.
    SwitchRole,
    /// Stop and wait for a human (`WAITING_FOR_USER`).
    AskUser { reason: String },
    /// Terminal: the loop is not converging and no escalation is left.
    GiveUp { reason: String },
}

#[derive(Debug, Clone)]
pub struct RepairLedger {
    attempts: Vec<RepairAttempt>,
    max_attempts: u32,
    escalated: bool,
    switched_role: bool,
}

impl Default for RepairLedger {
    fn default() -> Self {
        Self::new(4)
    }
}

impl RepairLedger {
    pub fn new(max_attempts: u32) -> Self {
        Self {
            attempts: Vec::new(),
            max_attempts,
            escalated: false,
            switched_role: false,
        }
    }

    pub fn attempts(&self) -> &[RepairAttempt] {
        &self.attempts
    }

    pub fn count(&self) -> u32 {
        self.attempts.len() as u32
    }

    /// Attempts made against one particular diagnosis fingerprint.
    pub fn count_for(&self, fingerprint: &str) -> u32 {
        self.attempts
            .iter()
            .filter(|a| a.diagnosis_fingerprint == fingerprint)
            .count() as u32
    }

    pub fn mark_escalated(&mut self) {
        self.escalated = true;
    }

    pub fn mark_switched_role(&mut self) {
        self.switched_role = true;
    }

    pub fn record(&mut self, mut attempt: RepairAttempt) {
        attempt.attempt = self.attempts.len() as u32 + 1;
        self.attempts.push(attempt);
    }

    /// Decide the next move. `next_fingerprint` is the diagnosis the
    /// upcoming attempt would target; `loop_finding` is whatever the
    /// [`crate::loop_detect::LoopDetector`] reported this cycle (if
    /// anything).
    pub fn decide(
        &self,
        next_fingerprint: &str,
        loop_finding: Option<&LoopFinding>,
    ) -> RepairDecision {
        if self.count() >= self.max_attempts {
            return RepairDecision::GiveUp {
                reason: format!("{} repair attempts made without a fix", self.count()),
            };
        }

        // The last attempt made things worse — don't keep digging on the
        // same diagnosis; broaden or escalate.
        if matches!(
            self.attempts.last().map(|a| a.outcome),
            Some(RepairOutcome::Regressed)
        ) {
            return self.escalate_or_ask("the last repair introduced new failures");
        }

        if let Some(finding) = loop_finding {
            return match finding {
                LoopFinding::Oscillation { .. } => RepairDecision::AskUser {
                    reason: "repair is oscillating between two states".into(),
                },
                LoopFinding::RepeatedFailure { .. }
                | LoopFinding::NoChangeIteration { .. }
                | LoopFinding::FrontierStalled { .. }
                | LoopFinding::ExactRepeat { .. } => {
                    self.escalate_or_ask(&format!("loop detector: {}", finding.code()))
                }
            };
        }

        // No loop flagged, but this exact diagnosis has come back a few
        // times: nudge the strategy / role before it turns into a loop.
        match self.count_for(next_fingerprint) {
            0 | 1 => RepairDecision::Continue,
            2 => self.escalate_or_ask("same diagnosis after two attempts"),
            _ => self.escalate_or_ask("same diagnosis after three attempts"),
        }
    }

    fn escalate_or_ask(&self, reason: &str) -> RepairDecision {
        if !self.escalated {
            RepairDecision::EscalateStrategy
        } else if !self.switched_role {
            RepairDecision::SwitchRole
        } else {
            RepairDecision::AskUser {
                reason: reason.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(fp: &str, outcome: RepairOutcome) -> RepairAttempt {
        RepairAttempt {
            attempt: 0,
            diagnosis_fingerprint: fp.into(),
            edit_summary: "edit".into(),
            outcome,
        }
    }

    #[test]
    fn continues_while_under_budget_and_making_headway() {
        let mut l = RepairLedger::new(4);
        l.record(attempt("fp1", RepairOutcome::Improved));
        assert_eq!(l.decide("fp2", None), RepairDecision::Continue);
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let mut l = RepairLedger::new(2);
        l.record(attempt("fp", RepairOutcome::NoChange));
        l.record(attempt("fp", RepairOutcome::NoChange));
        assert!(matches!(
            l.decide("fp", None),
            RepairDecision::GiveUp { .. }
        ));
    }

    #[test]
    fn escalates_then_switches_role_then_asks_user() {
        let mut l = RepairLedger::new(9);
        l.record(attempt("fp", RepairOutcome::NoChange));
        l.record(attempt("fp", RepairOutcome::NoChange));
        // third attempt on the same fingerprint → escalate strategy
        assert_eq!(l.decide("fp", None), RepairDecision::EscalateStrategy);
        l.mark_escalated();
        assert_eq!(l.decide("fp", None), RepairDecision::SwitchRole);
        l.mark_switched_role();
        assert!(matches!(
            l.decide("fp", None),
            RepairDecision::AskUser { .. }
        ));
    }

    #[test]
    fn oscillation_finding_goes_straight_to_the_user() {
        let l = RepairLedger::new(9);
        let finding = LoopFinding::Oscillation { period: 2 };
        assert!(matches!(
            l.decide("fp", Some(&finding)),
            RepairDecision::AskUser { .. }
        ));
    }

    #[test]
    fn repeated_failure_finding_escalates_first() {
        let l = RepairLedger::new(9);
        let finding = LoopFinding::RepeatedFailure {
            fingerprint: "x".into(),
            count: 3,
        };
        assert_eq!(
            l.decide("fp", Some(&finding)),
            RepairDecision::EscalateStrategy
        );
    }

    #[test]
    fn a_regression_broadens_rather_than_repeating() {
        let mut l = RepairLedger::new(9);
        l.record(attempt("fp", RepairOutcome::Regressed));
        assert_eq!(l.decide("fp", None), RepairDecision::EscalateStrategy);
    }

    #[test]
    fn count_for_is_per_fingerprint() {
        let mut l = RepairLedger::new(9);
        l.record(attempt("a", RepairOutcome::NoChange));
        l.record(attempt("b", RepairOutcome::NoChange));
        l.record(attempt("a", RepairOutcome::NoChange));
        assert_eq!(l.count_for("a"), 2);
        assert_eq!(l.count_for("b"), 1);
    }
}
