//! ToolSearchTool — lets the agent search available tools by keyword.
//!
//! Aligned with TS `ToolSearchTool.ts`.  This is useful when the agent has
//! many tools available and needs to discover which one fits a particular need.
//! Returns matching tool names and descriptions.

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

pub struct ToolSearchTool;

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str { "ToolSearch" }
    fn category(&self) -> ToolCategory { ToolCategory::Code }

    fn description(&self) -> &str {
        "Search for available tools by keyword. Use this when you're unsure which \
         tool to use for a task. Searches tool names and descriptions."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keyword(s) to search for in tool names and descriptions"
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let query = input["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'query'"))?
            .to_lowercase();

        // Search across tools available in context.messages metadata
        // Since we don't have direct registry access here, we use a static list
        // In a real implementation this would query the ToolRegistry
        let all_tools = built_in_tool_catalog();

        let matches: Vec<(&str, &str)> = all_tools
            .iter()
            .filter(|(name, desc)| {
                name.to_lowercase().contains(&query)
                    || desc.to_lowercase().contains(&query)
            })
            .copied()
            .collect();

        if matches.is_empty() {
            return Ok(ToolResult::text(format!(
                "No tools found matching '{}'. Try a broader search term.",
                query
            )));
        }

        let mut out = format!("Found {} tool(s) matching '{}':\n\n", matches.len(), query);
        for (name, desc) in &matches {
            out.push_str(&format!("  \x1b[1m{}\x1b[0m\n    {}\n\n", name, desc));
        }

        Ok(ToolResult::text(out))
    }
}

/// Static catalog of built-in tools for search.
fn built_in_tool_catalog() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Read", "Read the contents of a file from disk"),
        ("Edit", "Make precise text replacements in a file"),
        ("Write", "Create a new file or overwrite an existing file"),
        ("MultiEdit", "Apply multiple edits to the same file atomically"),
        ("Glob", "Find files matching a glob pattern"),
        ("Grep", "Search file contents using regex patterns"),
        ("LS", "List directory contents"),
        ("Bash", "Execute shell commands in bash"),
        ("PowerShell", "Execute PowerShell commands"),
        ("WebFetch", "Fetch content from a URL"),
        ("WebSearch", "Search the web for current information"),
        ("AskUser", "Ask the user a question and wait for response"),
        ("dispatch_agent", "Launch a sub-agent for independent tasks"),
        ("task_create", "Create a new task for tracking progress"),
        ("task_update", "Update an existing task's status or details"),
        ("task_get", "Get details of a specific task"),
        ("task_list", "List all tasks with status summaries"),
        ("TodoWrite", "Write a todo item to the project todo file"),
        ("TodoRead", "Read project todo items"),
        ("Config", "Get or set configuration values"),
        ("Sleep", "Pause execution for a specified duration"),
        ("NotebookEdit", "Edit Jupyter notebook cells"),
        ("ToolSearch", "Search available tools by keyword"),
        ("EnterPlanMode", "Enter plan mode for structured task planning"),
        ("ExitPlanMode", "Exit plan mode and begin execution"),
    ]
}
