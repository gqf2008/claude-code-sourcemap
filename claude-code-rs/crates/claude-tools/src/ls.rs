use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::path_util;

pub struct LsTool;

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &'static str { "LS" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn is_read_only(&self) -> bool { true }

    fn description(&self) -> &'static str {
        "Lists files and directories in a given path. Use this to explore project structure \
         and discover files. Prefer this over shell 'ls' for directory exploration."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The directory path to list. Relative paths are resolved from the working directory."
                },
                "ignore": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Glob patterns to ignore (e.g. [\"*.log\", \"node_modules\"])"
                }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let raw_path = input["path"].as_str().unwrap_or(".");
        let ignore: Vec<String> = input["ignore"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let dir = match path_util::resolve_path(raw_path, &context.cwd) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{e}"))),
        };

        if !dir.exists() {
            return Ok(ToolResult::error(format!("Path does not exist: {}", dir.display())));
        }
        if !dir.is_dir() {
            return Ok(ToolResult::error(format!("Not a directory: {}", dir.display())));
        }

        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();

            // Apply ignore patterns (simple prefix/suffix matching with *)
            if ignore.iter().any(|pat| glob_match(pat, &name)) {
                continue;
            }

            let meta = match entry.metadata().await {
                Ok(m) => m,
                Err(_) => continue, // skip entries with inaccessible metadata
            };
            let entry_type = if meta.is_dir() { "dir" } else { "file" };
            let size = if meta.is_file() { meta.len() } else { 0 };

            entries.push((name, entry_type, size));
        }

        entries.sort_by(|a, b| {
            // Dirs first, then files, then alphabetically
            match (a.1, b.1) {
                ("dir", "file") => std::cmp::Ordering::Less,
                ("file", "dir") => std::cmp::Ordering::Greater,
                _ => a.0.cmp(&b.0),
            }
        });

        let mut lines = vec![format!("{}:", dir.display())];
        for (name, kind, size) in &entries {
            if *kind == "dir" {
                lines.push(format!("  {name}/"));
            } else {
                lines.push(format!("  {}  ({})", name, human_size(*size)));
            }
        }

        if entries.is_empty() {
            lines.push("  (empty)".to_string());
        }

        Ok(ToolResult::text(lines.join("\n")))
    }
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    match bytes {
        b if b >= GB => format!("{:.1}GB", b as f64 / GB as f64),
        b if b >= MB => format!("{:.1}MB", b as f64 / MB as f64),
        b if b >= KB => format!("{:.1}KB", b as f64 / KB as f64),
        b => format!("{b}B"),
    }
}

/// Minimal glob matching: supports leading/trailing `*` wildcards and `*x*` contains.
fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" { return true; }
    // Handle *needle* (contains) before the prefix/suffix checks
    if pattern.starts_with('*') && pattern.ends_with('*') && pattern.len() >= 2 {
        return name.contains(&pattern[1..pattern.len() - 1]);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    pattern == name
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::tool::AbortSignal;
    use claude_core::permissions::PermissionMode;
    use tempfile::TempDir;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            cwd: dir.to_path_buf(),
            abort_signal: AbortSignal::new(),
            permission_mode: PermissionMode::Default,
            messages: vec![],
        }
    }

    fn result_text(r: &ToolResult) -> String {
        match &r.content[0] {
            claude_core::message::ToolResultContent::Text { text } => text.clone(),
            _ => String::new(),
        }
    }

    #[tokio::test]
    async fn lists_empty_directory() {
        let tmp = TempDir::new().unwrap();
        let tool = LsTool;
        let input = json!({"path": tmp.path().to_str().unwrap()});
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
        assert!(result_text(&result).contains("(empty)"));
    }

    #[tokio::test]
    async fn lists_files_and_dirs() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("readme.md"), "hello").unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main(){}").unwrap();

        let tool = LsTool;
        let input = json!({"path": tmp.path().to_str().unwrap()});
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        let text = result_text(&result);
        // Dir first
        assert!(text.find("src/").unwrap() < text.find("main.rs").unwrap());
        assert!(text.contains("readme.md"));
    }

    #[tokio::test]
    async fn ignore_pattern_works() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("app.log"), "log").unwrap();
        std::fs::write(tmp.path().join("main.rs"), "code").unwrap();

        let tool = LsTool;
        let input = json!({"path": tmp.path().to_str().unwrap(), "ignore": ["*.log"]});
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        let text = result_text(&result);
        assert!(!text.contains("app.log"));
        assert!(text.contains("main.rs"));
    }

    #[tokio::test]
    async fn nonexistent_path_returns_error() {
        let tmp = TempDir::new().unwrap();
        let tool = LsTool;
        let input = json!({"path": tmp.path().join("nonexistent").to_str().unwrap()});
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
    }

    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1024), "1.0KB");
        assert_eq!(human_size(1536), "1.5KB");
        assert_eq!(human_size(1024 * 1024), "1.0MB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0GB");
    }

    #[test]
    fn glob_match_patterns() {
        // Exact and wildcard
        assert!(glob_match("*", "anything"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "other"));
        // Suffix
        assert!(glob_match("*.log", "test.log"));
        assert!(!glob_match("*.log", "test.txt"));
        // Prefix
        assert!(glob_match("node_modules*", "node_modules"));
        assert!(glob_match("node_modules*", "node_modules_backup"));
        assert!(!glob_match("node_modules*", "src"));
        // Contains (*needle*)
        assert!(glob_match("*foo*", "foobar"));
        assert!(glob_match("*foo*", "barfoo"));
        assert!(glob_match("*foo*", "barfoobaz"));
        assert!(!glob_match("*foo*", "bar"));
        // Double star — should match everything like *
        assert!(glob_match("**", "anything"));
        assert!(glob_match("**", ""));
        // Single char pattern
        assert!(glob_match("*a", "data"));
        assert!(!glob_match("*a", "dab"));
    }
}
