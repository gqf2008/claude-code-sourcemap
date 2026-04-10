use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

/// Timeout for user input (5 minutes).
const USER_INPUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

pub struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &'static str { "AskUser" }
    fn description(&self) -> &'static str { "Ask the user a question and wait for a response." }

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
        println!("\n\x1b[33m? {question}\x1b[0m");
        print!("> ");
        let read_result = tokio::time::timeout(
            USER_INPUT_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                use std::io::Write;
                std::io::stdout().flush()?;
                let mut r = String::new();
                std::io::stdin().read_line(&mut r)?;
                Ok::<String, std::io::Error>(r)
            }),
        ).await;
        match read_result {
            Ok(Ok(Ok(response))) => Ok(ToolResult::text(response.trim().to_string())),
            Ok(Ok(Err(io_err))) => Err(io_err.into()),
            Ok(Err(join_err)) => Err(join_err.into()),
            Err(_elapsed) => Ok(ToolResult::error("User input timed out (5 minutes). Continuing without response.")),
        }
    }
}
