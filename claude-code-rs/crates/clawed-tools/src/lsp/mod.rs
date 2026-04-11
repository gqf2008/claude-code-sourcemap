//! `LSPTool` — Language Server Protocol integration for code intelligence.
//!
//! Aligned with TS `LSPTool`. Provides go-to-definition, find-references,
//! hover, and symbol lookup via language server processes.
//!
//! When an LSP server is configured (via `CLAUDE_LSP_<LANG>` env vars or
//! `~/.claude/settings.json`), true JSON-RPC protocol is used.
//! Falls back to ripgrep/regex-based symbol extraction otherwise.

pub mod client;
pub mod config;
pub mod transport;

use async_trait::async_trait;
use clawed_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct LspTool;

/// Default timeout for ripgrep searches (10 seconds).
const RG_TIMEOUT: Duration = Duration::from_secs(10);

/// Run ripgrep with a timeout. Returns stdout on success, or an error `ToolResult`.
async fn run_rg(cwd: &Path, args: &[&str]) -> Result<String, ToolResult> {
    let output = tokio::time::timeout(
        RG_TIMEOUT,
        tokio::process::Command::new("rg")
            .args(args)
            .current_dir(cwd)
            .output()
    ).await;

    match output {
        Ok(Ok(out)) => Ok(String::from_utf8_lossy(&out.stdout).to_string()),
        Ok(Err(_)) => Err(ToolResult::error(
            "ripgrep (rg) not found. Install ripgrep for code intelligence."
        )),
        Err(_) => Err(ToolResult::error(format!(
            "Search timed out after {}s. Try a more specific query.",
            RG_TIMEOUT.as_secs()
        ))),
    }
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &'static str { "LSP" }
    fn category(&self) -> ToolCategory { ToolCategory::Code }

    fn description(&self) -> &'static str {
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

        // Try real LSP client first if a server is configured for this file type.
        let lsp_configs = config::load_lsp_configs(cwd);
        if let Some((_name, server_cfg)) = config::find_server_for_file(&lsp_configs, &abs_path) {
            let op = operation.to_string();
            let path_clone = abs_path.clone();
            let cwd_clone = cwd.clone();
            let query = input["query"].as_str().unwrap_or("").to_string();
            let server_cfg = server_cfg.clone();
            let result = tokio::task::spawn_blocking(move || {
                try_lsp_client(server_cfg, &op, &path_clone, &cwd_clone, line, character, &query)
            }).await;

            match result {
                Ok(Ok(tr)) => return Ok(tr),
                Ok(Err(e)) => {
                    tracing::warn!("LSP client failed for {operation}, falling back to ripgrep: {e:#}");
                }
                Err(e) => {
                    tracing::warn!("LSP task panicked, falling back to ripgrep: {e}");
                }
            }
        }

        // Fallback: ripgrep / regex-based implementation.
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
                    "goToImplementation" => find_implementations(cwd, &word).await,
                    _ => Ok(ToolResult::error(format!("Operation '{operation}' not yet supported."))),
                }
            }
            _ => Ok(ToolResult::error(format!(
                "Unknown operation: '{operation}'. Supported: goToDefinition, goToImplementation, findReferences, hover, documentSymbol, workspaceSymbol"
            ))),
        }
    }
}

