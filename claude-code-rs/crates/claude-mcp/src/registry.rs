//! MCP registry — manages multiple MCP servers and their lifecycles.
//!
//! Extracted from `claude-tools/src/mcp/server.rs`. Provides:
//! - Config discovery from CLAUDE.md and settings
//! - Server startup / shutdown
//! - Tool name mapping (`mcp__server__tool`)
//! - Health monitoring

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::client::McpClient;
use crate::types::{McpServerConfig, McpToolDef, McpToolResult, McpResource, McpContent};

/// Prefix for MCP tool proxy names: `mcp__<server>__<tool>`.
pub const MCP_TOOL_PREFIX: &str = "mcp__";

/// Manages multiple MCP server connections.
pub struct McpManager {
    servers: Arc<RwLock<HashMap<String, McpClient>>>,
    configs: Arc<RwLock<Vec<McpServerConfig>>>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    #[must_use] 
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
            configs: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Load MCP server configs (from settings or CLAUDE.md parsed configs).
    pub async fn load_configs(&self, configs: Vec<McpServerConfig>) {
        info!("Loading {} MCP server configs", configs.len());
        let mut stored = self.configs.write().await;
        *stored = configs;
    }

    /// Start all configured servers.
    pub async fn start_all(&self) -> Result<()> {
        let configs = self.configs.read().await.clone();
        for config in &configs {
            if let Err(e) = self.start_server(config).await {
                warn!("Failed to start MCP server '{}': {}", config.name, e);
            }
        }
        Ok(())
    }

    /// Start a single MCP server by config.
    pub async fn start_server(&self, config: &McpServerConfig) -> Result<()> {
        {
            let servers = self.servers.read().await;
            if servers.contains_key(&config.name) {
                debug!("MCP server '{}' already running, skipping", config.name);
                return Ok(());
            }
        }

        let client = McpClient::connect(config)
            .await
            .with_context(|| format!("Failed to connect to MCP server '{}'", config.name))?;

        let mut servers = self.servers.write().await;
        servers.insert(config.name.clone(), client);
        info!("MCP server '{}' started", config.name);
        Ok(())
    }

    /// Stop a specific server.
    pub async fn stop_server(&self, name: &str) -> Result<()> {
        let mut servers = self.servers.write().await;
        if let Some(mut client) = servers.remove(name) {
            client.close().await?;
            info!("MCP server '{}' stopped", name);
        }
        Ok(())
    }

    /// Stop all servers.
    pub async fn stop_all(&self) -> Result<()> {
        let mut servers = self.servers.write().await;
        for (name, mut client) in servers.drain() {
            if let Err(e) = client.close().await {
                warn!("Error stopping MCP server '{}': {}", name, e);
            }
        }
        Ok(())
    }

    /// List all available tools from all running servers, with prefixed names.
    pub async fn list_all_tools(&self) -> Result<Vec<(String, McpToolDef)>> {
        let mut servers = self.servers.write().await;
        let mut all_tools = Vec::new();

        for (server_name, client) in servers.iter_mut() {
            match client.list_tools().await {
                Ok(tools) => {
                    for tool in tools {
                        let prefixed = format_mcp_tool_name(server_name, &tool.name);
                        all_tools.push((prefixed, tool));
                    }
                }
                Err(e) => {
                    warn!("Failed to list tools from MCP server '{}': {}", server_name, e);
                }
            }
        }

        Ok(all_tools)
    }

    /// List tools from a specific server only (avoids cross-server pollution).
    pub async fn list_tools_for(&self, server_name: &str) -> Result<Vec<(String, McpToolDef)>> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{}' not found or not running", server_name))?;

