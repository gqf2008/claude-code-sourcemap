use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::Path;
use tracing::{debug, warn};
use crate::path_util;

/// Extensions we support reading as base64-encoded images.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg"];

/// Maximum file size we'll read into memory (50 MB).
const MAX_READ_BYTES: u64 = 50 * 1024 * 1024;

/// Device files that would hang or cause issues when read.
const BLOCKED_DEVICE_PATHS: &[&str] = &[
    "/dev/zero", "/dev/random", "/dev/urandom", "/dev/null",
    "/dev/stdin", "/dev/stdout", "/dev/stderr",
    "/dev/fd/", "/proc/kcore",
];

/// Check if the first N bytes look like binary content.
fn is_binary(data: &[u8]) -> bool {
    let check_len = data.len().min(8192);
    let null_count = data[..check_len].iter().filter(|&&b| b == 0).count();
    null_count > 0
}

/// Find similar file names in the same directory (for suggestions on not-found).
fn find_similar_files(path: &Path, max_suggestions: usize) -> Vec<String> {
    let parent = match path.parent() {
        Some(p) if p.is_dir() => p,
        _ => return Vec::new(),
    };
    let target_name = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    if target_name.is_empty() { return Vec::new(); }

    let mut candidates: Vec<(String, usize)> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let name_lower = name.to_lowercase();

            // Simple similarity: count matching chars or check prefix/suffix
            let score = similarity_score(&target_name, &name_lower);
            if score > 0 {
                candidates.push((name, score));
            }
        }
    }

    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.iter()
        .take(max_suggestions)
        .map(|(name, _)| name.clone())
        .collect()
}

/// Simple string similarity score based on common subsequences.
fn similarity_score(a: &str, b: &str) -> usize {
    if a == b { return 100; }

    let mut score = 0;

    // Prefix match bonus
    let prefix_len = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    score += prefix_len * 3;

    // Extension match bonus
    let ext_a = a.rsplit('.').next().unwrap_or("");
    let ext_b = b.rsplit('.').next().unwrap_or("");
    if !ext_a.is_empty() && ext_a == ext_b {
        score += 5;
    }

    // Stem match — base name without extension
    let stem_a = a.rsplit('.').next_back().unwrap_or(a);
    let stem_b = b.rsplit('.').next_back().unwrap_or(b);
    if stem_a == stem_b {
        score += 10;
    }

    // Contains bonus
    if b.contains(a) || a.contains(b) {
        score += 8;
    }

    // Levenshtein-like: penalize only if reasonably close
    if a.len().abs_diff(b.len()) <= 3 {
        let common = a.chars().filter(|c| b.contains(*c)).count();
        score += common;
    }

    score
}

/// Format file modification time as a human-readable string.
fn format_mtime(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let datetime: chrono::DateTime<chrono::Utc> = modified.into();
    Some(datetime.format("%Y-%m-%d %H:%M:%S UTC").to_string())
}

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str { "Read" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

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
                "file_path": { "type": "string", "description": "Absolute path to read" },
                "offset": { "type": "integer", "description": "Start line (0-indexed)" },
                "limit": { "type": "integer", "description": "Number of lines to read" }
            },
            "required": ["file_path"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'file_path'"))?;

        // Block device files that would hang
        for blocked in BLOCKED_DEVICE_PATHS {
            if file_path.starts_with(blocked) {
                return Ok(ToolResult::error(format!(
                    "Cannot read device file: {} — this would hang or produce infinite output", file_path
                )));
            }
        }

        let path = match path_util::resolve_path(file_path, &context.cwd) {
            Ok(p) => p,
            Err(e) => {
                warn!(file_path, error = %e, "Path resolution rejected");
                return Ok(ToolResult::error(format!("{}", e)));
            }
        };
        if !path.exists() {
            debug!(path = %path.display(), "File not found");
            // Try to suggest similar files
            let suggestions = find_similar_files(&path, 5);
            let mut msg = format!("File not found: {}", path.display());
            if !suggestions.is_empty() {
                msg.push_str("\n\nDid you mean one of these?");
                for s in &suggestions {
                    msg.push_str(&format!("\n  - {}", path.parent().unwrap_or(Path::new("")).join(s).display()));
                }
            }
            return Ok(ToolResult::error(msg));
        }
        if path.is_dir() {
            return read_directory(&path).await;
        }

        // Check for image files — return base64
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        // Check file size before reading into memory (applies to ALL file types)
        if let Ok(meta) = tokio::fs::metadata(&path).await {
            if meta.len() > MAX_READ_BYTES {
                return Ok(ToolResult::error(format!(
                    "File too large: {} ({} bytes, limit is {} MB). \
                     Use offset/limit to read specific portions.",
                    path.display(), meta.len(), MAX_READ_BYTES / 1024 / 1024
                )));
            }
        }

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
        let total_lines = lines.len();
        let offset = input["offset"].as_u64().unwrap_or(0) as usize;
        let limit = input["limit"].as_u64().map(|l| l as usize);
        let end = limit.map_or(lines.len().min(offset + 2000), |l| (offset + l).min(lines.len()));

        let selected: Vec<String> = lines[offset.min(lines.len())..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>4}  {}", offset + i + 1, line))
            .collect();

        // Add file metadata header
        let mtime = format_mtime(&path).unwrap_or_default();
        let mut header = format!("File: {} ({} lines", path.display(), total_lines);
        if !mtime.is_empty() {
            header.push_str(&format!(", modified {}", mtime));
        }
        header.push(')');
        if end < total_lines {
            header.push_str(&format!("\nShowing lines {}-{} of {}", offset + 1, end, total_lines));
        }

        Ok(ToolResult::text(format!("{}\n{}", header, selected.join("\n"))))
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_binary ───────────────────────────────────────────────────────

    #[test]
    fn test_is_binary_utf8_text() {
        assert!(!is_binary(b"hello world"));
    }

    #[test]
    fn test_is_binary_with_null_byte() {
        assert!(is_binary(b"hello\x00world"));
    }

    #[test]
    fn test_is_binary_empty() {
        assert!(!is_binary(b""));
    }

    #[test]
    fn test_is_binary_pure_binary() {
        assert!(is_binary(&[0u8; 100]));
    }

    // ── similarity_score ────────────────────────────────────────────────

    #[test]
    fn test_similarity_exact_match() {
        assert_eq!(similarity_score("foo.rs", "foo.rs"), 100);
    }

    #[test]
    fn test_similarity_same_extension() {
        let score = similarity_score("bar.rs", "baz.rs");
        assert!(score > 0, "same extension should give non-zero score, got {}", score);
    }

    #[test]
    fn test_similarity_contains() {
        let score = similarity_score("main", "main.rs");
        assert!(score > 5, "contains should give high score, got {}", score);
    }

    #[test]
    fn test_similarity_totally_different() {
        let score = similarity_score("xyz", "abc");
        // No prefix, no extension match, no stem match, no contains
        assert!(score <= 5, "totally different should give very low score, got {}", score);
    }

    #[test]
    fn test_similarity_same_stem() {
        let score = similarity_score("foo.rs", "foo.ts");
        assert!(score > 0, "same stem should give non-zero score, got {}", score);
    }
}
