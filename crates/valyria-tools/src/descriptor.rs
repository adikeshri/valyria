//! Tool descriptors (§17): every tool exposes a structured schema, not
//! just a name and a prose description.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffect {
    ReadOnly,
    WritesFilesystem,
    ExecutesProcess,
}

#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub side_effect: SideEffect,
}
