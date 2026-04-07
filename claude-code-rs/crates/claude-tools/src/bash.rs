use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio::process::Command;
use std::collections::HashMap;

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
pub(crate) fn check_dangerous(command: &str) -> Option<&'static str> {
    let lower = command.to_lowercase();
    for &(pattern, reason, exact_boundary) in DANGEROUS_PATTERNS {
        if exact_boundary {
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
pub(crate) fn truncate_output(output: String) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output;
    }
    let keep = MAX_OUTPUT_BYTES / 2;
    // Find safe char boundaries for slicing
    let mut first_end = keep;
    while first_end > 0 && !output.is_char_boundary(first_end) {
        first_end -= 1;
    }
    let mut last_start = output.len() - keep;
    while last_start < output.len() && !output.is_char_boundary(last_start) {
        last_start += 1;
    }
    let skipped = output.len() - MAX_OUTPUT_BYTES;
    format!(
        "{}\n\n... [truncated {} bytes] ...\n\n{}",
        &output[..first_end], skipped, &output[last_start..]
    )
}

// ── Command Semantics ────────────────────────────────────────────────────────

/// Commands that exit non-zero for "no matches" — not a real error.
const SEARCH_COMMANDS: &[&str] = &["grep", "egrep", "fgrep", "rg", "ag", "ack", "git grep"];

/// Commands considered read-only (search or listing).
const READ_ONLY_COMMANDS: &[&str] = &[
    "cat", "head", "tail", "less", "more", "wc", "file", "stat", "du", "df",
    "ls", "tree", "find", "which", "type", "whereis", "locate",
    "grep", "egrep", "fgrep", "rg", "ag", "ack",
    "git log", "git show", "git diff", "git status", "git branch",
    "git stash list", "git remote", "git tag", "git rev-parse",
    "echo", "printf", "date", "whoami", "hostname", "uname", "pwd", "env", "printenv",
];

/// Commands that modify the filesystem or state.
const WRITE_COMMANDS: &[&str] = &[
    "rm", "mv", "cp", "mkdir", "touch", "chmod", "chown",
    "git add", "git commit", "git push", "git merge", "git rebase",
    "git checkout", "git switch", "git restore", "git reset",
    "npm install", "pip install", "cargo install", "apt install", "brew install",
    "make", "cmake", "cargo build", "npm run", "yarn",
];

/// Classify what kind of command this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandType {
    ReadOnly,
    Write,
    Search,
    Unknown,
}

/// Extract the base command from a potentially complex shell command.
fn extract_base_command(command: &str) -> &str {
    // Strip leading env vars, sudo, etc.
    let trimmed = command.trim();
    let without_env = trimmed.trim_start_matches(' ')
        .strip_prefix("sudo ")
        .unwrap_or(trimmed)
        .trim();

    // Take first command in a pipeline
    let first_cmd = without_env.split('|').next().unwrap_or(without_env).trim();

    // Remove env var assignments at the start
    let mut parts = first_cmd.split_whitespace();
    for part in &mut parts {
        if part.contains('=') && !part.starts_with('-') {
            continue;
        }
        return first_cmd[first_cmd.find(part).unwrap_or(0)..].trim();
    }
    first_cmd
}

/// Classify a command as read-only, write, or search.
pub(crate) fn classify_command(command: &str) -> CommandType {
    let base = extract_base_command(command).to_lowercase();

    for &s in SEARCH_COMMANDS {
        if base.starts_with(s) {
            return CommandType::Search;
        }
    }
    for &r in READ_ONLY_COMMANDS {
        if base.starts_with(r) {
            return CommandType::ReadOnly;
        }
    }
    for &w in WRITE_COMMANDS {
        if base.starts_with(w) {
            return CommandType::Write;
        }
    }
    CommandType::Unknown
}

