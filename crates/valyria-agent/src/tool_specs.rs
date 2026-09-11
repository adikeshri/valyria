//! Translate `valyria_tools::ToolDescriptor` into the model-facing
//! `valyria_model::ToolSpec` every `GenerateRequest` in the live agent
//! loop is bound with.

use valyria_model::ToolSpec;
use valyria_tools::ToolRuntime;

/// Registered but stubbed (`tools.not_yet_implemented` unconditionally) —
/// offering these lets a model "successfully" pick a tool call that can
/// never succeed. Excluded until the real search/index pipeline lands
/// (docs/ROADMAP.md's context/search wiring follow-up).
const EXCLUDED: &[&str] = &["search", "symbol_search"];

/// Every tool the model is allowed to call this turn — the full registry
/// minus the not-yet-implemented stubs.
pub(crate) fn bound_tool_specs(tools: &ToolRuntime) -> Vec<ToolSpec> {
    tools
        .descriptors()
        .into_iter()
        .filter(|d| !EXCLUDED.contains(&d.name))
        .map(|d| ToolSpec {
            name: d.name.to_string(),
            description: d.description.to_string(),
            input_schema: d.input_schema,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use valyria_permissions::PermissionEngine;
    use valyria_types::PermissionMode;
    use valyria_util::FixedClock;

    fn runtime() -> ToolRuntime {
        let clock: Arc<dyn valyria_util::Clock> = Arc::new(FixedClock::at_millis(0));
        let engine = Arc::new(PermissionEngine::new(PermissionMode::Manual, clock.clone()));
        ToolRuntime::new(valyria_tools::all_tools(), engine, clock)
    }

    #[test]
    fn excludes_not_yet_implemented_search_tools() {
        let specs = bound_tool_specs(&runtime());
        assert!(!specs.iter().any(|s| s.name == "search"));
        assert!(!specs.iter().any(|s| s.name == "symbol_search"));
    }

    #[test]
    fn includes_real_tools() {
        let specs = bound_tool_specs(&runtime());
        assert!(specs.iter().any(|s| s.name == "read_file"));
    }
}
