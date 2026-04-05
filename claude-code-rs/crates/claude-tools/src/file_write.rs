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
        "Writes a file to the local filesystem. Overwrites existing files if present. \
         If this is an existing file, you MUST use Read first. Prefer Edit for modifying \
         existing files — it only sends the diff. Use Write for new files or complete rewrites. \
         NEVER create documentation files (*.md) or README files unless explicitly requested."
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

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Read existing content (if any) before writing — avoids TOCTOU by
        // basing the "new vs overwrite" decision on the actual read result.
        match tokio::fs::read_to_string(&path).await {
            Ok(old) => {
                // File exists — show diff and overwrite
                crate::diff_ui::print_diff(file_path, &old, content);
                tokio::fs::write(&path, content).await?;
                Ok(ToolResult::text(format!("Wrote {}", path.display())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // New file
                print_create_diff(file_path, content);
                tokio::fs::write(&path, content).await?;
                Ok(ToolResult::text(format!("Created {}", path.display())))
            }
            Err(e) => {
                Ok(ToolResult::error(format!("Cannot read existing file: {}", e)))
            }
        }
    }
}
