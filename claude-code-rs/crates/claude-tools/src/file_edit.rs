use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::diff_ui::print_diff;
use crate::path_util;

// ── File State Cache ─────────────────────────────────────────────────────────

/// Cached state of a file at the time it was last read or edited.
#[derive(Debug, Clone)]
struct FileState {
    /// Content hash (simple hash for comparison).
    content_hash: u64,
    /// Size in bytes.
    size: u64,
}

impl FileState {
    fn from_content(content: &str, path: &std::path::Path) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        let content_hash = hasher.finish();

        let size = std::fs::metadata(path)
            .map(|m| m.len())
            .unwrap_or(0);

        Self { content_hash, size }
    }
}

lazy_static::lazy_static! {
    /// Global file state cache to detect external modifications between edits.
    static ref FILE_STATE_CACHE: Mutex<HashMap<String, FileState>> = Mutex::new(HashMap::new());
}

/// Check if a file has been modified externally since we last saw it.
fn check_external_modification(path: &std::path::Path, content: &str) -> Option<String> {
    let key = path.to_string_lossy().to_string();
    let cache = FILE_STATE_CACHE.lock().ok()?;

    if let Some(cached) = cache.get(&key) {
        let current = FileState::from_content(content, path);
        if current.content_hash != cached.content_hash {
            return Some(format!(
                "⚠️ File has been modified externally since last read/edit (size: {} → {})",
                cached.size, current.size
            ));
        }
    }
    None
}

/// Update the file state cache after a successful read or edit.
fn update_file_state(path: &std::path::Path, content: &str) {
    let key = path.to_string_lossy().to_string();
    if let Ok(mut cache) = FILE_STATE_CACHE.lock() {
        cache.insert(key, FileState::from_content(content, path));
    }
}

/// Count lines added and removed between old and new content.
fn count_line_changes(old: &str, new: &str) -> (usize, usize) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Simple diff: count lines that differ
    let mut added = 0usize;
    let mut removed = 0usize;

    // Use a simple approach: compare line counts + content
    if new_lines.len() > old_lines.len() {
        added += new_lines.len() - old_lines.len();
    } else if old_lines.len() > new_lines.len() {
        removed += old_lines.len() - new_lines.len();
    }

    // Count changed lines in the overlap region
    let overlap = old_lines.len().min(new_lines.len());
    for i in 0..overlap {
        if old_lines[i] != new_lines[i] {
            removed += 1;
            added += 1;
        }
    }

    (added, removed)
}

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str { "Edit" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

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

        if old_string.is_empty() {
            return Ok(ToolResult::error("old_string must not be empty"));
        }

        let path = match path_util::resolve_path(file_path, &context.cwd) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{}", e))),
        };

        let content = tokio::fs::read_to_string(&path).await?;

        // Check for external modifications
        if let Some(warning) = check_external_modification(&path, &content) {
            // Warn but don't block — let the edit proceed
            eprintln!("{}", warning);
        }

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

        // Count line changes
        let (added, removed) = count_line_changes(&content, &new_content);

        // Print colored diff before writing
        print_diff(file_path, &content, &new_content);

        tokio::fs::write(&path, &new_content).await?;

        // Update state cache
        update_file_state(&path, &new_content);

        let mut msg = format!("Edited {}", path.display());
        if added > 0 || removed > 0 {
            msg.push_str(&format!(" (+{} -{} lines)", added, removed));
        }
        Ok(ToolResult::text(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_line_changes() {
        let old = "line1\nline2\nline3";
        let new = "line1\nmodified\nline3\nline4";
        let (added, removed) = count_line_changes(old, new);
        assert!(added >= 1);
        assert!(removed >= 0);
    }

    #[test]
    fn test_file_state_cache() {
        let path = std::path::Path::new("/tmp/test_state_cache.txt");
        let content = "hello world";
        update_file_state(path, content);

        // Same content => no external modification
        assert!(check_external_modification(path, content).is_none());

        // Different content => detected
        assert!(check_external_modification(path, "changed").is_some());
    }
}
