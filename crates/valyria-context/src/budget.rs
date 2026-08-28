//! The budget model (§4.17): sections with `{ min, ideal, max, priority }`
//! and a small allocator that solves for a per-section token cap.
//!
//! The allocator's contract:
//!
//! * It reserves `reserve_for_output` off the top for the model's reply.
//! * A section with no candidates demands nothing and is allocated nothing,
//!   whatever its `min`.
//! * If the sections that *do* have candidates cannot all be given their
//!   `min`, allocation **fails loudly** ([`ContextError::BudgetInfeasible`])
//!   — the caller narrows the task, the context is never silently cut.
//! * Otherwise every demanding section gets at least its `min`, then
//!   sections are topped up toward `ideal` and then toward `max` in
//!   priority order until the tokens run out.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{ContextError, Result};

/// The parts an assembled prompt is divided into. Every candidate declares
/// which one it belongs to; the budget has one [`SectionSpec`] per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    /// The runtime's compiled-in policy prompt. Never degraded, never
    /// dropped.
    Policy,
    /// Authorized instruction files and user-authored memory.
    Instructions,
    /// Agent-extracted / task / repository memory.
    Memory,
    /// The task objective itself.
    Task,
    /// Retrieved repository code.
    Repository,
    /// Tool output, diffs, diagnostics — factual, untrusted-as-instructions.
    Evidence,
    /// Prior model turns replayed into context.
    History,
}

impl SectionKind {
    pub const ALL: [SectionKind; 7] = [
        SectionKind::Policy,
        SectionKind::Instructions,
        SectionKind::Memory,
        SectionKind::Task,
        SectionKind::Repository,
        SectionKind::Evidence,
        SectionKind::History,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SectionKind::Policy => "policy",
            SectionKind::Instructions => "instructions",
            SectionKind::Memory => "memory",
            SectionKind::Task => "task",
            SectionKind::Repository => "repository",
            SectionKind::Evidence => "evidence",
            SectionKind::History => "history",
        }
    }
}

/// One section's slice of the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionSpec {
    pub kind: SectionKind,
    /// The section is not worth including below this many tokens; if it has
    /// candidates and cannot be given this much, allocation fails.
    pub min: usize,
    /// The target size when tokens are plentiful.
    pub ideal: usize,
    /// The section never grows past this even if tokens are spare.
    pub max: usize,
    /// Higher wins tokens first when topping up toward `ideal`/`max`.
    pub priority: u8,
}

/// A whole-prompt budget: a token ceiling, an output reservation, and a
/// spec per section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    pub total_tokens: usize,
    pub reserve_for_output: usize,
    pub sections: Vec<SectionSpec>,
}

impl ContextBudget {
    /// A budget with the default section shape, scaled to `total_tokens`.
    /// The defaults reserve an eighth for output and give the policy and
    /// task sections small hard floors so they can never be squeezed out.
    pub fn new(total_tokens: usize) -> Self {
        let reserve_for_output = (total_tokens / 8).max(256).min(total_tokens);
        let body = total_tokens.saturating_sub(reserve_for_output).max(1);
        let pct = |n: usize| (body * n / 100).max(1);
        Self {
            total_tokens,
            reserve_for_output,
            sections: vec![
                SectionSpec {
                    kind: SectionKind::Policy,
                    min: 1,
                    ideal: pct(6),
                    max: pct(12),
                    priority: 250,
                },
                SectionSpec {
                    kind: SectionKind::Task,
                    min: 1,
                    ideal: pct(6),
                    max: pct(15),
                    priority: 240,
                },
                SectionSpec {
                    kind: SectionKind::Instructions,
                    min: 0,
                    ideal: pct(15),
                    max: pct(30),
                    priority: 200,
                },
                SectionSpec {
                    kind: SectionKind::Memory,
                    min: 0,
                    ideal: pct(10),
                    max: pct(20),
                    priority: 150,
                },
                SectionSpec {
                    kind: SectionKind::Evidence,
                    min: 0,
                    ideal: pct(25),
                    max: pct(55),
                    priority: 120,
                },
                SectionSpec {
                    kind: SectionKind::Repository,
                    min: 0,
                    ideal: pct(35),
                    max: pct(70),
                    priority: 100,
                },
                SectionSpec {
                    kind: SectionKind::History,
                    min: 0,
                    ideal: pct(8),
                    max: pct(20),
                    priority: 60,
                },
            ],
        }
    }

