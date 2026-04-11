//! MCP (Model Context Protocol) — tool implementations that use `clawed-mcp`.
//!
//! This module provides Tool trait implementations for MCP operations:
//! - `ListMcpResourcesTool` — list resources from connected MCP servers
//! - `ReadMcpResourceTool` — read a specific resource by URI
//! - `McpToolProxy` — dynamic proxy for MCP server tools
//!
//! All protocol types, transport, client, and registry logic live in `clawed-mcp`.

use std::sync::Arc;

use async_trait::async_trait;
use clawed_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use clawed_core::message::{ImageSource, ToolResultContent};

// Re-export from clawed-mcp for convenience
pub use clawed_mcp::{
    McpClient, McpContent, McpManager, McpResource, McpServerConfig, McpToolDef, McpToolResult,
    format_mcp_tool_name, parse_mcp_tool_name, is_mcp_tool,
    load_mcp_configs, discover_mcp_configs,
    BuiltinMcpServer,
};

pub use clawed_mcp::registry::MCP_TOOL_PREFIX;

/// Convert MCP result content into core `ToolResultContent`, handling both text and images.
fn mcp_result_to_tool_content(result: &McpToolResult) -> Vec<ToolResultContent> {
    let mut content = Vec::new();

    for c in &result.content {
        match c.content_type.as_str() {
            "text" => {
                if let Some(text) = &c.text {
                    content.push(ToolResultContent::Text { text: text.clone() });
                }
            }
            "image" => {
                if let Some(data) = &c.data {
                    let media_type = c.mime_type.as_deref()
                        .unwrap_or("image/png")
                        .to_string();
                    content.push(ToolResultContent::Image {
                        source: ImageSource {
                            media_type,
                            data: data.clone(),
                        },
                    });
                }
            }
            _ => {}
        }
    }

    if content.is_empty() {
        content.push(ToolResultContent::Text { text: String::new() });
    }
    content
}

// ── ListMcpResourcesTool ─────────────────────────────────────────────────────

/// Lists resources available from connected MCP servers.
pub struct ListMcpResourcesTool {
    pub manager: Arc<RwLock<McpManager>>,
}

#[async_trait]
impl Tool for ListMcpResourcesTool {
    fn name(&self) -> &'static str { "mcp_list_resources" }
    fn category(&self) -> ToolCategory { ToolCategory::Mcp }

    fn description(&self) -> &'static str {
        "List resources available from connected MCP servers. Resources are \
         data items (files, database entries, etc.) that MCP servers expose. \
         Set format to 'json' for structured output."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "Optional: filter by MCP server name"
                },
                "format": {
                    "type": "string",
                    "enum": ["text", "json"],
                    "description": "Output format: 'text' (default) or 'json'"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool { true }
    fn is_enabled(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let manager = self.manager.read().await;
        let server_filter = input["server"].as_str();
        let json_format = input["format"].as_str() == Some("json");

        let server_names = manager.server_names().await;
        if server_names.is_empty() {
            return Ok(ToolResult::text(
                "No MCP servers connected. Configure MCP servers in .mcp.json \
                 or use the --mcp flag to connect to a server."
            ));
        }

        if json_format {
            let mut result = json!({});
            for name in &server_names {
                if let Some(filter) = server_filter {
                    if name != filter { continue; }
                }
                match manager.list_resources_for(name).await {
                    Ok(resources) => {
                        let entries: Vec<Value> = resources.iter().map(|r| {
                            json!({
                                "uri": r.uri,
                                "name": r.name,
                                "description": r.description,
                                "mimeType": r.mime_type,
                            })
                        }).collect();
                        result[name] = json!(entries);
                    }
                    Err(e) => {
                        result[name] = json!({ "error": e.to_string() });
                    }
                }
            }
            Ok(ToolResult::text(serde_json::to_string_pretty(&result)?))
        } else {
            let mut output = String::new();
            for name in &server_names {
                if let Some(filter) = server_filter {
                    if name != filter { continue; }
                }
                match manager.list_resources_for(name).await {
                    Ok(resources) => {
                        if resources.is_empty() {
                            output.push_str(&format!("## {name} — no resources\n\n"));
                        } else {
                            output.push_str(&format!("## {} — {} resources\n", name, resources.len()));
                            for res in &resources {
                                let mime = res.mime_type.as_deref()
                                    .map(|m| format!(" [{m}]"))
                                    .unwrap_or_default();
                                output.push_str(&format!(
                                    "- **{}** (`{}`){}{}\n",
                                    res.name,
                                    res.uri,
                                    mime,
                                    res.description
                                        .as_deref()
                                        .map(|d| format!(" — {d}"))
                                        .unwrap_or_default()
                                ));
                            }
                            output.push('\n');
                        }
                    }
                    Err(e) => {
                        output.push_str(&format!("## {name} — error: {e}\n\n"));
                    }
                }
            }
            Ok(ToolResult::text(output))
        }
    }
}

