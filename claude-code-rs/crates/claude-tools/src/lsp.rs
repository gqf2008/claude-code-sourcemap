//! LSPTool — Language Server Protocol integration for code intelligence.
//!
//! Aligned with TS `LSPTool`. Provides go-to-definition, find-references,
//! hover, and symbol lookup via language server processes.
//!
//! This is a simplified implementation that shells out to ripgrep and
//! uses regex-based symbol extraction as a fallback when no LSP server
//! is available.

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct LspTool;

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str { "LSP" }
    fn category(&self) -> ToolCategory { ToolCategory::Code }

    fn description(&self) -> &str {
        "Interact with language servers for code intelligence. \
         Supports operations: goToDefinition, findReferences, hover, documentSymbol, \
         workspaceSymbol. Requires a language server to be available for the file type."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation", "filePath"],
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["goToDefinition", "findReferences", "hover", "documentSymbol", "workspaceSymbol"],
                    "description": "The LSP operation to perform."
                },
                "filePath": {
                    "type": "string",
                    "description": "Absolute or relative path to the source file."
                },
                "line": {
                    "type": "integer",
                    "description": "1-based line number for position-based operations."
                },
                "character": {
                    "type": "integer",
                    "description": "1-based character offset for position-based operations."
                },
                "query": {
                    "type": "string",
                    "description": "Search query for workspaceSymbol operation."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let cwd = &ctx.cwd;
        let operation = input["operation"].as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'operation' field."))?;
        let file_path = input["filePath"].as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'filePath' field."))?;

        let abs_path = resolve_path(cwd, file_path);
        if !abs_path.exists() {
            return Ok(ToolResult::error(format!("File not found: {}", abs_path.display())));
        }

        let line = input["line"].as_u64().unwrap_or(1) as usize;
        let character = input["character"].as_u64().unwrap_or(1) as usize;

        match operation {
            "documentSymbol" => {
                extract_document_symbols(&abs_path).await
            }
            "workspaceSymbol" => {
                let query = input["query"].as_str().unwrap_or("");
                search_workspace_symbols(cwd, query).await
            }
            "goToDefinition" | "findReferences" | "hover" | "goToImplementation" => {
                let word = get_word_at_position(&abs_path, line, character)?;
                match operation {
                    "goToDefinition" => find_definition(cwd, &word).await,
                    "findReferences" => find_references(cwd, &word).await,
                    "hover" => get_hover_info(&abs_path, line, &word),
                    _ => Ok(ToolResult::error(format!("Operation '{}' not yet supported.", operation))),
                }
            }
            _ => Ok(ToolResult::error(format!(
                "Unknown operation: '{}'. Supported: goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol",
                operation
            ))),
        }
    }
}

fn resolve_path(cwd: &Path, file_path: &str) -> PathBuf {
    let p = Path::new(file_path);
    if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
}

fn get_word_at_position(path: &Path, line: usize, character: usize) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(path)?;

    let target_line = content.lines().nth(line.saturating_sub(1))
        .ok_or_else(|| anyhow::anyhow!("Line {} out of range", line))?;

    let col = character.saturating_sub(1).min(target_line.len());
    let chars: Vec<char> = target_line.chars().collect();

    let mut start = col;
    while start > 0 && is_identifier_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_identifier_char(chars[end]) {
        end += 1;
    }

    if start == end {
        anyhow::bail!("No identifier at line {}, character {}", line, character);
    }

    Ok(chars[start..end].iter().collect())
}

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Extract document symbols using simple regex-based analysis.
async fn extract_document_symbols(path: &Path) -> anyhow::Result<ToolResult> {
    let content = std::fs::read_to_string(path)?;

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut symbols = Vec::new();

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let symbol = match ext {
            "rs" => extract_rust_symbol(trimmed),
            "ts" | "tsx" | "js" | "jsx" => extract_ts_symbol(trimmed),
            "py" => extract_py_symbol(trimmed),
            "go" => extract_go_symbol(trimmed),
            "java" | "kt" => extract_java_symbol(trimmed),
            _ => extract_generic_symbol(trimmed),
        };
        if let Some((kind, name)) = symbol {
            symbols.push(format!("  L{}: {} {}", i + 1, kind, name));
        }
    }

    if symbols.is_empty() {
        Ok(ToolResult::text("No symbols found in file."))
    } else {
        Ok(ToolResult::text(format!("Symbols in {}:\n{}", path.display(), symbols.join("\n"))))
    }
}

