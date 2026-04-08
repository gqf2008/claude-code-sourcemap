use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use ignore::WalkBuilder;
use regex::Regex;
use std::path::PathBuf;

/// Map short type names to glob patterns (aligned with ripgrep --type).
fn type_to_globs(ty: &str) -> Option<Vec<&'static str>> {
    match ty {
        "py" | "python" => Some(vec!["*.py", "*.pyi"]),
        "js" | "javascript" => Some(vec!["*.js", "*.mjs", "*.cjs"]),
        "ts" | "typescript" => Some(vec!["*.ts", "*.tsx", "*.mts", "*.cts"]),
        "rs" | "rust" => Some(vec!["*.rs"]),
        "go" => Some(vec!["*.go"]),
        "java" => Some(vec!["*.java"]),
        "c" => Some(vec!["*.c", "*.h"]),
        "cpp" => Some(vec!["*.cpp", "*.cc", "*.cxx", "*.hpp", "*.hxx", "*.h"]),
        "rb" | "ruby" => Some(vec!["*.rb"]),
        "php" => Some(vec!["*.php"]),
        "html" => Some(vec!["*.html", "*.htm"]),
        "css" => Some(vec!["*.css"]),
        "json" => Some(vec!["*.json"]),
        "yaml" | "yml" => Some(vec!["*.yaml", "*.yml"]),
        "toml" => Some(vec!["*.toml"]),
        "md" | "markdown" => Some(vec!["*.md", "*.markdown"]),
        "sh" | "shell" | "bash" => Some(vec!["*.sh", "*.bash"]),
        "sql" => Some(vec!["*.sql"]),
        "xml" => Some(vec!["*.xml"]),
        "swift" => Some(vec!["*.swift"]),
        "kt" | "kotlin" => Some(vec!["*.kt", "*.kts"]),
        "scala" => Some(vec!["*.scala"]),
        "r" => Some(vec!["*.r", "*.R"]),
        _ => None,
    }
}