// ── ReadMcpResourceTool ──────────────────────────────────────────────────────

/// Reads a specific resource from an MCP server by URI.
pub struct ReadMcpResourceTool {
    pub manager: Arc<RwLock<McpManager>>,
}

#[async_trait]
impl Tool for ReadMcpResourceTool {
    fn name(&self) -> &'static str { "mcp_read_resource" }
    fn category(&self) -> ToolCategory { ToolCategory::Mcp }

    fn description(&self) -> &'static str {
        "Read a specific resource from an MCP server by its URI. \
         Handles both text and binary content. For binary resources, \
         set save_to to write the decoded data to a file."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "MCP server name"
                },
                "uri": {
                    "type": "string",
                    "description": "Resource URI to read"
                },
                "save_to": {
                    "type": "string",
                    "description": "Optional: file path to save binary content to"
                }
            },
            "required": ["server", "uri"]
        })
    }

    fn is_read_only(&self) -> bool { true }
    fn is_enabled(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        use base64::Engine;

        let server = input["server"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'server'"))?;
        let uri = input["uri"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'uri'"))?;
        let save_to = input["save_to"].as_str();

        let manager = self.manager.read().await;
        let contents = manager.read_resource(server, uri).await?;

        let mut text_parts: Vec<String> = Vec::new();
        let mut blob_count = 0;

        for content in &contents {
            if let Some(text) = content.text.as_deref() {
                text_parts.push(text.to_string());
            } else if let Some(data) = content.data.as_deref() {
                blob_count += 1;
                let mime = content.mime_type.as_deref().unwrap_or("application/octet-stream");

                if let Some(path) = save_to {
                    match base64::engine::general_purpose::STANDARD.decode(data) {
                        Ok(bytes) => {
                            match std::fs::write(path, &bytes) {
                                Ok(()) => {
                                    text_parts.push(format!(
                                        "[Binary blob ({}, {} bytes) saved to: {}]",
                                        mime, bytes.len(), path
                                    ));
                                }
                                Err(e) => {
                                    text_parts.push(format!(
                                        "[Binary blob ({}, {} bytes) — failed to save: {}]",
                                        mime, bytes.len(), e
                                    ));
                                }
                            }
                        }
                        Err(e) => {
                            text_parts.push(format!("[Binary blob ({mime}) — base64 decode error: {e}]"));
                        }
                    }
                } else {
                    let size_hint = data.len() * 3 / 4;
                    text_parts.push(format!(
                        "[Binary blob: {uri} ({mime}, ~{size_hint} bytes). Use save_to to write to disk.]"
                    ));
                }
            }
        }

        if text_parts.is_empty() {
            if blob_count > 0 {
                Ok(ToolResult::text(format!(
                    "Resource '{uri}' contains {blob_count} binary blob(s) but no text content."
                )))
            } else {
                Ok(ToolResult::text(format!("Resource '{uri}' returned no content.")))
            }
        } else {
            Ok(ToolResult::text(text_parts.join("\n")))
        }
    }
}

// ── McpToolProxy ─────────────────────────────────────────────────────────────

/// Dynamic proxy tool that dispatches calls to MCP server tools.
pub struct McpToolProxy {
    pub qualified_name: String,
    pub server_name: String,
    pub tool_name: String,
    pub tool_description: String,
    pub tool_schema: Value,
    pub read_only: bool,
    /// Category override (defaults to `Mcp`). Builtin servers may use `ComputerUse` etc.
    pub category: ToolCategory,
    pub manager: Arc<RwLock<McpManager>>,
}

#[async_trait]
impl Tool for McpToolProxy {
    fn name(&self) -> &str { &self.qualified_name }
    fn category(&self) -> ToolCategory { self.category }
    fn description(&self) -> &str { &self.tool_description }
    fn input_schema(&self) -> Value { self.tool_schema.clone() }
    fn is_read_only(&self) -> bool { self.read_only }
    fn is_enabled(&self) -> bool { true }

    fn to_auto_classifier_input(&self, _input: &Value) -> Value {
        // MCP tools may carry arbitrary user data — only expose the tool name
        json!({"MCP": {"server": &self.server_name, "tool": &self.tool_name}})
    }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let manager = self.manager.read().await;
        let result = manager
            .call_tool_direct(&self.server_name, &self.tool_name, input)
            .await?;

        let content = mcp_result_to_tool_content(&result);

        Ok(ToolResult {
            content,
            is_error: result.is_error,
            structured_output: None,
        })
    }
}

