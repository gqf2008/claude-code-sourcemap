//! `/pr-comments <PR#>` — fetch and display PR review comments.
//!
//! Uses `gh api` to retrieve review comments for a pull request,
//! parses them into a structured format, and displays them grouped
//! by file. Optionally sends them to the AI engine for analysis.

use claude_agent::engine::QueryEngine;
use std::path::Path;

/// A single review comment from a PR.
#[derive(Debug, Clone)]
pub struct PrComment {
    pub id: u64,
    pub file: String,
    pub line: Option<u64>,
    pub diff_hunk: String,
    pub body: String,
    pub author: String,
    pub in_reply_to_id: Option<u64>,
    #[allow(dead_code)]
    pub created_at: String,
}

/// Parsed PR comment thread (grouped by file + line).
#[derive(Debug, Clone)]
pub struct CommentThread {
    pub file: String,
    pub line: Option<u64>,
    pub diff_hunk: String,
    pub comments: Vec<PrComment>,
}

/// Fetch PR review comments via `gh api`.
fn fetch_pr_comments(pr_number: u64, cwd: &Path) -> Result<Vec<PrComment>, String> {
    // Get owner/repo from git remote
    let remote = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("Failed to get git remote: {}", e))?;

    let url = String::from_utf8_lossy(&remote.stdout).trim().to_string();
    let (owner, repo) = parse_github_remote(&url)
        .ok_or_else(|| format!("Cannot parse GitHub owner/repo from: {}", url))?;

    let api_path = format!("/repos/{}/{}/pulls/{}/comments", owner, repo, pr_number);

    let output = std::process::Command::new("gh")
        .args(["api", &api_path, "--paginate"])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("Failed to run `gh api`: {}", e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh api failed: {}", err.trim()));
    }

    let body = String::from_utf8_lossy(&output.stdout);
    parse_comments_json(&body)
}

/// Parse GitHub remote URL into (owner, repo).
fn parse_github_remote(url: &str) -> Option<(String, String)> {
    // SSH: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let rest = rest.trim_end_matches(".git").trim();
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }
    // HTTPS: https://github.com/owner/repo.git
    if url.contains("github.com") {
        let path = url.split("github.com").nth(1)?;
        let path = path.trim_start_matches('/').trim_start_matches(':');
        let path = path.trim_end_matches(".git").trim();
        let parts: Vec<&str> = path.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }
    None
}

/// Parse JSON response from GitHub API into PrComment list.
fn parse_comments_json(json: &str) -> Result<Vec<PrComment>, String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("Failed to parse JSON: {}", e))?;

    let arr = value.as_array()
        .ok_or("Expected JSON array")?;

    let mut comments = Vec::new();
    for item in arr {
        let comment = PrComment {
            id: item["id"].as_u64().unwrap_or(0),
            file: item["path"].as_str().unwrap_or("unknown").to_string(),
            line: item["line"].as_u64().or_else(|| item["original_line"].as_u64()),
            diff_hunk: item["diff_hunk"].as_str().unwrap_or("").to_string(),
            body: item["body"].as_str().unwrap_or("").to_string(),
            author: item["user"]["login"].as_str().unwrap_or("unknown").to_string(),
            in_reply_to_id: item["in_reply_to_id"].as_u64(),
            created_at: item["created_at"].as_str().unwrap_or("").to_string(),
        };
        comments.push(comment);
    }

    Ok(comments)
}

/// Group comments into threads by file and in_reply_to chains.
fn group_into_threads(comments: Vec<PrComment>) -> Vec<CommentThread> {
    use std::collections::BTreeMap;

    // Group by (file, line_or_reply_root)
    let mut file_groups: BTreeMap<String, Vec<PrComment>> = BTreeMap::new();
    for c in &comments {
        file_groups.entry(c.file.clone()).or_default().push(c.clone());
    }

    let mut threads = Vec::new();
    for (file, file_comments) in file_groups {
        // Sub-group by thread (in_reply_to_id chains)
        let mut roots: Vec<PrComment> = Vec::new();
        let mut replies: Vec<PrComment> = Vec::new();

        for c in file_comments {
            if c.in_reply_to_id.is_some() {
                replies.push(c);
            } else {
                roots.push(c);
            }
        }

        for root in roots {
            let root_id = root.id;
            let line = root.line;
            let diff_hunk = root.diff_hunk.clone();
            let mut thread_comments = vec![root];

            // Find replies to this root
            thread_comments.extend(
                replies.iter()
                    .filter(|r| r.in_reply_to_id == Some(root_id))
                    .cloned()
            );

            threads.push(CommentThread {
                file: file.clone(),
                line,
                diff_hunk,
                comments: thread_comments,
            });
        }
    }

    threads
}

