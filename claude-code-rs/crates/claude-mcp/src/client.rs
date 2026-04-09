//! MCP client — protocol-level operations over a transport.
//!
//! Implements the MCP lifecycle: initialize → list tools/resources → call tool → close.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::transport::StdioTransport;
use crate::types::{ServerInfo, ServerCapabilities, McpToolDef, McpServerConfig, McpToolResult, McpResource, McpContent};

/// MCP client wrapping a transport with protocol-level operations.
pub struct McpClient {
    transport: StdioTransport,
    pub server_name: String,
    pub server_info: Option<ServerInfo>,
    pub capabilities: ServerCapabilities,
    tools_cache: Option<Vec<McpToolDef>>,
}

impl McpClient {
    /// Connect to an MCP server and perform initialization handshake.
    pub async fn connect(config: &McpServerConfig) -> Result<Self> {
        info!("Connecting to MCP server '{}': {} {}", config.name, config.command, config.args.join(" "));

        let mut transport = StdioTransport::spawn(&config.command, &config.args, &config.env)
            .await
            .with_context(|| format!("Failed to start MCP server '{}'", config.name))?;

        let init_result = transport
            .request(
                "initialize",
                Some(json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "roots": { "listChanged": true }
                    },
                    "clientInfo": {
                        "name": "claude-code-rs",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                })),
            )
            .await
            .with_context(|| format!("MCP initialize failed for '{}'", config.name))?;

        let capabilities: ServerCapabilities = serde_json::from_value(
            init_result
                .get("capabilities")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new())),
        )
        .with_context(|| {
            format!(
                "Failed to parse capabilities from MCP server '{}': {:?}",
                config.name,
                init_result.get("capabilities")
            )
        })?;

        let server_info: Option<ServerInfo> = init_result
            .get("serverInfo")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok());

        debug!(
            "MCP server '{}' initialized: {:?}, capabilities: tools={}, resources={}",
            config.name, server_info,
            capabilities.tools.is_some(),
            capabilities.resources.is_some(),
        );

        transport.notify("notifications/initialized", None).await?;

        Ok(Self {
            transport,
            server_name: config.name.clone(),
            server_info,
            capabilities,
            tools_cache: None,
        })
    }

    /// List tools provided by this MCP server.
    pub async fn list_tools(&mut self) -> Result<Vec<McpToolDef>> {
        if let Some(ref cached) = self.tools_cache {
            return Ok(cached.clone());
        }

        let result = self
            .transport
            .request("tools/list", Some(json!({})))
            .await
            .context("MCP tools/list failed")?;

        let tools: Vec<McpToolDef> = serde_json::from_value(
            result.get("tools").cloned().unwrap_or(Value::Array(vec![])),
        )
        .context("Failed to parse MCP tools list")?;

        info!("MCP server '{}': {} tools available", self.server_name, tools.len());
        self.tools_cache = Some(tools.clone());
        Ok(tools)
    }

    /// Call a tool on this MCP server.
    pub async fn call_tool(&mut self, tool_name: &str, arguments: Value) -> Result<McpToolResult> {
        debug!("MCP call: {}/{}", self.server_name, tool_name);

        let result = self
            .transport
            .request(
                "tools/call",
                Some(json!({
                    "name": tool_name,
                    "arguments": arguments
                })),
            )
            .await
            .with_context(|| format!("MCP tools/call '{tool_name}' failed"))?;

        let tool_result: McpToolResult = serde_json::from_value(result)
            .context("Failed to parse MCP tool result")?;

        if tool_result.is_error {
            warn!("MCP tool '{}' returned error: {}", tool_name, tool_result.text());
        }

        Ok(tool_result)
    }

    /// List resources provided by this MCP server.
    pub async fn list_resources(&mut self) -> Result<Vec<McpResource>> {
        if self.capabilities.resources.is_none() {
            return Ok(Vec::new());
        }

        let result = self
            .transport
            .request("resources/list", Some(json!({})))
            .await
            .context("MCP resources/list failed")?;

        let resources: Vec<McpResource> = serde_json::from_value(
            result.get("resources").cloned().unwrap_or(Value::Array(vec![])),
        )
        .context("Failed to parse MCP resources list")?;

        Ok(resources)
    }

    /// Read a specific resource by URI.
    pub async fn read_resource(&mut self, uri: &str) -> Result<Vec<McpContent>> {
        let result = self
            .transport
            .request("resources/read", Some(json!({ "uri": uri })))
            .await
            .with_context(|| format!("MCP resources/read '{uri}' failed"))?;

        let contents: Vec<McpContent> = serde_json::from_value(
            result.get("contents").cloned().unwrap_or(Value::Array(vec![])),
        )
        .context("Failed to parse MCP resource contents")?;

        Ok(contents)
    }

    /// Disconnect from the MCP server.
    pub async fn close(&mut self) -> Result<()> {
        info!("Disconnecting MCP server '{}'", self.server_name);
        self.transport.close().await
    }

    /// Check if the server is still running.
    pub fn is_alive(&mut self) -> bool {
        self.transport.is_alive()
    }

    /// Invalidate the tools cache.
    pub fn invalidate_tools_cache(&mut self) {
        self.tools_cache = None;
    }

    /// Handle a `notifications/tools/list_changed` notification.
    pub async fn handle_tool_list_changed(&mut self) -> Result<Vec<McpToolDef>> {
        info!("MCP server '{}': tool list changed notification received", self.server_name);
        self.tools_cache = None;
        self.list_tools().await
    }
}
