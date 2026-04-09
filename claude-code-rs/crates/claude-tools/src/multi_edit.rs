use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::path_util;

/// Applies multiple consecutive string replacements to a single file atomically.
/// This is more efficient than calling Edit multiple times for the same file.
pub struct MultiEditTool;

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &'static str { "MultiEdit" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn description(&self) -> &'static str {
        "Perform multiple edits to a single file in one atomic operation. Each edit replaces an \
         exact unique string with new content. Edits are applied sequentially in the given order. \
         Use this instead of multiple Edit calls on the same file."
    }

    fn to_auto_classifier_input(&self, input: &Value) -> Value {
        // Only pass path and edit count; strip content
        let path = input.get("file_path").cloned().unwrap_or(Value::Null);
        let edit_count = input.get("edits").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        json!({"MultiEdit": {"path": path, "edit_count": edit_count}})
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "edits": {
                    "type": "array",
                    "description": "List of edits to apply in order",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": {
                                "type": "string",
                                "description": "Exact string to replace. Must appear exactly once in the file."
                            },
                            "new_string": {
                                "type": "string",
                                "description": "Replacement string"
                            }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["file_path", "edits"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'file_path'"))?;

        let path = match path_util::resolve_path(file_path, &context.cwd) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{e}"))),
        };

        let edits = input["edits"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Missing 'edits' array"))?;

        if edits.is_empty() {
            return Ok(ToolResult::error("No edits provided."));
        }

        let original = tokio::fs::read_to_string(&path).await?;

        // Pre-validate: check all old_strings are present and unique in original
        // and detect overlapping regions before modifying anything
        let mut regions: Vec<(usize, usize, usize)> = Vec::new(); // (start, end, edit_index)
        for (i, edit) in edits.iter().enumerate() {
            let old_str = edit["old_string"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Edit {i} missing 'old_string'"))?;
            if old_str.is_empty() {
                return Ok(ToolResult::error(format!("Edit {i}: old_string must not be empty")));
            }
            let count = original.matches(old_str).count();
            if count == 0 {
                return Ok(ToolResult::error(format!(
                    "Edit {}: old_string not found in file.\nold_string: {:?}",
                    i, truncate(old_str, 100)
                )));
            }
            if count > 1 {
                return Ok(ToolResult::error(format!(
                    "Edit {}: old_string found {} times — must be unique.\nold_string: {:?}",
                    i, count, truncate(old_str, 100)
                )));
            }
            if let Some(pos) = original.find(old_str) {
                regions.push((pos, pos + old_str.len(), i));
            }
        }

        // Check for overlapping regions
        regions.sort_by_key(|r| r.0);
        for w in regions.windows(2) {
            if w[0].1 > w[1].0 {
                return Ok(ToolResult::error(format!(
                    "Edits {} and {} have overlapping regions ({}-{} and {}-{}). \
                     Split into separate Edit calls or merge into one edit.",
                    w[0].2, w[1].2, w[0].0, w[0].1, w[1].0, w[1].1
                )));
            }
        }

        // Apply edits using offset-based replacement on the original content.
        // We already know each old_string's position in the original (from regions).
        // Sort regions by start position descending so replacements don't shift
        // earlier offsets.
        let mut content = original.clone();
        let mut indexed_edits: Vec<(usize, usize, &str)> = Vec::new(); // (start, end, new_string)
        for &(start, end, edit_idx) in &regions {
            let new_str = edits[edit_idx]["new_string"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Edit {} missing 'new_string'", edit_idx))?;
            indexed_edits.push((start, end, new_str));
        }
        // Apply from end to start so byte offsets remain valid
        indexed_edits.sort_by(|a, b| b.0.cmp(&a.0));
        for (start, end, new_str) in indexed_edits {
            content.replace_range(start..end, new_str);
        }

        tokio::fs::write(&path, &content).await?;

        // Print diff of net changes (original → final)
        crate::diff_ui::print_diff(file_path, &original, &content);

        Ok(ToolResult::text(format!(
            "Applied {} edit(s) to {}",
            edits.len(),
            path.display()
        )))
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
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

    fn result_text(r: &ToolResult) -> &str {
        match &r.content[0] {
            claude_core::message::ToolResultContent::Text { text } => text.as_str(),
            _ => "",
        }
    }

    #[tokio::test]
    async fn single_edit_succeeds() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, "hello world").unwrap();

        let tool = MultiEditTool;
        let input = json!({
            "file_path": file.to_str().unwrap(),
            "edits": [{"old_string": "hello", "new_string": "goodbye"}]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "goodbye world");
    }

    #[tokio::test]
    async fn multiple_non_overlapping_edits() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, "aaa bbb ccc").unwrap();

        let tool = MultiEditTool;
        let input = json!({
            "file_path": file.to_str().unwrap(),
            "edits": [
                {"old_string": "aaa", "new_string": "AAA"},
                {"old_string": "ccc", "new_string": "CCC"}
            ]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "AAA bbb CCC");
    }

    #[tokio::test]
    async fn old_string_not_found_returns_error() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, "hello world").unwrap();

        let tool = MultiEditTool;
        let input = json!({
            "file_path": file.to_str().unwrap(),
            "edits": [{"old_string": "missing", "new_string": "x"}]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("not found"));
    }

    #[tokio::test]
    async fn non_unique_old_string_returns_error() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, "aaa bbb aaa").unwrap();

        let tool = MultiEditTool;
        let input = json!({
            "file_path": file.to_str().unwrap(),
            "edits": [{"old_string": "aaa", "new_string": "x"}]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("2 times"));
    }

    #[tokio::test]
    async fn overlapping_edits_detected() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, "hello world end").unwrap();

        let tool = MultiEditTool;
        let input = json!({
            "file_path": file.to_str().unwrap(),
            "edits": [
                {"old_string": "hello world", "new_string": "X"},
                {"old_string": "world end", "new_string": "Y"}
            ]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error, "overlapping edits should be rejected");
        assert!(result_text(&result).contains("overlapping"));
    }

    #[tokio::test]
    async fn empty_edits_array_rejected() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();

        let tool = MultiEditTool;
        let input = json!({
            "file_path": file.to_str().unwrap(),
            "edits": []
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("No edits"));
    }

    #[tokio::test]
    async fn empty_old_string_rejected() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();

        let tool = MultiEditTool;
        let input = json!({
            "file_path": file.to_str().unwrap(),
            "edits": [{"old_string": "", "new_string": "x"}]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("empty"));
    }

    #[tokio::test]
    async fn adjacent_non_overlapping_edits_succeed() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, "AABBCC").unwrap();

        let tool = MultiEditTool;
        let input = json!({
            "file_path": file.to_str().unwrap(),
            "edits": [
                {"old_string": "AA", "new_string": "xx"},
                {"old_string": "BB", "new_string": "yy"},
                {"old_string": "CC", "new_string": "zz"}
            ]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error, "adjacent edits should succeed: {}", result_text(&result));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "xxyyzz");
    }

    #[tokio::test]
    async fn unicode_edits_work_correctly() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, "Hello 你好 world 🎉").unwrap();

        let tool = MultiEditTool;
        let input = json!({
            "file_path": file.to_str().unwrap(),
            "edits": [{"old_string": "你好", "new_string": "世界"}]
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "Hello 世界 world 🎉");
    }
}
