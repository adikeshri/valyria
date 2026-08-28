//! Bounded plan repair (§4.25: "invalid plans are returned to the model
//! with structured errors — bounded repair attempts — never accepted
//! silently").
//!
//! Mirrors `valyria_agent::repair::RepairLedger` in spirit: track the
//! attempts, decide when to stop. The feedback string is deliberately
//! structured — one line per error carrying its machine code — so the
//! model gets an actionable list, not a paragraph.

use crate::validate::PlanError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRepairAttempt {
    pub attempt: u32,
    /// The error codes this rejected revision carried.
    pub error_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRepairDecision {
    /// Hand `feedback` back to the model and validate its next revision.
    Retry { feedback: String },
    /// Out of attempts — fail the task with `reason`.
    GiveUp { reason: String },
}

#[derive(Debug, Clone)]
pub struct PlanRepairLedger {
    attempts: Vec<PlanRepairAttempt>,
    max_attempts: u32,
}

impl Default for PlanRepairLedger {
    fn default() -> Self {
        Self::new(3)
    }
}

impl PlanRepairLedger {
    pub fn new(max_attempts: u32) -> Self {
        Self {
            attempts: Vec::new(),
            max_attempts,
        }
    }

    /// Rebuild a ledger mid-run after a crash: `prior_rejections` rounds
    /// have already happened (recorded durably in the journal) and count
    /// toward `max_attempts`, but their per-round error codes are not
    /// replayed here.
    pub fn resumed(max_attempts: u32, prior_rejections: u32) -> Self {
        let attempts = (1..=prior_rejections)
            .map(|attempt| PlanRepairAttempt {
                attempt,
                error_codes: vec!["(from a prior run)".to_string()],
            })
            .collect();
        Self {
            attempts,
            max_attempts,
        }
    }

    pub fn count(&self) -> u32 {
        self.attempts.len() as u32
    }

    pub fn attempts(&self) -> &[PlanRepairAttempt] {
        &self.attempts
    }

    /// Record a rejected revision and decide what happens next.
    pub fn record_and_decide(&mut self, errors: &[PlanError]) -> PlanRepairDecision {
        let attempt = self.attempts.len() as u32 + 1;
        self.attempts.push(PlanRepairAttempt {
            attempt,
            error_codes: errors.iter().map(|e| e.code.as_str().to_string()).collect(),
        });

        if attempt >= self.max_attempts {
            return PlanRepairDecision::GiveUp {
                reason: format!(
                    "plan still invalid after {attempt} attempts ({})",
                    summarize_codes(errors)
                ),
            };
        }

        PlanRepairDecision::Retry {
            feedback: render_feedback(errors),
        }
    }
}

fn summarize_codes(errors: &[PlanError]) -> String {
    let mut codes: Vec<&str> = errors.iter().map(|e| e.code.as_str()).collect();
    codes.sort_unstable();
    codes.dedup();
    codes.join(", ")
}

/// The structured block handed back to the model: one line per problem,
/// `[code] (step) message — hint`.
pub fn render_feedback(errors: &[PlanError]) -> String {
    let mut out = String::from("The plan was rejected. Fix every item and resubmit:\n");
    for e in errors {
        let step = e
            .step
            .as_ref()
            .map(|s| format!(" (step `{s}`)"))
            .unwrap_or_default();
        out.push_str(&format!(
            "- [{}]{step} {} — {}\n",
            e.code.as_str(),
            e.message,
            e.hint
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::{PlanError, PlanErrorCode};

    fn err(code: PlanErrorCode) -> PlanError {
        PlanError {
            code,
            step: None,
            message: format!("{} happened", code.as_str()),
            hint: "fix it".into(),
        }
    }

    #[test]
    fn retries_until_the_bound_then_gives_up() {
        let mut ledger = PlanRepairLedger::new(3);
        let errs = vec![err(PlanErrorCode::CyclicDependency)];
        assert!(matches!(
            ledger.record_and_decide(&errs),
            PlanRepairDecision::Retry { .. }
        ));
        assert!(matches!(
            ledger.record_and_decide(&errs),
            PlanRepairDecision::Retry { .. }
        ));
        assert!(matches!(
            ledger.record_and_decide(&errs),
            PlanRepairDecision::GiveUp { .. }
        ));
        assert_eq!(ledger.count(), 3);
    }

    #[test]
    fn feedback_names_every_error_code() {
        let errs = vec![
            err(PlanErrorCode::CyclicDependency),
            err(PlanErrorCode::MutatingStepWithoutVerification),
        ];
        let fb = render_feedback(&errs);
        assert!(fb.contains("cyclic_dependency"));
        assert!(fb.contains("mutating_step_without_verification"));
    }

    #[test]
    fn giveup_reason_summarizes_codes() {
        let mut ledger = PlanRepairLedger::new(1);
        let d = ledger.record_and_decide(&[err(PlanErrorCode::EmptyPlan)]);
        match d {
            PlanRepairDecision::GiveUp { reason } => assert!(reason.contains("empty_plan")),
            _ => panic!("expected GiveUp"),
        }
    }
}