fn extract_rust_symbol(line: &str) -> Option<(&'static str, String)> {
    if line.starts_with("pub fn ") || line.starts_with("fn ") || line.starts_with("pub(crate) fn ") {
        let name = line.split("fn ").nth(1)?.split('(').next()?.trim().to_string();
        Some(("fn", name))
    } else if line.starts_with("pub struct ") || line.starts_with("struct ") {
        let name = line.split("struct ").nth(1)?.split([' ', '{', '(']).next()?.trim().to_string();
        Some(("struct", name))
    } else if line.starts_with("pub enum ") || line.starts_with("enum ") {
        let name = line.split("enum ").nth(1)?.split([' ', '{']).next()?.trim().to_string();
        Some(("enum", name))
    } else if line.starts_with("pub trait ") || line.starts_with("trait ") {
        let name = line.split("trait ").nth(1)?.split([' ', '{', ':']).next()?.trim().to_string();
        Some(("trait", name))
    } else if line.starts_with("impl ") {
        let rest = line.strip_prefix("impl ")?;
        let name = rest.split([' ', '{']).next()?.trim().to_string();
        Some(("impl", name))
    } else if line.starts_with("pub const ") || line.starts_with("const ") {
        let name = line.split("const ").nth(1)?.split([' ', ':']).next()?.trim().to_string();
        Some(("const", name))
    } else if line.starts_with("pub type ") || line.starts_with("type ") {
        let name = line.split("type ").nth(1)?.split([' ', '=']).next()?.trim().to_string();
        Some(("type", name))
    } else {
        None
    }
}

fn extract_ts_symbol(line: &str) -> Option<(&'static str, String)> {
    let stripped = line.strip_prefix("export ").unwrap_or(line);
    let stripped = stripped.strip_prefix("default ").unwrap_or(stripped);
    let stripped = stripped.strip_prefix("async ").unwrap_or(stripped);
    let stripped = stripped.strip_prefix("declare ").unwrap_or(stripped);

    if stripped.starts_with("function ") {
        let name = stripped.strip_prefix("function ")?.split(['(', '<']).next()?.trim().to_string();
        Some(("function", name))
    } else if stripped.starts_with("class ") {
        let name = stripped.strip_prefix("class ")?.split([' ', '{', '<']).next()?.trim().to_string();
        Some(("class", name))
    } else if stripped.starts_with("interface ") {
        let name = stripped.strip_prefix("interface ")?.split([' ', '{', '<']).next()?.trim().to_string();
        Some(("interface", name))
    } else if stripped.starts_with("type ") {
        let name = stripped.strip_prefix("type ")?.split([' ', '=', '<']).next()?.trim().to_string();
        Some(("type", name))
    } else if stripped.starts_with("enum ") {
        let name = stripped.strip_prefix("enum ")?.split([' ', '{']).next()?.trim().to_string();
        Some(("enum", name))
    } else if stripped.starts_with("const ") || stripped.starts_with("let ") || stripped.starts_with("var ") {
        let rest = stripped.splitn(2, ' ').nth(1)?;
        let name = rest.split([' ', ':', '=']).next()?.trim().to_string();
        if name.len() > 1 { Some(("variable", name)) } else { None }
    } else {
        None
    }
}

fn extract_py_symbol(line: &str) -> Option<(&'static str, String)> {
    if line.starts_with("def ") || line.starts_with("async def ") {
        let name = line.split("def ").nth(1)?.split('(').next()?.trim().to_string();
        Some(("def", name))
    } else if line.starts_with("class ") {
        let name = line.strip_prefix("class ")?.split(['(', ':']).next()?.trim().to_string();
        Some(("class", name))
    } else {
        None
    }
}

fn extract_go_symbol(line: &str) -> Option<(&'static str, String)> {
    if line.starts_with("func ") {
        let rest = line.strip_prefix("func ")?;
        let name = if rest.starts_with('(') {
            // Method: func (r *Receiver) Name(...)
            rest.split(')').nth(1)?.trim().split('(').next()?.trim().to_string()
        } else {
            rest.split('(').next()?.trim().to_string()
        };
        Some(("func", name))
    } else if line.starts_with("type ") {
        let rest = line.strip_prefix("type ")?;
        let name = rest.split(' ').next()?.trim().to_string();
        Some(("type", name))
    } else {
        None
    }
}

