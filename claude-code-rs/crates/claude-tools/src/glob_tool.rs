use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::path_util;

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str { "Glob" }

    fn description(&self) -> &str {
        "Find files matching a glob pattern."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "e.g. **/*.rs" },
                "path": { "type": "string", "description": "Search root (default: cwd)" }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let pattern = input["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'pattern'"))?;
        let search_dir = match input["path"].as_str() {
            Some(p) => match path_util::resolve_path(p, &context.cwd) {
                Ok(resolved) => resolved,
                Err(e) => return Ok(ToolResult::error(format!("{}", e))),
            },
            None => context.cwd.clone(),
        };
        let full = search_dir.join(pattern).to_string_lossy().to_string();
        let mut matches: Vec<String> = Vec::new();
        for entry in glob::glob(&full).map_err(|e| anyhow::anyhow!("Bad glob: {}", e))? {
            if let Ok(path) = entry {
                matches.push(path.display().to_string());
            }
        }
        matches.sort();
        if matches.is_empty() {
            Ok(ToolResult::text("No files matched."))
        } else {
            Ok(ToolResult::text(matches.join("\n")))
        }
    }
}
