use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

pub struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str { "AskUser" }
    fn description(&self) -> &str { "Ask the user a question and wait for a response." }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "question": { "type": "string" } },
            "required": ["question"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let question = input["question"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'question'"))?.to_string();
        println!("\n\x1b[33m? {}\x1b[0m", question);
        print!("> ");
        let response = tokio::task::spawn_blocking(move || {
            use std::io::Write;
            std::io::stdout().flush()?;
            let mut r = String::new();
            std::io::stdin().read_line(&mut r)?;
            Ok::<String, std::io::Error>(r)
        }).await??;
        Ok(ToolResult::text(response.trim().to_string()))
    }
}
