//! MCP (Model Context Protocol) stubs — placeholder for future implementation.
//!
//! MCP allows Claude Code to connect to external tool servers that expose
//! resources and tools via a standard protocol.  This module provides the
//! trait definitions and stub tool implementations.  Full MCP support
//! (stdio/SSE transport, tool discovery, resource listing) is deferred.

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

// ── MCP Connection (stub) ────────────────────────────────────────────────────

/// Represents an MCP server connection (placeholder).
#[allow(dead_code)]
pub struct McpServer {
    pub name: String,
    pub transport: McpTransport,
    pub status: McpStatus,
}

#[allow(dead_code)]
pub enum McpTransport {
    Stdio { command: String, args: Vec<String> },
    Sse { url: String },
}

#[allow(dead_code)]
pub enum McpStatus {
    Connecting,
    Connected,
    Disconnected,
    Error(String),
}

// ── ListMcpResourcesTool ─────────────────────────────────────────────────────

pub struct ListMcpResourcesTool;

#[async_trait]
impl Tool for ListMcpResourcesTool {
    fn name(&self) -> &str { "mcp_list_resources" }
    fn category(&self) -> ToolCategory { ToolCategory::Mcp }

    fn description(&self) -> &str {
        "List resources available from connected MCP servers. Resources are \
         data items (files, database entries, etc.) that MCP servers expose."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "Optional: filter by MCP server name"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool { true }
    fn is_enabled(&self) -> bool { false } // disabled until MCP is implemented

    async fn call(&self, _input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::text(
            "MCP support is not yet available. Configure MCP servers in your \
             CLAUDE.md or settings to enable this feature."
        ))
    }
}

// ── ReadMcpResourceTool ──────────────────────────────────────────────────────

pub struct ReadMcpResourceTool;

#[async_trait]
impl Tool for ReadMcpResourceTool {
    fn name(&self) -> &str { "mcp_read_resource" }
    fn category(&self) -> ToolCategory { ToolCategory::Mcp }

    fn description(&self) -> &str {
        "Read a specific resource from an MCP server by its URI."
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
                }
            },
            "required": ["server", "uri"]
        })
    }

    fn is_read_only(&self) -> bool { true }
    fn is_enabled(&self) -> bool { false }

    async fn call(&self, _input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::text("MCP support is not yet available."))
    }
}

// ── McpTool (dynamic proxy) ──────────────────────────────────────────────────

pub struct McpTool;

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str { "mcp" }
    fn category(&self) -> ToolCategory { ToolCategory::Mcp }

    fn description(&self) -> &str {
        "Execute a tool provided by a connected MCP server. MCP tools are \
         dynamically discovered from server capabilities."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "MCP server name"
                },
                "tool": {
                    "type": "string",
                    "description": "Tool name on the MCP server"
                },
                "input": {
                    "type": "object",
                    "description": "Input parameters for the MCP tool"
                }
            },
            "required": ["server", "tool"]
        })
    }

    fn is_read_only(&self) -> bool { false }
    fn is_enabled(&self) -> bool { false }

    async fn call(&self, _input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::text("MCP support is not yet available."))
    }
}
