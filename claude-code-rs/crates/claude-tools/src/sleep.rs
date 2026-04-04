use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

/// Sleep for N milliseconds — useful for rate limiting or timed polling.
pub struct SleepTool;

#[async_trait]
impl Tool for SleepTool {
    fn name(&self) -> &str { "Sleep" }

    fn description(&self) -> &str {
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
        Ok(ToolResult::text(format!("Slept for {}ms", ms)))
    }
}
