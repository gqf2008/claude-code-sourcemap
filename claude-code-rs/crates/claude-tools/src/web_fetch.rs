use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str { "WebFetch" }
    fn category(&self) -> ToolCategory { ToolCategory::Web }
    fn description(&self) -> &str { "Fetch a URL and return its text content." }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string" },
                "max_length": { "type": "integer", "description": "Max chars (default 5000)" }
            },
            "required": ["url"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let url = input["url"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'url'"))?;
        let max_len = input["max_length"].as_u64().unwrap_or(5000) as usize;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let resp = client.get(url).send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        let truncated = if body.chars().count() > max_len {
            let s: String = body.chars().take(max_len).collect();
            format!("{}...\n[Truncated {}/{} chars]", s, max_len, body.chars().count())
        } else { body };
        if status.is_success() { Ok(ToolResult::text(truncated)) }
        else { Ok(ToolResult::error(format!("HTTP {}: {}", status, truncated))) }
    }
}
