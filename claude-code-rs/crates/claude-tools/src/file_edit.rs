use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::Path;

use crate::diff_ui::print_diff;

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str { "Edit" }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact, unique string match with new content."
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

        let path = if Path::new(file_path).is_absolute() {
            std::path::PathBuf::from(file_path)
        } else {
            context.cwd.join(file_path)
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
