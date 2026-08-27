//! `FakeModelRuntime` (D12): a `ModelRuntime` that plays back a fixed
//! [`Scenario`] instead of calling a real model. Nearly all agent-loop
//! tests, and Phase 3's walking-skeleton demo, run against this.
//!
//! **Load-bearing design point**: the turn index is not internal mutable
//! state on this struct — it comes from `req.turn_hint`, which the caller
//! (`valyria-agent`'s step driver) derives from a durable journal query
//! (`TaskManager::count_model_calls`) on every call. A freshly-constructed
//! `FakeModelRuntime` is therefore a pure function of `(scenario,
//! turn_hint)`: after a crash, recovery never needs to fast-forward a live
//! object, it just recomputes the index from the journal and asks for that
//! turn again, so replay is deterministic by construction.

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use valyria_model::{
    Capabilities, Chunk, Completion, FinishReason, GenerateRequest, Health, ModelError,
    ModelRuntime, TokenUsage, ToolCall,
};
use valyria_util::CancellationToken;

use crate::error::FakeRuntimeError;
use crate::scenario::{Scenario, ScriptedTurn};

pub struct FakeModelRuntime {
    scenario: Scenario,
    capabilities: Capabilities,
}

impl FakeModelRuntime {
    pub fn from_scenario(scenario: Scenario) -> Self {
        Self {
            scenario,
            capabilities: Capabilities {
                context_length: 1_000_000,
                supports_native_tools: true,
                supports_grammar: false,
                supports_streaming: true,
            },
        }
    }

    fn turn_for(&self, turn_hint: Option<usize>) -> Result<&ScriptedTurn, ModelError> {
        let index = turn_hint.unwrap_or(0);
        self.scenario.turns.get(index).ok_or_else(|| {
            let err = FakeRuntimeError::TurnIndexOutOfRange {
                scenario: self.scenario.name.clone(),
                index,
                len: self.scenario.turns.len(),
            };
            ModelError::MalformedOutput {
                detail: err.to_string(),
            }
        })
    }

    fn completion_for(turn: &ScriptedTurn, turn_index: usize) -> Completion {
        match turn {
            ScriptedTurn::ToolCall { name, arguments } => Completion {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: format!("call_{turn_index}"),
                    name: name.clone(),
                    arguments: arguments.clone(),
                }],
                finish_reason: FinishReason::ToolCalls,
                usage: TokenUsage::default(),
            },
            ScriptedTurn::Finish { summary } => Completion {
                text: summary.clone(),
                tool_calls: vec![],
                finish_reason: FinishReason::Stop,
                usage: TokenUsage::default(),
            },
            ScriptedTurn::Ask { question } => Completion {
                text: question.clone(),
                tool_calls: vec![],
                finish_reason: FinishReason::Ask,
                usage: TokenUsage::default(),
            },
            ScriptedTurn::Malformed { raw } => Completion {
                text: raw.clone(),
                tool_calls: vec![],
                finish_reason: FinishReason::Length,
                usage: TokenUsage::default(),
            },
        }
    }
}

#[async_trait]
impl ModelRuntime for FakeModelRuntime {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    async fn health(&self) -> Health {
        Health::Healthy
    }

    fn count_tokens(&self, text: &str) -> usize {
        // Matches valyria_util::HeuristicTokenCounter's ~4-chars/token
        // estimate so budget math is consistent before a real tokenizer
        // exists.
        text.chars()
            .count()
            .div_ceil(4)
            .max(if text.is_empty() { 0 } else { 1 })
    }

    async fn generate(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> Result<Completion, ModelError> {
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let index = req.turn_hint.unwrap_or(0);
        let turn = self.turn_for(req.turn_hint)?;
        Ok(Self::completion_for(turn, index))
    }

    fn stream(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<Chunk, ModelError>> {
        let index = req.turn_hint.unwrap_or(0);
        let result = if cancel.is_cancelled() {
            Err(ModelError::Cancelled)
        } else {
            self.turn_for(req.turn_hint)
                .map(|turn| Self::completion_for(turn, index))
        };
        let chunk = result.map(|completion| Chunk {
            delta: completion.text,
            tool_call_delta: completion.tool_calls.into_iter().next(),
            done: true,
        });
        stream::once(async move { chunk }).boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_model::{GenerateRequest, Message};

    fn scenario() -> Scenario {
        Scenario {
            name: "test".into(),
            turns: vec![
                ScriptedTurn::ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                },
                ScriptedTurn::Ask {
                    question: "which file?".into(),
                },
                ScriptedTurn::Finish {
                    summary: "done".into(),
                },
            ],
        }
    }

    #[tokio::test]
    async fn tool_call_turn_maps_to_tool_calls_completion() {
        let runtime = FakeModelRuntime::from_scenario(scenario());
        let req = GenerateRequest::new(vec![Message::user("go")]).with_turn_hint(0);
        let completion = runtime
            .generate(req, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completion.finish_reason, FinishReason::ToolCalls);
        assert_eq!(completion.tool_calls.len(), 1);
        assert_eq!(completion.tool_calls[0].name, "read_file");
    }

    #[tokio::test]
    async fn ask_turn_maps_to_ask_finish_reason() {
        let runtime = FakeModelRuntime::from_scenario(scenario());
        let req = GenerateRequest::new(vec![Message::user("go")]).with_turn_hint(1);
        let completion = runtime
            .generate(req, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completion.finish_reason, FinishReason::Ask);
        assert_eq!(completion.text, "which file?");
    }

    #[tokio::test]
    async fn finish_turn_maps_to_stop() {
        let runtime = FakeModelRuntime::from_scenario(scenario());
        let req = GenerateRequest::new(vec![Message::user("go")]).with_turn_hint(2);
        let completion = runtime
            .generate(req, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completion.finish_reason, FinishReason::Stop);
        assert_eq!(completion.text, "done");
    }

    #[tokio::test]
    async fn missing_turn_hint_defaults_to_index_zero() {
        let runtime = FakeModelRuntime::from_scenario(scenario());
        let req = GenerateRequest::new(vec![Message::user("go")]);
        let completion = runtime
            .generate(req, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completion.finish_reason, FinishReason::ToolCalls);
    }

    #[tokio::test]
    async fn out_of_range_turn_index_errors() {
        let runtime = FakeModelRuntime::from_scenario(scenario());
        let req = GenerateRequest::new(vec![Message::user("go")]).with_turn_hint(99);
        let err = runtime
            .generate(req, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ModelError::MalformedOutput { .. }));
    }

    #[tokio::test]
    async fn cancelled_token_short_circuits() {
        let runtime = FakeModelRuntime::from_scenario(scenario());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let req = GenerateRequest::new(vec![Message::user("go")]);
        let err = runtime.generate(req, cancel).await.unwrap_err();
        assert!(matches!(err, ModelError::Cancelled));
    }

    #[tokio::test]
    async fn same_scenario_same_index_is_pure() {
        // Two independently constructed runtimes over the same scenario
        // return identical completions for the same turn_hint — this is
        // the property crash recovery depends on.
        let a = FakeModelRuntime::from_scenario(scenario());
        let b = FakeModelRuntime::from_scenario(scenario());
        let req = || GenerateRequest::new(vec![Message::user("go")]).with_turn_hint(0);
        let ca = a.generate(req(), CancellationToken::new()).await.unwrap();
        let cb = b.generate(req(), CancellationToken::new()).await.unwrap();
        assert_eq!(ca, cb);
    }
}
