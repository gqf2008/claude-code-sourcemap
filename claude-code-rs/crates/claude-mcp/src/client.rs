//! MCP client — protocol-level operations over a transport.
//!
//! Implements the MCP lifecycle: initialize → list tools/resources → call tool → close.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::transport::StdioTransport;
use crate::types::{ServerInfo, ServerCapabilities, McpToolDef, McpServerConfig, McpToolResult, McpResource, McpContent, McpPrompt, McpPromptMessage};

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

    /// List prompts provided by this MCP server.
    pub async fn list_prompts(&mut self) -> Result<Vec<McpPrompt>> {
        if self.capabilities.prompts.is_none() {
            return Ok(Vec::new());
        }

        let result = self
            .transport
            .request("prompts/list", Some(json!({})))
            .await
            .context("MCP prompts/list failed")?;

        let prompts: Vec<McpPrompt> = serde_json::from_value(
            result.get("prompts").cloned().unwrap_or(Value::Array(vec![])),
        )
        .context("Failed to parse MCP prompts list")?;

        info!("MCP server '{}': {} prompts available", self.server_name, prompts.len());
        Ok(prompts)
    }

    /// Get a specific prompt by name, with optional arguments.
    pub async fn get_prompt(
        &mut self,
        name: &str,
        arguments: Option<serde_json::Map<String, Value>>,
    ) -> Result<Vec<McpPromptMessage>> {
        let mut params = json!({ "name": name });
        if let Some(args) = arguments {
            params["arguments"] = Value::Object(args);
        }

        let result = self
            .transport
            .request("prompts/get", Some(params))
            .await
            .with_context(|| format!("MCP prompts/get '{name}' failed"))?;

        let messages: Vec<McpPromptMessage> = serde_json::from_value(
            result.get("messages").cloned().unwrap_or(Value::Array(vec![])),
        )
        .context("Failed to parse MCP prompt messages")?;

        Ok(messages)
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

    // ── Resource subscriptions ───────────────────────────────────────────────

    /// Subscribe to updates for a resource URI.
    ///
    /// Requires server `resources.subscribe` capability.
    pub async fn subscribe_resource(&mut self, uri: &str) -> Result<()> {
        debug!("MCP subscribe resource: {}/{}", self.server_name, uri);
        self.transport
            .request("resources/subscribe", Some(json!({ "uri": uri })))
            .await
            .with_context(|| format!("MCP resources/subscribe '{uri}' failed"))?;
        Ok(())
    }

    /// Unsubscribe from resource updates.
    pub async fn unsubscribe_resource(&mut self, uri: &str) -> Result<()> {
        debug!("MCP unsubscribe resource: {}/{}", self.server_name, uri);
        self.transport
            .request("resources/unsubscribe", Some(json!({ "uri": uri })))
            .await
            .with_context(|| format!("MCP resources/unsubscribe '{uri}' failed"))?;
        Ok(())
    }

    // ── Elicitation ──────────────────────────────────────────────────────────

    /// Send an elicitation request to gather structured input from the user.
    ///
    /// Requires server `elicitation` capability.
    pub async fn create_elicitation(
        &mut self,
        message: &str,
        requested_schema: Value,
    ) -> Result<crate::types::ElicitationResponse> {
        debug!("MCP elicitation: {}: {}", self.server_name, message);
        let result = self
            .transport
            .request(
                "elicitation/create",
                Some(json!({
                    "message": message,
                    "requestedSchema": requested_schema
                })),
            )
            .await
            .with_context(|| format!("MCP elicitation/create failed for '{}'", self.server_name))?;

        serde_json::from_value(result).context("Failed to parse elicitation response")
    }

    /// Check if the server supports elicitation.
    pub fn supports_elicitation(&self) -> bool {
        self.capabilities.elicitation.is_some()
    }

    /// Check if the server supports resource subscriptions.
    pub fn supports_resource_subscriptions(&self) -> bool {
        // resources capability with subscribe=true
        self.capabilities
            .resources
            .as_ref()
            .and_then(|v| v.get("subscribe"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Get the stored config name.
    pub fn name(&self) -> &str {
        &self.server_name
    }
}
