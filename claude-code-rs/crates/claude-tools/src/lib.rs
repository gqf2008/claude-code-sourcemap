// ── File I/O tools (always included) ─────────────────────────────────────────
pub mod file_read;
pub mod file_edit;
pub mod file_write;
pub mod multi_edit;
pub mod glob_tool;
pub mod grep;
pub mod ls;

// ── Shell / execution tools ─────────────────────────────────────────────────
#[cfg(feature = "shell")]
pub mod bash;
#[cfg(feature = "shell")]
pub mod powershell;
#[cfg(feature = "shell")]
pub mod repl;

// ── Web tools ───────────────────────────────────────────────────────────────
#[cfg(feature = "web")]
pub mod web_fetch;
#[cfg(feature = "web")]
pub mod web_search;

// ── Code intelligence tools ─────────────────────────────────────────────────
#[cfg(feature = "code")]
pub mod lsp;
#[cfg(feature = "code")]
pub mod notebook;
pub mod diff_ui;

// ── Git tools ───────────────────────────────────────────────────────────────
#[cfg(feature = "git")]
pub mod git;
#[cfg(feature = "git")]
pub mod worktree;

// ── Interaction tools ───────────────────────────────────────────────────────
pub mod ask_user;
pub mod send_message;

// ── Agent / orchestration tools ─────────────────────────────────────────────
pub mod task;
pub mod skill_tool;
pub mod plan_mode;
pub mod agent_tool;

// ── Management tools ────────────────────────────────────────────────────────
pub mod todo;
pub mod config_tool;
pub mod context;
pub mod sleep;
pub mod tool_search;

