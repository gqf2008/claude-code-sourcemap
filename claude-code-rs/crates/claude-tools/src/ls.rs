use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::path_util;

pub struct LsTool;

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str { "LS" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn is_read_only(&self) -> bool { true }

    fn description(&self) -> &str {
        "Lists files and directories in a given path. Use this to explore project structure \
         and discover files. Prefer this over shell 'ls' for directory exploration."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The directory path to list. Relative paths are resolved from the working directory."
                },
                "ignore": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Glob patterns to ignore (e.g. [\"*.log\", \"node_modules\"])"
                }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let raw_path = input["path"].as_str().unwrap_or(".");
        let ignore: Vec<String> = input["ignore"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let dir = match path_util::resolve_path(raw_path, &context.cwd) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{}", e))),
        };

        if !dir.exists() {
            return Ok(ToolResult::error(format!("Path does not exist: {}", dir.display())));
        }
        if !dir.is_dir() {
            return Ok(ToolResult::error(format!("Not a directory: {}", dir.display())));
        }

        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();

            // Apply ignore patterns (simple prefix/suffix matching with *)
            if ignore.iter().any(|pat| glob_match(pat, &name)) {
                continue;
            }

            let meta = entry.metadata().await?;
            let entry_type = if meta.is_dir() { "dir" } else { "file" };
            let size = if meta.is_file() { meta.len() } else { 0 };

            entries.push((name, entry_type, size));
        }

        entries.sort_by(|a, b| {
            // Dirs first, then files, then alphabetically
            match (a.1, b.1) {
                ("dir", "file") => std::cmp::Ordering::Less,
                ("file", "dir") => std::cmp::Ordering::Greater,
                _ => a.0.cmp(&b.0),
            }
        });

        let mut lines = vec![format!("{}:", dir.display())];
        for (name, kind, size) in &entries {
            if *kind == "dir" {
                lines.push(format!("  {}/", name));
            } else {
                lines.push(format!("  {}  ({})", name, human_size(*size)));
            }
        }

        if entries.is_empty() {
            lines.push("  (empty)".to_string());
        }

        Ok(ToolResult::text(lines.join("\n")))
    }
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    match bytes {
        b if b >= GB => format!("{:.1}GB", b as f64 / GB as f64),
        b if b >= MB => format!("{:.1}MB", b as f64 / MB as f64),
        b if b >= KB => format!("{:.1}KB", b as f64 / KB as f64),
        b => format!("{}B", b),
    }
}

/// Minimal glob matching: supports leading/trailing `*` wildcards.
fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" { return true; }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    pattern == name
}
