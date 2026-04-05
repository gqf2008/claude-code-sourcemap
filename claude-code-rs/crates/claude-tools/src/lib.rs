// ── File I/O tools ───────────────────────────────────────────────────────────
pub mod file_read;
pub mod file_edit;
pub mod file_write;
pub mod multi_edit;
pub mod glob_tool;
pub mod grep;
pub mod ls;

// ── Shell / execution tools ─────────────────────────────────────────────────
pub mod bash;
pub mod powershell;
pub mod repl;

// ── Web tools ───────────────────────────────────────────────────────────────
pub mod web_fetch;
pub mod web_search;

// ── Code intelligence tools ─────────────────────────────────────────────────
pub mod lsp;
pub mod notebook;
pub mod diff_ui;

// ── Git tools ───────────────────────────────────────────────────────────────
pub mod git;
pub mod worktree;

// ── Interaction tools ───────────────────────────────────────────────────────
pub mod ask_user;
pub mod send_message;

// ── Agent / orchestration tools ─────────────────────────────────────────────
pub mod task;
pub mod skill_tool;
pub mod plan_mode;

// ── Management tools ────────────────────────────────────────────────────────
pub mod todo;
pub mod config_tool;
pub mod context;
pub mod sleep;
pub mod tool_search;

// ── MCP (Model Context Protocol) ────────────────────────────────────────────
pub mod mcp;

// ── Internal utilities (not tools) ──────────────────────────────────────────
pub mod path_util;

use std::collections::HashMap;
use std::sync::Arc;
use claude_core::tool::{DynTool, Tool};

/// Tool category for grouping and filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    File,
    Shell,
    Web,
    Code,
    Git,
    Interaction,
    Agent,
    Management,
    Mcp,
}

impl ToolCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::File => "File I/O",
            Self::Shell => "Shell",
            Self::Web => "Web",
            Self::Code => "Code Intelligence",
            Self::Git => "Git",
            Self::Interaction => "Interaction",
            Self::Agent => "Agent",
            Self::Management => "Management",
            Self::Mcp => "MCP",
        }
    }
}

/// Map a tool name to its category.
pub fn tool_category(name: &str) -> ToolCategory {
    match name {
        "FileRead" | "FileEdit" | "FileWrite" | "MultiEdit"
        | "Glob" | "Grep" | "ListDir" => ToolCategory::File,

        "Bash" | "PowerShell" | "REPL" => ToolCategory::Shell,

        "WebFetch" | "WebSearch" => ToolCategory::Web,

        "LSP" | "NotebookEdit" | "DiffUI" => ToolCategory::Code,

        "Git" | "GitStatus" | "EnterWorktree" | "ExitWorktree" => ToolCategory::Git,

        "AskUser" | "SendUserMessage" => ToolCategory::Interaction,

        "TaskCreate" | "TaskUpdate" | "TaskGet" | "TaskList"
        | "TaskOutput" | "TaskStop" | "Skill"
        | "EnterPlanMode" | "ExitPlanMode" => ToolCategory::Agent,

        "TodoWrite" | "TodoRead" | "Config" | "ContextInspect"
        | "Verify" | "Sleep" | "ToolSearch" => ToolCategory::Management,

        _ => ToolCategory::Mcp, // MCP proxy tools and unknown
    }
}

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
        // MCP resource tools require a manager — use register_mcp() to add them
        registry
    }

    /// Return tools filtered by category.
    pub fn by_category(&self, category: ToolCategory) -> Vec<(&str, &DynTool)> {
        self.tools
            .iter()
            .filter(|(name, _)| tool_category(name) == category)
            .map(|(name, tool)| (name.as_str(), tool))
            .collect()
    }

    /// Return a summary of tool counts by category.
    pub fn category_summary(&self) -> Vec<(ToolCategory, usize)> {
        let mut counts: HashMap<ToolCategory, usize> = HashMap::new();
        for name in self.tools.keys() {
            *counts.entry(tool_category(name)).or_insert(0) += 1;
        }
        let mut result: Vec<_> = counts.into_iter().collect();
        result.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        result
    }

    /// Register MCP tools with a shared manager.
    /// Call this after connecting to MCP servers.
    pub fn register_mcp(&mut self, manager: std::sync::Arc<tokio::sync::RwLock<mcp::McpManager>>) {
        self.tools.remove("mcp_list_resources");
        self.tools.remove("mcp_read_resource");
        self.register(mcp::ListMcpResourcesTool { manager: manager.clone() });
        self.register(mcp::ReadMcpResourceTool { manager });
    }

    /// Register dynamically-discovered MCP tool proxies.
    pub fn register_mcp_proxies(&mut self, proxies: Vec<mcp::McpToolProxy>) {
        for proxy in proxies {
            let name = proxy.qualified_name.clone();
            self.tools.insert(name, std::sync::Arc::new(proxy));
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