/// Attempt to use a real LSP server for the given operation.
/// Runs synchronously (intended for `spawn_blocking`).
fn try_lsp_client(
    server_cfg: config::LspServerConfig,
    operation: &str,
    abs_path: &Path,
    cwd: &Path,
    line: usize,
    character: usize,
    query: &str,
) -> anyhow::Result<ToolResult> {
    use client::LspClient;
    let mut lsp = LspClient::start(&server_cfg, cwd)?;

    // line/character are 1-based from user input; LSP uses 0-based.
    let lsp_line = line.saturating_sub(1) as u32;
    let lsp_char = character.saturating_sub(1) as u32;

    let lang_id = config::language_id_for_path(abs_path);
    lsp.open_file(abs_path, lang_id)?;

    let result = match operation {
        "goToDefinition" => {
            let locs = lsp.go_to_definition(abs_path, lsp_line, lsp_char)?;
            if locs.is_empty() {
                ToolResult::text("No definition found.")
            } else {
                let lines: Vec<String> = locs.iter()
                    .map(|l| format!("{}:{}", l.file_path, l.line + 1))
                    .collect();
                ToolResult::text(format!("Definition(s):\n{}", lines.join("\n")))
            }
        }
        "findReferences" => {
            let locs = lsp.find_references(abs_path, lsp_line, lsp_char)?;
            if locs.is_empty() {
                ToolResult::text("No references found.")
            } else {
                let lines: Vec<String> = locs.iter()
                    .map(|l| format!("{}:{}", l.file_path, l.line + 1))
                    .collect();
                ToolResult::text(format!("References ({}):\n{}", locs.len(), lines.join("\n")))
            }
        }
        "hover" => {
            match lsp.hover(abs_path, lsp_line, lsp_char)? {
                Some(text) => ToolResult::text(format!("Hover info:\n{text}")),
                None => ToolResult::text("No hover information available."),
            }
        }
        "documentSymbol" => {
            let syms = lsp.document_symbols(abs_path)?;
            if syms.is_empty() {
                ToolResult::text("No symbols found.")
            } else {
                let lines: Vec<String> = syms.iter()
                    .map(|s| format!("  L{}: {} {}", s.line + 1, s.kind, s.name))
                    .collect();
                ToolResult::text(format!("Symbols in {}:\n{}", abs_path.display(), lines.join("\n")))
            }
        }
        "workspaceSymbol" => {
            let syms = lsp.workspace_symbols(query)?;
            if syms.is_empty() {
                ToolResult::text(format!("No symbols matching '{query}'."))
            } else {
                let lines: Vec<String> = syms.iter()
                    .map(|s| {
                        let loc = s.file_path.as_deref().unwrap_or("?");
                        format!("  {} {} ({}:{})", s.kind, s.name, loc, s.line + 1)
                    })
                    .collect();
                ToolResult::text(format!("Workspace symbols matching '{query}':\n{}", lines.join("\n")))
            }
        }
        other => return Err(anyhow::anyhow!("Unsupported operation for LSP: {other}")),
    };

    lsp.shutdown(); // takes ownership
    Ok(result)
}

fn resolve_path(cwd: &Path, file_path: &str) -> PathBuf {
    let p = Path::new(file_path);
    if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
}

fn get_word_at_position(path: &Path, line: usize, character: usize) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(path)?;

    let target_line = content.lines().nth(line.saturating_sub(1))
        .ok_or_else(|| anyhow::anyhow!("Line {line} out of range"))?;

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
        anyhow::bail!("No identifier at line {line}, character {character}");
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
        let rest = stripped.split_once(' ')?.1;
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
    let result = match run_rg(cwd, &["--no-heading", "--line-number", "--max-count", "30", "-e", &pattern]).await {
        Ok(text) => text,
        Err(e) => return Ok(e),
    };

    if result.is_empty() {
        Ok(ToolResult::text(format!("No symbols matching '{query}' found.")))
    } else {
        Ok(ToolResult::text(format!("Symbols matching '{}':\n{}", query, result.trim())))
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
        let text = match run_rg(cwd, &["--no-heading", "--line-number", "--max-count", "10", "-e", pattern]).await {
            Ok(t) => t,
            Err(e) => return Ok(e),
        };
        for line in text.lines() {
            if !line.is_empty() {
                results.push(line.to_string());
            }
        }
    }

    if results.is_empty() {
        Ok(ToolResult::text(format!("No definition found for '{word}'.")))
    } else {
        results.truncate(20);
        Ok(ToolResult::text(format!("Possible definitions of '{}':\n{}", word, results.join("\n"))))
    }
}

/// Find references to a symbol using ripgrep.
async fn find_references(cwd: &Path, word: &str) -> anyhow::Result<ToolResult> {
    let text = match run_rg(cwd, &["--no-heading", "--line-number", "--max-count", "50", "-w", word]).await {
        Ok(t) => t,
        Err(e) => return Ok(e),
    };
    let count = text.lines().count();

    if count == 0 {
        Ok(ToolResult::text(format!("No references found for '{word}'.")))
    } else {
        Ok(ToolResult::text(format!("References to '{}' ({} found):\n{}", word, count, text.trim())))
    }
}