/// Create `McpToolProxy` instances for all tools discovered from connected servers.
pub async fn create_mcp_tool_proxies(
    manager: Arc<RwLock<McpManager>>,
) -> anyhow::Result<Vec<McpToolProxy>> {
    let mgr = manager.read().await;
    let tools = mgr.list_all_tools().await?;
    drop(mgr);

    let mut proxies = Vec::new();
    for (qualified_name, tool_def) in tools {
        let (server_name, tool_name) = match parse_mcp_tool_name(&qualified_name) {
            Some((s, t)) => (s, t),
            None => continue,
        };
        let read_only = tool_def
            .annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false);

        proxies.push(McpToolProxy {
            qualified_name,
            server_name,
            tool_name,
            tool_description: tool_def.description.unwrap_or_default(),
            tool_schema: tool_def.input_schema.unwrap_or(json!({"type": "object"})),
            read_only,
            category: ToolCategory::Mcp,
            manager: manager.clone(),
        });
    }

    Ok(proxies)
}

/// Create `McpToolProxy` instances for a specific built-in server with short tool names
/// and a custom category (e.g. `ToolCategory::ComputerUse`).
///
/// Unlike `create_mcp_tool_proxies` which uses `mcp__server__tool` naming,
/// builtin tools use their bare tool name for backward compatibility.
pub fn create_builtin_tool_proxies(
    server: &dyn BuiltinMcpServer,
    category: ToolCategory,
    read_only_tools: &[&str],
    manager: Arc<RwLock<McpManager>>,
) -> Vec<McpToolProxy> {
    let server_name = server.server_name().to_string();
    let tool_defs = server.list_tools();

    tool_defs
        .into_iter()
        .map(|tool_def| {
            let tool_name = tool_def.name.clone();
            let read_only = tool_def
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint)
                .unwrap_or_else(|| read_only_tools.contains(&tool_name.as_str()));

            McpToolProxy {
                qualified_name: tool_name.clone(),
                server_name: server_name.clone(),
                tool_name,
                tool_description: tool_def.description.unwrap_or_default(),
                tool_schema: tool_def.input_schema.unwrap_or(json!({"type": "object"})),
                read_only,
                category,
                manager: manager.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_tool_proxy_name() {
        let manager = Arc::new(RwLock::new(McpManager::new()));
        let proxy = McpToolProxy {
            qualified_name: "mcp__github__create_issue".to_string(),
            server_name: "github".to_string(),
            tool_name: "create_issue".to_string(),
            tool_description: "Create a GitHub issue".to_string(),
            tool_schema: json!({"type": "object"}),
            read_only: false,
            category: ToolCategory::Mcp,
            manager,
        };
        assert_eq!(proxy.name(), "mcp__github__create_issue");
        assert!(!proxy.is_read_only());
        assert!(proxy.is_enabled());
        assert_eq!(proxy.category(), ToolCategory::Mcp);
    }
}
