use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::Path;
use crate::path_util;

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str { "Read" }

    fn description(&self) -> &str {
        "Read file contents with optional line range. Also lists directories."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path to read" },
                "offset": { "type": "integer", "description": "Start line (0-indexed)" },
                "limit": { "type": "integer", "description": "Number of lines" }
            },
            "required": ["file_path"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'file_path'"))?;

        let path = match path_util::resolve_path(file_path, &context.cwd) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{}", e))),
        };
        if !path.exists() {
            return Ok(ToolResult::error(format!("File not found: {}", path.display())));
        }
        if path.is_dir() {
            return read_directory(&path).await;
        }

        let content = tokio::fs::read_to_string(&path).await?;
        let lines: Vec<&str> = content.lines().collect();
        let offset = input["offset"].as_u64().unwrap_or(0) as usize;
        let limit = input["limit"].as_u64().map(|l| l as usize);
        let end = limit.map_or(lines.len(), |l| (offset + l).min(lines.len()));

        let selected: Vec<String> = lines[offset.min(lines.len())..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>4}  {}", offset + i + 1, line))
            .collect();

        Ok(ToolResult::text(selected.join("\n")))
    }
}

async fn read_directory(path: &Path) -> anyhow::Result<ToolResult> {
    let mut entries = Vec::new();
    let mut dir = tokio::fs::read_dir(path).await?;
    while let Some(entry) = dir.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type().await?.is_dir() {
            entries.push(format!("  {}/", name));
        } else {
            entries.push(format!("  {}", name));
        }
    }
    entries.sort();
    Ok(ToolResult::text(entries.join("\n")))
}
