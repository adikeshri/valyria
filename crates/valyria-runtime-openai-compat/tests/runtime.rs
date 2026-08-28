//! The adapter driven end to end against the scripted [`MockTransport`]:
//! health, buffered generation, native tool calls, SSE streaming, and
//! cancellation on both paths.

use futures::StreamExt;
use serde_json::json;
use valyria_model::{
    Capabilities, FinishReason, GenerateRequest, Message, ModelError, ModelRuntime,
};
use valyria_runtime_openai_compat::{MockTransport, OpenAiCompatRuntime};
use valyria_util::CancellationToken;

fn caps() -> Capabilities {
    OpenAiCompatRuntime::<MockTransport>::conservative_capabilities(8192)
}

fn runtime(transport: MockTransport) -> OpenAiCompatRuntime<MockTransport> {
    OpenAiCompatRuntime::new(transport, "test-model", caps())
}

#[tokio::test]
async fn health_reflects_the_server_probe() {
    let up = runtime(MockTransport::new().with_response("/health", br#"{"status":"ok"}"#.to_vec()));
    assert_eq!(up.health().await, valyria_model::Health::Healthy);

    let down = MockTransport::new();
    down.set_down(true);
    let rt = runtime(down);
    assert!(matches!(
        rt.health().await,
        valyria_model::Health::Unavailable { .. }
    ));
}

#[tokio::test]
async fn generate_maps_a_plain_completion_and_sends_a_well_formed_request() {
    let transport = MockTransport::new().with_json_response(
        "/v1/chat/completions",
        json!({
            "choices": [{"message": {"content": "the answer is 42"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 5}
        }),
    );
    let rt = runtime(transport);

    let req = GenerateRequest::new(vec![Message::system("be terse"), Message::user("q?")]);
    let completion = rt.generate(req, CancellationToken::new()).await.unwrap();

    assert_eq!(completion.text, "the answer is 42");
    assert_eq!(completion.finish_reason, FinishReason::Stop);
    assert_eq!(completion.usage.completion_tokens, 5);

    let sent = rt.transport().last_body().unwrap();
    assert_eq!(sent["model"], "test-model");
    assert_eq!(sent["stream"], false);
    assert_eq!(sent["messages"][1]["content"], "q?");
}

#[tokio::test]
async fn generate_extracts_native_tool_calls() {
    let transport = MockTransport::new().with_json_response(
        "/v1/chat/completions",
        json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_7",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\": \"src/main.rs\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }),
    );
    let rt = runtime(transport);

    let completion = rt
        .generate(
            GenerateRequest::new(vec![Message::user("open main")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(completion.finish_reason, FinishReason::ToolCalls);
    assert_eq!(completion.tool_calls.len(), 1);
    assert_eq!(completion.tool_calls[0].name, "read_file");
    assert_eq!(completion.tool_calls[0].arguments["path"], "src/main.rs");
}

#[tokio::test]
async fn unreachable_server_is_a_retryable_unavailable_error() {
    let transport = MockTransport::new();
    transport.set_down(true);
    let rt = runtime(transport);
    let err = rt
        .generate(
            GenerateRequest::new(vec![Message::user("hi")]),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ModelError::Unavailable { .. }));
    assert!(valyria_types::ErrorCode::retryable(&err));
}

#[tokio::test]
async fn malformed_body_is_reported_not_panicked() {
    let transport =
        MockTransport::new().with_response("/v1/chat/completions", b"<html>gateway error".to_vec());
    let rt = runtime(transport);
    let err = rt
        .generate(
            GenerateRequest::new(vec![Message::user("hi")]),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ModelError::MalformedOutput { .. }));
}

#[tokio::test]
async fn generate_honours_a_pre_cancelled_token() {
    let transport = MockTransport::new().with_json_response(
        "/v1/chat/completions",
        json!({"choices": [{"message": {"content": "x"}, "finish_reason": "stop"}]}),
    );
    let rt = runtime(transport);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = rt
        .generate(GenerateRequest::new(vec![Message::user("hi")]), cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, ModelError::Cancelled));
}

#[tokio::test]
async fn stream_reassembles_deltas_and_terminates_on_done() {
    let events = vec![
        r#"{"choices":[{"delta":{"content":"Hel"}}]}"#.to_string(),
        r#"{"choices":[{"delta":{"content":"lo"}}]}"#.to_string(),
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#.to_string(),
        "[DONE]".to_string(),
    ];
    let transport = MockTransport::new().with_sse("/v1/chat/completions", events);
    let rt = runtime(transport);

    let mut stream = rt.stream(
        GenerateRequest::new(vec![Message::user("hi")]),
        CancellationToken::new(),
    );
    let mut text = String::new();
    let mut saw_done = false;
    while let Some(item) = stream.next().await {
        let chunk = item.unwrap();
        text.push_str(&chunk.delta);
        saw_done |= chunk.done;
    }
    assert_eq!(text, "Hello");
    assert!(saw_done);
}

#[tokio::test]
async fn stream_stops_when_the_token_is_cancelled_midway() {
    let events: Vec<String> = (0..100)
        .map(|i| format!(r#"{{"choices":[{{"delta":{{"content":"{i} "}}}}]}}"#))
        .collect();
    let transport = MockTransport::new().with_sse("/v1/chat/completions", events);
    let rt = runtime(transport);

    let cancel = CancellationToken::new();
    let mut stream = rt.stream(
        GenerateRequest::new(vec![Message::user("count")]),
        cancel.clone(),
    );

    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.delta, "0 ");
    cancel.cancel();

    let next = stream.next().await.unwrap();
    assert!(matches!(next, Err(ModelError::Cancelled)));
    assert!(
        stream.next().await.is_none(),
        "stream must end after cancel"
    );
}
