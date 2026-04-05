pub mod bash;
pub mod file_read;
pub mod file_edit;
pub mod file_write;
pub mod glob_tool;
pub mod grep;
pub mod web_fetch;
pub mod ask_user;
pub mod ls;
pub mod todo;
pub mod multi_edit;
pub mod diff_ui;
pub mod sleep;
pub mod config_tool;
pub mod powershell;

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
        registry
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
