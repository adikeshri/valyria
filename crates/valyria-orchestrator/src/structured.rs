//! The tool-call transport ladder (D5). Open-weight models emit tool calls
//! in wildly different shapes; a single `serde_json::from_str` on the model
//! text makes half of them look broken. This module is the tolerant
//! recovery layer:
//!
//! 1. **Native** — the adapter already parsed a `tool_calls` array; use it.
//! 2. **Fenced / tagged text** — strip ```` ```json ```` fences,
//!    `<tool_call>` / `[TOOL_CALLS]` / `<|python_tag|>` wrappers and prose,
//!    pull the first balanced JSON value, tolerate trailing commas, and
//!    accept every common object shape
//!    (`{name,arguments}`, `{tool,args}`, `{function:{...}}`, arrays …).
//! 3. **Bounded reformat-retry** — [`resolve_tool_calls`] feeds the parse
//!    error back to the model as evidence and asks again, at most
//!    `max_reformat_retries` times, before failing the turn.

use serde_json::Value;
use valyria_model::{Capabilities, Completion, GenerateRequest, Message, ModelRuntime, ToolCall};
use valyria_util::CancellationToken;

use crate::error::{OrchestratorError, Result};

/// The outcome of trying to read a tool call out of a completion.
#[derive(Debug, Clone, PartialEq)]
pub enum Extraction {
    /// One or more tool calls were recovered.
    Calls(Vec<ToolCall>),
    /// The model produced an ordinary answer; no tool call was attempted.
    NoCall,
}

/// The model *tried* to call a tool but the payload could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ExtractError(pub String);

/// Read tool calls from a completion, native path first, text recovery
/// second.
pub fn extract(
    completion: &Completion,
    _caps: &Capabilities,
) -> std::result::Result<Extraction, ExtractError> {
    if !completion.tool_calls.is_empty() {
        return Ok(Extraction::Calls(completion.tool_calls.clone()));
    }
    recover_from_text(&completion.text)
}

/// Tier 2: recover a tool call from free-form model text.
pub fn recover_from_text(text: &str) -> std::result::Result<Extraction, ExtractError> {
    let intended = looks_like_a_tool_call_attempt(text);
    let cleaned = strip_wrappers(text);

    let Some(slice) = first_json_value(&cleaned) else {
        return if intended {
            Err(ExtractError(
                "output is wrapped like a tool call but contains no JSON value".into(),
            ))
        } else {
            Ok(Extraction::NoCall)
        };
    };

    let value = parse_lenient(slice)
        .map_err(|e| ExtractError(format!("invalid JSON in tool call: {e}")))?;

    match interpret(&value) {
        Interpreted::Calls(calls) if !calls.is_empty() => Ok(Extraction::Calls(calls)),
        Interpreted::Calls(_) => Ok(Extraction::NoCall),
        Interpreted::NotACall => {
            if intended {
                Err(ExtractError(
                    "output is shaped like a tool call but has no recognizable name/arguments"
                        .into(),
                ))
            } else {
                Ok(Extraction::NoCall)
            }
        }
        Interpreted::Broken(why) => Err(ExtractError(why)),
    }
}

/// Run the full ladder against a live model: generate, extract, and on a
/// parse failure feed the error back and retry, bounded.
pub async fn resolve_tool_calls<M: ModelRuntime + ?Sized>(
    model: &M,
    mut req: GenerateRequest,
    cancel: &CancellationToken,
    max_reformat_retries: u32,
) -> Result<Vec<ToolCall>> {
    let caps = model.capabilities();
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let completion = model.generate(req.clone(), cancel.child()).await?;
        match extract(&completion, &caps) {
            Ok(Extraction::Calls(calls)) => return Ok(calls),
            Ok(Extraction::NoCall) => return Ok(Vec::new()),
            Err(ExtractError(detail)) => {
                if attempts > max_reformat_retries {
                    return Err(OrchestratorError::UnparseableToolCall { attempts, detail });
                }
                tracing::warn!(attempt = attempts, %detail, "reformat-retrying tool call");
                req.messages
                    .push(Message::assistant(completion.text.clone()));
                req.messages.push(Message::user(format!(
                    "That was not a valid tool call: {detail}. Respond with exactly one JSON \
                     object of the form {{\"name\": \"<tool>\", \"arguments\": {{ ... }}}} and \
                     nothing else."
                )));
                if let Some(hint) = req.turn_hint {
                    req.turn_hint = Some(hint + 1);
                }
            }
        }
    }
}

// --- wrapper stripping ------------------------------------------------

fn strip_wrappers(text: &str) -> String {
    let mut s = text.trim().to_string();

    // Llama 3.1 python tag / Mistral tool-calls prefix.
    for prefix in ["<|python_tag|>", "[TOOL_CALLS]", "TOOL_CALL:", "Tool call:"] {
        if let Some(rest) = s.trim_start().strip_prefix(prefix) {
            s = rest.trim().to_string();
        }
    }

    // Hermes / Qwen style tag pairs — keep the inner text.
    for (open, close) in [
        ("<tool_call>", "</tool_call>"),
        ("<tool_calls>", "</tool_calls>"),
        ("<function_call>", "</function_call>"),
    ] {
        if let (Some(a), Some(b)) = (s.find(open), s.rfind(close)) {
            if b > a {
                s = s[a + open.len()..b].trim().to_string();
            }
        }
    }

    // ```json ... ``` or ``` ... ``` fenced block — take the first fence's
    // contents.
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        let after = after.strip_prefix("json").unwrap_or(after);
        let after = after.strip_prefix("JSON").unwrap_or(after);
        if let Some(end) = after.find("```") {
            s = after[..end].trim().to_string();
        } else {
            s = after.trim().to_string();
        }
    }

    s
}

