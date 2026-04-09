//! Model attribution and git diff utilities.
//!
//! These types were originally in the MCP client module but are not MCP-related.
//! They are used by git/commit tools for attribution tracking and diff summaries.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ── Model Attribution ────────────────────────────────────────────────────────

/// Attribution metadata for tracking which model produced content.
/// Used to generate `Co-Authored-By` lines in commits and PR descriptions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attribution {
    /// Model identifier (e.g., "claude-sonnet-4-6")
    pub model_id: String,
    /// Human-readable model name
    pub model_name: String,
    /// Optional session URL for traceability
    pub session_url: Option<String>,
}

impl Attribution {
    /// Create attribution from a model ID.
    #[must_use] 
    pub fn from_model(model_id: &str) -> Self {
        let model_name = claude_core::model::display_name_any(model_id);
        Self {
            model_id: model_id.to_string(),
            model_name,
            session_url: None,
        }
    }

    /// Generate a Co-Authored-By line for git commits.
    #[must_use] 
    pub fn co_authored_by(&self) -> String {
        format!(
            "Co-Authored-By: {} <noreply@anthropic.com>",
            self.model_name
        )
    }

    /// Generate an attribution block for PR descriptions.
    #[must_use] 
    pub fn pr_attribution_block(&self) -> String {
        let mut block = format!("---\n_Generated with {}._", self.model_name);
        if let Some(ref url) = self.session_url {
            block.push_str(&format!(" [Session]({url})"));
        }
        block
    }
}

// ── Structured GitDiffResult ─────────────────────────────────────────────────

/// Structured representation of a git diff output.
/// Replaces raw string passing with parsed, size-limited diff data.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitDiffResult {
    /// Total files changed
    pub files_changed: usize,
    /// Total insertions
    pub insertions: usize,
    /// Total deletions
    pub deletions: usize,
    /// Per-file statistics
    pub file_stats: Vec<GitFileStat>,
    /// Whether the diff was truncated
    pub truncated: bool,
    /// Truncation reason if applicable
    pub truncation_reason: Option<String>,
}

/// Per-file diff statistics from `git diff --numstat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitFileStat {
    pub file: String,
    pub insertions: usize,
    pub deletions: usize,
    /// Whether this file's diff was too large and was omitted
    pub content_omitted: bool,
}

impl GitDiffResult {
    /// Diff size limits aligned with TS `diff.ts`
    const MAX_FILES: usize = 50;
    const MAX_TOTAL_BYTES: usize = 1_000_000; // 1MB
    const MAX_LINES_PER_FILE: usize = 400;

    /// Parse output of `git diff --numstat` into structured stats.
    #[must_use] 
    pub fn parse_numstat(numstat_output: &str) -> Self {
        let mut result = Self::default();

        for line in numstat_output.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() != 3 {
                continue;
            }

            let ins = parts[0].parse::<usize>().unwrap_or(0);
            let del = parts[1].parse::<usize>().unwrap_or(0);
            let file = parts[2].to_string();

            result.insertions += ins;
            result.deletions += del;

            if result.file_stats.len() >= Self::MAX_FILES {
                result.truncated = true;
                result.truncation_reason =
                    Some(format!("Exceeded {} file limit", Self::MAX_FILES));
                break;
            }

            result.file_stats.push(GitFileStat {
                file,
                insertions: ins,
                deletions: del,
                content_omitted: ins + del > Self::MAX_LINES_PER_FILE,
            });
        }

