use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio::process::Command;

/// Maximum output size in bytes before truncation.
const MAX_OUTPUT_BYTES: usize = 100_000;

/// Patterns that indicate dangerous/destructive commands.
/// Each tuple: (pattern, reason, exact_boundary) — when exact_boundary is true,
/// the pattern must match followed by whitespace/end-of-string (not a path continuation).
const DANGEROUS_PATTERNS: &[(&str, &str, bool)] = &[
    ("rm -rf /", "Refusing to delete root filesystem", true),
    ("rm -rf /*", "Refusing to delete root filesystem", false),
    ("rm -rf ~", "Refusing to delete home directory", true),
    ("mkfs.", "Refusing to format filesystem", false),
    (":(){:|:&};:", "Refusing to execute fork bomb", false),
    ("dd if=/dev/", "Refusing to run raw disk operations", false),
    ("chmod -R 777 /", "Refusing to change root permissions", true),
    ("chown -R", "Be cautious with recursive ownership changes", false),
];

/// Git operations that should be blocked unless explicitly requested.
const BLOCKED_GIT_PATTERNS: &[(&str, &str)] = &[
    ("git push --force", "Force push blocked — use --force-with-lease if needed"),
    ("git push -f ", "Force push blocked — use --force-with-lease if needed"),
    ("git reset --hard", "Hard reset blocked — could lose uncommitted changes"),
    ("git clean -f", "Clean forced blocked — could delete untracked files"),
    ("git checkout -- .", "Mass checkout blocked — could discard all changes"),
    ("--no-verify", "Skipping hooks is not allowed unless explicitly requested"),
    ("--no-gpg-sign", "Skipping GPG signing is not allowed unless explicitly requested"),
    ("git config ", "Modifying git config is not allowed unless explicitly requested"),
];

/// Check if a command matches any dangerous pattern.
pub fn check_dangerous(command: &str) -> Option<&'static str> {
    let lower = command.to_lowercase();
    for &(pattern, reason, exact_boundary) in DANGEROUS_PATTERNS {
        if exact_boundary {
            // Must match pattern followed by end-of-string or whitespace (not /path)
            if let Some(pos) = lower.find(pattern) {
                let after = pos + pattern.len();
                if after >= lower.len() || lower.as_bytes()[after] == b' ' {
                    return Some(reason);
                }
            }
        } else if lower.contains(pattern) {
            return Some(reason);
        }
    }
    for (pattern, reason) in BLOCKED_GIT_PATTERNS {
        if lower.contains(pattern) {
            return Some(reason);
        }
    }
    None
}

/// Truncate output to prevent context explosion.
pub fn truncate_output(output: String) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output;
    }
    // Keep first and last portions
    let keep = MAX_OUTPUT_BYTES / 2;
    let first = &output[..keep];
    let last = &output[output.len() - keep..];
    let skipped = output.len() - MAX_OUTPUT_BYTES;
    format!(
        "{}\n\n... [truncated {} bytes] ...\n\n{}",
        first, skipped, last
    )
}

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "Bash" }
    fn category(&self) -> ToolCategory { ToolCategory::Shell }

    fn description(&self) -> &str {
        "Execute a shell command in the working directory. Use for system commands, \
         git operations, build commands, and running programs. Do NOT use for file operations \
         when dedicated tools exist (Read, Edit, Write, Glob, Grep). \
         Git safety: NEVER update git config, NEVER run destructive git commands \
         (force push, reset --hard, clean -f) unless explicitly requested, NEVER skip hooks, \
         always create NEW commits (not amend) unless asked, prefer staging specific files \
         over 'git add -A'."
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

        // Security: check for dangerous patterns
        if let Some(reason) = check_dangerous(command) {
            return Ok(ToolResult::error(format!("🚫 {}\nCommand: {}", reason, command)));
        }

        let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("bash", "-c") };

        let child = Command::new(shell)
            .arg(flag)
            .arg(command)
            .current_dir(&context.cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to execute: {}", e))?;

        let child_id = child.id();

        match tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            child.wait_with_output(),
        ).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut result = stdout.to_string();
                if !stderr.is_empty() {
                    if !result.is_empty() { result.push('\n'); }
                    result.push_str("STDERR:\n");
                    result.push_str(&stderr);
                }

                // Truncate large output to prevent context overflow
                let result = truncate_output(result);

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
            Ok(Err(e)) => Err(anyhow::anyhow!("Process error: {}", e)),
            Err(_) => {
                if let Some(pid) = child_id {
                    #[cfg(unix)]
                    { let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status(); }
                    #[cfg(windows)]
                    { let _ = std::process::Command::new("taskkill").args(["/F", "/T", "/PID", &pid.to_string()]).status(); }
                }
                Ok(ToolResult::error(format!("Command timed out after {}ms", timeout_ms)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dangerous_patterns_blocked() {
        assert!(check_dangerous("rm -rf /").is_some());
        assert!(check_dangerous("sudo rm -rf /home").is_none()); // /home is ok
        assert!(check_dangerous("rm -rf ~").is_some());
        assert!(check_dangerous("git push --force origin main").is_some());
        assert!(check_dangerous("git push origin main").is_none());
        assert!(check_dangerous("git reset --hard HEAD~1").is_some());
        assert!(check_dangerous("git reset --soft HEAD~1").is_none());
        assert!(check_dangerous("git commit --no-verify").is_some());
        assert!(check_dangerous("git config user.email foo").is_some());
    }

    #[test]
    fn test_truncate_output() {
        let short = "hello world".to_string();
        assert_eq!(truncate_output(short.clone()), short);

        let long = "x".repeat(MAX_OUTPUT_BYTES + 1000);
        let truncated = truncate_output(long);
        assert!(truncated.len() < MAX_OUTPUT_BYTES + 100);
        assert!(truncated.contains("[truncated"));
    }
}