/// Find implementations of a trait/interface/class using ripgrep patterns.
async fn find_implementations(cwd: &Path, word: &str) -> anyhow::Result<ToolResult> {
    let escaped = regex::escape(word);
    let patterns = [
        // Rust: impl Trait for Type, impl Type
        format!(r"impl\s+({escaped}\s+for\s+\w+|\w+<[^>]*>\s+for\s+\w+|{escaped})\b"),
        // TS/Java: class X implements Y, class X extends Y
        format!(r"class\s+\w+\s+(implements|extends)\s+.*\b{escaped}\b"),
        // Python: class X(Y)
        format!(r"class\s+\w+\([^)]*\b{escaped}\b"),
        // Go: func (r *Type) MethodName
        format!(r"func\s+\([^)]+\)\s+{escaped}\b"),
    ];

    let mut results = Vec::new();
    for pattern in &patterns {
        let text = match run_rg(cwd, &["--no-heading", "--line-number", "--max-count", "30", "-e", pattern]).await {
            Ok(t) => t,
            Err(e) => return Ok(e),
        };
        for line in text.lines() {
            if !line.is_empty() && !results.contains(&line.to_string()) {
                results.push(line.to_string());
            }
        }
    }

    if results.is_empty() {
        Ok(ToolResult::text(format!("No implementations found for '{word}'.")))
    } else {
        Ok(ToolResult::text(format!(
            "Implementations of '{}' ({} found):\n{}",
            word,
            results.len(),
            results.join("\n")
        )))
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
    for (idx, line) in lines[start..end].iter().enumerate() {
        let i = start + idx;
        let marker = if i == target_idx { "→" } else { " " };
        context_lines.push(format!("{} {:>4} │ {}", marker, i + 1, line));
    }

    Ok(ToolResult::text(format!(
        "Hover info for '{}' at {}:{}:\n{}",
        word, path.display(), line, context_lines.join("\n")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── is_identifier_char ──────────────────────────────────────────

    #[test]
    fn identifier_char_alpha() {
        assert!(is_identifier_char('a'));
        assert!(is_identifier_char('Z'));
        assert!(is_identifier_char('0'));
        assert!(is_identifier_char('9'));
    }

    #[test]
    fn identifier_char_underscore() {
        assert!(is_identifier_char('_'));
    }

    #[test]
    fn identifier_char_special_false() {
        assert!(!is_identifier_char(' '));
        assert!(!is_identifier_char('-'));
        assert!(!is_identifier_char('('));
        assert!(!is_identifier_char('.'));
        assert!(!is_identifier_char(':'));
        assert!(!is_identifier_char('<'));
    }

    // ── resolve_path ────────────────────────────────────────────────

    #[test]
    fn resolve_absolute() {
        let cwd = Path::new(if cfg!(windows) { "C:\\projects" } else { "/home/user/projects" });
        let abs = if cfg!(windows) { "C:\\tmp\\file.rs" } else { "/tmp/file.rs" };
        let result = resolve_path(cwd, abs);
        assert_eq!(result, PathBuf::from(abs));
    }

    #[test]
    fn resolve_relative() {
        let cwd = Path::new(if cfg!(windows) { "C:\\projects" } else { "/home/user/projects" });
        let result = resolve_path(cwd, "src/main.rs");
        assert_eq!(result, cwd.join("src/main.rs"));
    }

    #[test]
    fn resolve_relative_with_dots() {
        let cwd = Path::new(if cfg!(windows) { "C:\\projects" } else { "/home/user/projects" });
        let result = resolve_path(cwd, "../other/file.rs");
        assert_eq!(result, cwd.join("../other/file.rs"));
    }

    // ── extract_rust_symbol ─────────────────────────────────────────

    #[test]
    fn rust_pub_fn() {
        assert_eq!(
            extract_rust_symbol("pub fn handle_request(ctx: &Context) -> Result<()> {"),
            Some(("fn", "handle_request".into()))
        );
    }

    #[test]
    fn rust_private_fn() {
        assert_eq!(
            extract_rust_symbol("fn helper() {"),
            Some(("fn", "helper".into()))
        );
    }

    #[test]
    fn rust_pub_crate_fn() {
        assert_eq!(
            extract_rust_symbol("pub(crate) fn internal_fn(x: i32) {"),
            Some(("fn", "internal_fn".into()))
        );
    }

    #[test]
    fn rust_struct() {
        assert_eq!(
            extract_rust_symbol("struct Config {"),
            Some(("struct", "Config".into()))
        );
    }

    #[test]
    fn rust_pub_enum() {
        assert_eq!(
            extract_rust_symbol("pub enum Direction {"),
            Some(("enum", "Direction".into()))
        );
    }

    #[test]
    fn rust_trait() {
        assert_eq!(
            extract_rust_symbol("trait MyTrait: Send + Sync {"),
            Some(("trait", "MyTrait".into()))
        );
    }

    #[test]
    fn rust_impl() {
        assert_eq!(
            extract_rust_symbol("impl Foo {"),
            Some(("impl", "Foo".into()))
        );
    }

    #[test]
    fn rust_const() {
        assert_eq!(
            extract_rust_symbol("pub const MAX_SIZE: usize = 1024;"),
            Some(("const", "MAX_SIZE".into()))
        );
    }

    #[test]
    fn rust_type() {
        assert_eq!(
            extract_rust_symbol("type Alias = Vec<String>;"),
            Some(("type", "Alias".into()))
        );
    }

    #[test]
    fn rust_no_match() {
        assert_eq!(extract_rust_symbol("let x = 42;"), None);
        assert_eq!(extract_rust_symbol("// pub fn commented()"), None);
        assert_eq!(extract_rust_symbol("println!(\"hello\");"), None);
    }

    // ── extract_ts_symbol ───────────────────────────────────────────

    #[test]
    fn ts_function() {
        assert_eq!(
            extract_ts_symbol("function greet(name: string) {"),
            Some(("function", "greet".into()))
        );
    }

    #[test]
    fn ts_export_async_function() {
        assert_eq!(
            extract_ts_symbol("export async function fetchData() {"),
            Some(("function", "fetchData".into()))
        );
    }

    #[test]
    fn ts_export_class() {
        assert_eq!(
            extract_ts_symbol("export class MyService {"),
            Some(("class", "MyService".into()))
        );
    }

    #[test]
    fn ts_interface() {
        assert_eq!(
            extract_ts_symbol("interface IConfig {"),
            Some(("interface", "IConfig".into()))
        );
    }

    #[test]
    fn ts_declare_type() {
        assert_eq!(
            extract_ts_symbol("declare type Alias = string;"),
            Some(("type", "Alias".into()))
        );
    }

    #[test]
    fn ts_enum() {
        assert_eq!(
            extract_ts_symbol("enum Direction {"),
            Some(("enum", "Direction".into()))
        );
    }

    #[test]
    fn ts_const() {
        assert_eq!(
            extract_ts_symbol("export const API_URL = 'https://example.com';"),
            Some(("variable", "API_URL".into()))
        );
    }

    #[test]
    fn ts_no_match() {
        assert_eq!(extract_ts_symbol("console.log('hello');"), None);
        assert_eq!(extract_ts_symbol("import { X } from 'y';"), None);
    }

    // ── extract_py_symbol ───────────────────────────────────────────

    #[test]
    fn py_def() {
        assert_eq!(
            extract_py_symbol("def process(data):"),
            Some(("def", "process".into()))
        );
    }

    #[test]
    fn py_async_def() {
        assert_eq!(
            extract_py_symbol("async def fetch_data(url):"),
            Some(("def", "fetch_data".into()))
        );
    }

    #[test]
    fn py_class() {
        assert_eq!(
            extract_py_symbol("class MyModel(BaseModel):"),
            Some(("class", "MyModel".into()))
        );
    }

    #[test]
    fn py_no_match() {
        assert_eq!(extract_py_symbol("x = 42"), None);
        assert_eq!(extract_py_symbol("    def indented(self):"), None);
        assert_eq!(extract_py_symbol("import os"), None);
    }

    // ── extract_go_symbol ───────────────────────────────────────────

    #[test]
    fn go_func() {
        assert_eq!(
            extract_go_symbol("func HandleRequest(w http.ResponseWriter, r *http.Request) {"),
            Some(("func", "HandleRequest".into()))
        );
    }

    #[test]
    fn go_method() {
        assert_eq!(
            extract_go_symbol("func (s *Server) Start() error {"),
            Some(("func", "Start".into()))
        );
    }

    #[test]
    fn go_type() {
        assert_eq!(
            extract_go_symbol("type Config struct {"),
            Some(("type", "Config".into()))
        );
    }

    #[test]
    fn go_no_match() {
        assert_eq!(extract_go_symbol("var x = 10"), None);
        assert_eq!(extract_go_symbol("package main"), None);
    }

    // ── get_word_at_position ────────────────────────────────────────

    #[test]
    fn word_at_middle() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        let mut f = std::fs::File::create(&file).unwrap();
        writeln!(f, "fn hello_world() {{}}").unwrap();

        // line 1, character 6 → inside "hello_world"
        let word = get_word_at_position(&file, 1, 6).unwrap();
        assert_eq!(word, "hello_world");
    }

    #[test]
    fn word_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        let mut f = std::fs::File::create(&file).unwrap();
        writeln!(f, "my_var = 42").unwrap();

        let word = get_word_at_position(&file, 1, 1).unwrap();
        assert_eq!(word, "my_var");
    }

    #[test]
    fn no_identifier_at_position() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        let mut f = std::fs::File::create(&file).unwrap();
        writeln!(f, "a + b").unwrap();

        // character 3 → the '+' (space then +)
        let result = get_word_at_position(&file, 1, 3);
        assert!(result.is_err());
    }
}
