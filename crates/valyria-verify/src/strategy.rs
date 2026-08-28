//! Escalation strategy (§28): given a change set and the confirmed
//! tooling, decide *what to run next* — not as a fixed sequence but as a
//! cost/value pick, maximizing the chance of catching a regression per
//! second spent.
//!
//! The ordering the planner falls back to when it has nothing better:
//! syntax/type check → targeted tests for the changed files → a broader
//! test run → lint/format → the full suite. Two hard rules on top of the
//! ordering: **early exit on failure** (a failing check goes straight to
//! diagnosis; there is no point running the slower ones), and a
//! **mandatory broad run before `COMPLETED`** (a green targeted test is
//! not evidence the whole thing still builds).

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::command::{CommandKind, VerifyCommand};

/// The files (and, where known, symbols) a task changed — the input that
/// makes verification targeted instead of always-full.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    pub files: Vec<PathBuf>,
    pub symbols: Vec<String>,
}

impl ChangeSet {
    pub fn from_files(files: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            files: files.into_iter().collect(),
            symbols: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// A test command narrowed to a subset of the suite, plus which changed
/// files it is believed to cover and a rough cost. The graph (above this
/// crate's layer) is what maps changed symbols → covering tests; the
/// driver hands the result in here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetedCheck {
    pub command: VerifyCommand,
    pub covers: Vec<PathBuf>,
    pub est_secs: u32,
}

/// How localized / expensive a step is. Lower tiers run first because
/// they are cheaper and their signal is more precise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Compile / type check — fastest, catches the largest class of
    /// regressions (nothing else matters if it doesn't build).
    Syntax,
    /// Tests believed to cover the changed files.
    TargetedTest,
    /// A broader test run than the targeted ones but not the whole suite
    /// (e.g. the changed package).
    RelatedTests,
    /// Lint / format.
    Style,
    /// The full test suite — the mandatory broad run before completion.
    Full,
}

