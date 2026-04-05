//! MCP client — protocol-level operations over a transport.
//!
//! Implements the MCP lifecycle: initialize → list tools/resources → call tool → close.
//! Aligned with the Model Context Protocol specification.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use super::transport::StdioTransport;

// ── MCP protocol types ───────────────────────────────────────────────────────

/// Server capabilities returned during initialization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(default)]
    pub tools: Option<Value>,
    #[serde(default)]
    pub resources: Option<Value>,
    #[serde(default)]
    pub prompts: Option<Value>,
    #[serde(default)]
    pub logging: Option<Value>,
}

/// Server info returned during initialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// An MCP tool definition returned by `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<Value>,
    #[serde(default)]
    pub annotations: Option<McpToolAnnotations>,
}

/// Tool annotations providing hints about behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolAnnotations {
    #[serde(default, rename = "readOnlyHint")]
    pub read_only_hint: Option<bool>,
    #[serde(default, rename = "destructiveHint")]
    pub destructive_hint: Option<bool>,
    #[serde(default, rename = "openWorldHint")]
    pub open_world_hint: Option<bool>,
}

/// An MCP resource definition returned by `resources/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
}

/// Content from reading an MCP resource or tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
}

/// Result of calling an MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    #[serde(default)]
    pub content: Vec<McpContent>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

impl McpToolResult {
    /// Extract text content from the result.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ── MCP Client ───────────────────────────────────────────────────────────────

/// Configuration for connecting to an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

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

        // Send initialize request
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

        // Parse server capabilities
        let capabilities: ServerCapabilities = serde_json::from_value(
            init_result.get("capabilities").cloned().unwrap_or(Value::Null),
        )
        .unwrap_or_default();

        let server_info: Option<ServerInfo> = init_result
            .get("serverInfo")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok());

        debug!(
            "MCP server '{}' initialized: {:?}, capabilities: tools={}, resources={}",
            config.name,
            server_info,
            capabilities.tools.is_some(),
            capabilities.resources.is_some(),
        );

        // Send initialized notification (signals we're ready)
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
            .with_context(|| format!("MCP tools/call '{}' failed", tool_name))?;

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
            result
                .get("resources")
                .cloned()
                .unwrap_or(Value::Array(vec![])),
        )
        .context("Failed to parse MCP resources list")?;

        Ok(resources)
    }

    /// Read a specific resource by URI.
    pub async fn read_resource(&mut self, uri: &str) -> Result<Vec<McpContent>> {
        let result = self
            .transport
            .request(
                "resources/read",
                Some(json!({ "uri": uri })),
            )
            .await
            .with_context(|| format!("MCP resources/read '{}' failed", uri))?;

        let contents: Vec<McpContent> = serde_json::from_value(
            result
                .get("contents")
                .cloned()
                .unwrap_or(Value::Array(vec![])),
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

    /// Invalidate the tools cache (e.g., after server tools change notification).
    pub fn invalidate_tools_cache(&mut self) {
        self.tools_cache = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_tool_result_text() {
        let result = McpToolResult {
            content: vec![
                McpContent {
                    content_type: "text".to_string(),
                    text: Some("Hello".to_string()),
                    data: None,
                    mime_type: None,
                },
                McpContent {
                    content_type: "text".to_string(),
                    text: Some("World".to_string()),
                    data: None,
                    mime_type: None,
                },
            ],
            is_error: false,
        };
        assert_eq!(result.text(), "Hello\nWorld");
    }

    #[test]
    fn test_mcp_tool_result_empty() {
        let result = McpToolResult {
            content: vec![],
            is_error: false,
        };
        assert_eq!(result.text(), "");
    }

    #[test]
    fn test_server_config_deserialize() {
        let json = r#"{"name":"test","command":"node","args":["server.js"],"env":{"PORT":"3000"}}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "test");
        assert_eq!(config.command, "node");
        assert_eq!(config.args, vec!["server.js"]);
        assert_eq!(config.env.get("PORT"), Some(&"3000".to_string()));
    }

    #[test]
    fn test_tool_annotations_deserialize() {
        let json = r#"{"name":"read","description":"Read a file","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}"#;
        let tool: McpToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "read");
        assert!(tool.annotations.as_ref().unwrap().read_only_hint.unwrap());
    }

    #[test]
    fn test_mcp_content_types() {
        let text_content = McpContent {
            content_type: "text".to_string(),
            text: Some("hello".to_string()),
            data: None,
            mime_type: None,
        };
        assert_eq!(text_content.text.unwrap(), "hello");

        let image_content = McpContent {
            content_type: "image".to_string(),
            text: None,
            data: Some("base64data".to_string()),
            mime_type: Some("image/png".to_string()),
        };
        assert!(image_content.text.is_none());
        assert_eq!(image_content.data.unwrap(), "base64data");
    }
}
