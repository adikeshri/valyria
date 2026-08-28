//! Translation between `valyria-model`'s adapter-agnostic vocabulary and
//! the OpenAI `/v1/chat/completions` wire format that llama-server, vLLM,
//! Ollama and LM Studio all speak.

use serde::Deserialize;
use serde_json::{json, Value};
use valyria_model::{Completion, FinishReason, GenerateRequest, Role, TokenUsage, ToolCall};

/// Build the JSON body for `POST /v1/chat/completions`.
pub fn build_chat_request(model: &str, req: &GenerateRequest, stream: bool) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let mut obj = json!({ "role": role_str(m.role), "content": m.content });
            if let Some(id) = &m.tool_call_id {
                obj["tool_call_id"] = json!(id);
            }
            obj
        })
        .collect();

    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": stream,
        "temperature": req.sampling.temperature,
        "top_p": req.sampling.top_p,
    });

    if let Some(max) = req.sampling.max_tokens {
        body["max_tokens"] = json!(max);
    }
    if !req.sampling.stop.is_empty() {
        body["stop"] = json!(req.sampling.stop);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect(),
        );
        body["tool_choice"] = json!("auto");
    }
    body
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

// --- non-streaming response --------------------------------------------

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    message: ChoiceMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCall>>,
}

#[derive(Debug, Deserialize)]
struct WireToolCall {
    #[serde(default)]
    id: Option<String>,
    function: WireFunction,
}

#[derive(Debug, Deserialize)]
struct WireFunction {
    name: String,
    /// OpenAI encodes arguments as a JSON *string*.
    #[serde(default)]
    arguments: String,
}

#[derive(Debug, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

/// Parse a full (non-streamed) completion body.
pub fn parse_completion(bytes: &[u8]) -> Result<Completion, String> {
    let resp: ChatResponse =
        serde_json::from_slice(bytes).map_err(|e| format!("chat response: {e}"))?;
    let choice = resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| "chat response had no choices".to_string())?;

    let tool_calls: Vec<ToolCall> = choice
        .message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(i, tc)| ToolCall {
            id: tc.id.unwrap_or_else(|| format!("call_{i}")),
            name: tc.function.name,
            arguments: parse_arguments(&tc.function.arguments),
        })
        .collect();

    let finish_reason = match choice.finish_reason.as_deref() {
        Some("stop") => FinishReason::Stop,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("length") => FinishReason::Length,
        _ if !tool_calls.is_empty() => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    };

    Ok(Completion {
        text: choice.message.content.unwrap_or_default(),
        tool_calls,
        finish_reason,
        usage: resp
            .usage
            .map(|u| TokenUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
            })
            .unwrap_or_default(),
    })
}

/// A tool-call arguments string that isn't valid JSON is kept verbatim as a
/// string value rather than dropped — the orchestrator's transport ladder
/// is what decides whether to retry.
fn parse_arguments(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

// --- streaming chunks -------------------------------------------------

#[derive(Debug, Deserialize)]
struct StreamResponse {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCall>>,
}

/// Parse one SSE `data:` payload into an incremental [`Chunk`]. `done` is
/// set when the payload carries a `finish_reason`.
pub fn parse_stream_chunk(payload: &str) -> Result<valyria_model::Chunk, String> {
    let resp: StreamResponse =
        serde_json::from_str(payload).map_err(|e| format!("stream chunk: {e}"))?;
    let choice = resp.choices.into_iter().next().unwrap_or(StreamChoice {
        delta: StreamDelta::default(),
        finish_reason: None,
    });

    let tool_call_delta = choice
        .delta
        .tool_calls
        .and_then(|v| v.into_iter().next())
        .map(|tc| ToolCall {
            id: tc.id.unwrap_or_else(|| "call_0".to_string()),
            name: tc.function.name,
            arguments: parse_arguments(&tc.function.arguments),
        });

    Ok(valyria_model::Chunk {
        delta: choice.delta.content.unwrap_or_default(),
        tool_call_delta,
        done: choice.finish_reason.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_model::{Message, SamplingParams, ToolSpec};

    #[test]
    fn request_maps_messages_tools_and_sampling() {
        let req = GenerateRequest {
            messages: vec![
                Message::system("be terse"),
                Message::user("hi"),
                Message::tool_result("call_1", "42"),
            ],
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            sampling: SamplingParams {
                temperature: 0.3,
                top_p: 0.8,
                max_tokens: Some(256),
                stop: vec!["END".into()],
            },
            turn_hint: None,
        };
        let body = build_chat_request("m", &req, false);
        assert_eq!(body["model"], "m");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["max_tokens"], 256);
        assert_eq!(body["stop"][0], "END");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn parses_a_plain_text_completion() {
        let bytes = br#"{"choices":[{"message":{"content":"hello there"},"finish_reason":"stop"}],
                         "usage":{"prompt_tokens":10,"completion_tokens":3}}"#;
        let c = parse_completion(bytes).unwrap();
        assert_eq!(c.text, "hello there");
        assert_eq!(c.finish_reason, FinishReason::Stop);
        assert_eq!(c.usage.prompt_tokens, 10);
        assert!(c.tool_calls.is_empty());
    }

    #[test]
    fn parses_native_tool_calls_with_string_arguments() {
        let bytes = br#"{"choices":[{"message":{"content":null,"tool_calls":[
            {"id":"call_abc","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}
        ]},"finish_reason":"tool_calls"}]}"#;
        let c = parse_completion(bytes).unwrap();
        assert_eq!(c.finish_reason, FinishReason::ToolCalls);
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].id, "call_abc");
        assert_eq!(c.tool_calls[0].name, "read_file");
        assert_eq!(c.tool_calls[0].arguments["path"], "a.txt");
    }

    #[test]
    fn tool_calls_without_finish_reason_still_classify_as_tool_calls() {
        let bytes = br#"{"choices":[{"message":{"tool_calls":[
            {"function":{"name":"x","arguments":""}}]}}]}"#;
        let c = parse_completion(bytes).unwrap();
        assert_eq!(c.finish_reason, FinishReason::ToolCalls);
        assert_eq!(c.tool_calls[0].id, "call_0");
        assert_eq!(c.tool_calls[0].arguments, json!({}));
    }

    #[test]
    fn missing_choices_is_an_error_not_a_panic() {
        assert!(parse_completion(br#"{"choices":[]}"#).is_err());
        assert!(parse_completion(b"not json").is_err());
    }

    #[test]
    fn stream_chunk_carries_content_delta_and_done_flag() {
        let mid = parse_stream_chunk(r#"{"choices":[{"delta":{"content":"hel"}}]}"#).unwrap();
        assert_eq!(mid.delta, "hel");
        assert!(!mid.done);
        let last =
            parse_stream_chunk(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#).unwrap();
        assert!(last.done);
    }
}