impl Tier {
    /// Rough prior probability that a step of this tier catches a
    /// regression introduced by an arbitrary change, used for the
    /// cost/value sort within a tier group.
    fn regression_catch_prior(self) -> f32 {
        match self {
            Tier::Syntax => 0.9,
            Tier::TargetedTest => 0.8,
            Tier::RelatedTests => 0.6,
            Tier::Style => 0.2,
            Tier::Full => 0.7,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationStep {
    pub command: VerifyCommand,
    pub tier: Tier,
    pub rationale: String,
    /// Best-effort seconds estimate — the sort key numerator's
    /// denominator.
    pub est_secs: u32,
}

impl VerificationStep {
    fn value_density(&self) -> f32 {
        self.tier.regression_catch_prior() / (self.est_secs.max(1) as f32)
    }
}

/// An ordered list of checks. Callers walk it with [`VerificationPlan::next_after`],
/// which enforces early-exit-on-failure, and check
/// [`VerificationPlan::needs_broad_run`] before allowing `COMPLETED`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationPlan {
    pub steps: Vec<VerificationStep>,
}

impl VerificationPlan {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn first(&self) -> Option<&VerificationStep> {
        self.steps.first()
    }

    /// The next step to run given how many have already been executed and
    /// whether the last one passed. A failure stops escalation (returns
    /// `None` → the caller goes to diagnosis); success advances.
    pub fn next_after(&self, executed: usize, last_passed: bool) -> Option<&VerificationStep> {
        if executed > 0 && !last_passed {
            return None;
        }
        self.steps.get(executed)
    }

    /// True if there is at least one `Full`-tier step in the plan that has
    /// not been executed yet — a plan with `executed` steps done may not
    /// complete while this holds.
    pub fn needs_broad_run(&self, executed: usize) -> bool {
        self.steps
            .iter()
            .enumerate()
            .any(|(i, s)| i >= executed && s.tier == Tier::Full)
            || (self.steps.iter().any(|s| s.tier == Tier::Full) && executed < self.steps.len())
    }

    /// Whether every `Full`-tier step has been executed.
    pub fn broad_run_satisfied(&self, executed: usize) -> bool {
        self.steps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.tier == Tier::Full)
            .all(|(i, _)| i < executed)
    }
}

/// Options tuning the plan.
#[derive(Debug, Clone)]
pub struct StrategyOptions {
    /// Include lint/format steps. Off during a repair loop (style is not
    /// what a repair is chasing) and on for a final pre-completion pass.
    pub include_style: bool,
    /// Append the full suite as the terminal broad run.
    pub require_full_suite: bool,
}

impl Default for StrategyOptions {
    fn default() -> Self {
        Self {
            include_style: true,
            require_full_suite: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct EscalationStrategy;

impl EscalationStrategy {
    /// Build the ordered plan. `available` is the confirmed tooling (see
    /// [`crate::discovery::validate`]); `targeted` is whatever the graph
    /// could narrow down; `changeset` drives the rationale text and
    /// whether targeted steps are worth adding at all.
    pub fn plan(
        available: &[VerifyCommand],
        changeset: &ChangeSet,
        targeted: &[TargetedCheck],
        opts: &StrategyOptions,
    ) -> VerificationPlan {
        let mut steps: Vec<VerificationStep> = Vec::new();
        let mut used: BTreeSet<String> = BTreeSet::new();

        // 1. Syntax / typecheck / build — cheapest, broadest.
        for cmd in available
            .iter()
            .filter(|c| matches!(c.kind, CommandKind::Build | CommandKind::Typecheck))
        {
            if used.insert(cmd.identity()) {
                steps.push(VerificationStep {
                    command: cmd.clone(),
                    tier: Tier::Syntax,
                    rationale: "compile/type check before running anything slower".into(),
                    est_secs: 20,
                });
            }
        }

        // 2. Targeted tests for the changed files.
        for tc in targeted {
            if used.insert(tc.command.identity()) {
                let covers: Vec<String> =
                    tc.covers.iter().map(|p| p.display().to_string()).collect();
                steps.push(VerificationStep {
                    command: tc.command.clone(),
                    tier: Tier::TargetedTest,
                    rationale: if covers.is_empty() {
                        "test believed to cover the change".into()
                    } else {
                        format!("covers {}", covers.join(", "))
                    },
                    est_secs: tc.est_secs.max(1),
                });
            }
        }

        // 3. A broad test command that is not the terminal full suite —
        //    only meaningful when there were targeted steps to escalate
        //    from.
        let test_cmds: Vec<&VerifyCommand> = available
            .iter()
            .filter(|c| c.kind == CommandKind::Test)
            .collect();
        if !steps.iter().any(|s| s.tier == Tier::TargetedTest) {
            // Nothing targeted — the first test command becomes the
            // primary (still Full-tier so the broad-run rule is satisfied
            // by it).
        } else if let Some(first_test) = test_cmds.first() {
            if used.insert(format!("related:{}", first_test.identity())) {
                steps.push(VerificationStep {
                    command: (*first_test).clone(),
                    tier: Tier::RelatedTests,
                    rationale: "broaden from the targeted tests".into(),
                    est_secs: 60,
                });
            }
        }

        // 4. Style.
        if opts.include_style {
            for cmd in available
                .iter()
                .filter(|c| matches!(c.kind, CommandKind::Lint | CommandKind::Format))
            {
                if used.insert(cmd.identity()) {
                    steps.push(VerificationStep {
                        command: cmd.clone(),
                        tier: Tier::Style,
                        rationale: "style gate".into(),
                        est_secs: 15,
                    });
                }
            }
        }

        // 5. The mandatory broad run: the full test suite.
        if opts.require_full_suite {
            if let Some(full) = test_cmds
                .iter()
                .max_by_key(|c| c.source.confidence())
                .copied()
            {
                // Even if this exact command already ran as RelatedTests,
                // record a Full-tier entry so the broad-run rule has
                // something to point at.
                steps.push(VerificationStep {
                    command: full.clone(),
                    tier: Tier::Full,
                    rationale: if changeset.is_empty() {
                        "full suite (no change set was provided)".into()
                    } else {
                        "mandatory full run before completion".into()
                    },
                    est_secs: 120,
                });
            }
        }

        // Stable sort by tier, then by value density within a tier.
        steps.sort_by(|a, b| {
            a.tier.cmp(&b.tier).then(
                b.value_density()
                    .partial_cmp(&a.value_density())
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });

        VerificationPlan { steps }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandSource;

    fn c(kind: CommandKind, program: &str, args: &[&str]) -> VerifyCommand {
        VerifyCommand::new(
            kind,
            program,
            args.to_vec(),
            CommandSource::Manifest {
                file: "Cargo.toml".into(),
            },
        )
    }

    #[test]
    fn plan_orders_syntax_first_and_full_last() {
        let available = vec![
            c(CommandKind::Test, "cargo", &["test"]),
            c(CommandKind::Build, "cargo", &["build"]),
            c(CommandKind::Lint, "cargo", &["clippy"]),
        ];
        let plan = EscalationStrategy::plan(
            &available,
            &ChangeSet::default(),
            &[],
            &StrategyOptions::default(),
        );
        assert_eq!(plan.steps.first().unwrap().tier, Tier::Syntax);
        assert_eq!(plan.steps.last().unwrap().tier, Tier::Full);
    }

    #[test]
    fn targeted_checks_slot_in_before_the_broad_run() {
        let available = vec![c(CommandKind::Test, "cargo", &["test"])];
        let targeted = vec![TargetedCheck {
            command: c(CommandKind::Test, "cargo", &["test", "--test", "math"]),
            covers: vec![PathBuf::from("src/math.rs")],
            est_secs: 5,
        }];
        let plan = EscalationStrategy::plan(
            &available,
            &ChangeSet::from_files([PathBuf::from("src/math.rs")]),
            &targeted,
            &StrategyOptions::default(),
        );
        let tiers: Vec<Tier> = plan.steps.iter().map(|s| s.tier).collect();
        let ti = tiers.iter().position(|t| *t == Tier::TargetedTest).unwrap();
        let fi = tiers.iter().position(|t| *t == Tier::Full).unwrap();
        assert!(ti < fi);
        assert!(plan.steps[ti].rationale.contains("src/math.rs"));
    }

    #[test]
    fn next_after_stops_on_failure() {
        let available = vec![
            c(CommandKind::Build, "cargo", &["build"]),
            c(CommandKind::Test, "cargo", &["test"]),
        ];
        let plan = EscalationStrategy::plan(
            &available,
            &ChangeSet::default(),
            &[],
            &StrategyOptions::default(),
        );
        assert!(plan.next_after(0, true).is_some());
        assert!(
            plan.next_after(1, false).is_none(),
            "failure halts escalation"
        );
        assert!(plan.next_after(1, true).is_some());
    }

    #[test]
    fn broad_run_is_required_until_the_full_step_runs() {
        let available = vec![c(CommandKind::Test, "cargo", &["test"])];
        let plan = EscalationStrategy::plan(
            &available,
            &ChangeSet::default(),
            &[],
            &StrategyOptions::default(),
        );
        let full_idx = plan
            .steps
            .iter()
            .position(|s| s.tier == Tier::Full)
            .unwrap();
        assert!(plan.needs_broad_run(full_idx));
        assert!(!plan.broad_run_satisfied(full_idx));
        assert!(plan.broad_run_satisfied(full_idx + 1));
    }

    #[test]
    fn style_can_be_excluded_for_a_repair_pass() {
        let available = vec![
            c(CommandKind::Test, "cargo", &["test"]),
            c(CommandKind::Lint, "cargo", &["clippy"]),
        ];
        let opts = StrategyOptions {
            include_style: false,
            require_full_suite: true,
        };
        let plan = EscalationStrategy::plan(&available, &ChangeSet::default(), &[], &opts);
        assert!(plan.steps.iter().all(|s| s.tier != Tier::Style));
    }

    #[test]
    fn empty_tooling_yields_empty_plan() {
        let plan =
            EscalationStrategy::plan(&[], &ChangeSet::default(), &[], &StrategyOptions::default());
        assert!(plan.is_empty());
        assert!(plan.next_after(0, true).is_none());
    }
}