        let tools = client.list_tools().await?;
        Ok(tools
            .into_iter()
            .map(|t| {
                let prefixed = format_mcp_tool_name(server_name, &t.name);
                (prefixed, t)
            })
            .collect())
    }

    /// Call a tool by its prefixed name (`mcp__server__tool`).
    pub async fn call_tool(
        &self,
        prefixed_name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolResult> {
        let (server_name, tool_name) =
            parse_mcp_tool_name(prefixed_name).context("Invalid MCP tool name")?;

        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(&server_name)
            .with_context(|| format!("MCP server '{server_name}' not found or not running"))?;

        client.call_tool(&tool_name, arguments).await
    }

    /// List resources from all running servers.
    pub async fn list_all_resources(&self) -> Result<Vec<(String, McpResource)>> {
        let mut servers = self.servers.write().await;
        let mut all_resources = Vec::new();

        for (server_name, client) in servers.iter_mut() {
            match client.list_resources().await {
                Ok(resources) => {
                    for resource in resources {
                        all_resources.push((server_name.clone(), resource));
                    }
                }
                Err(e) => {
                    warn!("Failed to list resources from MCP server '{}': {}", server_name, e);
                }
            }
        }

        Ok(all_resources)
    }

    /// Read a resource by URI from a specific server.
    pub async fn read_resource(&self, server_name: &str, uri: &str) -> Result<Vec<McpContent>> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{server_name}' not found"))?;

        client.read_resource(uri).await
    }

    /// Get the names of all running servers.
    pub async fn running_servers(&self) -> Vec<String> {
        let servers = self.servers.read().await;
        servers.keys().cloned().collect()
    }

    /// Alias for `running_servers()` — backwards compatible.
    pub async fn server_names(&self) -> Vec<String> {
        self.running_servers().await
    }

    /// Call a tool directly by server name and tool name.
    pub async fn call_tool_direct(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolResult> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{server_name}' not found or not running"))?;

        client.call_tool(tool_name, arguments).await
    }

    /// List resources from a specific server by name.
    pub async fn list_resources_for(&self, server_name: &str) -> Result<Vec<McpResource>> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{server_name}' not found"))?;

        client.list_resources().await
    }

    /// Connect to an MCP server from config and register it (backwards-compat).
    pub async fn connect_server(&self, config: &McpServerConfig) -> Result<()> {
        self.start_server(config).await
    }

    /// Disconnect a server by name (backwards-compat).
    pub async fn disconnect_server(&self, name: &str) -> Result<()> {
        self.stop_server(name).await
    }

    /// Disconnect all servers (backwards-compat).
    pub async fn disconnect_all(&self) -> Result<()> {
        self.stop_all().await
    }

    /// Number of running servers.
    pub async fn server_count(&self) -> usize {
        let servers = self.servers.read().await;
        servers.len()
    }

    /// Check if any servers are configured.
    pub async fn has_configs(&self) -> bool {
        let configs = self.configs.read().await;
        !configs.is_empty()
    }

    /// Health check — remove dead servers.
    pub async fn cleanup_dead_servers(&self) {
        let mut servers = self.servers.write().await;
        let dead: Vec<String> = {
            let mut dead_names = Vec::new();
            for (name, client) in servers.iter_mut() {
                if !client.is_alive() {
                    dead_names.push(name.clone());
                }
            }
            dead_names
        };

        for name in &dead {
            warn!("MCP server '{}' is dead, removing", name);
            if let Some(mut client) = servers.remove(name) {
                // Timeout to prevent holding write lock indefinitely
                let close_timeout = std::time::Duration::from_secs(5);
                if tokio::time::timeout(close_timeout, client.close()).await.is_err() {
                    warn!("MCP server '{}' close timed out after {}s", name, close_timeout.as_secs());
                }
            }
        }
    }

    /// Refresh tools for a specific server (after `list_changed` notification).
    pub async fn refresh_tools(&self, server_name: &str) -> Result<Vec<McpToolDef>> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{server_name}' not found"))?;

        client.handle_tool_list_changed().await
    }
}

// ── Tool name utilities ──────────────────────────────────────────────────────

/// Format an MCP tool name: `mcp__<server>__<tool>`.
#[must_use] 
pub fn format_mcp_tool_name(server_name: &str, tool_name: &str) -> String {
    format!("{MCP_TOOL_PREFIX}{server_name}__{tool_name}")
}

