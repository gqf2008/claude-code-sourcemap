use async_trait::async_trait;
use clawed_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;

use crate::bash::truncate_output;

/// Default timeout for git commands (30 seconds).
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// `GitTool` — safe wrapper for common git operations.
///
/// Provides a structured interface for git commands that's safer than raw Bash.
/// Read-only commands (status, log, diff, branch) are always allowed; write
/// commands (add, commit, push, checkout, stash) need permission.
pub struct GitTool;

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &'static str { "Git" }
    fn category(&self) -> ToolCategory { ToolCategory::Git }

    fn description(&self) -> &'static str {
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
                "Subcommand '{subcommand}' not allowed. Use one of: {allowed:?}"
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

        let output = tokio::time::timeout(
            GIT_TIMEOUT,
            tokio::process::Command::new("git")
                .args(&cmd_args)
                .current_dir(&context.cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
        ).await;

        let output = match output {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Ok(ToolResult::error(format!("Git command failed to start: {e}"))),
            Err(_) => return Ok(ToolResult::error(format!(
                "Git command timed out after {}s. Consider breaking the operation into smaller steps.",
                GIT_TIMEOUT.as_secs()
            ))),
        };

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
            Ok(ToolResult::error(format!("git {subcommand} failed:\n{text}")))
        }
    }
}

/// `GitStatusTool` — quick read-only git status check.
///
/// This is concurrency-safe and read-only, optimized for frequent use
/// by the agent to check repository state before/after operations.
pub struct GitStatusTool;

