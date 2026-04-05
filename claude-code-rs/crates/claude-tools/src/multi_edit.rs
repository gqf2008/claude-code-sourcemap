use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::path_util;

/// Applies multiple consecutive string replacements to a single file atomically.
/// This is more efficient than calling Edit multiple times for the same file.
pub struct MultiEditTool;

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str { "MultiEdit" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn description(&self) -> &str {
        "Perform multiple edits to a single file in one atomic operation. Each edit replaces an \
         exact unique string with new content. Edits are applied sequentially in the given order. \
         Use this instead of multiple Edit calls on the same file."
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
            Err(e) => return Ok(ToolResult::error(format!("{}", e))),
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
                .ok_or_else(|| anyhow::anyhow!("Edit {} missing 'old_string'", i))?;
            if old_str.is_empty() {
                return Ok(ToolResult::error(format!("Edit {}: old_string must not be empty", i)));
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

        // Apply edits sequentially (safe since we pre-validated)
        let mut content = original.clone();
        for (i, edit) in edits.iter().enumerate() {
            let old_str = edit["old_string"].as_str().unwrap();
            let new_str = edit["new_string"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Edit {} missing 'new_string'", i))?;
            content = content.replacen(old_str, new_str, 1);
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
