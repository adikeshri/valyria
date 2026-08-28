//! What an adapter can do, and its current health. Deliberately minimal:
//! the Phase 9 offline slice (catalog, model store, OpenAI-compatible
//! adapter, transport ladder) needs only `supports_native_tools` /
//! `supports_grammar` / `supports_streaming` / `context_length`.
//! vision/embeddings/batch/logprobs/kv-cache-reuse fields land additively
//! (§4.20) when an adapter that reports them arrives; adding a field here
//! is not a breaking change for existing callers since every field is
//! data, not a trait method.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub context_length: u32,
    pub supports_native_tools: bool,
    pub supports_grammar: bool,
    pub supports_streaming: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Degraded { reason: String },
    Unavailable { reason: String },
}

impl Health {
    pub fn is_usable(&self) -> bool {
        !matches!(self, Health::Unavailable { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degraded_is_still_usable() {
        assert!(Health::Degraded {
            reason: "slow".into()
        }
        .is_usable());
    }

    #[test]
    fn unavailable_is_not_usable() {
        assert!(!Health::Unavailable {
            reason: "down".into()
        }
        .is_usable());
    }
}