/// Format threads for terminal display.
fn format_threads_display(threads: &[CommentThread]) -> String {
    let mut out = String::new();
    let total_comments: usize = threads.iter().map(|t| t.comments.len()).sum();
    out.push_str(&format!(
        "\x1b[1mPR Review Comments\x1b[0m — {} thread(s), {} comment(s)\n\n",
        threads.len(), total_comments
    ));

    for (i, thread) in threads.iter().enumerate() {
        let line_info = thread.line
            .map(|l| format!(":{}", l))
            .unwrap_or_default();

        out.push_str(&format!(
            "\x1b[1m[{}] {}{}\x1b[0m\n",
            i + 1, thread.file, line_info
        ));

        // Show truncated diff hunk
        if !thread.diff_hunk.is_empty() {
            let hunk_lines: Vec<&str> = thread.diff_hunk.lines().collect();
            let show = if hunk_lines.len() > 5 { &hunk_lines[hunk_lines.len()-5..] } else { &hunk_lines };
            for line in show {
                let color = if line.starts_with('+') {
                    "\x1b[32m"
                } else if line.starts_with('-') {
                    "\x1b[31m"
                } else {
                    "\x1b[2m"
                };
                out.push_str(&format!("  {}{}\x1b[0m\n", color, line));
            }
        }

        for comment in &thread.comments {
            let is_reply = comment.in_reply_to_id.is_some();
            let indent = if is_reply { "    " } else { "  " };
            let prefix = if is_reply { "↳ " } else { "" };
            out.push_str(&format!(
                "{}\x1b[36m{}@{}\x1b[0m: {}\n",
                indent, prefix, comment.author,
                comment.body.lines().next().unwrap_or("")
            ));
            // Show remaining lines indented
            for line in comment.body.lines().skip(1) {
                out.push_str(&format!("{}  {}\n", indent, line));
            }
        }
        out.push('\n');
    }

    out
}

/// Build a prompt for AI analysis of PR comments.
fn build_analysis_prompt(threads: &[CommentThread]) -> String {
    let mut prompt = String::from(
        "Analyze these PR review comments. For each thread, explain:\n\
         1. What the reviewer is asking for\n\
         2. Whether the feedback is actionable\n\
         3. Suggest a fix if applicable\n\n"
    );

    for (i, thread) in threads.iter().enumerate() {
        prompt.push_str(&format!("## Thread {} — {}\n", i + 1, thread.file));
        if !thread.diff_hunk.is_empty() {
            prompt.push_str("```diff\n");
            prompt.push_str(&thread.diff_hunk);
            prompt.push_str("\n```\n");
        }
        for comment in &thread.comments {
            let prefix = if comment.in_reply_to_id.is_some() { "Reply" } else { "Comment" };
            prompt.push_str(&format!("{} by @{}: {}\n", prefix, comment.author, comment.body));
        }
        prompt.push('\n');
    }

    prompt
}

