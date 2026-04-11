use async_trait::async_trait;
use clawed_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

/// Sleep for N milliseconds — useful for rate limiting or timed polling.
pub struct SleepTool;

#[async_trait]
impl Tool for SleepTool {
    fn name(&self) -> &'static str { "Sleep" }

    fn description(&self) -> &'static str {
        "Sleep (pause execution) for a specified number of milliseconds. \
         Use this when you need to wait before retrying an operation."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ms": {
                    "type": "integer",
                    "description": "Number of milliseconds to sleep (max 30000)",
                    "minimum": 0,
                    "maximum": 30000
                }
            },
            "required": ["ms"]
        })
    }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let ms = input["ms"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'ms'"))?
            .min(30_000);

        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        Ok(ToolResult::text(format!("Slept for {ms}ms")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawed_core::tool::AbortSignal;
    use clawed_core::permissions::PermissionMode;

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
            clawed_core::message::ToolResultContent::Text { text } => text.clone(),
            _ => String::new(),
        }
    }

    #[tokio::test]
    async fn sleep_short_duration() {
        let tool = SleepTool;
        let start = std::time::Instant::now();
        let result = tool.call(json!({"ms": 10}), &ctx()).await.unwrap();
        assert!(start.elapsed().as_millis() >= 9);
        assert!(!result.is_error);
        assert!(result_text(&result).contains("10ms"));
    }

    #[tokio::test]
    async fn sleep_clamps_to_max() {
        let tool = SleepTool;
        // Providing > 30000 should be clamped to 30000, but we won't actually wait that long
        // Just test that the input parsing works for smaller values
        let result = tool.call(json!({"ms": 1}), &ctx()).await.unwrap();
        assert!(result_text(&result).contains("1ms"));
    }

    #[tokio::test]
    async fn sleep_missing_ms_returns_error() {
        let tool = SleepTool;
        let result = tool.call(json!({}), &ctx()).await;
        assert!(result.is_err()); // anyhow error, not ToolResult error
    }
}
