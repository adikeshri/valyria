//! `Orchestrator::generate_action` — the tool-call ladder wired into the
//! role-bound generate path (A7). Complements `tests/transport_ladder.rs`
//! (which exercises the ladder's free functions directly) by proving the
//! *orchestrator-level* contract: a clean native turn costs exactly one
//! model call, messy output gets recovered or retried, and the result
//! comes back as a normal `Completion` the driver doesn't need to treat
//! specially.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::json;
use valyria_model::{
    Capabilities, Chunk, Completion, FinishReason, GenerateRequest, Health, Message, ModelError,
    ModelRuntime,
};
use valyria_orchestrator::{Orchestrator, OrchestratorError, Role};
use valyria_runtime_fake::{FakeModelRuntime, Scenario, ScriptedTurn};
use valyria_util::CancellationToken;

fn scenario(turns: Vec<ScriptedTurn>) -> Scenario {
    Scenario {
        name: "generate_action".into(),
        turns,
    }
}

fn req() -> GenerateRequest {
    GenerateRequest::new(vec![Message::user("do the thing")]).with_turn_hint(0)
}

/// Wraps a `ModelRuntime` and counts every `generate` call — used to prove
/// the fast path issues exactly one.
struct CountingRuntime {
    inner: FakeModelRuntime,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ModelRuntime for CountingRuntime {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    async fn health(&self) -> Health {
        self.inner.health().await
    }
    fn count_tokens(&self, text: &str) -> usize {
        self.inner.count_tokens(text)
    }
    async fn generate(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> Result<Completion, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.generate(req, cancel).await
    }
    fn stream(
        &self,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<Chunk, ModelError>> {
        self.inner.stream(req, cancel)
    }
}

#[tokio::test]
async fn native_single_tool_call_is_the_fast_path_no_extra_generate() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counting = CountingRuntime {
        inner: FakeModelRuntime::from_scenario(scenario(vec![ScriptedTurn::ToolCall {
            name: "read_file".into(),
            arguments: json!({"path": "a.rs"}),
        }])),
        calls: calls.clone(),
    };
    let orch = Orchestrator::new();
    orch.bind(Role::PrimaryCoder, Arc::new(counting));

    let completion = orch
        .generate_action(Role::PrimaryCoder, req(), CancellationToken::new(), 2)
        .await
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(completion.finish_reason, FinishReason::ToolCalls);
    assert_eq!(completion.tool_calls.len(), 1);
    assert_eq!(completion.tool_calls[0].name, "read_file");
}

#[tokio::test]
async fn fenced_json_answer_is_recovered_into_a_tool_call() {
    let orch = Orchestrator::new();
    orch.bind(
        Role::PrimaryCoder,
        Arc::new(FakeModelRuntime::from_scenario(scenario(vec![
            ScriptedTurn::Malformed {
                raw: "```json\n{\"name\":\"read_file\",\"arguments\":{\"path\":\"a\"}}\n```".into(),
            },
        ]))),
    );

    let completion = orch
        .generate_action(Role::PrimaryCoder, req(), CancellationToken::new(), 2)
        .await
        .unwrap();

    assert_eq!(completion.finish_reason, FinishReason::ToolCalls);
    assert_eq!(completion.tool_calls.len(), 1);
    assert_eq!(completion.tool_calls[0].name, "read_file");
}

#[tokio::test]
async fn multi_tool_call_collapses_to_the_first() {
    let orch = Orchestrator::new();
    orch.bind(
        Role::PrimaryCoder,
        Arc::new(FakeModelRuntime::from_scenario(scenario(vec![
            ScriptedTurn::Malformed {
                raw: json!([
                    {"name": "read_file", "arguments": {"path": "a"}},
                    {"name": "read_file", "arguments": {"path": "b"}},
                ])
                .to_string(),
            },
        ]))),
    );

    let completion = orch
        .generate_action(Role::PrimaryCoder, req(), CancellationToken::new(), 2)
        .await
        .unwrap();

    assert_eq!(completion.tool_calls.len(), 1);
    assert_eq!(completion.tool_calls[0].arguments["path"], "a");
}

#[tokio::test]
async fn unparseable_after_the_retry_budget_errors() {
    // `resolve_action`'s retry path bumps `turn_hint` by one each pass, so
    // scripting N+1 unparseable turns covers a budget of N retries.
    let bad = ScriptedTurn::Malformed {
        raw: "<tool_call>{ not json }</tool_call>".into(),
    };
    let orch = Orchestrator::new();
    orch.bind(
        Role::PrimaryCoder,
        Arc::new(FakeModelRuntime::from_scenario(scenario(vec![
            bad.clone(),
            bad.clone(),
            bad,
        ]))),
    );

    let err = orch
        .generate_action(Role::PrimaryCoder, req(), CancellationToken::new(), 2)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        OrchestratorError::UnparseableToolCall { attempts: 3, .. }
    ));
}

#[tokio::test]
async fn plain_finish_maps_to_stop() {
    let orch = Orchestrator::new();
    orch.bind(
        Role::PrimaryCoder,
        Arc::new(FakeModelRuntime::from_scenario(scenario(vec![
            ScriptedTurn::Finish {
                summary: "all done".into(),
            },
        ]))),
    );

    let completion = orch
        .generate_action(Role::PrimaryCoder, req(), CancellationToken::new(), 2)
        .await
        .unwrap();
    assert_eq!(completion.finish_reason, FinishReason::Stop);
    assert_eq!(completion.text, "all done");
}

#[tokio::test]
async fn ask_maps_through_unchanged() {
    let orch = Orchestrator::new();
    orch.bind(
        Role::PrimaryCoder,
        Arc::new(FakeModelRuntime::from_scenario(scenario(vec![
            ScriptedTurn::Ask {
                question: "which file?".into(),
            },
        ]))),
    );

    let completion = orch
        .generate_action(Role::PrimaryCoder, req(), CancellationToken::new(), 2)
        .await
        .unwrap();
    assert_eq!(completion.finish_reason, FinishReason::Ask);
    assert_eq!(completion.text, "which file?");
}