/// Handle `/pr-comments <PR#>` command.
pub(crate) async fn handle_pr_comments(
    engine: &QueryEngine,
    pr_number: u64,
    cwd: &Path,
) {
    if pr_number == 0 {
        eprintln!("\x1b[33mUsage: /pr-comments <PR#>\x1b[0m");
        eprintln!("Example: /pr-comments 42");
        return;
    }

    eprintln!("\x1b[2mFetching review comments for PR #{}...\x1b[0m", pr_number);

    let comments = match fetch_pr_comments(pr_number, cwd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("\x1b[31mFailed to fetch PR comments: {}\x1b[0m", e);
            return;
        }
    };

    if comments.is_empty() {
        println!("No review comments on PR #{}.", pr_number);
        return;
    }

    let threads = group_into_threads(comments);
    let display = format_threads_display(&threads);
    println!("{}", display);

    // Send to AI for analysis
    let prompt = build_analysis_prompt(&threads);
    let stream = engine.submit(&prompt).await;
    let cost = engine.cost_tracker();
    let model = {
        let s = engine.state().read().await;
        s.model.clone()
    };
    if let Err(e) = crate::output::print_stream(stream, &model, Some(cost), None).await {
        eprintln!("\x1b[31mAnalysis error: {}\x1b[0m", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_remote_ssh() {
        let (owner, repo) = parse_github_remote("git@github.com:anthropics/claude-code.git").unwrap();
        assert_eq!(owner, "anthropics");
        assert_eq!(repo, "claude-code");
    }

    #[test]
    fn parse_github_remote_https() {
        let (owner, repo) = parse_github_remote("https://github.com/user/repo.git").unwrap();
        assert_eq!(owner, "user");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn parse_github_remote_https_no_git() {
        let (owner, repo) = parse_github_remote("https://github.com/user/repo").unwrap();
        assert_eq!(owner, "user");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn parse_github_remote_invalid() {
        assert!(parse_github_remote("https://gitlab.com/user/repo").is_none());
    }

    #[test]
    fn parse_comments_json_basic() {
        let json = r#"[
            {
                "id": 1,
                "path": "src/main.rs",
                "line": 42,
                "diff_hunk": "@@ -1,3 +1,5 @@\n+new line",
                "body": "Please fix this",
                "user": { "login": "reviewer" },
                "in_reply_to_id": null,
                "created_at": "2024-01-01T00:00:00Z"
            }
        ]"#;
        let comments = parse_comments_json(json).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].file, "src/main.rs");
        assert_eq!(comments[0].line, Some(42));
        assert_eq!(comments[0].author, "reviewer");
    }

    #[test]
    fn parse_comments_json_with_reply() {
        let json = r#"[
            {"id": 1, "path": "a.rs", "line": 1, "diff_hunk": "", "body": "root", "user": {"login": "a"}, "in_reply_to_id": null, "created_at": ""},
            {"id": 2, "path": "a.rs", "line": 1, "diff_hunk": "", "body": "reply", "user": {"login": "b"}, "in_reply_to_id": 1, "created_at": ""}
        ]"#;
        let comments = parse_comments_json(json).unwrap();
        assert_eq!(comments.len(), 2);
        assert!(comments[1].in_reply_to_id.is_some());
    }

    #[test]
    fn group_threads_basic() {
        let comments = vec![
            PrComment {
                id: 1, file: "a.rs".into(), line: Some(10), diff_hunk: "hunk".into(),
                body: "fix".into(), author: "rev".into(), in_reply_to_id: None, created_at: String::new(),
            },
            PrComment {
                id: 2, file: "a.rs".into(), line: Some(10), diff_hunk: "hunk".into(),
                body: "ok".into(), author: "dev".into(), in_reply_to_id: Some(1), created_at: String::new(),
            },
            PrComment {
                id: 3, file: "b.rs".into(), line: Some(5), diff_hunk: "hunk2".into(),
                body: "other".into(), author: "rev".into(), in_reply_to_id: None, created_at: String::new(),
            },
        ];

        let threads = group_into_threads(comments);
        assert_eq!(threads.len(), 2);
        // First thread (a.rs) should have 2 comments
        assert_eq!(threads[0].comments.len(), 2);
        // Second thread (b.rs) should have 1 comment
        assert_eq!(threads[1].comments.len(), 1);
    }

    #[test]
    fn format_threads_display_output() {
        let threads = vec![CommentThread {
            file: "src/main.rs".into(),
            line: Some(42),
            diff_hunk: "+new code".into(),
            comments: vec![PrComment {
                id: 1, file: "src/main.rs".into(), line: Some(42),
                diff_hunk: "+new code".into(), body: "Looks good".into(),
                author: "reviewer".into(), in_reply_to_id: None, created_at: String::new(),
            }],
        }];

        let output = format_threads_display(&threads);
        assert!(output.contains("PR Review Comments"));
        assert!(output.contains("src/main.rs:42"));
        assert!(output.contains("@reviewer"));
    }
}