/// Parse an MCP tool name: `mcp__<server>__<tool>` → (server, tool).
#[must_use] 
pub fn parse_mcp_tool_name(prefixed: &str) -> Option<(String, String)> {
    let rest = prefixed.strip_prefix(MCP_TOOL_PREFIX)?;
    let sep_pos = rest.find("__")?;
    let server = rest[..sep_pos].to_string();
    let tool = rest[sep_pos + 2..].to_string();
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

/// Check if a tool name is an MCP proxy tool.
#[must_use] 
pub fn is_mcp_tool(name: &str) -> bool {
    name.starts_with(MCP_TOOL_PREFIX)
}

// ── Config loading utilities ─────────────────────────────────────────────────

/// Load MCP server configs from a `.mcp.json` file.
pub fn load_mcp_configs(path: &std::path::Path) -> Result<Vec<McpServerConfig>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read MCP config: {}", path.display()))?;

    let parsed: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Invalid JSON in MCP config: {}", path.display()))?;

    let servers = parsed
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .context("Missing 'mcpServers' in MCP config")?;

    let mut configs = Vec::new();
    for (name, config) in servers {
        let command = config["command"]
            .as_str()
            .with_context(|| format!("Missing 'command' for MCP server '{name}'"))?
            .to_string();

        let args: Vec<String> = config
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let env: HashMap<String, String> = config
            .get("env")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        configs.push(McpServerConfig {
            name: name.clone(),
            command,
            args,
            env,
        });
    }

    info!("Loaded {} MCP server configs from {}", configs.len(), path.display());
    Ok(configs)
}

/// Discover `.mcp.json` files in standard locations.
#[must_use] 
pub fn discover_mcp_configs(cwd: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();

    // Project-level: <cwd>/.mcp.json
    let project = cwd.join(".mcp.json");
    if project.exists() {
        paths.push(project);
    }

    // Project-level: <cwd>/.claude/mcp.json
    let project_claude = cwd.join(".claude").join("mcp.json");
    if project_claude.exists() {
        paths.push(project_claude);
    }

    // Walk up ancestors for .claude/mcp.json (stop at filesystem root)
    let mut ancestor = cwd.parent();
    while let Some(dir) = ancestor {
        let ancestor_path = dir.join(".claude").join("mcp.json");
        if ancestor_path.exists() {
            paths.push(ancestor_path);
        }
        ancestor = dir.parent();
    }

    // User-level: ~/.claude/.mcp.json (legacy) and ~/.claude/mcp.json
    if let Some(home) = dirs::home_dir() {
        let user_legacy = home.join(".claude").join(".mcp.json");
        if user_legacy.exists() {
            paths.push(user_legacy);
        }
        let user_new = home.join(".claude").join("mcp.json");
        if user_new.exists() && !paths.contains(&user_new) {
            paths.push(user_new);
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tool_name() {
        assert_eq!(format_mcp_tool_name("fs", "readFile"), "mcp__fs__readFile");
    }

    #[test]
    fn parse_tool_name() {
        assert_eq!(
            parse_mcp_tool_name("mcp__fs__readFile"),
            Some(("fs".to_string(), "readFile".to_string()))
        );
    }

    #[test]
    fn parse_invalid_name() {
        assert_eq!(parse_mcp_tool_name("not_mcp"), None);
        assert_eq!(parse_mcp_tool_name("mcp__"), None);
        assert_eq!(parse_mcp_tool_name("mcp____tool"), None);
    }

    #[test]
    fn is_mcp_tool_check() {
        assert!(is_mcp_tool("mcp__fs__readFile"));
        assert!(!is_mcp_tool("FileReadTool"));
    }

    #[test]
    fn parse_tool_name_with_double_underscore_in_name() {
        let result = parse_mcp_tool_name("mcp__my_server__read__file");
        assert_eq!(result, Some(("my_server".to_string(), "read__file".to_string())));
    }

    #[tokio::test]
    async fn manager_new_has_no_servers() {
        let mgr = McpManager::new();
        assert_eq!(mgr.server_count().await, 0);
        assert!(!mgr.has_configs().await);
    }

    #[tokio::test]
    async fn manager_load_configs() {
        let mgr = McpManager::new();
        let configs = vec![McpServerConfig {
            name: "test".to_string(),
            command: "echo".to_string(),
            args: vec![],
            env: HashMap::new(),
        }];
        mgr.load_configs(configs).await;
        assert!(mgr.has_configs().await);
    }
}
