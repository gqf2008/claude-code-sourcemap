//! /review command handler — code review on git changes or PRs.

use clawed_agent::engine::QueryEngine;
use crate::output::print_stream;

/// Launch a code review on recent git changes or a specific PR.
pub(crate) async fn handle_review(engine: &QueryEngine, custom_prompt: &str, cwd: &std::path::Path) {
    // Check if reviewing a specific PR (e.g., "/review #123" or "/review 123")
    let pr_number = custom_prompt
        .trim()
        .strip_prefix('#')
        .or(Some(custom_prompt.trim()))
        .and_then(|s| s.parse::<u64>().ok());

    if let Some(pr_num) = pr_number {
        handle_review_pr(engine, pr_num, cwd).await;
        return;
    }

    let diff_output = std::process::Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(cwd)
        .output();

    let (diff, source) = match diff_output {
        Ok(out) => {
            let d = String::from_utf8_lossy(&out.stdout).to_string();
            if d.is_empty() {
                let staged = std::process::Command::new("git")
                    .args(["diff", "--cached"])
                    .current_dir(cwd)
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if staged.is_empty() {
                    println!("没有可 review 的更改。先做一些更改吧。");
                    return;
                }
                (staged, "staged")
            } else {
                (d, "unstaged")
            }
        }
        Err(e) => {
            eprintln!("\x1b[31m获取 git diff 失败: {}\x1b[0m", e);
            return;
        }
    };

    // Show visual diff preview with file-level stats
    let file_stats = parse_diff_file_stats(&diff);
    if !file_stats.is_empty() {
        let total_added: usize = file_stats.iter().map(|(_, a, _)| a).sum();
        let total_removed: usize = file_stats.iter().map(|(_, _, r)| r).sum();
        eprintln!(
            "\x1b[35m[Code Review]\x1b[0m {} 更改 ({}) — \x1b[32m+{}\x1b[0m / \x1b[31m-{}\x1b[0m 行",
            file_stats.len(),
            source,
            total_added,
            total_removed,
        );
        print_file_stats(&file_stats);
        eprintln!();
    } else {
        eprintln!("\x1b[35m[Code Review]\x1b[0m");
    }

    let review_prompt = if custom_prompt.is_empty() {
        format!(
            "Review 以下代码更改，检查 bug、风格问题、安全隐患和改进建议。\
             请具体指出文件路径和行号。\n\n\
             ```diff\n{}\n```",
            diff
        )
    } else {
        format!("{}\n\n```diff\n{}\n```", custom_prompt, diff)
    };

    let model = { engine.state().read().await.model.clone() };
    let stream = engine.submit(&review_prompt).await;
    if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker()), None).await {
        eprintln!("\x1b[31mReview 错误: {}\x1b[0m", e);
    }
}

/// Print per-file change statistics in a compact format.
fn print_file_stats(file_stats: &[(String, usize, usize)]) {
    for (file, added, removed) in file_stats {
        let stat = if *added > 0 && *removed > 0 {
            format!("\x1b[32m+{}\x1b[0m/\x1b[31m-{}\x1b[0m", added, removed)
        } else if *added > 0 {
            format!("\x1b[32m+{}\x1b[0m", added)
        } else {
            format!("\x1b[31m-{}\x1b[0m", removed)
        };
        eprintln!("  \x1b[2m•\x1b[0m {} ({})", file, stat);
    }
}

/// Parse a unified diff to extract per-file line statistics.
/// Returns Vec of (filename, lines_added, lines_removed).
fn parse_diff_file_stats(diff: &str) -> Vec<(String, usize, usize)> {
    let mut results: Vec<(String, usize, usize)> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut added = 0usize;
    let mut removed = 0usize;

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            // Flush previous file
            if let Some(file) = current_file.take() {
                if added > 0 || removed > 0 {
                    results.push((file, added, removed));
                }
            }
            current_file = Some(rest.to_string());
            added = 0;
            removed = 0;
        } else if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
    }
    // Flush last file
    if let Some(file) = current_file {
        if added > 0 || removed > 0 {
            results.push((file, added, removed));
        }
    }
    results
}

