pub mod bash;
pub mod file_read;
pub mod file_edit;
pub mod file_write;
pub mod glob_tool;
pub mod grep;
pub mod web_fetch;
pub mod web_search;
pub mod ask_user;
pub mod ls;
pub mod todo;
pub mod task;
pub mod multi_edit;
pub mod diff_ui;
pub mod sleep;
pub mod config_tool;
pub mod powershell;
pub mod notebook;
pub mod plan_mode;
pub mod tool_search;
pub mod mcp;
pub mod skill_tool;
pub mod repl;
pub mod send_message;
pub mod git;
pub mod context;
pub mod worktree;
pub mod lsp;
pub mod path_util;

use std::collections::HashMap;
use std::sync::Arc;
use claude_core::tool::{DynTool, Tool};

pub struct ToolRegistry {
    tools: HashMap<String, DynTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        let name = tool.name().to_string();
        self.tools.insert(name, Arc::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<&DynTool> {
        self.tools.get(name)
    }

    pub fn all(&self) -> Vec<&DynTool> {
        self.tools.values().collect()
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// Create a registry pre-loaded with all built-in tools
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(bash::BashTool);
        registry.register(file_read::FileReadTool);
        registry.register(file_edit::FileEditTool);
        registry.register(file_write::FileWriteTool);
        registry.register(glob_tool::GlobTool);
        registry.register(grep::GrepTool);
        registry.register(web_fetch::WebFetchTool);
        registry.register(ask_user::AskUserTool);
        registry.register(ls::LsTool);
        registry.register(todo::TodoWriteTool);
        registry.register(todo::TodoReadTool);
        registry.register(multi_edit::MultiEditTool);
        registry.register(sleep::SleepTool);
        registry.register(config_tool::ConfigTool);
        registry.register(powershell::PowerShellTool);
        registry.register(task::TaskCreateTool);
        registry.register(task::TaskUpdateTool);
        registry.register(task::TaskGetTool);
        registry.register(task::TaskListTool);
        registry.register(task::TaskOutputTool);
        registry.register(task::TaskStopTool);
        registry.register(web_search::WebSearchTool);
        registry.register(notebook::NotebookEditTool);
        registry.register(plan_mode::EnterPlanModeTool);
        registry.register(plan_mode::ExitPlanModeTool);
        registry.register(tool_search::ToolSearchTool);
        registry.register(skill_tool::SkillTool);
        registry.register(repl::ReplTool);
        registry.register(send_message::SendUserMessageTool);
        registry.register(git::GitTool);
        registry.register(git::GitStatusTool);
        registry.register(context::ContextInspectTool);
        registry.register(context::VerifyTool);
        registry.register(worktree::EnterWorktreeTool);
        registry.register(worktree::ExitWorktreeTool);
        registry.register(lsp::LspTool);
        // MCP tools registered but disabled until MCP is implemented
        registry.register(mcp::ListMcpResourcesTool);
        registry.register(mcp::ReadMcpResourceTool);
        registry.register(mcp::McpTool);
        registry
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
