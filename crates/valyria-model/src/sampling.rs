//! Sampling parameters. Kept adapter-agnostic — a runtime that doesn't
//! support a given knob (e.g. `top_p` on a greedy-only backend) is free to
//! ignore it; there is no capability negotiation for sampling params in
//! Phase 3 (real adapters land in Phase 9).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SamplingParams {
    pub temperature: f32,
    pub top_p: f32,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
}

impl Default for SamplingParams {
    fn default() -> Self {
        Self {
            temperature: 0.2,
            top_p: 1.0,
            max_tokens: None,
            stop: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_low_temperature_deterministic_leaning() {
        let params = SamplingParams::default();
        assert_eq!(params.temperature, 0.2);
        assert!(params.stop.is_empty());
    }
}
