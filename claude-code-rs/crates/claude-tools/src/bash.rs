use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio::process::Command;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "Bash" }

    fn description(&self) -> &str {
        "Execute a shell command in the working directory."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to execute" },
                "timeout": { "type": "integer", "description": "Timeout in ms (default 120000)" }
            },
            "required": ["command"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command'"))?;
        let timeout_ms = input["timeout"].as_u64().unwrap_or(120_000);

        let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("bash", "-c") };

        let output = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            Command::new(shell)
                .arg(flag)
                .arg(command)
                .current_dir(&context.cwd)
                .output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Command timed out after {}ms", timeout_ms))?
        .map_err(|e| anyhow::anyhow!("Failed to execute: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut result = stdout.to_string();
        if !stderr.is_empty() {
            if !result.is_empty() { result.push('\n'); }
            result.push_str("STDERR:\n");
            result.push_str(&stderr);
        }

        if output.status.success() {
            Ok(ToolResult::text(if result.is_empty() { "(no output)".into() } else { result }))
        } else {
            Ok(ToolResult::error(format!(
                "Exit code {}\n{}",
                output.status.code().unwrap_or(-1),
                result
            )))
        }
    }
}
