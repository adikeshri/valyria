//! Parsing a model's [`Completion`] into a driver-level decision (§4.24's
//! "Select" step). Sketched only in prose in docs/PLAN.md — this is the
//! concrete implementation: `ActionRequest` did not exist anywhere in the
//! workspace before this crate.

use valyria_model::{Completion, FinishReason};

use crate::error::AgentError;

#[derive(Debug, Clone, PartialEq)]
pub enum ActionRequest {
    ToolCall {
        tool: String,
        input: serde_json::Value,
    },
    Finish {
        summary: String,
    },
    Ask {
        question: String,
    },
}

impl ActionRequest {
    pub fn from_completion(completion: &Completion) -> Result<Self, AgentError> {
        match completion.finish_reason {
            FinishReason::ToolCalls => {
                if completion.tool_calls.len() != 1 {
                    return Err(AgentError::MalformedCompletion {
                        detail: format!(
                            "expected exactly one tool call, got {}",
                            completion.tool_calls.len()
                        ),
                    });
                }
                let call = &completion.tool_calls[0];
                Ok(ActionRequest::ToolCall {
                    tool: call.name.clone(),
                    input: call.arguments.clone(),
                })
            }
            FinishReason::Stop => Ok(ActionRequest::Finish {
                summary: completion.text.clone(),
            }),
            FinishReason::Ask => Ok(ActionRequest::Ask {
                question: completion.text.clone(),
            }),
            FinishReason::Length | FinishReason::Cancelled => {
                Err(AgentError::MalformedCompletion {
                    detail: format!(
                        "unsupported finish_reason for an agent turn: {:?}",
                        completion.finish_reason
                    ),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_model::{TokenUsage, ToolCall};

    fn completion(
        finish_reason: FinishReason,
        text: &str,
        tool_calls: Vec<ToolCall>,
    ) -> Completion {
        Completion {
            text: text.to_string(),
            tool_calls,
            finish_reason,
            usage: TokenUsage::default(),
        }
    }

    #[test]
    fn single_tool_call_maps_to_tool_call_action() {
        let c = completion(
            FinishReason::ToolCalls,
            "",
            vec![ToolCall {
                id: "call_0".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "a.txt"}),
            }],
        );
        let action = ActionRequest::from_completion(&c).unwrap();
        assert_eq!(
            action,
            ActionRequest::ToolCall {
                tool: "read_file".into(),
                input: serde_json::json!({"path": "a.txt"})
            }
        );
    }

    #[test]
    fn zero_tool_calls_with_tool_calls_finish_reason_is_malformed() {
        let c = completion(FinishReason::ToolCalls, "", vec![]);
        let err = ActionRequest::from_completion(&c).unwrap_err();
        assert!(matches!(err, AgentError::MalformedCompletion { .. }));
    }

    #[test]
    fn multiple_tool_calls_is_malformed() {
        let call = ToolCall {
            id: "c".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        let c = completion(FinishReason::ToolCalls, "", vec![call.clone(), call]);
        let err = ActionRequest::from_completion(&c).unwrap_err();
        assert!(matches!(err, AgentError::MalformedCompletion { .. }));
    }

    #[test]
    fn stop_maps_to_finish() {
        let c = completion(FinishReason::Stop, "all done", vec![]);
        assert_eq!(
            ActionRequest::from_completion(&c).unwrap(),
            ActionRequest::Finish {
                summary: "all done".into()
            }
        );
    }

    #[test]
    fn ask_maps_to_ask() {
        let c = completion(FinishReason::Ask, "which file?", vec![]);
        assert_eq!(
            ActionRequest::from_completion(&c).unwrap(),
            ActionRequest::Ask {
                question: "which file?".into()
            }
        );
    }

    #[test]
    fn length_and_cancelled_are_malformed_for_an_agent_turn() {
        for reason in [FinishReason::Length, FinishReason::Cancelled] {
            let c = completion(reason, "", vec![]);
            assert!(matches!(
                ActionRequest::from_completion(&c).unwrap_err(),
                AgentError::MalformedCompletion { .. }
            ));
        }
    }
}
