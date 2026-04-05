use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::Path;
use crate::path_util;

/// Extensions we support reading as base64-encoded images.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg"];

/// Check if the first N bytes look like binary content.
fn is_binary(data: &[u8]) -> bool {
    let check_len = data.len().min(8192);
    let null_count = data[..check_len].iter().filter(|&&b| b == 0).count();
    null_count > 0
}

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str { "Read" }

    fn description(&self) -> &str {
        "Reads a file from the local filesystem. The file_path must be an absolute path. \
         By default reads up to 2000 lines from the beginning. Use offset/limit to read \
         specific portions. Results use cat -n format with line numbers starting at 1. \
         Can read images (PNG, JPG) and Jupyter notebooks (.ipynb). \
         Can only read files, not directories — use Bash ls for directories."
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

        // Check for image files — return base64
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            return read_image(&path, &ext).await;
        }

        // Check for Jupyter notebooks
        if ext == "ipynb" {
            return read_notebook(&path).await;
        }

        // Read raw bytes first to detect binary
        let raw_bytes = tokio::fs::read(&path).await?;
        if is_binary(&raw_bytes) {
            let size = raw_bytes.len();
            let mime = match ext.as_str() {
                "pdf" => "application/pdf",
                "zip" => "application/zip",
                "tar" => "application/x-tar",
                "gz" => "application/gzip",
                "exe" => "application/x-executable",
                "wasm" => "application/wasm",
                _ => "application/octet-stream",
            };
            return Ok(ToolResult::text(format!(
                "Binary file: {} ({}, {} bytes)\nCannot display binary content. \
                 Use appropriate tools to process this file type.",
                path.display(), mime, size
            )));
        }

        let content = String::from_utf8_lossy(&raw_bytes);
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

async fn read_image(path: &Path, ext: &str) -> anyhow::Result<ToolResult> {
    use base64::Engine;
    let data = tokio::fs::read(path).await?;
    let media_type = match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "image/png",
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    Ok(ToolResult::text(format!(
        "[Image: {} ({}, {} bytes)]\nBase64: {}...({} chars total)",
        path.file_name().unwrap_or_default().to_string_lossy(),
        media_type,
        data.len(),
        &b64[..b64.len().min(100)],
        b64.len()
    )))
}

async fn read_notebook(path: &Path) -> anyhow::Result<ToolResult> {
    let content = tokio::fs::read_to_string(path).await?;
    let notebook: Value = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("Invalid notebook JSON: {}", e))?;

    let mut output = String::new();
    output.push_str(&format!("# Notebook: {}\n\n", path.file_name().unwrap_or_default().to_string_lossy()));

    if let Some(cells) = notebook["cells"].as_array() {
        for (i, cell) in cells.iter().enumerate() {
            let cell_type = cell["cell_type"].as_str().unwrap_or("unknown");
            output.push_str(&format!("## Cell {} ({})\n", i + 1, cell_type));

            if let Some(source) = cell["source"].as_array() {
                for line in source {
                    if let Some(s) = line.as_str() {
                        output.push_str(s);
                    }
                }
                output.push('\n');
            }

            // Show outputs for code cells
            if cell_type == "code" {
                if let Some(outputs) = cell["outputs"].as_array() {
                    for out in outputs {
                        if let Some(text) = out["text"].as_array() {
                            output.push_str("### Output:\n");
                            for line in text {
                                if let Some(s) = line.as_str() {
                                    output.push_str(s);
                                }
                            }
                            output.push('\n');
                        }
                    }
                }
            }
            output.push('\n');
        }
    }

    Ok(ToolResult::text(output))
}