        result.files_changed = result.file_stats.len();
        result
    }

    /// Run `git diff --numstat` in the given directory and parse results.
    pub fn from_git(cwd: &std::path::Path, args: &[&str]) -> Result<Self> {
        let mut cmd_args = vec!["diff", "--numstat"];
        cmd_args.extend_from_slice(args);

        let output = std::process::Command::new("git")
            .args(&cmd_args)
            .current_dir(cwd)
            .output()
            .context("Failed to run git diff --numstat")?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git diff --numstat failed: {}", err.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut result = Self::parse_numstat(&stdout);

        let total_lines: usize = result
            .file_stats
            .iter()
            .map(|f| f.insertions + f.deletions)
            .sum();
        if total_lines > Self::MAX_TOTAL_BYTES / 80 {
            result.truncated = true;
            result.truncation_reason = Some("Diff too large".to_string());
        }

        Ok(result)
    }

    /// Format as a compact summary string.
    #[must_use] 
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} file(s) changed, {} insertion(s)(+), {} deletion(s)(-)",
            self.files_changed, self.insertions, self.deletions
        );
        if self.truncated {
            if let Some(ref reason) = self.truncation_reason {
                out.push_str(&format!(" [truncated: {reason}]"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Attribution tests ────────────────────────────────────────────

    #[test]
    fn attribution_from_model() {
        let attr = Attribution::from_model("claude-sonnet-4-6");
        assert_eq!(attr.model_id, "claude-sonnet-4-6");
        assert!(!attr.model_name.is_empty());
        assert!(attr.session_url.is_none());
    }

    #[test]
    fn attribution_co_authored_by() {
        let attr = Attribution::from_model("claude-sonnet-4-6");
        let line = attr.co_authored_by();
        assert!(line.starts_with("Co-Authored-By:"));
        assert!(line.contains("noreply@anthropic.com"));
    }

    #[test]
    fn attribution_pr_block() {
        let mut attr = Attribution::from_model("claude-opus-4-6");
        attr.session_url = Some("https://example.com/session/123".into());
        let block = attr.pr_attribution_block();
        assert!(block.contains("Generated with"));
        assert!(block.contains("[Session]"));
    }

    #[test]
    fn attribution_pr_block_no_url() {
        let attr = Attribution::from_model("claude-haiku-4-5");
        let block = attr.pr_attribution_block();
        assert!(block.contains("Generated with"));
        assert!(!block.contains("[Session]"));
    }

    // ── GitDiffResult tests ──────────────────────────────────────────

    #[test]
    fn parse_numstat_basic() {
        let input = "10\t5\tsrc/main.rs\n3\t1\tREADME.md\n";
        let result = GitDiffResult::parse_numstat(input);
        assert_eq!(result.files_changed, 2);
        assert_eq!(result.insertions, 13);
        assert_eq!(result.deletions, 6);
        assert_eq!(result.file_stats.len(), 2);
        assert_eq!(result.file_stats[0].file, "src/main.rs");
        assert!(!result.truncated);
    }

    #[test]
    fn parse_numstat_empty() {
        let result = GitDiffResult::parse_numstat("");
        assert_eq!(result.files_changed, 0);
        assert_eq!(result.insertions, 0);
        assert_eq!(result.deletions, 0);
    }

    #[test]
    fn parse_numstat_large_file_omitted() {
        let input = "500\t10\tlarge_file.rs\n";
        let result = GitDiffResult::parse_numstat(input);
        assert!(result.file_stats[0].content_omitted);
    }

    #[test]
    fn parse_numstat_truncation_at_limit() {
        let mut input = String::new();
        for i in 0..55 {
            input.push_str(&format!("1\t0\tfile{i}.rs\n"));
        }
        let result = GitDiffResult::parse_numstat(&input);
        assert_eq!(result.file_stats.len(), 50);
        assert!(result.truncated);
        assert!(result.truncation_reason.unwrap().contains("50"));
    }

    #[test]
    fn diff_summary_format() {
        let result = GitDiffResult {
            files_changed: 3,
            insertions: 42,
            deletions: 10,
            file_stats: vec![],
            truncated: false,
            truncation_reason: None,
        };
        let summary = result.summary();
        assert!(summary.contains("3 file(s)"));
        assert!(summary.contains("42 insertion(s)"));
        assert!(summary.contains("10 deletion(s)"));
    }

    #[test]
    fn diff_summary_truncated() {
        let result = GitDiffResult {
            files_changed: 50,
            insertions: 1000,
            deletions: 500,
            file_stats: vec![],
            truncated: true,
            truncation_reason: Some("Exceeded 50 file limit".into()),
        };
        let summary = result.summary();
        assert!(summary.contains("[truncated:"));
    }
}