/// Interpret exit code in context — e.g., grep returning 1 means "no matches", not error.
pub(crate) fn interpret_exit_code(command: &str, code: i32) -> (bool, Option<String>) {
    if code == 0 {
        return (true, None);
    }

    let cmd_type = classify_command(command);

    // Search commands: exit code 1 = no matches found (not an error)
    if cmd_type == CommandType::Search && code == 1 {
        return (true, Some("No matches found (exit code 1 is normal for search commands)".to_string()));
    }

    // diff: exit code 1 = differences found
    let base = extract_base_command(command).to_lowercase();
    if (base.starts_with("diff") || base.starts_with("git diff")) && code == 1 {
        return (true, Some("Differences found (exit code 1 is normal for diff)".to_string()));
    }

    // test/[: exit code 1 = condition false
    if (base.starts_with("test ") || base.starts_with("[ ")) && code == 1 {
        return (true, Some("Condition evaluated to false".to_string()));
    }

    (false, None)
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
                "timeout": { "type": "integer", "description": "Timeout in ms (default 120000)" },
                "working_directory": { "type": "string", "description": "Override working directory" },
                "environment": {
                    "type": "object",
                    "description": "Additional environment variables",
                    "additionalProperties": { "type": "string" }
                }
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

        // Resolve working directory (allow override)
        let cwd = match input["working_directory"].as_str() {
            Some(dir) => {
                let p = std::path::Path::new(dir);
                if p.is_absolute() && p.is_dir() { p.to_path_buf() }
                else { context.cwd.clone() }
            }
            None => context.cwd.clone(),
        };

        // Parse environment overrides
        let env_overrides: HashMap<String, String> = input["environment"]
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("bash", "-c") };

        let mut cmd = Command::new(shell);
        cmd.arg(flag)
            .arg(command)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        for (k, v) in &env_overrides {
            cmd.env(k, v);
        }

        let child = cmd.spawn()
            .map_err(|e| anyhow::anyhow!("Failed to execute: {}", e))?;

        let child_id = child.id();
        let abort = context.abort_signal.clone();

        // Race: child completion vs timeout vs abort signal
        let result = tokio::select! {
            r = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                child.wait_with_output(),
            ) => r,
            _ = async {
                loop {
                    if abort.is_aborted() { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            } => {
                // Abort signal fired — kill the child
                if let Some(pid) = child_id {
                    kill_process(pid);
                }
                return Ok(ToolResult::error("Interrupted by user (Ctrl+C)".to_string()));
            }
        };

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut result = stdout.to_string();
                if !stderr.is_empty() {
                    if !result.is_empty() { result.push('\n'); }
                    result.push_str("STDERR:\n");
                    result.push_str(&stderr);
                }

                let result = truncate_output(result);

                let exit_code = output.status.code().unwrap_or(-1);

                if output.status.success() {
                    Ok(ToolResult::text(if result.is_empty() { "(no output)".into() } else { result }))
                } else {
                    // Context-aware exit code interpretation
                    let (ok, note) = interpret_exit_code(command, exit_code);
                    if ok {
                        let text = match note {
                            Some(n) => {
                                if result.is_empty() { n }
                                else { format!("{}\n({})", result, n) }
                            }
                            None => result,
                        };
                        Ok(ToolResult::text(if text.is_empty() { "(no output)".into() } else { text }))
                    } else {
                        Ok(ToolResult::error(format!(
                            "Exit code {}\n{}",
                            exit_code, result
                        )))
                    }
                }
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("Process error: {}", e)),
            Err(_) => {
                if let Some(pid) = child_id {
                    kill_process(pid);
                }
                Ok(ToolResult::error(format!("Command timed out after {}ms", timeout_ms)))
            }
        }
    }
}

