use valyria_types::ErrorCode;

use crate::budget::SectionKind;

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    /// Phase 3 explicit-file path: a single named file did not fit in the
    /// flat budget.
    #[error("including `{path}` would exceed the context budget (needs ~{needed} tokens, {remaining} remaining)")]
    BudgetExceeded {
        path: String,
        needed: usize,
        remaining: usize,
    },
    #[error("failed to read `{path}` for context: {message}")]
    ReadFailed { path: String, message: String },

    /// The budget allocator could not satisfy every section's minimum with
    /// the tokens available. The caller must narrow the task rather than
    /// have the context silently truncated (§4.17).
    #[error("context budget is infeasible: section minimums need {needed} tokens but only {available} are available")]
    BudgetInfeasible { needed: usize, available: usize },

    /// A [`Trust::Policy`](valyria_types::Trust) item could not be placed —
    /// this should be impossible if the allocator honored the policy
    /// section's minimum, so it indicates a misconfigured budget.
    #[error("the runtime policy prompt ({needed} tokens) does not fit the policy section's allocation ({allocated})")]
    PolicyDoesNotFit { needed: usize, allocated: usize },

    /// A candidate was tagged with a section that the budget has no spec
    /// for.
    #[error("no budget spec for section {0:?}")]
    UnbudgetedSection(SectionKind),

    /// A retriever failed. `source` is the underlying error's display.
    #[error("retrieval failed: {0}")]
    Retrieval(String),
}

impl ErrorCode for ContextError {
    fn code(&self) -> &'static str {
        match self {
            ContextError::BudgetExceeded { .. } => "context.budget_exceeded",
            ContextError::ReadFailed { .. } => "context.read_failed",
            ContextError::BudgetInfeasible { .. } => "context.budget_infeasible",
            ContextError::PolicyDoesNotFit { .. } => "context.policy_does_not_fit",
            ContextError::UnbudgetedSection(_) => "context.unbudgeted_section",
            ContextError::Retrieval(_) => "context.retrieval_failed",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, ContextError>;