// ── MCP (Model Context Protocol) ────────────────────────────────────────────
#[cfg(feature = "mcp")]
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
        "Read" | "FileRead" | "Edit" | "FileEdit" | "Write" | "FileWrite"
        | "MultiEdit" | "Glob" | "Grep" | "LS" | "ListDir" => ToolCategory::File,

        "Bash" | "PowerShell" | "REPL" => ToolCategory::Shell,

        "WebFetch" | "WebSearch" => ToolCategory::Web,

        "LSP" | "NotebookEdit" | "DiffUI" | "ToolSearch" => ToolCategory::Code,

        "Git" | "GitStatus" | "EnterWorktree" | "ExitWorktree" => ToolCategory::Git,

        "AskUser" | "SendUserMessage" => ToolCategory::Interaction,

        "TaskCreate" | "TaskUpdate" | "TaskGet" | "TaskList"
        | "TaskOutput" | "TaskStop" | "Skill" | "Agent"
        | "task_create" | "task_update" | "task_get" | "task_list"
        | "task_output" | "task_stop"
        | "EnterPlanMode" | "ExitPlanMode" => ToolCategory::Agent,

        "TodoWrite" | "TodoRead" | "Config" | "ContextInspect"
        | "Verify" | "Sleep" => ToolCategory::Management,

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

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Create a registry pre-loaded with all built-in tools
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();

        // File I/O (always included)
        registry.register(file_read::FileReadTool);
        registry.register(file_edit::FileEditTool);
        registry.register(file_write::FileWriteTool);
        registry.register(glob_tool::GlobTool);
        registry.register(grep::GrepTool);
        registry.register(ls::LsTool);
        registry.register(multi_edit::MultiEditTool);

        // Shell
        #[cfg(feature = "shell")]
        {
            registry.register(bash::BashTool);
            registry.register(powershell::PowerShellTool);
            registry.register(repl::ReplTool);
        }

        // Web
        #[cfg(feature = "web")]
        {
            registry.register(web_fetch::WebFetchTool);
            registry.register(web_search::WebSearchTool);
        }

        // Git
        #[cfg(feature = "git")]
        {
            registry.register(git::GitTool);
            registry.register(git::GitStatusTool);
            registry.register(worktree::EnterWorktreeTool);
            registry.register(worktree::ExitWorktreeTool);
        }

        // Code intelligence
        #[cfg(feature = "code")]
        {
            registry.register(lsp::LspTool);
            registry.register(notebook::NotebookEditTool);
        }

        // Interaction (always included)
        registry.register(ask_user::AskUserTool);
        registry.register(send_message::SendUserMessageTool);

        // Agent / orchestration (always included)
        registry.register(task::TaskCreateTool);
        registry.register(task::TaskUpdateTool);
        registry.register(task::TaskGetTool);
        registry.register(task::TaskListTool);
        registry.register(task::TaskOutputTool);
        registry.register(task::TaskStopTool);
        registry.register(plan_mode::EnterPlanModeTool);
        registry.register(plan_mode::ExitPlanModeTool);
        registry.register(skill_tool::SkillTool);
        // Note: AgentTool (DispatchAgentTool) is registered by the engine builder
        // in claude-agent, not here, because it requires ApiClient and coordinator state.

        // Management (always included)
        registry.register(todo::TodoWriteTool);
        registry.register(todo::TodoReadTool);
        registry.register(sleep::SleepTool);
        registry.register(config_tool::ConfigTool);
        registry.register(tool_search::ToolSearchTool);
        registry.register(context::ContextInspectTool);
        registry.register(context::VerifyTool);

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
    #[cfg(feature = "mcp")]
    pub fn register_mcp(&mut self, manager: std::sync::Arc<tokio::sync::RwLock<mcp::McpManager>>) {
        self.tools.remove("mcp_list_resources");
        self.tools.remove("mcp_read_resource");
        self.register(mcp::ListMcpResourcesTool { manager: manager.clone() });
        self.register(mcp::ReadMcpResourceTool { manager });
    }

    /// Register dynamically-discovered MCP tool proxies.
    #[cfg(feature = "mcp")]
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

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::tool::{Tool, ToolContext, ToolResult, ToolCategory as CoreCategory};
    use async_trait::async_trait;

    struct DummyTool { nm: &'static str }
    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str { self.nm }
        fn description(&self) -> &str { "dummy" }
        fn input_schema(&self) -> serde_json::Value { serde_json::json!({}) }
        async fn call(&self, _input: serde_json::Value, _ctx: &ToolContext) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::text("ok"))
        }
        fn category(&self) -> CoreCategory { CoreCategory::Session }
    }

    #[test]
    fn registry_new_is_empty() {
        let r = ToolRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn registry_register_and_get() {
        let mut r = ToolRegistry::new();
        r.register(DummyTool { nm: "Foo" });
        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());
        assert!(r.get("Foo").is_some());
        assert!(r.get("Bar").is_none());
    }

    #[test]
    fn registry_names() {
        let mut r = ToolRegistry::new();
        r.register(DummyTool { nm: "Alpha" });
        r.register(DummyTool { nm: "Beta" });
        let mut names = r.names();
        names.sort();
        assert_eq!(names, vec!["Alpha", "Beta"]);
    }

    #[test]
    fn registry_all() {
        let mut r = ToolRegistry::new();
        r.register(DummyTool { nm: "X" });
        r.register(DummyTool { nm: "Y" });
        assert_eq!(r.all().len(), 2);
    }

    #[test]
    fn registry_overwrite_same_name() {
        let mut r = ToolRegistry::new();
        r.register(DummyTool { nm: "Dup" });
        r.register(DummyTool { nm: "Dup" });
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn registry_with_defaults_has_tools() {
        let r = ToolRegistry::with_defaults();
        assert!(r.len() >= 20, "Expected 20+ default tools, got {}", r.len());
        // Verify some core tools (actual registered names)
        assert!(r.get("Read").is_some());
        assert!(r.get("Edit").is_some());
        assert!(r.get("Glob").is_some());
        assert!(r.get("Grep").is_some());
        assert!(r.get("AskUser").is_some());
        assert!(r.get("Sleep").is_some());
    }

    #[test]
    fn registry_default_trait() {
        let r = ToolRegistry::default();
        assert!(r.is_empty());
    }

    #[test]
    fn tool_category_label() {
        assert_eq!(ToolCategory::File.label(), "File I/O");
        assert_eq!(ToolCategory::Shell.label(), "Shell");
        assert_eq!(ToolCategory::Mcp.label(), "MCP");
    }

    #[test]
    fn tool_category_mapping() {
        // Actual registered names
        assert_eq!(tool_category("Read"), ToolCategory::File);
        assert_eq!(tool_category("Edit"), ToolCategory::File);
        assert_eq!(tool_category("Write"), ToolCategory::File);
        assert_eq!(tool_category("Glob"), ToolCategory::File);
        assert_eq!(tool_category("Grep"), ToolCategory::File);
        assert_eq!(tool_category("LS"), ToolCategory::File);
        // Legacy aliases
        assert_eq!(tool_category("FileRead"), ToolCategory::File);
        assert_eq!(tool_category("Bash"), ToolCategory::Shell);
        assert_eq!(tool_category("WebFetch"), ToolCategory::Web);
        assert_eq!(tool_category("LSP"), ToolCategory::Code);
        assert_eq!(tool_category("ToolSearch"), ToolCategory::Code);
        assert_eq!(tool_category("Git"), ToolCategory::Git);
        assert_eq!(tool_category("AskUser"), ToolCategory::Interaction);
        assert_eq!(tool_category("TaskCreate"), ToolCategory::Agent);
        assert_eq!(tool_category("task_create"), ToolCategory::Agent);
        assert_eq!(tool_category("Sleep"), ToolCategory::Management);
        assert_eq!(tool_category("unknown_mcp_thing"), ToolCategory::Mcp);
    }

    #[test]
    fn by_category_filters() {
        let r = ToolRegistry::with_defaults();
        // Shell tools: Bash, PowerShell, REPL (feature-gated but default-enabled)
        let shell_tools = r.by_category(ToolCategory::Shell);
        assert!(shell_tools.len() >= 2, "Expected 2+ shell tools, got {}", shell_tools.len());
        for (name, _) in &shell_tools {
            assert_eq!(tool_category(name), ToolCategory::Shell);
        }
    }

    #[test]
    fn category_summary_covers_all() {
        let r = ToolRegistry::with_defaults();
        let summary = r.category_summary();
        let total: usize = summary.iter().map(|(_, c)| c).sum();
        assert_eq!(total, r.len());
    }
}
