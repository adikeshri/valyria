//! What a tool's `execute` returns: a structured result for the runtime
//! plus a budget-aware rendered form for the model (§17) — the model never
//! sees a raw, unbounded blob, it sees a summary.

use serde_json::Value;

#[derive(Debug, Clone)]
pub enum ToolOutcome {
    Success {
        structured: Value,
        rendered: String,
    },
    Failure {
        code: &'static str,
        message: String,
        rendered: String,
    },
}

impl ToolOutcome {
    pub fn success(structured: Value, rendered: impl Into<String>) -> Self {
        ToolOutcome::Success {
            structured,
            rendered: rendered.into(),
        }
    }

    pub fn failure(code: &'static str, message: impl Into<String>) -> Self {
        let message = message.into();
        ToolOutcome::Failure {
            code,
            rendered: format!("error: {message}"),
            message,
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, ToolOutcome::Success { .. })
    }
}