#[async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &'static str { "GitStatus" }
    fn category(&self) -> ToolCategory { ToolCategory::Git }

    fn description(&self) -> &'static str {
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
        // Get branch name + status in one go (with timeout)
        let branch = tokio::time::timeout(
            GIT_TIMEOUT,
            tokio::process::Command::new("git")
                .args(["branch", "--show-current"])
                .current_dir(&context.cwd)
                .output()
        ).await;

        let status = tokio::time::timeout(
            GIT_TIMEOUT,
            tokio::process::Command::new("git")
                .args(["status", "--porcelain", "-b"])
                .current_dir(&context.cwd)
                .output()
        ).await;

        let mut text = String::new();

        if let Ok(Ok(b)) = branch {
            let name = String::from_utf8_lossy(&b.stdout).trim().to_string();
            if !name.is_empty() {
                text.push_str(&format!("Branch: {name}\n"));
            }
        }

        if let Ok(Ok(s)) = status {
            let lines = String::from_utf8_lossy(&s.stdout);
            let file_lines: Vec<&str> = lines.lines().skip(1).collect(); // skip ## branch line
            if file_lines.is_empty() {
                text.push_str("Working tree: clean\n");
            } else {
                let staged = file_lines.iter().filter(|l| {
                    l.len() >= 2 && !l.starts_with(' ') && !l.starts_with('?')
                }).count();
                let unstaged = file_lines.iter().filter(|l| {
                    l.len() >= 2 && l.chars().nth(1).is_some_and(|c| c != ' ') && !l.starts_with('?')
                }).count();
                let untracked = file_lines.iter().filter(|l| l.starts_with("??")).count();

                text.push_str(&format!(
                    "Changes: {staged} staged, {unstaged} unstaged, {untracked} untracked\n"
                ));
                for line in &file_lines {
                    text.push_str(&format!("  {line}\n"));
                }
            }
        }

        if text.is_empty() {
            text = "Not a git repository or git not available.".to_string();
        }

        Ok(ToolResult::text(text.trim().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawed_core::tool::Tool;
    use clawed_core::permissions::PermissionMode;
    use std::path::PathBuf;

    fn test_context() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("."),
            abort_signal: Default::default(),
            permission_mode: PermissionMode::Default,
            messages: vec![],
        }
    }

    fn get_text(result: &ToolResult) -> String {
        result.content.iter().filter_map(|c| {
            if let clawed_core::message::ToolResultContent::Text { text } = c { Some(text.clone()) } else { None }
        }).collect::<String>()
    }

    // ── Tool metadata ───────────────────────────────────────────────────────

    #[test]
    fn git_tool_name() {
        assert_eq!(GitTool.name(), "Git");
    }

    #[test]
    fn git_tool_category() {
        assert_eq!(GitTool.category(), ToolCategory::Git);
    }

    #[test]
    fn git_tool_not_read_only() {
        assert!(!GitTool.is_read_only());
    }

    #[test]
    fn git_status_tool_is_read_only() {
        assert!(GitStatusTool.is_read_only());
        assert!(GitStatusTool.is_concurrency_safe());
    }

    // ── Safety validation via call() ────────────────────────────────────────

    #[tokio::test]
    async fn git_rejects_unknown_subcommand() {
        let input = json!({"subcommand": "hack"});
        let result = GitTool.call(input, &test_context()).await.unwrap();
        assert!(result.is_error);
        assert!(get_text(&result).contains("not allowed"));
    }

    #[tokio::test]
    async fn git_rejects_no_verify() {
        let input = json!({"subcommand": "commit", "args": ["--no-verify", "-m", "test"]});
        let result = GitTool.call(input, &test_context()).await.unwrap();
        assert!(result.is_error);
        assert!(get_text(&result).contains("--no-verify"));
    }

    #[tokio::test]
    async fn git_allows_status() {
        let input = json!({"subcommand": "status"});
        let result = GitTool.call(input, &test_context()).await.unwrap();
        let txt = get_text(&result);
        assert!(!txt.contains("not allowed"));
    }

    #[tokio::test]
    async fn git_missing_subcommand() {
        let input = json!({});
        let result = GitTool.call(input, &test_context()).await;
        assert!(result.is_err());
    }

    #[test]
    fn git_tool_schema_has_subcommand() {
        let schema = GitTool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("subcommand")));
    }

    #[test]
    fn git_status_tool_name() {
        assert_eq!(GitStatusTool.name(), "GitStatus");
    }

    // ── git safety edge cases ───────────────────────────────────────────
    // NOTE: "push" and "reset" are NOT in the allowed subcommands list,
    // so they are blocked at the allowlist level before force/hard checks run.

    #[tokio::test]
    async fn git_push_not_in_allowlist() {
        let input = json!({"subcommand": "push", "args": ["--force", "origin", "main"]});
        let result = GitTool.call(input, &test_context()).await.unwrap();
        assert!(result.is_error);
        assert!(get_text(&result).contains("not allowed"), "push should be blocked by allowlist");
    }

    #[tokio::test]
    async fn git_push_short_force_also_blocked() {
        let input = json!({"subcommand": "push", "args": ["-f", "origin", "main"]});
        let result = GitTool.call(input, &test_context()).await.unwrap();
        assert!(result.is_error);
        assert!(get_text(&result).contains("not allowed"));
    }

    #[tokio::test]
    async fn git_reset_not_in_allowlist() {
        let input = json!({"subcommand": "reset", "args": ["--hard", "HEAD~3"]});
        let result = GitTool.call(input, &test_context()).await.unwrap();
        assert!(result.is_error);
        assert!(get_text(&result).contains("not allowed"));
    }

    #[tokio::test]
    async fn git_reset_soft_also_not_in_allowlist() {
        let input = json!({"subcommand": "reset", "args": ["--soft", "HEAD~1"]});
        let result = GitTool.call(input, &test_context()).await.unwrap();
        assert!(result.is_error);
        assert!(get_text(&result).contains("not allowed"));
    }

    #[tokio::test]
    async fn git_gc_not_in_allowlist() {
        let input = json!({"subcommand": "gc", "args": []});
        let result = GitTool.call(input, &test_context()).await.unwrap();
        assert!(result.is_error);
        assert!(get_text(&result).contains("not allowed"));
    }

    #[tokio::test]
    async fn git_allows_all_read_subcommands() {
        // Verify all these subcommands pass the allowlist check.
        // They may produce real git output (or errors like "not a git repo"), but
        // should NOT produce the specific "Subcommand 'X' not allowed" error.
        let allowed_subs = ["status", "log", "branch", "show", "blame", "rev-parse", "reflog", "diff"];
        for sub in &allowed_subs {
            let input = json!({"subcommand": sub});
            let result = GitTool.call(input, &test_context()).await.unwrap();
            let text = get_text(&result);
            let err_prefix = format!("Subcommand '{sub}' not allowed");
            assert!(
                !text.starts_with(&err_prefix),
                "'{}' should pass allowlist, but got: {}",
                sub, &text[..text.len().min(100)]
            );
        }
    }
}