    /// Tokens available for context after the output reservation.
    pub fn available(&self) -> usize {
        self.total_tokens.saturating_sub(self.reserve_for_output)
    }

    pub fn spec(&self, kind: SectionKind) -> Option<&SectionSpec> {
        self.sections.iter().find(|s| s.kind == kind)
    }

    /// Override one section's spec (used by callers that know a task needs,
    /// say, an unusually large evidence section).
    pub fn with_section(mut self, spec: SectionSpec) -> Self {
        self.sections.retain(|s| s.kind != spec.kind);
        self.sections.push(spec);
        self
    }
}

/// The allocator's output: a token cap per section, and the headroom it
/// worked with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allocation {
    pub available: usize,
    pub per_section: BTreeMap<SectionKind, usize>,
}

impl Allocation {
    pub fn cap(&self, kind: SectionKind) -> usize {
        self.per_section.get(&kind).copied().unwrap_or(0)
    }

    pub fn total_allocated(&self) -> usize {
        self.per_section.values().copied().sum()
    }
}

/// Solve for a per-section token cap given how many tokens each section
/// would use if unconstrained (`demand`). `demand` may omit sections with
/// nothing to contribute.
pub fn allocate(
    budget: &ContextBudget,
    demand: &BTreeMap<SectionKind, usize>,
) -> Result<Allocation> {
    let available = budget.available();

    // Every section that has demand must have a spec.
    for kind in demand.keys() {
        if budget.spec(*kind).is_none() {
            return Err(ContextError::UnbudgetedSection(*kind));
        }
    }

    // Phase 1: hard floors. A section's floor is its `min` capped by what
    // it actually wants (no point reserving 500 tokens for a 3-token
    // section).
    let mut alloc: BTreeMap<SectionKind, usize> = BTreeMap::new();
    let mut floor_total: usize = 0;
    for spec in &budget.sections {
        let want = demand.get(&spec.kind).copied().unwrap_or(0);
        let floor = spec.min.min(want);
        alloc.insert(spec.kind, floor);
        floor_total = floor_total.saturating_add(floor);
    }

    if floor_total > available {
        return Err(ContextError::BudgetInfeasible {
            needed: floor_total,
            available,
        });
    }

    let mut remaining = available - floor_total;

    // Sections that still want more, richest priority first. Ties broken by
    // section kind order for determinism.
    let mut order: Vec<&SectionSpec> = budget.sections.iter().collect();
    order.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.kind.cmp(&b.kind))
    });

    // Phase 2: top up toward `ideal`, then Phase 3 toward `max`.
    for target_of in [|s: &SectionSpec| s.ideal, |s: &SectionSpec| s.max] {
        for spec in &order {
            if remaining == 0 {
                break;
            }
            let want = demand.get(&spec.kind).copied().unwrap_or(0);
            let target = target_of(spec).min(want);
            let current = alloc[&spec.kind];
            if target > current {
                let grant = (target - current).min(remaining);
                alloc.insert(spec.kind, current + grant);
                remaining -= grant;
            }
        }
    }

    Ok(Allocation {
        available,
        per_section: alloc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demand(pairs: &[(SectionKind, usize)]) -> BTreeMap<SectionKind, usize> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn a_section_with_no_demand_gets_nothing_even_with_a_min() {
        let budget = ContextBudget {
            total_tokens: 1000,
            reserve_for_output: 0,
            sections: vec![SectionSpec {
                kind: SectionKind::Evidence,
                min: 100,
                ideal: 500,
                max: 800,
                priority: 100,
            }],
        };
        let alloc = allocate(&budget, &demand(&[])).unwrap();
        assert_eq!(alloc.cap(SectionKind::Evidence), 0);
    }

    #[test]
    fn floors_that_do_not_fit_are_a_loud_error() {
        let budget = ContextBudget {
            total_tokens: 50,
            reserve_for_output: 0,
            sections: vec![
                SectionSpec {
                    kind: SectionKind::Policy,
                    min: 40,
                    ideal: 40,
                    max: 40,
                    priority: 250,
                },
                SectionSpec {
                    kind: SectionKind::Task,
                    min: 40,
                    ideal: 40,
                    max: 40,
                    priority: 240,
                },
            ],
        };
        let err = allocate(
            &budget,
            &demand(&[(SectionKind::Policy, 100), (SectionKind::Task, 100)]),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ContextError::BudgetInfeasible {
                needed: 80,
                available: 50
            }
        ));
    }

    #[test]
    fn priority_decides_who_gets_scarce_top_up_tokens() {
        let budget = ContextBudget {
            total_tokens: 120,
            reserve_for_output: 0,
            sections: vec![
                SectionSpec {
                    kind: SectionKind::Evidence,
                    min: 10,
                    ideal: 100,
                    max: 100,
                    priority: 200,
                },
                SectionSpec {
                    kind: SectionKind::Repository,
                    min: 10,
                    ideal: 100,
                    max: 100,
                    priority: 100,
                },
            ],
        };
        // Floors take 20, leaving 100. Evidence (higher priority) fills to
        // its ideal first (90 more), and the last 10 trickle to Repository.
        let alloc = allocate(
            &budget,
            &demand(&[(SectionKind::Evidence, 100), (SectionKind::Repository, 100)]),
        )
        .unwrap();
        assert_eq!(alloc.cap(SectionKind::Evidence), 100);
        assert_eq!(alloc.cap(SectionKind::Repository), 20);
        assert_eq!(alloc.total_allocated(), 120);
    }

    #[test]
    fn demand_below_ideal_caps_the_grant() {
        let budget = ContextBudget::new(10_000);
        let alloc = allocate(&budget, &demand(&[(SectionKind::Repository, 42)])).unwrap();
        assert_eq!(alloc.cap(SectionKind::Repository), 42);
    }

    #[test]
    fn pathological_zero_budget_with_no_demand_is_fine() {
        let budget = ContextBudget::new(0);
        let alloc = allocate(&budget, &demand(&[])).unwrap();
        assert_eq!(alloc.total_allocated(), 0);
        assert_eq!(alloc.available, 0);
    }

    #[test]
    fn reserve_larger_than_total_saturates_to_zero_available() {
        let budget = ContextBudget {
            total_tokens: 100,
            reserve_for_output: 1000,
            sections: ContextBudget::new(100).sections,
        };
        assert_eq!(budget.available(), 0);
        // No demand -> still fine.
        assert!(allocate(&budget, &demand(&[])).is_ok());
        // Any demand with a non-zero floor -> infeasible, not a panic.
        let err = allocate(&budget, &demand(&[(SectionKind::Task, 10)])).unwrap_err();
        assert!(matches!(err, ContextError::BudgetInfeasible { .. }));
    }

    #[test]
    fn huge_demand_does_not_overflow_and_is_capped_at_max() {
        let budget = ContextBudget::new(4_000);
        let alloc = allocate(
            &budget,
            &demand(&[(SectionKind::Repository, usize::MAX / 2)]),
        )
        .unwrap();
        let max = budget.spec(SectionKind::Repository).unwrap().max;
        assert!(alloc.cap(SectionKind::Repository) <= max);
        assert!(alloc.total_allocated() <= budget.available());
    }

    #[test]
    fn unbudgeted_section_in_demand_is_rejected() {
        let budget = ContextBudget {
            total_tokens: 100,
            reserve_for_output: 0,
            sections: vec![SectionSpec {
                kind: SectionKind::Policy,
                min: 0,
                ideal: 10,
                max: 10,
                priority: 1,
            }],
        };
        let err = allocate(&budget, &demand(&[(SectionKind::Evidence, 5)])).unwrap_err();
        assert!(matches!(
            err,
            ContextError::UnbudgetedSection(SectionKind::Evidence)
        ));
    }

    #[test]
    fn never_allocates_more_than_available() {
        for total in [0usize, 1, 7, 100, 999, 50_000] {
            let budget = ContextBudget::new(total);
            let d = demand(&[
                (SectionKind::Policy, 10_000),
                (SectionKind::Task, 10_000),
                (SectionKind::Instructions, 10_000),
                (SectionKind::Memory, 10_000),
                (SectionKind::Evidence, 10_000),
                (SectionKind::Repository, 10_000),
                (SectionKind::History, 10_000),
            ]);
            match allocate(&budget, &d) {
                Ok(a) => assert!(a.total_allocated() <= a.available),
                Err(ContextError::BudgetInfeasible { .. }) => {}
                Err(e) => panic!("unexpected error for total={total}: {e}"),
            }
        }
    }
}
