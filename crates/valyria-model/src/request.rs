//! The one request shape every `ModelRuntime::generate`/`stream` call takes,
//! independent of adapter.

use serde::{Deserialize, Serialize};

use crate::message::{Message, ToolSpec};
use crate::sampling::SamplingParams;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub sampling: SamplingParams,
    /// Optional hint for scripted/deterministic adapters (the fake runtime,
    /// and future eval harnesses) telling them which turn of a fixed script
    /// to play next, so the adapter can stay pure (no internal mutable
    /// cursor) and a crash-recovery replay re-derives the same value from
    /// the durable journal instead of resuming in-memory state. Real
    /// adapters ignore this field and decide their next action from
    /// `messages` alone.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub turn_hint: Option<usize>,
}

impl GenerateRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            sampling: SamplingParams::default(),
            turn_hint: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_sampling(mut self, sampling: SamplingParams) -> Self {
        self.sampling = sampling;
        self
    }

    pub fn with_turn_hint(mut self, turn_hint: usize) -> Self {
        self.turn_hint = Some(turn_hint);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;

    #[test]
    fn builder_defaults_to_no_tools() {
        let req = GenerateRequest::new(vec![Message::user("hi")]);
        assert!(req.tools.is_empty());
        assert_eq!(req.sampling, SamplingParams::default());
    }

    #[test]
    fn serde_round_trip() {
        let req = GenerateRequest::new(vec![Message::user("hi")]);
        let json = serde_json::to_string(&req).unwrap();
        let back: GenerateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }
}
