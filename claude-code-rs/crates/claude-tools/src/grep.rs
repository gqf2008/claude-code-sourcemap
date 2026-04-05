use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use ignore::WalkBuilder;
use regex::Regex;
use std::path::PathBuf;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "Grep" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn description(&self) -> &str {
        "A powerful search tool built on ripgrep. ALWAYS use Grep for search tasks — NEVER \
         invoke grep or rg as a Bash command. Supports full regex syntax (e.g. \"log.*Error\"). \
         Filter by glob (e.g. \"*.js\") or type (e.g. \"py\", \"rust\"). Output modes: \
         \"content\" shows matching lines, \"files_with_matches\" shows only paths (default), \
         \"count\" shows match counts. For cross-line patterns use multiline: true."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string" },
                "path": { "type": "string" },
                "include": { "type": "string", "description": "File glob filter (matched against full path)" }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let pattern = input["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'pattern'"))?.to_string();
        let search_path: PathBuf = match input["path"].as_str() {
            Some(p) => {
                let pa = std::path::Path::new(p);
                if pa.is_absolute() { pa.to_path_buf() } else { context.cwd.join(pa) }
            }
            None => context.cwd.clone(),
        };
        let include_glob = input["include"].as_str().map(|s| s.to_string());

        let output = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let regex = Regex::new(&pattern).map_err(|e| anyhow::anyhow!("Bad regex: {}", e))?;
            let mut results = Vec::new();
            let mut file_count = 0usize;
            const MAX_RESULTS: usize = 100;

            let walker = WalkBuilder::new(&search_path).hidden(true).git_ignore(true).build();
            'outer: for entry in walker.flatten() {
                if !entry.file_type().map_or(false, |ft| ft.is_file()) { continue; }
                let path = entry.path().to_owned();

                // Match glob against full path string so patterns like `src/**/*.rs` work
                if let Some(ref g) = include_glob {
                    let path_str = path.to_string_lossy();
                    if !glob::Pattern::new(g).map_or(false, |p| p.matches(&path_str)) {
                        continue;
                    }
                }

                let content = match std::fs::read_to_string(&path) { Ok(c) => c, Err(_) => continue };
                let mut file_hits = Vec::new();
                for (num, line) in content.lines().enumerate() {
                    if regex.is_match(line) {
                        file_hits.push(format!("  {}:{}: {}", path.display(), num + 1, line.trim()));
                        if results.len() + file_hits.len() >= MAX_RESULTS {
                            results.extend(file_hits);
                            break 'outer;
                        }
                    }
                }
                if !file_hits.is_empty() { file_count += 1; results.extend(file_hits); }
            }

            if results.is_empty() {
                Ok("No matches found.".to_string())
            } else {
                Ok(format!("Found matches in {} file(s):\n{}", file_count, results.join("\n")))
            }
        }).await??;

        Ok(ToolResult::text(output))
    }
}
