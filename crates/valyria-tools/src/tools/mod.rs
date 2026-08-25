pub mod environment;
pub mod fs;
pub mod git;
pub(crate) mod helpers;
pub mod process;
pub mod search;

pub use environment::InspectEnvironmentTool;
pub use fs::{
    DeleteFileTool, EditFileTool, ListDirectoryTool, MoveFileTool, ReadFileTool, WriteFileTool,
};
pub use git::{GitBlameTool, GitDiffTool, GitLogTool, GitShowTool, GitStatusTool};
pub use process::{RunCommandTool, RunFormatterTool, RunLinterTool, RunTestTool};
pub use search::{SearchTool, SymbolSearchTool};

use std::sync::Arc;

use crate::runtime::ToolRegistry;
use crate::tool_trait::Tool;

/// A registry pre-populated with every first-class tool (§17). This is
/// the one place that has to know about every tool that exists; adding a
/// new tool means adding one line here plus its module.
pub fn all_tools() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ReadFileTool::default()),
        Arc::new(WriteFileTool::default()),
        Arc::new(EditFileTool::default()),
        Arc::new(DeleteFileTool::default()),
        Arc::new(MoveFileTool::default()),
        Arc::new(ListDirectoryTool::default()),
        Arc::new(GitStatusTool::default()),
        Arc::new(GitDiffTool::default()),
        Arc::new(GitLogTool::default()),
        Arc::new(GitShowTool::default()),
        Arc::new(GitBlameTool::default()),
        Arc::new(RunCommandTool::default()),
        Arc::new(RunTestTool::default()),
        Arc::new(RunFormatterTool::default()),
        Arc::new(RunLinterTool::default()),
        Arc::new(InspectEnvironmentTool::default()),
        Arc::new(SearchTool::default()),
        Arc::new(SymbolSearchTool::default()),
    ];
    for tool in tools {
        registry.register(tool);
    }
    registry
}
