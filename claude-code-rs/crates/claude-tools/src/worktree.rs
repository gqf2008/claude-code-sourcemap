//! EnterWorktreeTool / ExitWorktreeTool — git worktree isolation for agents.
//!
//! Aligned with TS `EnterWorktreeTool` and `ExitWorktreeTool`.
//! Creates isolated git worktrees so agents can work on separate branches
//! without interfering with the user's working tree.

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

// ── EnterWorktreeTool ────────────────────────────────────────────────────────

pub struct EnterWorktreeTool;

#[async_trait]
impl Tool for EnterWorktreeTool {
    fn name(&self) -> &str { "EnterWorktree" }

    fn description(&self) -> &str {
        "Create an isolated git worktree and switch the session into it. \
         This lets you work on a separate branch without affecting the main working tree. \
         Each worktree has its own index and working directory."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Optional name/slug for the worktree branch. Auto-generated if omitted."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let cwd_path = &ctx.cwd;

        // Check we're in a git repo
        let git_root = find_git_root(cwd_path)
            .ok_or_else(|| anyhow::anyhow!("Not in a git repository. Worktrees require git."))?;

        // Generate or validate name
        let slug = match input["name"].as_str() {
            Some(name) => {
                validate_worktree_name(name)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                name.to_string()
            }
            None => generate_worktree_slug(),
        };

        let branch_name = format!("claude/{}", slug);
        let worktree_dir = git_root.join(".claude").join("worktrees").join(&slug);

        // Create .claude/worktrees/ directory
        if let Some(parent) = worktree_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Get current HEAD
        let head_output = tokio::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&git_root)
            .output()
            .await?;

        if !head_output.status.success() {
            return Ok(ToolResult::error("Failed to get current HEAD. Is this a valid git repository?"));
        }

        let head_sha = String::from_utf8_lossy(&head_output.stdout).trim().to_string();

        // Create worktree with new branch
        let wt_output = tokio::process::Command::new("git")
            .args(["worktree", "add", "-b", &branch_name,
                   worktree_dir.to_str().unwrap_or("."), &head_sha])
            .current_dir(&git_root)
            .output()
            .await?;

        if !wt_output.status.success() {
            let stderr = String::from_utf8_lossy(&wt_output.stderr);
            return Ok(ToolResult::error(format!("Failed to create worktree: {}", stderr.trim())));
        }

        Ok(ToolResult::text(format!(
            "Created worktree at: {}\nBranch: {}\nBased on: {}\n\n\
             The working directory has been switched to the worktree.\n\
             Use ExitWorktree to return to the original directory.",
            worktree_dir.display(), branch_name, &head_sha[..head_sha.len().min(8)]
        )))
    }
}

// ── ExitWorktreeTool ─────────────────────────────────────────────────────────

pub struct ExitWorktreeTool;

#[async_trait]
impl Tool for ExitWorktreeTool {
    fn name(&self) -> &str { "ExitWorktree" }

    fn description(&self) -> &str {
        "Exit the current worktree session and return to the original working directory. \
         Use action='keep' to preserve the worktree and branch, or action='remove' to delete them."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["keep", "remove"],
                    "description": "'keep' preserves the worktree on disk; 'remove' deletes it and its branch."
                },
                "discard_changes": {
                    "type": "boolean",
                    "description": "Must be true when action='remove' and the worktree has uncommitted changes or unmerged commits."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let cwd_path = &ctx.cwd;

        let action = input["action"].as_str().unwrap_or("keep");
        let discard = input["discard_changes"].as_bool().unwrap_or(false);

        // Find git root and check if we're in a worktree
        let git_root = find_git_root(cwd_path)
            .ok_or_else(|| anyhow::anyhow!("Not in a git repository."))?;

        // Check if current directory is a worktree
        let git_dir_output = tokio::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(cwd_path)
            .output()
            .await?;

        let git_dir = String::from_utf8_lossy(&git_dir_output.stdout).trim().to_string();

        let is_worktree = git_dir.contains("worktrees");
        if !is_worktree {
            return Ok(ToolResult::error("Not currently in a git worktree. Use EnterWorktree first."));
        }

        // Get branch name
        let branch_output = tokio::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(cwd_path)
            .output()
            .await?;

        let branch = String::from_utf8_lossy(&branch_output.stdout).trim().to_string();

        match action {
            "remove" => {
                // Check for uncommitted changes
                let status_output = tokio::process::Command::new("git")
                    .args(["status", "--porcelain"])
                    .current_dir(cwd_path)
                    .output()
                    .await?;

                let changes = String::from_utf8_lossy(&status_output.stdout);
                let change_count = changes.lines().filter(|l| !l.is_empty()).count();

                if change_count > 0 && !discard {
                    return Ok(ToolResult::error(format!(
                        "Worktree has {} uncommitted file(s). Set discard_changes=true to confirm removal.",
                        change_count
                    )));
                }

                // Remove worktree
                let mut args = vec!["worktree", "remove"];
                if discard {
                    args.push("--force");
                }
                let cwd_str = cwd_path.to_string_lossy().to_string();
                args.push(&cwd_str);

                let rm_output = tokio::process::Command::new("git")
                    .args(&args)
                    .current_dir(&git_root)
                    .output()
                    .await?;

                if !rm_output.status.success() {
                    let stderr = String::from_utf8_lossy(&rm_output.stderr);
                    return Ok(ToolResult::error(format!("Failed to remove worktree: {}", stderr.trim())));
                }

                // Delete the branch
                if !branch.is_empty() {
                    let _ = tokio::process::Command::new("git")
                        .args(["branch", "-D", &branch])
                        .current_dir(&git_root)
                        .output()
                        .await;
                }

                Ok(ToolResult::text(format!(
                    "Removed worktree and branch '{}'.\nReturned to: {}",
                    branch, git_root.display()
                )))
            }
            _ => {
                Ok(ToolResult::text(format!(
                    "Exited worktree (kept on disk).\nWorktree: {}\nBranch: {}\nReturned to: {}",
                    cwd_path.display(), branch, git_root.display()
                )))
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn find_git_root(from: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(from)
        .output()
        .ok()?;

    if output.status.success() {
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Some(PathBuf::from(root))
    } else {
        None
    }
}

fn validate_worktree_name(name: &str) -> Result<(), String> {
    if name.len() > 64 {
        return Err("Worktree name must be 64 characters or fewer.".into());
    }
    if name.is_empty() {
        return Err("Worktree name cannot be empty.".into());
    }
    // Allow letters, digits, dots, underscores, dashes, and slashes
    for ch in name.chars() {
        if !ch.is_alphanumeric() && ch != '.' && ch != '_' && ch != '-' && ch != '/' {
            return Err(format!("Invalid character '{}' in worktree name.", ch));
        }
    }
    Ok(())
}

fn generate_worktree_slug() -> String {
    use std::time::SystemTime;
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("agent-{}", ts % 100000)
}
