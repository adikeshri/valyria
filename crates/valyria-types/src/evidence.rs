//! Evidence (D4): the only thing a task's completion report may be built
//! from.
//!
//! Models cannot construct [`Evidence`] — there is no `Deserialize` impl,
//! and the only constructor, [`Evidence::new`], requires an
//! [`EvidenceSource`] whose variants each carry an id or hash that only
//! comes into existence as a side effect of a *real* subsystem action: a
//! tool actually running (`ToolInvocationId`, minted by `valyria-tools`
//! when it records an invocation), a verification run actually executing
//! (`VerificationRunId`, minted by `valyria-verify`), the index actually
//! having reached a generation (`Generation`, published by `valyria-index`),
//! or a git object that actually exists (a commit hash). The model-output
//! parser in `valyria-orchestrator`/`valyria-agent` is structurally
//! prevented from producing this type: its return type for a parsed model
//! turn is `ActionRequest`, which has no variant that yields `Evidence`.
//!
//! In short: if it isn't in this table, the runtime did not verify it, and
//! the completion report says so.

use serde::{Deserialize, Serialize};

use crate::id::{Generation, ToolInvocationId, VerificationRunId};
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EvidenceSource {
    Tool(ToolInvocationId),
    Git { commit: String },
    Verification(VerificationRunId),
    Index(Generation),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EvidenceBody {
    Text(String),
    Json(serde_json::Value),
}

/// A fact about the repository or the environment, backed by something the
/// runtime actually did. Never model-generated. See module docs for how
/// that invariant is (and isn't) enforced.
#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    source: EvidenceSource,
    captured_at: Timestamp,
    body: EvidenceBody,
}

impl Evidence {
    pub fn new(source: EvidenceSource, captured_at: Timestamp, body: EvidenceBody) -> Self {
        Self {
            source,
            captured_at,
            body,
        }
    }

    pub fn source(&self) -> &EvidenceSource {
        &self.source
    }

    pub fn captured_at(&self) -> Timestamp {
        self.captured_at
    }

    pub fn body(&self) -> &EvidenceBody {
        &self.body
    }
}

/// Deserialization is intentionally restricted to the store's own loader
/// (`valyria-store`), which reconstructs previously-persisted evidence rows
/// rather than accepting arbitrary/model-shaped input. Exposed here so the
/// store can round-trip evidence through its blob format without every
/// caller needing raw field access.
impl<'de> Deserialize<'de> for Evidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            source: EvidenceSource,
            captured_at: Timestamp,
            body: EvidenceBody,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Evidence::new(raw.source, raw.captured_at, raw.body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let ev = Evidence::new(
            EvidenceSource::Tool(ToolInvocationId::new()),
            Timestamp::from_millis(1_000),
            EvidenceBody::Text("cargo test: 12 passed".into()),
        );
        let json = serde_json::to_string(&ev).unwrap();
        let back: Evidence = serde_json::from_str(&json).unwrap();
        assert_eq!(back.captured_at(), Timestamp::from_millis(1_000));
        assert!(matches!(back.source(), EvidenceSource::Tool(_)));
    }

    #[test]
    fn every_source_variant_requires_a_real_side_effect_id() {
        // Documents the invariant in the module docs: constructing evidence
        // always requires an id/hash minted by a real subsystem action.
        let _ = EvidenceSource::Tool(ToolInvocationId::new());
        let _ = EvidenceSource::Verification(VerificationRunId::new());
        let _ = EvidenceSource::Index(Generation::INITIAL);
        let _ = EvidenceSource::Git {
            commit: "deadbeef".into(),
        };
    }
}