/// Kill a child process by PID (platform-specific).
/// Silently ignores failures (process may have already exited).
fn kill_process(pid: u32) {
    if pid == 0 {
        // pid 0 on Unix would kill the entire process group — never do that
        return;
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dangerous_patterns_blocked() {
        assert!(check_dangerous("rm -rf /").is_some());
        assert!(check_dangerous("sudo rm -rf /home").is_none());
        assert!(check_dangerous("rm -rf ~").is_some());
        assert!(check_dangerous("git push --force origin main").is_some());
        assert!(check_dangerous("git push origin main").is_none());
        assert!(check_dangerous("git reset --hard HEAD~1").is_some());
        assert!(check_dangerous("git reset --soft HEAD~1").is_none());
        assert!(check_dangerous("git commit --no-verify").is_some());
        assert!(check_dangerous("git config user.email foo").is_some());
    }

    #[test]
    fn test_kill_process_pid_zero_is_noop() {
        // pid 0 on Unix would kill entire process group — must be rejected
        kill_process(0); // should not panic or do anything harmful
    }

    #[test]
    fn test_kill_process_nonexistent_pid() {
        // Killing a non-existent PID should not panic
        kill_process(999_999_999);
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

    #[test]
    fn test_command_classification() {
        assert_eq!(classify_command("grep foo bar.txt"), CommandType::Search);
        assert_eq!(classify_command("rg pattern src/"), CommandType::Search);
        assert_eq!(classify_command("cat file.txt"), CommandType::ReadOnly);
        assert_eq!(classify_command("ls -la"), CommandType::ReadOnly);
        assert_eq!(classify_command("git log --oneline"), CommandType::ReadOnly);
        assert_eq!(classify_command("rm -rf dist/"), CommandType::Write);
        assert_eq!(classify_command("git commit -m 'msg'"), CommandType::Write);
        assert_eq!(classify_command("npm install"), CommandType::Write);
        assert_eq!(classify_command("some-custom-script"), CommandType::Unknown);
        // With sudo prefix
        assert_eq!(classify_command("sudo cat /etc/passwd"), CommandType::ReadOnly);
        // With env vars
        assert_eq!(classify_command("NODE_ENV=prod echo hello"), CommandType::ReadOnly);
    }

    #[test]
    fn test_exit_code_interpretation() {
        // grep returning 1 = no matches (not error)
        let (ok, note) = interpret_exit_code("grep foo bar.txt", 1);
        assert!(ok);
        assert!(note.unwrap().contains("No matches"));

        // grep returning 2 = actual error
        let (ok, _) = interpret_exit_code("grep foo bar.txt", 2);
        assert!(!ok);

        // diff returning 1 = differences found
        let (ok, note) = interpret_exit_code("diff a.txt b.txt", 1);
        assert!(ok);
        assert!(note.unwrap().contains("Differences"));

        // regular command returning 1 = error
        let (ok, _) = interpret_exit_code("npm run build", 1);
        assert!(!ok);
    }

    // ── extract_base_command ────────────────────────────────────────────

    #[test]
    fn test_extract_base_command_simple() {
        let base = extract_base_command("ls -la");
        assert!(base.starts_with("ls"), "expected 'ls', got '{}'", base);
    }

    #[test]
    fn test_extract_base_command_sudo() {
        let base = extract_base_command("sudo apt update");
        assert!(base.starts_with("apt"), "expected 'apt', got '{}'", base);
    }

    #[test]
    fn test_extract_base_command_pipeline() {
        let base = extract_base_command("cat file | grep foo");
        assert!(base.starts_with("cat"), "expected 'cat', got '{}'", base);
    }

    #[test]
    fn test_extract_base_command_env_vars() {
        let base = extract_base_command("FOO=bar BAZ=1 node script.js");
        assert!(base.starts_with("node"), "expected 'node', got '{}'", base);
    }

    #[test]
    fn test_extract_base_command_complex() {
        let base = extract_base_command("sudo ENV=1 git status");
        assert!(base.contains("git"), "expected 'git' in '{}'", base);
    }

    // ── classify_command (additional) ───────────────────────────────────

    #[test]
    fn test_classify_ls_readonly() {
        assert_eq!(classify_command("ls"), CommandType::ReadOnly);
    }

    #[test]
    fn test_classify_cat_readonly() {
        assert_eq!(classify_command("cat foo"), CommandType::ReadOnly);
    }

    #[test]
    fn test_classify_grep_search() {
        assert_eq!(classify_command("grep pattern file"), CommandType::Search);
    }

    #[test]
    fn test_classify_find_readonly() {
        // `find` is in READ_ONLY_COMMANDS, not SEARCH_COMMANDS
        assert_eq!(classify_command("find . -name '*.rs'"), CommandType::ReadOnly);
    }

    #[test]
    fn test_classify_git_commit_write() {
        assert_eq!(classify_command("git commit -m 'msg'"), CommandType::Write);
    }

    #[test]
    fn test_classify_rm_write() {
        assert_eq!(classify_command("rm -rf foo"), CommandType::Write);
    }

    #[test]
    fn test_classify_npm_install_write() {
        assert_eq!(classify_command("npm install"), CommandType::Write);
    }

    #[test]
    fn test_classify_unknown_command() {
        assert_eq!(classify_command("someunknowncommand"), CommandType::Unknown);
    }

    // ── interpret_exit_code (additional) ────────────────────────────────

    #[test]
    fn test_exit_code_success() {
        let (ok, note) = interpret_exit_code("ls", 0);
        assert!(ok);
        assert!(note.is_none());
    }

    #[test]
    fn test_exit_code_grep_no_matches() {
        let (ok, note) = interpret_exit_code("grep foo", 1);
        assert!(ok);
        assert!(note.is_some());
    }

    #[test]
    fn test_exit_code_ls_real_error() {
        let (ok, note) = interpret_exit_code("ls", 2);
        assert!(!ok);
        assert!(note.is_none());
    }

    #[test]
    fn test_exit_code_diff_differences() {
        let (ok, note) = interpret_exit_code("diff a b", 1);
        assert!(ok);
        assert!(note.unwrap().contains("Differences"));
    }

    #[test]
    fn test_exit_code_git_diff_differences() {
        let (ok, note) = interpret_exit_code("git diff", 1);
        assert!(ok);
        assert!(note.unwrap().contains("Differences"));
    }

    #[test]
    fn test_exit_code_rm_error() {
        let (ok, note) = interpret_exit_code("rm foo", 1);
        assert!(!ok);
        assert!(note.is_none());
    }

    // ── abort-triggered child kill ─────────────────────────────────────

    #[tokio::test]
    async fn test_bash_abort_interrupts_long_command() {
        use claude_core::tool::{AbortSignal, Tool, ToolContext};
        use claude_core::permissions::PermissionMode;

        let tool = BashTool;
        let abort = AbortSignal::new();

        // Set abort BEFORE calling the tool
        abort.abort();

        let ctx = ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: abort,
            permission_mode: PermissionMode::BypassAll,
            messages: Vec::new(),
        };

        // Use a long-running command; abort should stop it immediately
        let cmd = if cfg!(windows) { "ping 127.0.0.1 -n 60" } else { "sleep 60" };
        let result = tool.call(
            serde_json::json!({ "command": cmd }),
            &ctx,
        ).await.unwrap();

        // Should be interrupted, not timed out
        assert!(result.is_error, "Expected error result for interrupted command");
        let text = format!("{:?}", result.content);
        assert!(text.contains("Interrupted"), "Expected 'Interrupted', got: {}", text);
    }
}
