//! Chat message and tool-call vocabulary shared by every `ModelRuntime`
//! adapter, so `valyria-orchestrator` and `valyria-context` can build a
//! [`GenerateRequest`](crate::request::GenerateRequest) without knowing
//! which adapter will serve it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// One turn in a conversation. `tool_call_id` is set only on a `Tool`-role
/// message, linking a tool result back to the `ToolCall` that requested it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// A tool the model is allowed to call this turn, mirroring
/// `valyria_tools::ToolDescriptor` at the model boundary without creating a
/// dependency on layer 3 from layer 4 (`valyria-orchestrator` is what
/// translates between the two).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// One tool invocation requested by the model in its completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_carries_call_id() {
        let msg = Message::tool_result("call_1", "42");
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn non_tool_messages_have_no_call_id() {
        assert_eq!(Message::user("hi").tool_call_id, None);
    }

    #[test]
    fn message_serde_round_trip() {
        let msg = Message::assistant("hello");
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}
