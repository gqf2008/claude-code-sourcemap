//! Plan mode tools — `EnterPlanMode` / `ExitPlanMode`.
//!
//! Aligned with TS `EnterPlanModeTool.ts` and `ExitPlanModeV2Tool.ts`.
//! Plan mode restricts the agent to read-only operations for exploration
//! and planning before committing to changes.

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

// ── EnterPlanModeTool ────────────────────────────────────────────────────────

pub struct EnterPlanModeTool;

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
            "properties": {}
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, _input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        // In the TS implementation this sets the permission context to plan mode
        // and disables file writes. For our Rust port we return instructions
        // and the agent loop respects plan mode restrictions.
        Ok(ToolResult::text(
            "Plan mode activated. You are now in exploration/planning phase.\n\n\
             In plan mode:\n\
             - Only read-only tools are available (Read, Glob, Grep, LS, WebFetch, WebSearch)\n\
             - File writes (Edit, Write, Bash with side effects) are disabled\n\
             - Focus on understanding the codebase structure and designing your approach\n\n\
             When you have a clear plan, use ExitPlanMode to begin implementation."
        ))
    }
}

// ── ExitPlanModeTool ─────────────────────────────────────────────────────────

pub struct ExitPlanModeTool;

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &'static str { "ExitPlanMode" }

    fn description(&self) -> &'static str {
        "Exit plan mode and begin implementation. Call this after you have explored \
         the codebase and designed your approach. All tools will become available again."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "plan_summary": {
                    "type": "string",
                    "description": "Brief summary of the plan to execute"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let summary = input["plan_summary"]
            .as_str()
            .unwrap_or("(no summary provided)");

        Ok(ToolResult::text(format!(
            "Plan mode deactivated. All tools are now available.\n\n\
             Plan summary: {summary}\n\n\
             You may now proceed with implementation."
        )))
    }
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
    }

    #[tokio::test]
    async fn exit_plan_mode_with_summary() {
        let tool = ExitPlanModeTool;
        let result = tool.call(json!({"plan_summary": "Refactor auth module"}), &ctx()).await.unwrap();
        assert!(!result.is_error);
        let text = result_text(&result);
        assert!(text.contains("deactivated"));
        assert!(text.contains("Refactor auth module"));
    }

    #[tokio::test]
    async fn exit_plan_mode_no_summary() {
        let tool = ExitPlanModeTool;
        let result = tool.call(json!({}), &ctx()).await.unwrap();
        assert!(!result.is_error);
        assert!(result_text(&result).contains("no summary provided"));
    }

    #[test]
    fn tool_names() {
        assert_eq!(EnterPlanModeTool.name(), "EnterPlanMode");
        assert_eq!(ExitPlanModeTool.name(), "ExitPlanMode");
    }
}
