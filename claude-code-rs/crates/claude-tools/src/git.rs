use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::process::Stdio;

use crate::bash::truncate_output;

/// GitTool — safe wrapper for common git operations.
///
/// Provides a structured interface for git commands that's safer than raw Bash.
/// Read-only commands (status, log, diff, branch) are always allowed; write
/// commands (add, commit, push, checkout, stash) need permission.
pub struct GitTool;

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str { "Git" }
    fn category(&self) -> ToolCategory { ToolCategory::Git }

    fn description(&self) -> &str {
        "Run git commands. Supports common operations: status, diff, log, branch, \
         add, commit, checkout, stash, show, blame. Safer than running raw shell commands."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subcommand": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "branch", "show", "blame",
                             "add", "commit", "checkout", "stash", "tag", "remote",
                             "cherry-pick", "rebase", "merge", "fetch", "pull",
                             "rev-parse", "reflog"],
                    "description": "The git subcommand to run."
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Additional arguments for the git command."
                }
            },
            "required": ["subcommand"]
        })
    }

    fn is_read_only(&self) -> bool { false }

    fn is_concurrency_safe(&self) -> bool { false }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let subcommand = input["subcommand"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'subcommand'"))?;

        let args: Vec<String> = input["args"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        // Validate subcommand is allowed
        let allowed = [
            "status", "diff", "log", "branch", "show", "blame",
            "add", "commit", "checkout", "stash", "tag", "remote",
            "cherry-pick", "rebase", "merge", "fetch", "pull",
            "rev-parse", "reflog",
        ];
        if !allowed.contains(&subcommand) {
            return Ok(ToolResult::error(format!(
                "Subcommand '{}' not allowed. Use one of: {:?}", subcommand, allowed
            )));
        }

        // Safety: block dangerous patterns
        for arg in &args {
            if (arg.contains("--force") || arg == "-f")
                && subcommand == "push" {
                    return Ok(ToolResult::error(
                        "Force push is not allowed for safety. Use --force-with-lease if needed."
                    ));
                }
            if arg == "--hard" && subcommand == "reset" {
                return Ok(ToolResult::error(
                    "Hard reset blocked — could lose uncommitted changes."
                ));
            }
            if arg == "--no-verify" {
                return Ok(ToolResult::error(
                    "Skipping hooks (--no-verify) is not allowed unless explicitly requested."
                ));
            }
        }

        let mut cmd_args = vec!["--no-pager".to_string(), subcommand.to_string()];
        cmd_args.extend(args);

        let output = tokio::process::Command::new("git")
            .args(&cmd_args)
            .current_dir(&context.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut text = String::new();
        if !stdout.is_empty() {
            text.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !text.is_empty() { text.push('\n'); }
            text.push_str(&stderr);
        }
        if text.is_empty() {
            text = "(no output)".to_string();
        }

        // Truncate very large outputs
        let text = truncate_output(text);

        if output.status.success() {
            Ok(ToolResult::text(text))
        } else {
            Ok(ToolResult::error(format!("git {} failed:\n{}", subcommand, text)))
        }
    }
}

/// GitStatusTool — quick read-only git status check.
///
/// This is concurrency-safe and read-only, optimized for frequent use
/// by the agent to check repository state before/after operations.
pub struct GitStatusTool;

#[async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str { "GitStatus" }
    fn category(&self) -> ToolCategory { ToolCategory::Git }

    fn description(&self) -> &str {
        "Quick git status check: shows branch, staged/unstaged changes, and untracked files."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    fn is_read_only(&self) -> bool { true }
    fn is_concurrency_safe(&self) -> bool { true }

    async fn call(&self, _input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        // Get branch name + status in one go
        let branch = tokio::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&context.cwd)
            .output()
            .await;

        let status = tokio::process::Command::new("git")
            .args(["status", "--porcelain", "-b"])
            .current_dir(&context.cwd)
            .output()
            .await;

        let mut text = String::new();

        if let Ok(b) = branch {
            let name = String::from_utf8_lossy(&b.stdout).trim().to_string();
            if !name.is_empty() {
                text.push_str(&format!("Branch: {}\n", name));
            }
        }

        if let Ok(s) = status {
            let lines = String::from_utf8_lossy(&s.stdout);
            let file_lines: Vec<&str> = lines.lines().skip(1).collect(); // skip ## branch line
            if file_lines.is_empty() {
                text.push_str("Working tree: clean\n");
            } else {
                let staged = file_lines.iter().filter(|l| {
                    l.len() >= 2 && !l.starts_with(' ') && !l.starts_with('?')
                }).count();
                let unstaged = file_lines.iter().filter(|l| {
                    l.len() >= 2 && l.chars().nth(1).map(|c| c != ' ').unwrap_or(false) && !l.starts_with('?')
                }).count();
                let untracked = file_lines.iter().filter(|l| l.starts_with("??")).count();

                text.push_str(&format!(
                    "Changes: {} staged, {} unstaged, {} untracked\n",
                    staged, unstaged, untracked
                ));
                for line in &file_lines {
                    text.push_str(&format!("  {}\n", line));
                }
            }
        }

        if text.is_empty() {
            text = "Not a git repository or git not available.".to_string();
        }

        Ok(ToolResult::text(text.trim().to_string()))
    }
}
