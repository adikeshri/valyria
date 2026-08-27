//! The result of a `generate` call, and the incremental chunk shape for
//! `stream`.

use serde::{Deserialize, Serialize};

use crate::message::ToolCall;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Cancelled,
    /// The model wants to ask the user a clarifying question before
    /// proceeding, distinct from finishing the task (`Stop`) or calling a
    /// tool (`ToolCalls`). `Completion.text` carries the question.
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: TokenUsage,
}

/// One incremental piece of a streamed completion. `done` marks the final
/// chunk; a real adapter's stream must always terminate with one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub delta: String,
    pub tool_call_delta: Option<ToolCall>,
    pub done: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_defaults_to_zero() {
        assert_eq!(
            TokenUsage::default(),
            TokenUsage {
                prompt_tokens: 0,
                completion_tokens: 0
            }
        );
    }

    #[test]
    fn completion_serde_round_trip() {
        let completion = Completion {
            text: "done".into(),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            usage: TokenUsage {
                prompt_tokens: 3,
                completion_tokens: 1,
            },
        };
        let json = serde_json::to_string(&completion).unwrap();
        let back: Completion = serde_json::from_str(&json).unwrap();
        assert_eq!(completion, back);
    }
}
