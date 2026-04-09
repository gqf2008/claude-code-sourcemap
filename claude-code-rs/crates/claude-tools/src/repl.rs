use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// `REPLTool` — execute code in a Python, Node.js, or Bash interpreter.
///
/// Mirrors the TS `REPLTool`: runs code snippets in an ephemeral subprocess and
/// returns stdout/stderr.  Each call starts a fresh process (no persistent state
/// across calls) for simplicity.  A future enhancement could keep a long-running
/// REPL process alive.
pub struct ReplTool;

#[async_trait]
impl Tool for ReplTool {
    fn name(&self) -> &'static str { "REPL" }
    fn category(&self) -> ToolCategory { ToolCategory::Shell }

    fn description(&self) -> &'static str {
        "Execute code in a REPL (Python, Node.js, or Bash). \
         Each invocation runs the code in a fresh subprocess. \
         Use this for quick computations, data exploration, or testing snippets."
    }

    fn to_auto_classifier_input(&self, input: &Value) -> Value {
        // Pass language + code (needed for risk assessment), strip timeout
        let lang = input.get("language").cloned().unwrap_or(Value::Null);
        let code = input.get("code").cloned().unwrap_or(Value::Null);
        json!({"REPL": {"language": lang, "code": code}})
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "language": {
                    "type": "string",
                    "enum": ["python", "node", "bash"],
                    "description": "The language/runtime to use."
                },
                "code": {
                    "type": "string",
                    "description": "The code to execute."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "Max execution time in seconds (default: 30, max: 120)."
                }
            },
            "required": ["language", "code"]
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let language = input["language"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'language'"))?;

        let code = input["code"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'code'"))?;

        let timeout_secs = input["timeout_seconds"]
            .as_u64()
            .unwrap_or(30)
            .min(120);

        if context.abort_signal.is_aborted() {
            return Ok(ToolResult::error("Interrupted"));
        }

        let (cmd, args): (&str, Vec<&str>) = match language {
            "python" => {
                // Try python3 first, fall back to python
                if which_exists("python3").await {
                    ("python3", vec!["-c", code])
                } else {
                    ("python", vec!["-c", code])
                }
            }
            "node" => ("node", vec!["-e", code]),
            "bash" => {
                #[cfg(windows)]
                {
                    // On Windows, try bash (Git Bash / WSL), fall back to powershell
                    if which_exists("bash").await {
                        ("bash", vec!["-c", code])
                    } else {
                        ("powershell", vec!["-NoProfile", "-Command", code])
                    }
                }
                #[cfg(not(windows))]
                {
                    ("bash", vec!["-c", code])
                }
            }
            other => {
                return Ok(ToolResult::error(format!(
                    "Unsupported language: '{other}'. Use python, node, or bash."
                )));
            }
        };

        // For multiline code in python/node, use stdin piping
        let use_stdin = (language == "python" || language == "node") && code.contains('\n');

        let mut child = if use_stdin {
            let stdin_args: Vec<&str> = match language {
                "python" => vec!["-"],
                "node" => vec!["--input-type=module"],
                _ => unreachable!(),
            };
            tokio::process::Command::new(cmd)
                .args(&stdin_args)
                .current_dir(&context.cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?
        } else {
            tokio::process::Command::new(cmd)
                .args(&args)
                .current_dir(&context.cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?
        };

        // Write code to stdin if using piped mode
        if use_stdin {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(code.as_bytes()).await?;
                drop(stdin); // Close stdin to signal EOF
            }
        }

        // Get child PID for kill on timeout
        let child_pid = child.id();

        // Wait with timeout — use select! so we can kill on timeout
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            child.wait_with_output(),
        ).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);

                let mut text = String::new();
                if !stdout.is_empty() {
                    text.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !text.is_empty() { text.push('\n'); }
                    text.push_str("[stderr]\n");
                    text.push_str(&stderr);
                }
                if text.is_empty() {
                    text = format!("(process exited with code {exit_code})");
                }

                // Truncate very large outputs
                if text.len() > 50_000 {
                    text.truncate(50_000);
                    text.push_str("\n... [output truncated at 50KB]");
                }

                if output.status.success() {
                    Ok(ToolResult::text(text))
                } else {
                    Ok(ToolResult::error(format!("Exit code {exit_code}\n{text}")))
                }
            }
            Ok(Err(e)) => Ok(ToolResult::error(format!("Process error: {e}"))),
            Err(_) => {
                // Timeout — kill the child process
                if let Some(pid) = child_pid {
                    #[cfg(unix)]
                    {
                        use std::process::Command as StdCommand;
                        let _ = StdCommand::new("kill").arg("-9").arg(pid.to_string()).status();
                    }
                    #[cfg(windows)]
                    {
                        use std::process::Command as StdCommand;
                        let _ = StdCommand::new("taskkill").args(["/F", "/T", "/PID", &pid.to_string()]).status();
                    }
                }
                Ok(ToolResult::error(format!(
                    "Execution timed out after {timeout_secs}s (process killed)"
                )))
            }
        }
    }
}

/// Check if a command exists on PATH.
async fn which_exists(cmd: &str) -> bool {
    tokio::process::Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok()
}
