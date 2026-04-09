use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::diff_ui::print_create_diff;
use crate::path_util;

pub struct FileWriteTool;

/// Maximum content size we'll write (10 MB).
const MAX_WRITE_BYTES: usize = 10 * 1024 * 1024;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &'static str { "Write" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn description(&self) -> &'static str {
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

        if content.len() > MAX_WRITE_BYTES {
            return Ok(ToolResult::error(format!(
                "Content too large ({} bytes, limit is {} MB). Break the write into smaller files.",
                content.len(), MAX_WRITE_BYTES / 1024 / 1024
            )));
        }

        let path = match path_util::resolve_path(file_path, &context.cwd) {
            Ok(p) => p,
            Err(e) => {
                warn!(file_path, error = %e, "Write path resolution rejected");
                return Ok(ToolResult::error(format!("{e}")));
            }
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
                debug!(path = %path.display(), bytes = content.len(), "Overwrote existing file");
                Ok(ToolResult::text(format!("Wrote {}", path.display())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // New file
                print_create_diff(file_path, content);
                tokio::fs::write(&path, content).await?;
                debug!(path = %path.display(), bytes = content.len(), "Created new file");
                Ok(ToolResult::text(format!("Created {}", path.display())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                // Existing binary file — overwrite without diff
                tokio::fs::write(&path, content).await?;
                debug!(path = %path.display(), "Overwrote binary file");
                Ok(ToolResult::text(format!("Wrote {} (binary file, no diff)", path.display())))
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Cannot read existing file for diff");
                Ok(ToolResult::error(format!("Cannot read existing file: {e}")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::tool::Tool;
    use claude_core::permissions::PermissionMode;

    fn test_context() -> ToolContext {
        ToolContext {
            cwd: std::env::temp_dir(),
            permission_mode: PermissionMode::Default,
            abort_signal: Default::default(),
            messages: vec![],
        }
    }

    #[tokio::test]
    async fn write_rejects_oversized_content() {
        let big = "x".repeat(MAX_WRITE_BYTES + 1);
        let input = json!({ "file_path": "/tmp/test_big.txt", "content": big });
        let result = FileWriteTool.call(input, &test_context()).await.unwrap();
        assert!(result.is_error, "should reject oversized content");
    }
}
