//! PowerShellTool — execute Windows PowerShell commands.
//!
//! Available only on Windows. On other platforms the tool is registered but
//! returns an informational error.

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::bash::{check_dangerous, truncate_output};

pub struct PowerShellTool;

#[async_trait]
impl Tool for PowerShellTool {
    fn name(&self) -> &str { "PowerShell" }

    fn description(&self) -> &str {
        "Execute a PowerShell command on Windows. Returns stdout, stderr, and exit code. \
         Use this for Windows-specific operations, file system tasks, or system administration."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "PowerShell command or script to execute"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (default: 30000, max: 120000)",
                    "minimum": 1000,
                    "maximum": 120000
                }
            },
            "required": ["command"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (input, context);
            return Ok(ToolResult::error(
                "PowerShellTool is only available on Windows. Use BashTool on Unix systems."
            ));
        }

        #[cfg(target_os = "windows")]
        {
            let command = input["command"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing 'command'"))?;
            let timeout_ms = input["timeout_ms"]
                .as_u64()
                .unwrap_or(30_000)
                .min(120_000);

            // Security: check for dangerous patterns
            if let Some(reason) = check_dangerous(command) {
                return Ok(ToolResult::error(format!("🚫 {}\nCommand: {}", reason, command)));
            }

            use std::process::Stdio;
            use tokio::process::Command;
            use tokio::time::{timeout, Duration};

            let child = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", command])
                .current_dir(&context.cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| anyhow::anyhow!("Failed to spawn powershell.exe: {}", e))?;

            let result = timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await;

            match result {
                Ok(Ok(output)) => {
                    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                    let exit_code = output.status.code().unwrap_or(-1);

                    let mut response = String::new();
                    if !stdout.is_empty() {
                        response.push_str(&stdout);
                    }
                    if !stderr.is_empty() {
                        if !response.is_empty() { response.push('\n'); }
                        response.push_str("STDERR:\n");
                        response.push_str(&stderr);
                    }
                    if exit_code != 0 {
                        if !response.is_empty() { response.push('\n'); }
                        response.push_str(&format!("Exit code: {}", exit_code));
                    }

                    if response.is_empty() {
                        response = "(no output)".to_string();
                    }

                    // Truncate large output
                    let response = truncate_output(response);

                    let is_error = exit_code != 0;
                    if is_error {
                        Ok(ToolResult::error(response))
                    } else {
                        Ok(ToolResult::text(response))
                    }
                }
                Ok(Err(e)) => Ok(ToolResult::error(format!("Process error: {}", e))),
                Err(_) => Ok(ToolResult::error(format!("Command timed out after {}ms", timeout_ms))),
            }
        }
    }
}