fn looks_like_a_tool_call_attempt(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "tool_call",
        "\"name\"",
        "\"arguments\"",
        "\"parameters\"",
        "python_tag",
        "```json",
    ]
    .iter()
    .any(|m| t.contains(m))
}

// --- balanced JSON extraction --------------------------------------

/// The first balanced `{...}` or `[...]` in `s`, string-aware so braces
/// inside string literals don't unbalance the scan.
fn first_json_value(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{' || b == b'[')?;
    let open = bytes[start];
    let close = if open == b'{' { b'}' } else { b']' };

    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            x if x == open => depth += 1,
            x if x == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_lenient(slice: &str) -> std::result::Result<Value, String> {
    match serde_json::from_str::<Value>(slice) {
        Ok(v) => Ok(v),
        Err(_) => {
            let cleaned = remove_trailing_commas(slice);
            serde_json::from_str::<Value>(&cleaned).map_err(|e| e.to_string())
        }
    }
}

fn remove_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_str = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            ',' => {
                // Skip whitespace to see if the next non-space is a closer.
                let mut lookahead = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() {
                        lookahead.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if matches!(chars.peek(), Some('}') | Some(']')) {
                    // drop the comma, keep the whitespace
                    out.push_str(&lookahead);
                } else {
                    out.push(',');
                    out.push_str(&lookahead);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

// --- shape interpretation ----------------------------------------

enum Interpreted {
    Calls(Vec<ToolCall>),
    NotACall,
    Broken(String),
}

fn interpret(value: &Value) -> Interpreted {
    match value {
        Value::Array(items) => {
            let mut calls = Vec::new();
            for item in items {
                match interpret_object(item, calls.len()) {
                    ObjOutcome::Call(c) => calls.push(c),
                    ObjOutcome::NotACall => {}
                    ObjOutcome::Broken(w) => return Interpreted::Broken(w),
                }
            }
            Interpreted::Calls(calls)
        }
        Value::Object(_) => match interpret_object(value, 0) {
            ObjOutcome::Call(c) => Interpreted::Calls(vec![c]),
            ObjOutcome::NotACall => Interpreted::NotACall,
            ObjOutcome::Broken(w) => Interpreted::Broken(w),
        },
        _ => Interpreted::NotACall,
    }
}

enum ObjOutcome {
    Call(ToolCall),
    NotACall,
    Broken(String),
}

fn interpret_object(value: &Value, index: usize) -> ObjOutcome {
    let Some(obj) = value.as_object() else {
        return ObjOutcome::NotACall;
    };

    // Nested wrappers: {"function": {...}} / {"tool_call": {...}}.
    for nest in ["function", "tool_call", "toolCall", "call"] {
        if let Some(inner) = obj.get(nest) {
            if inner.is_object() {
                return interpret_object(inner, index);
            }
        }
    }

    let name = [
        "name",
        "tool",
        "tool_name",
        "function_name",
        "recipient_name",
    ]
    .iter()
    .find_map(|k| obj.get(*k).and_then(Value::as_str));

    let Some(name) = name else {
        return ObjOutcome::NotACall;
    };

    let args_value = ["arguments", "args", "parameters", "params", "input"]
        .iter()
        .find_map(|k| obj.get(*k));

    let arguments = match args_value {
        None => Value::Object(Default::default()),
        Some(Value::String(s)) => {
            if s.trim().is_empty() {
                Value::Object(Default::default())
            } else {
                match parse_lenient(s) {
                    Ok(v) => v,
                    Err(e) => {
                        return ObjOutcome::Broken(format!(
                            "tool {name:?} arguments string is not valid JSON: {e}"
                        ))
                    }
                }
            }
        }
        Some(other) => other.clone(),
    };

    ObjOutcome::Call(ToolCall {
        id: format!("call_{index}"),
        name: name.to_string(),
        arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Capabilities {
        Capabilities {
            context_length: 8192,
            supports_native_tools: true,
            supports_grammar: false,
            supports_streaming: true,
        }
    }

    #[test]
    fn native_tool_calls_pass_straight_through() {
        let c = Completion {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_x".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "a"}),
            }],
            finish_reason: valyria_model::FinishReason::ToolCalls,
            usage: Default::default(),
        };
        assert_eq!(
            extract(&c, &caps()).unwrap(),
            Extraction::Calls(c.tool_calls.clone())
        );
    }

    #[test]
    fn plain_prose_is_not_a_tool_call() {
        assert_eq!(
            recover_from_text("I think the bug is in the parser.").unwrap(),
            Extraction::NoCall
        );
    }

    #[test]
    fn broken_json_inside_a_tool_call_wrapper_is_an_error() {
        let err = recover_from_text("<tool_call>{ this is not json }</tool_call>").unwrap_err();
        assert!(err.0.contains("JSON"));
    }
}
