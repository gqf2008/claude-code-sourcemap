//! MCP (Model Context Protocol) — connect to external tool servers.
//!
//! This module provides a full MCP client implementation supporting:
//! - Stdio transport (spawn child processes)
//! - JSON-RPC 2.0 protocol
//! - Tool discovery and dynamic proxy
//! - Resource listing and reading
//! - Multi-server management
//!
//! Aligned with the TS `services/mcp/client.ts` and `tools/MCPTool/MCPTool.ts`.

pub mod client;
pub mod server;
pub mod transport;

use std::sync::Arc;

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio::sync::RwLock;

pub use client::{McpClient, McpContent, McpServerConfig, McpToolDef, McpToolResult};
pub use server::{
    build_mcp_tool_name, discover_mcp_configs, load_mcp_configs, parse_mcp_tool_name, McpManager,
};

// ── ListMcpResourcesTool ─────────────────────────────────────────────────────

/// Lists resources available from connected MCP servers.
///
/// Supports two output formats: human-readable markdown (default) and
/// structured JSON for programmatic use.
pub struct ListMcpResourcesTool {
    pub manager: Arc<RwLock<McpManager>>,
}

#[async_trait]
impl Tool for ListMcpResourcesTool {
    fn name(&self) -> &str { "mcp_list_resources" }
    fn category(&self) -> ToolCategory { ToolCategory::Mcp }

    fn description(&self) -> &str {
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
            // Structured JSON output
            let mut result = json!({});
            for name in &server_names {
                if let Some(filter) = server_filter {
                    if name != filter { continue; }
                }
                match manager.list_resources(name).await {
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
            // Human-readable markdown output
            let mut output = String::new();
            for name in &server_names {
                if let Some(filter) = server_filter {
                    if name != filter { continue; }
                }
                match manager.list_resources(name).await {
                    Ok(resources) => {
                        if resources.is_empty() {
                            output.push_str(&format!("## {} — no resources\n\n", name));
                        } else {
                            output.push_str(&format!("## {} — {} resources\n", name, resources.len()));
                            for res in &resources {
                                let mime = res.mime_type.as_deref()
                                    .map(|m| format!(" [{}]", m))
                                    .unwrap_or_default();
                                output.push_str(&format!(
                                    "- **{}** (`{}`){}{}\n",
                                    res.name,
                                    res.uri,
                                    mime,
                                    res.description
                                        .as_deref()
                                        .map(|d| format!(" — {}", d))
                                        .unwrap_or_default()
                                ));
                            }
                            output.push('\n');
                        }
                    }
                    Err(e) => {
                        output.push_str(&format!("## {} — error: {}\n\n", name, e));
                    }
                }
            }
            Ok(ToolResult::text(output))
        }
    }
}

// ── ReadMcpResourceTool ──────────────────────────────────────────────────────

/// Reads a specific resource from an MCP server by URI.
///
/// Handles both text and binary (base64-encoded blob) content.
/// For binary content, can optionally write to disk and return the file path.
pub struct ReadMcpResourceTool {
    pub manager: Arc<RwLock<McpManager>>,
}

#[async_trait]
impl Tool for ReadMcpResourceTool {
    fn name(&self) -> &str { "mcp_read_resource" }
    fn category(&self) -> ToolCategory { ToolCategory::Mcp }

    fn description(&self) -> &str {
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
                    // Decode base64 and write to file
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
                            text_parts.push(format!("[Binary blob ({}) — base64 decode error: {}]", mime, e));
                        }
                    }
                } else {
                    // Report blob metadata without writing
                    let size_hint = data.len() * 3 / 4; // approximate decoded size
                    text_parts.push(format!(
                        "[Binary blob: {} ({}, ~{} bytes). Use save_to to write to disk.]",
                        uri, mime, size_hint
                    ));
                }
            }
        }

        if text_parts.is_empty() {
            if blob_count > 0 {
                Ok(ToolResult::text(format!(
                    "Resource '{}' contains {} binary blob(s) but no text content.", uri, blob_count
                )))
            } else {
                Ok(ToolResult::text(format!("Resource '{}' returned no content.", uri)))
            }
        } else {
            Ok(ToolResult::text(text_parts.join("\n")))
        }
    }
}

// ── McpToolProxy ─────────────────────────────────────────────────────────────

/// Dynamic proxy tool that dispatches calls to MCP server tools.
///
/// Each instance represents one specific tool on one specific MCP server.
/// The tool name is fully-qualified: `mcp__<server>__<tool>`.
pub struct McpToolProxy {
    pub qualified_name: String,
    pub server_name: String,
    pub tool_name: String,
    pub tool_description: String,
    pub tool_schema: Value,
    pub read_only: bool,
    pub manager: Arc<RwLock<McpManager>>,
}

#[async_trait]
impl Tool for McpToolProxy {
    fn name(&self) -> &str { &self.qualified_name }
    fn category(&self) -> ToolCategory { ToolCategory::Mcp }
    fn description(&self) -> &str { &self.tool_description }
    fn input_schema(&self) -> Value { self.tool_schema.clone() }
    fn is_read_only(&self) -> bool { self.read_only }
    fn is_enabled(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let manager = self.manager.read().await;
        let result = manager
            .call_tool(&self.server_name, &self.tool_name, input)
            .await?;

        if result.is_error {
            Ok(ToolResult::error(format!(
                "MCP tool error ({}): {}",
                self.tool_name,
                result.text()
            )))
        } else {
            let text = result.text();
            if text.is_empty() {
                Ok(ToolResult::text("Tool completed with no output."))
            } else {
                Ok(ToolResult::text(text))
            }
        }
    }
}

/// Create McpToolProxy instances for all tools discovered from connected servers.
pub async fn create_mcp_tool_proxies(
    manager: Arc<RwLock<McpManager>>,
) -> anyhow::Result<Vec<McpToolProxy>> {
    let mgr = manager.read().await;
    let tools = mgr.all_tools().await?;
    drop(mgr);

    let mut proxies = Vec::new();
    for (server_name, tool_def) in tools {
        let qualified = build_mcp_tool_name(&server_name, &tool_def.name);
        let read_only = tool_def
            .annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false);

        proxies.push(McpToolProxy {
            qualified_name: qualified,
            server_name,
            tool_name: tool_def.name,
            tool_description: tool_def.description.unwrap_or_default(),
            tool_schema: tool_def.input_schema.unwrap_or(json!({"type": "object"})),
            read_only,
            manager: manager.clone(),
        });
    }

    Ok(proxies)
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
            manager,
        };
        assert_eq!(proxy.name(), "mcp__github__create_issue");
        assert!(!proxy.is_read_only());
        assert!(proxy.is_enabled());
        assert_eq!(proxy.category(), ToolCategory::Mcp);
    }
}