/// Check if a path matches any of the given glob patterns.
fn matches_type_globs(path: &std::path::Path, globs: &[&str]) -> bool {
    let filename = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    globs.iter().any(|g| {
        glob::Pattern::new(g).is_ok_and(|p| p.matches(filename))
    })
}

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "Grep" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn description(&self) -> &str {
        "A powerful search tool built on ripgrep. ALWAYS use Grep for search tasks — NEVER \
         invoke grep or rg as a Bash command. Supports full regex syntax (e.g. \"log.*Error\"). \
         Filter by glob (e.g. \"*.js\") or type (e.g. \"py\", \"rust\"). Output modes: \
         \"content\" shows matching lines, \"files_with_matches\" shows only paths (default), \
         \"count\" shows match counts. For cross-line patterns use multiline: true."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "Directory or file to search in" },
                "include": { "type": "string", "description": "Glob filter (e.g. \"*.js\")" },
                "type": { "type": "string", "description": "File type filter (e.g. \"py\", \"rust\", \"ts\")" },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "Output format (default: files_with_matches)"
                },
                "multiline": { "type": "boolean", "description": "Enable multiline matching" },
                "context_lines": { "type": "integer", "description": "Lines of context around matches (like -C)" },
                "before_context": { "type": "integer", "description": "Lines before match (like -B)" },
                "after_context": { "type": "integer", "description": "Lines after match (like -A)" },
                "case_insensitive": { "type": "boolean", "description": "Case insensitive search" },
                "head_limit": { "type": "integer", "description": "Max number of results to return" }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let pattern = input["pattern"].as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern'"))?.to_string();

        let search_path: PathBuf = match input["path"].as_str() {
            Some(p) => {
                let pa = std::path::Path::new(p);
                if pa.is_absolute() { pa.to_path_buf() } else { context.cwd.join(pa) }
            }
            None => context.cwd.clone(),
        };
        let include_glob = input["include"].as_str().map(|s| s.to_string());
        let type_filter = input["type"].as_str().map(|s| s.to_string());
        let output_mode = input["output_mode"].as_str().unwrap_or("files_with_matches").to_string();
        let multiline = input["multiline"].as_bool().unwrap_or(false);
        let case_insensitive = input["case_insensitive"].as_bool().unwrap_or(false);
        let context_lines = input["context_lines"].as_u64().map(|n| n as usize);
        let before_context = input["before_context"].as_u64().map(|n| n as usize);
        let after_context = input["after_context"].as_u64().map(|n| n as usize);
        let head_limit = input["head_limit"].as_u64().map(|n| n as usize);

        let max_results = head_limit.unwrap_or(100);

        // Build final pattern (with flags) and check length AFTER wrapping
        let final_pattern = if case_insensitive {
            format!("(?i){}", pattern)
        } else {
            pattern.clone()
        };
        const MAX_PATTERN_LEN: usize = 4096;
        if final_pattern.len() > MAX_PATTERN_LEN {
            return Ok(ToolResult::error(format!(
                "Pattern too long ({} chars, limit is {}). Use a simpler pattern.",
                final_pattern.len(), MAX_PATTERN_LEN
            )));
        }

        let output = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let regex = Regex::new(&final_pattern)
                .map_err(|e| anyhow::anyhow!("Bad regex: {}", e))?;

            let type_globs: Option<Vec<&str>> = type_filter.as_deref()
                .and_then(type_to_globs);

            // Compute context window
            let ctx_before = before_context.or(context_lines).unwrap_or(0);
            let ctx_after = after_context.or(context_lines).unwrap_or(0);

            let mut results = Vec::new();
            let mut file_count = 0usize;
            let mut total_matches = 0usize;

            let walker = WalkBuilder::new(&search_path).hidden(true).git_ignore(true).build();
            'outer: for entry in walker.flatten() {
                if !entry.file_type().is_some_and(|ft| ft.is_file()) { continue; }
                let path = entry.path().to_owned();

                // Type filter
                if let Some(ref globs) = type_globs {
                    if !matches_type_globs(&path, globs) { continue; }
                }

                // Glob filter
                if let Some(ref g) = include_glob {
                    let path_str = path.to_string_lossy();
                    if !glob::Pattern::new(g).is_ok_and(|p| p.matches(&path_str)) {
                        continue;
                    }
                }

                let content = match std::fs::read_to_string(&path) { Ok(c) => c, Err(_) => continue };

                if multiline {
                    // Multiline: search across entire content
                    if regex.is_match(&content) {
                        file_count += 1;
                        let match_count = regex.find_iter(&content).count();
                        total_matches += match_count;

                        match output_mode.as_str() {
                            "files_with_matches" => {
                                results.push(path.display().to_string());
                                if results.len() >= max_results { break 'outer; }
                            }
                            "count" => {
                                results.push(format!("{}:{}", path.display(), match_count));
                                if results.len() >= max_results { break 'outer; }
                            }
                            _ => {
                                // content mode: show matched regions with line numbers
                                for m in regex.find_iter(&content) {
                                    let line_num = content[..m.start()].matches('\n').count() + 1;
                                    let matched = m.as_str();
                                    let preview: String = matched.chars().take(200).collect();
                                    results.push(format!("  {}:{}: {}", path.display(), line_num, preview));
                                    if results.len() >= max_results { break 'outer; }
                                }
                            }
                        }
                    }
                } else {
                    // Line-by-line matching
                    let lines: Vec<&str> = content.lines().collect();
                    let mut file_hits = Vec::new();

                    for (num, line) in lines.iter().enumerate() {
                        if regex.is_match(line) {
                            total_matches += 1;

                            match output_mode.as_str() {
                                "files_with_matches" => {
                                    file_hits.push(path.display().to_string());
                                    break; // One hit per file is enough
                                }
                                "count" => {
                                    // Just count, don't add individual lines
                                }
                                _ => {
                                    // Content mode with context
                                    if ctx_before > 0 || ctx_after > 0 {
                                        let start = num.saturating_sub(ctx_before);
                                        let end = (num + 1 + ctx_after).min(lines.len());
                                        for (idx, line) in lines[start..end].iter().enumerate() {
                                            let i = start + idx;
                                            let prefix = if i == num { ">" } else { " " };
                                            file_hits.push(format!(
                                                "  {}{}:{}: {}", prefix, path.display(), i + 1, line
                                            ));
                                        }
                                        if end < lines.len() {
                                            file_hits.push("  --".to_string());
                                        }
                                    } else {
                                        file_hits.push(format!(
                                            "  {}:{}: {}", path.display(), num + 1, line.trim()
                                        ));
                                    }
                                }
                            }

                            if results.len() + file_hits.len() >= max_results {
                                results.extend(file_hits);
                                break 'outer;
                            }
                        }
                    }

                    if output_mode == "count" && total_matches > 0 {
                        let file_match_count = lines.iter().filter(|l| regex.is_match(l)).count();
                        if file_match_count > 0 {
                            file_count += 1;
                            results.push(format!("{}:{}", path.display(), file_match_count));
                            if results.len() >= max_results { break 'outer; }
                        }
                    } else if !file_hits.is_empty() {
                        file_count += 1;
                        results.extend(file_hits);
                    }
                }
            }

            if results.is_empty() {
                Ok("No matches found.".to_string())
            } else {
                match output_mode.as_str() {
                    "count" => {
                        Ok(format!("{} match(es) in {} file(s):\n{}", total_matches, file_count, results.join("\n")))
                    }
                    "files_with_matches" => {
                        Ok(format!("{} file(s) matched:\n{}", file_count, results.join("\n")))
                    }
                    _ => {
                        Ok(format!("Found {} match(es) in {} file(s):\n{}", total_matches, file_count, results.join("\n")))
                    }
                }
            }
        }).await??;

        Ok(ToolResult::text(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── type_to_globs ────────────────────────────────────────────────────

    #[test]
    fn type_to_globs_python() {
        let globs = type_to_globs("py").unwrap();
        assert!(globs.contains(&"*.py"));
        assert!(globs.contains(&"*.pyi"));
    }

    #[test]
    fn type_to_globs_javascript() {
        let globs = type_to_globs("js").unwrap();
        assert!(globs.contains(&"*.js"));
        assert!(globs.contains(&"*.mjs"));
        assert!(globs.contains(&"*.cjs"));
    }

    #[test]
    fn type_to_globs_typescript() {
        let globs = type_to_globs("ts").unwrap();
        assert!(globs.contains(&"*.ts"));
        assert!(globs.contains(&"*.tsx"));
        assert!(globs.contains(&"*.mts"));
        assert!(globs.contains(&"*.cts"));
    }

    #[test]
    fn type_to_globs_rust() {
        let globs = type_to_globs("rs").unwrap();
        assert_eq!(globs, vec!["*.rs"]);
    }

    #[test]
    fn type_to_globs_unknown() {
        assert!(type_to_globs("brainfuck").is_none());
    }

    #[test]
    fn type_to_globs_returns_none_for_empty() {
        assert!(type_to_globs("").is_none());
    }

    #[test]
    fn type_to_globs_all_aliases() {
        // Each alias pair should yield the same result
        assert_eq!(type_to_globs("py"), type_to_globs("python"));
        assert_eq!(type_to_globs("js"), type_to_globs("javascript"));
        assert_eq!(type_to_globs("ts"), type_to_globs("typescript"));
        assert_eq!(type_to_globs("rs"), type_to_globs("rust"));
        assert_eq!(type_to_globs("rb"), type_to_globs("ruby"));
        assert_eq!(type_to_globs("md"), type_to_globs("markdown"));
        assert_eq!(type_to_globs("sh"), type_to_globs("shell"));
        assert_eq!(type_to_globs("sh"), type_to_globs("bash"));
        assert_eq!(type_to_globs("yaml"), type_to_globs("yml"));
        assert_eq!(type_to_globs("kt"), type_to_globs("kotlin"));
    }

    // ── matches_type_globs ───────────────────────────────────────────────

    #[test]
    fn matches_type_globs_match() {
        let path = Path::new("src/main.rs");
        assert!(matches_type_globs(path, &["*.rs"]));
    }

    #[test]
    fn matches_type_globs_no_match() {
        let path = Path::new("src/main.rs");
        assert!(!matches_type_globs(path, &["*.py", "*.js"]));
    }

    #[test]
    fn matches_type_globs_multiple_extensions() {
        let globs = type_to_globs("ts").unwrap();
        assert!(matches_type_globs(Path::new("app.ts"), &globs));
        assert!(matches_type_globs(Path::new("App.tsx"), &globs));
        assert!(matches_type_globs(Path::new("lib.mts"), &globs));
        assert!(matches_type_globs(Path::new("lib.cts"), &globs));
        assert!(!matches_type_globs(Path::new("lib.js"), &globs));
    }

    // ── pattern length limit ─────────────────────────────────────────────

    #[tokio::test]
    async fn grep_rejects_long_pattern() {
        use claude_core::tool::{Tool, ToolContext};
        use claude_core::permissions::PermissionMode;

        let ctx = ToolContext {
            cwd: std::env::temp_dir(),
            permission_mode: PermissionMode::Default,
            abort_signal: Default::default(),
            messages: vec![],
        };
        let pattern = "a".repeat(5000);
        let input = serde_json::json!({ "pattern": pattern });
        let result = GrepTool.call(input, &ctx).await.unwrap();
        assert!(result.is_error, "should reject long pattern");
    }

    fn grep_ctx(dir: &std::path::Path) -> ToolContext {
        use claude_core::permissions::PermissionMode;
        ToolContext {
            cwd: dir.to_path_buf(),
            permission_mode: PermissionMode::Default,
            abort_signal: Default::default(),
            messages: vec![],
        }
    }

    #[tokio::test]
    async fn grep_invalid_regex_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        let input = serde_json::json!({ "pattern": "[invalid(" });
        let result = GrepTool.call(input, &grep_ctx(tmp.path())).await;
        // Invalid regex should return Err (not panic)
        assert!(result.is_err(), "invalid regex should produce error");
    }

    #[tokio::test]
    async fn grep_empty_pattern_matches_all() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        let input = serde_json::json!({ "pattern": "" });
        // Empty regex matches everything — that's valid behavior
        let result = GrepTool.call(input, &grep_ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn grep_case_insensitive_flag() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "Hello World\nGOODBYE").unwrap();
        // default output_mode is "files_with_matches" — just show file name
        let input = serde_json::json!({ "pattern": "hello", "case_insensitive": true, "output_mode": "content" });
        let result = GrepTool.call(input, &grep_ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error, "case insensitive search should work");
        let text = result.content.iter().map(|c| match c {
            claude_core::message::ToolResultContent::Text { text } => text.clone(),
            _ => String::new(),
        }).collect::<String>();
        // Should find the line containing "Hello" via case-insensitive match
        assert!(text.contains("Hello"), "should match Hello case-insensitively: {}", text);
    }

    #[tokio::test]
    async fn grep_no_matches_is_not_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello world").unwrap();
        let input = serde_json::json!({ "pattern": "zzz_no_match" });
        let result = GrepTool.call(input, &grep_ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn grep_type_filter_narrows_results() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn hello()").unwrap();
        std::fs::write(tmp.path().join("b.py"), "def hello()").unwrap();
        let input = serde_json::json!({ "pattern": "hello", "type": "rs" });
        let result = GrepTool.call(input, &grep_ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
        let text = result.content.iter().map(|c| match c {
            claude_core::message::ToolResultContent::Text { text } => text.clone(),
            _ => String::new(),
        }).collect::<String>();
        assert!(text.contains("a.rs"));
        assert!(!text.contains("b.py"));
    }

    // ── type_to_globs edge cases ─────────────────────────────────────

    #[test]
    fn type_to_globs_unknown_returns_none() {
        assert!(type_to_globs("fortran77").is_none());
    }

    #[test]
    fn type_to_globs_aliases_equivalent() {
        assert_eq!(type_to_globs("py"), type_to_globs("python"));
        assert_eq!(type_to_globs("js"), type_to_globs("javascript"));
        assert_eq!(type_to_globs("ts"), type_to_globs("typescript"));
        assert_eq!(type_to_globs("rs"), type_to_globs("rust"));
    }
}
