//! Plan mode tools — EnterPlanMode / ExitPlanMode.
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
    fn name(&self) -> &str { "EnterPlanMode" }

    fn description(&self) -> &str {
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
    fn name(&self) -> &str { "ExitPlanMode" }

    fn description(&self) -> &str {
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
             Plan summary: {}\n\n\
             You may now proceed with implementation.",
            summary
        )))
    }
}
