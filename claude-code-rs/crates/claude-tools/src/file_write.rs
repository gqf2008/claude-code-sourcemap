use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::diff_ui::print_create_diff;
use crate::path_util;

pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str { "Write" }

    fn description(&self) -> &str {
        "Create a new file or overwrite an existing file with the provided content."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["file_path", "content"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let file_path = input["file_path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'file_path'"))?;
        let content = input["content"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'content'"))?;

        let path = match path_util::resolve_path(file_path, &context.cwd) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{}", e))),
        };

        let is_new = !path.exists();

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        if is_new {
            print_create_diff(file_path, content);
            tokio::fs::write(&path, content).await?;
            Ok(ToolResult::text(format!("Created {}", path.display())))
        } else {
            // Overwrite: show diff
            let old = tokio::fs::read_to_string(&path).await.unwrap_or_default();
            crate::diff_ui::print_diff(file_path, &old, content);
            tokio::fs::write(&path, content).await?;
            Ok(ToolResult::text(format!("Wrote {}", path.display())))
        }
    }
}
