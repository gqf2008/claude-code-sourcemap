use async_trait::async_trait;
use clawed_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::path_util;

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str { "Glob" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn description(&self) -> &'static str {
        "Fast file pattern matching tool that works with any codebase size. Supports glob \
         patterns like \"**/*.js\" or \"src/**/*.ts\". Returns matching file paths sorted by \
         modification time. Use when you need to find files by name patterns. For open-ended \
         search requiring multiple rounds, use the Agent tool instead."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "e.g. **/*.rs" },
                "path": { "type": "string", "description": "Search root (default: cwd)" }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let pattern = input["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'pattern'"))?;

        // Prevent directory traversal via pattern
        if pattern.contains("..") || std::path::Path::new(pattern).is_absolute() {
            return Ok(ToolResult::error("Pattern cannot contain '..' or be an absolute path"));
        }

        let search_dir = match input["path"].as_str() {
            Some(p) => match path_util::resolve_path_safe(p, &context.cwd) {
                Ok(resolved) => resolved,
                Err(e) => return Ok(ToolResult::error(format!("{e}"))),
            },
            None => context.cwd.clone(),
        };
        let full = search_dir.join(pattern).to_string_lossy().to_string();
        let mut matches: Vec<String> = Vec::new();
        for entry in glob::glob(&full).map_err(|e| anyhow::anyhow!("Bad glob: {e}"))? {
            match entry {
                Ok(path) => {
                    // Both paths must canonicalize successfully — reject on failure
                    // to prevent symlink-based boundary escape
                    let Ok(resolved) = path.canonicalize() else { continue };
                    let Ok(search_canonical) = search_dir.canonicalize() else { continue };
                    if resolved.starts_with(&search_canonical) {
                        matches.push(resolved.display().to_string());
                    }
                }
                Err(_) => continue,
            }
        }
        matches.sort();
        if matches.is_empty() {
            Ok(ToolResult::text("No files matched."))
        } else {
            Ok(ToolResult::text(matches.join("\n")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawed_core::tool::AbortSignal;
    use clawed_core::permissions::PermissionMode;

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
            clawed_core::message::ToolResultContent::Text { text } => text.clone(),
            _ => String::new(),
        }
    }

    #[tokio::test]
    async fn glob_finds_matching_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main(){}").unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "pub mod lib;").unwrap();
        std::fs::write(tmp.path().join("readme.md"), "# hello").unwrap();

        let tool = GlobTool;
        let input = json!({"pattern": "*.rs"});
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
        let text = result_text(&result);
        assert!(text.contains("main.rs"));
        assert!(text.contains("lib.rs"));
        assert!(!text.contains("readme.md"));
    }

    #[tokio::test]
    async fn glob_no_matches() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = GlobTool;
        let input = json!({"pattern": "*.xyz"});
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result_text(&result).contains("No files matched"));
    }

    #[tokio::test]
    async fn glob_rejects_dotdot_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = GlobTool;
        let input = json!({"pattern": "../*.rs"});
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn glob_missing_pattern_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = GlobTool;
        let result = tool.call(json!({}), &ctx(tmp.path())).await;
        assert!(result.is_err());
    }
}
