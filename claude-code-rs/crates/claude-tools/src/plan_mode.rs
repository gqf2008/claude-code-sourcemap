//! Plan mode tools — `EnterPlanMode` / `ExitPlanMode`.
//!
//! Aligned with TS `EnterPlanModeTool.ts` and `ExitPlanModeV2Tool.ts`.
//! Plan mode restricts the agent to read-only operations for exploration
//! and planning before committing to changes.

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

/// Tools that are available during plan mode (read-only operations).
pub const PLAN_MODE_ALLOWED_TOOLS: &[&str] = &[
    "FileReadTool",
    "GrepTool",
    "GlobTool",
    "LSTool",
    "WebFetchTool",
    "WebSearchTool",
    "TodoWriteTool",
    "AskUserQuestionTool",
    "EnterPlanMode",
    "ExitPlanMode",
];

/// Check if a tool is allowed in plan mode.
pub fn is_plan_mode_tool(name: &str) -> bool {
    PLAN_MODE_ALLOWED_TOOLS.contains(&name)
}

// ── EnterPlanModeTool ────────────────────────────────────────────────────────

pub struct EnterPlanModeTool;

/// System prompt describing plan mode workflow (aligned with TS prompt.ts).
const PLAN_MODE_PROMPT: &str = "\
Plan mode activated. You are now in the exploration and planning phase.

## What Happens in Plan Mode

In plan mode, only read-only tools are available:
- **FileReadTool** — Read file contents
- **GrepTool** — Search file contents with regex
- **GlobTool** — Find files by name pattern
- **LSTool** — List directory contents
- **WebFetchTool** — Fetch web pages
- **WebSearchTool** — Search the web
- **TodoWriteTool** — Track tasks

File modification tools (FileEditTool, FileWriteTool, BashTool, etc.) are disabled.

## Your Workflow

1. **Explore** the codebase using read-only tools to understand structure and patterns
2. **Analyze** the problem space and identify relevant files and dependencies
3. **Design** your approach — consider edge cases, test strategies, and potential issues
4. **Present** your plan to the user for review and approval
5. **Exit** plan mode with `ExitPlanMode` when the plan is approved

## Plan File

Your plan will be saved to a markdown file. Update it as you explore and refine \
your approach. The user can review and edit the plan file directly.

Focus on understanding before acting. A well-researched plan leads to better implementation.";

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &'static str { "EnterPlanMode" }

    fn description(&self) -> &'static str {
        "Enter plan mode for complex tasks requiring exploration and design. \
         In plan mode, only read-only tools are available. Use this when you need \
         to understand the codebase before making changes."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Optional description of what you plan to explore or accomplish"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let description = input["description"].as_str().unwrap_or("");
        let mut text = PLAN_MODE_PROMPT.to_string();
        if !description.is_empty() {
            text.push_str(&format!("\n\n## Goal\n\n{description}"));
        }
        // The actual permission mode transition is handled by the executor/query loop
        // after seeing a successful EnterPlanMode tool result.
        Ok(ToolResult::text(text))
    }
}

// ── ExitPlanModeTool ─────────────────────────────────────────────────────────

pub struct ExitPlanModeTool;

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &'static str { "ExitPlanMode" }

    fn description(&self) -> &'static str {
        "Exit plan mode and begin implementation. Call this after you have explored \
         the codebase and designed your approach. Provide the plan summary for the \
         user to review. All tools will become available again."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "plan": {
                    "type": "string",
                    "description": "The implementation plan in markdown format. This will be saved to the plan file."
                }
            },
            "required": ["plan"]
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let plan = input["plan"]
            .as_str()
            .unwrap_or("(no plan provided)");

        // Save plan to disk if we have a plan directory
        let saved_path = save_plan_content(plan);

        let mut text = String::from(
            "Plan mode deactivated. All tools are now available for implementation.\n\n"
        );
        text.push_str("## Plan\n\n");
        text.push_str(plan);
        if let Some(path) = saved_path {
            text.push_str(&format!("\n\n_Plan saved to: {}_", path));
        }
        text.push_str("\n\nYou may now proceed with implementation following the plan above.");

        // The actual permission mode restoration is handled by the executor/query loop
        // after seeing a successful ExitPlanMode tool result.
        Ok(ToolResult::text(text))
    }
}

/// Save plan content to the plans directory.
/// Returns the file path if saved successfully, None otherwise.
fn save_plan_content(plan: &str) -> Option<String> {
    let base = claude_core::config::Settings::claude_dir()?;
    let plans_dir = base.join("plans");
    std::fs::create_dir_all(&plans_dir).ok()?;

    // Use a timestamp-based filename as a simple approach
    let now = chrono::Local::now();
    let filename = format!("plan-{}.md", now.format("%Y%m%d-%H%M%S"));
    let path = plans_dir.join(&filename);
    std::fs::write(&path, format!("# Plan\n\n{plan}\n")).ok()?;
    Some(path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::tool::AbortSignal;
    use claude_core::permissions::PermissionMode;

    fn ctx() -> ToolContext {
        ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: AbortSignal::new(),
            permission_mode: PermissionMode::Default,
            messages: vec![],
        }
    }

    fn result_text(r: &ToolResult) -> String {
        match &r.content[0] {
            claude_core::message::ToolResultContent::Text { text } => text.clone(),
            _ => String::new(),
        }
    }

    #[tokio::test]
    async fn enter_plan_mode() {
        let tool = EnterPlanModeTool;
        let result = tool.call(json!({}), &ctx()).await.unwrap();
        assert!(!result.is_error);
        let text = result_text(&result);
        assert!(text.contains("Plan mode activated"));
        assert!(text.contains("read-only"));
        assert!(text.contains("ExitPlanMode"));
    }

    #[tokio::test]
    async fn enter_plan_mode_with_description() {
        let tool = EnterPlanModeTool;
        let result = tool.call(json!({"description": "Refactor auth module"}), &ctx()).await.unwrap();
        let text = result_text(&result);
        assert!(text.contains("Refactor auth module"));
        assert!(text.contains("## Goal"));
    }

    #[tokio::test]
    async fn exit_plan_mode_with_plan() {
        let tool = ExitPlanModeTool;
        let result = tool.call(json!({"plan": "1. Add tests\n2. Refactor"}), &ctx()).await.unwrap();
        assert!(!result.is_error);
        let text = result_text(&result);
        assert!(text.contains("deactivated"));
        assert!(text.contains("Add tests"));
        assert!(text.contains("proceed with implementation"));
    }

    #[tokio::test]
    async fn exit_plan_mode_no_plan() {
        let tool = ExitPlanModeTool;
        let result = tool.call(json!({}), &ctx()).await.unwrap();
        assert!(!result.is_error);
        assert!(result_text(&result).contains("no plan provided"));
    }

    #[test]
    fn tool_names() {
        assert_eq!(EnterPlanModeTool.name(), "EnterPlanMode");
        assert_eq!(ExitPlanModeTool.name(), "ExitPlanMode");
    }

    #[test]
    fn plan_mode_allowed_tools_includes_expected() {
        assert!(is_plan_mode_tool("FileReadTool"));
        assert!(is_plan_mode_tool("GrepTool"));
        assert!(is_plan_mode_tool("GlobTool"));
        assert!(is_plan_mode_tool("LSTool"));
        assert!(is_plan_mode_tool("ExitPlanMode"));
        assert!(!is_plan_mode_tool("FileEditTool"));
        assert!(!is_plan_mode_tool("BashTool"));
        assert!(!is_plan_mode_tool("FileWriteTool"));
    }
}