fn extract_java_symbol(line: &str) -> Option<(&'static str, String)> {
    // Remove access modifiers
    let stripped = line
        .replace("public ", "").replace("private ", "").replace("protected ", "")
        .replace("static ", "").replace("final ", "").replace("abstract ", "");
    let stripped = stripped.trim();

    if stripped.starts_with("class ") {
        let name = stripped.strip_prefix("class ")?.split([' ', '{', '<']).next()?.trim().to_string();
        Some(("class", name))
    } else if stripped.starts_with("interface ") {
        let name = stripped.strip_prefix("interface ")?.split([' ', '{', '<']).next()?.trim().to_string();
        Some(("interface", name))
    } else if stripped.contains('(') && !stripped.starts_with("if ") && !stripped.starts_with("for ") {
        // Likely a method
        let before_paren = stripped.split('(').next()?;
        let parts: Vec<&str> = before_paren.split_whitespace().collect();
        if parts.len() >= 2 {
            let name = parts.last()?.to_string();
            Some(("method", name))
        } else {
            None
        }
    } else {
        None
    }
}

fn extract_generic_symbol(line: &str) -> Option<(&'static str, String)> {
    if line.starts_with("function ") || line.starts_with("def ") || line.starts_with("fn ") {
        let word = line.split_whitespace().nth(1)?.split('(').next()?.trim().to_string();
        Some(("function", word))
    } else {
        None
    }
}

/// Search workspace symbols via ripgrep.
async fn search_workspace_symbols(cwd: &Path, query: &str) -> anyhow::Result<ToolResult> {
    if query.is_empty() {
        return Ok(ToolResult::error("'query' is required for workspaceSymbol operation."));
    }

    let pattern = format!(r"(fn|function|def|class|struct|enum|trait|interface|type)\s+{}", regex::escape(query));
    let output = tokio::process::Command::new("rg")
        .args(["--no-heading", "--line-number", "--max-count", "30", "-e", &pattern])
        .current_dir(cwd)
        .output()
        .await;

    match output {
        Ok(out) => {
            let result = String::from_utf8_lossy(&out.stdout);
            if result.is_empty() {
                Ok(ToolResult::text(format!("No symbols matching '{}' found.", query)))
            } else {
                Ok(ToolResult::text(format!("Symbols matching '{}':\n{}", query, result.trim())))
            }
        }
        Err(_) => {
            Ok(ToolResult::error("ripgrep (rg) not found. Install ripgrep for workspace symbol search."))
        }
    }
}

/// Find definition of a symbol using ripgrep.
async fn find_definition(cwd: &Path, word: &str) -> anyhow::Result<ToolResult> {
    let patterns = [
        format!(r"(fn|function|def|class|struct|enum|trait|interface|type)\s+{}\b", regex::escape(word)),
        format!(r"(const|let|var)\s+{}\s*[:=]", regex::escape(word)),
    ];

    let mut results = Vec::new();
    for pattern in &patterns {
        let output = tokio::process::Command::new("rg")
            .args(["--no-heading", "--line-number", "--max-count", "10", "-e", pattern])
            .current_dir(cwd)
            .output()
            .await;

        if let Ok(out) = output {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if !line.is_empty() {
                    results.push(line.to_string());
                }
            }
        }
    }

    if results.is_empty() {
        Ok(ToolResult::text(format!("No definition found for '{}'.", word)))
    } else {
        results.truncate(20);
        Ok(ToolResult::text(format!("Possible definitions of '{}':\n{}", word, results.join("\n"))))
    }
}

/// Find references to a symbol using ripgrep.
async fn find_references(cwd: &Path, word: &str) -> anyhow::Result<ToolResult> {
    let output = tokio::process::Command::new("rg")
        .args(["--no-heading", "--line-number", "--max-count", "50", "-w", word])
        .current_dir(cwd)
        .output()
        .await?;

    let text = String::from_utf8_lossy(&output.stdout);
    let count = text.lines().count();

    if count == 0 {
        Ok(ToolResult::text(format!("No references found for '{}'.", word)))
    } else {
        Ok(ToolResult::text(format!("References to '{}' ({} found):\n{}", word, count, text.trim())))
    }
}

/// Get hover-like info by reading surrounding context.
fn get_hover_info(path: &Path, line: usize, word: &str) -> anyhow::Result<ToolResult> {
    let content = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();
    let target_idx = line.saturating_sub(1);

    let start = target_idx.saturating_sub(5);
    let end = (target_idx + 4).min(lines.len());

    let mut context_lines = Vec::new();
    for i in start..end {
        let marker = if i == target_idx { "→" } else { " " };
        context_lines.push(format!("{} {:>4} │ {}", marker, i + 1, lines[i]));
    }

    Ok(ToolResult::text(format!(
        "Hover info for '{}' at {}:{}:\n{}",
        word, path.display(), line, context_lines.join("\n")
    )))
}