/// Review a specific PR by number using gh CLI.
async fn handle_review_pr(engine: &QueryEngine, pr_number: u64, cwd: &std::path::Path) {
    // Check gh CLI availability
    let gh_available = std::process::Command::new("gh")
        .args(["--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !gh_available {
        eprintln!("\x1b[31m需要 gh CLI 来 review PR。请安装: https://cli.github.com\x1b[0m");
        return;
    }

    // Get PR info
    let pr_info = std::process::Command::new("gh")
        .args(["pr", "view", &pr_number.to_string(), "--json", "title,body,author,state,additions,deletions"])
        .current_dir(cwd)
        .output();

    let pr_meta = match pr_info {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            eprintln!("\x1b[31m获取 PR #{} 信息失败: {}\x1b[0m", pr_number, err.trim());
            return;
        }
        Err(e) => {
            eprintln!("\x1b[31m运行 gh 失败: {}\x1b[0m", e);
            return;
        }
    };

    // Get PR diff
    let pr_diff = std::process::Command::new("gh")
        .args(["pr", "diff", &pr_number.to_string()])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    if pr_diff.is_empty() {
        eprintln!("\x1b[33mPR #{} 没有差异内容。\x1b[0m", pr_number);
        return;
    }

    // Show file-level diff stats
    let file_stats = parse_diff_file_stats(&pr_diff);
    if !file_stats.is_empty() {
        let total_added: usize = file_stats.iter().map(|(_, a, _)| a).sum();
        let total_removed: usize = file_stats.iter().map(|(_, _, r)| r).sum();
        eprintln!(
            "\x1b[35m[PR Review]\x1b[0m PR #{} — {} 文件, \x1b[32m+{}\x1b[0m / \x1b[31m-{}\x1b[0m 行",
            pr_number, file_stats.len(), total_added, total_removed,
        );
        print_file_stats(&file_stats);
        eprintln!();
    } else {
        println!("\x1b[35m[PR Review]\x1b[0m 正在分析 PR #{}…", pr_number);
    }

    let truncated_diff = if pr_diff.len() > 15000 {
        format!("{}…\n[已截断, 共 {} 字节]", &pr_diff[..15000], pr_diff.len())
    } else {
        pr_diff
    };

    let prompt = format!(
        "Review PR #{num} 的代码更改。\n\n\
         PR 信息:\n```json\n{meta}\n```\n\n\
         请检查:\n\
         - Bug 和逻辑错误\n\
         - 安全隐患\n\
         - 代码风格和最佳实践\n\
         - 性能问题\n\
         - 测试覆盖\n\n\
         请具体指出文件路径和行号，给出可操作的改进建议。\n\n\
         差异:\n```diff\n{diff}\n```",
        num = pr_number,
        meta = pr_meta.trim(),
        diff = truncated_diff,
    );

    let model = { engine.state().read().await.model.clone() };
    let stream = engine.submit(&prompt).await;
    if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker()), None).await {
        eprintln!("\x1b[31mPR Review 错误: {}\x1b[0m", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_diff_file_stats_basic() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,5 @@
 fn main() {
-    println!(\"hello\");
+    println!(\"hello world\");
+    println!(\"goodbye\");
+    // new comment
 }
";
        let stats = parse_diff_file_stats(diff);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].0, "src/main.rs");
        assert_eq!(stats[0].1, 3); // added
        assert_eq!(stats[0].2, 1); // removed
    }

    #[test]
    fn test_parse_diff_file_stats_multi_file() {
        let diff = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1,2 @@
 line1
+line2
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1,2 +1 @@
 keep
-remove
";
        let stats = parse_diff_file_stats(diff);
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0], ("a.rs".to_string(), 1, 0));
        assert_eq!(stats[1], ("b.rs".to_string(), 0, 1));
    }

    #[test]
    fn test_parse_diff_file_stats_empty() {
        assert!(parse_diff_file_stats("").is_empty());
        assert!(parse_diff_file_stats("no diff here").is_empty());
    }
}
