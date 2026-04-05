use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::diff_ui::print_diff;
use crate::path_util;

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str { "Edit" }

    fn description(&self) -> &str {
        "Performs exact string replacements in files. You must use Read at least once before \
         editing. The edit will FAIL if old_string is not unique in the file — provide more \
         surrounding context to make it unique. \
         Preserve exact indentation from the file content (after the line number prefix). \
         ALWAYS prefer editing existing files over creating new ones."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let file_path = input["file_path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'file_path'"))?;
        let old_string = input["old_string"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'old_string'"))?;
        let new_string = input["new_string"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'new_string'"))?;

        let path = match path_util::resolve_path(file_path, &context.cwd) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{}", e))),
        };

        let content = tokio::fs::read_to_string(&path).await?;
        let count = content.matches(old_string).count();
        if count == 0 {
            return Ok(ToolResult::error("old_string not found in file."));
        }
        if count > 1 {
            return Ok(ToolResult::error(format!(
                "old_string found {} times — must be unique.", count
            )));
        }

        let new_content = content.replacen(old_string, new_string, 1);

        // Print colored diff before writing
        print_diff(file_path, &content, &new_content);

        tokio::fs::write(&path, &new_content).await?;
        Ok(ToolResult::text(format!("Edited {}", path.display())))
    }
}
