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

/// Trait for in-process MCP servers (no subprocess transport needed).
///
/// Allows registering servers that run in the same process (e.g. Computer Use)
/// alongside normal MCP servers managed via stdio/SSE transport.
pub trait BuiltinMcpServer: Send + Sync {
    /// The server name used for tool routing (e.g. "computer-use").
    fn server_name(&self) -> &str;

    /// List available tools (same semantics as MCP `tools/list`).
    fn list_tools(&self) -> Vec<McpToolDef>;

    /// Call a tool by name (same semantics as MCP `tools/call`).
    fn call_tool(&self, tool_name: &str, input: serde_json::Value) -> McpToolResult;
}

/// Manages multiple MCP server connections and built-in in-process servers.
pub struct McpManager {
    servers: Arc<RwLock<HashMap<String, McpClient>>>,
    configs: Arc<RwLock<Vec<McpServerConfig>>>,
    /// In-process servers that don't need subprocess transport.
    builtin_servers: Arc<RwLock<HashMap<String, Arc<dyn BuiltinMcpServer>>>>,
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
            builtin_servers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an in-process MCP server.
    pub async fn register_builtin(&self, server: Arc<dyn BuiltinMcpServer>) {
        let name = server.server_name().to_string();
        let tool_count = server.list_tools().len();
        let mut builtins = self.builtin_servers.write().await;
        builtins.insert(name.clone(), server);
        info!("Registered built-in MCP server '{name}' with {tool_count} tools");
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

    /// List all available tools from all running servers (including builtins), with prefixed names.
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
        drop(servers);

        // Include builtin servers
        let builtins = self.builtin_servers.read().await;
        for (server_name, server) in builtins.iter() {
            for tool in server.list_tools() {
                let prefixed = format_mcp_tool_name(server_name, &tool.name);
                all_tools.push((prefixed, tool));
            }
        }

        Ok(all_tools)
    }

    /// List tools from a specific server only (avoids cross-server pollution).
    /// Checks builtin servers first, then external MCP servers.
    pub async fn list_tools_for(&self, server_name: &str) -> Result<Vec<(String, McpToolDef)>> {
        // Check builtin first
        {
            let builtins = self.builtin_servers.read().await;
            if let Some(server) = builtins.get(server_name) {
                let tools = server.list_tools();
                return Ok(tools
                    .into_iter()
                    .map(|t| {
                        let prefixed = format_mcp_tool_name(server_name, &t.name);
                        (prefixed, t)
                    })
                    .collect());
            }
        }

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

    /// Get the names of all running servers (including builtins).
    pub async fn running_servers(&self) -> Vec<String> {
        let servers = self.servers.read().await;
        let builtins = self.builtin_servers.read().await;
        let mut names: Vec<String> = servers.keys().cloned().collect();
        names.extend(builtins.keys().cloned());
        names
    }

    /// Alias for `running_servers()` — backwards compatible.
    pub async fn server_names(&self) -> Vec<String> {
        self.running_servers().await
    }

    /// Call a tool directly by server name and tool name.
    ///
    /// Checks built-in servers first, then external MCP servers.
    pub async fn call_tool_direct(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolResult> {
        // Check builtin servers first
        {
            let builtins = self.builtin_servers.read().await;
            if let Some(server) = builtins.get(server_name) {
                let server = server.clone();
                drop(builtins);
                // Run sync call_tool on blocking thread to avoid blocking the runtime
                let tool = tool_name.to_string();
                let result = tokio::task::spawn_blocking(move || {
                    server.call_tool(&tool, arguments)
                }).await?;
                return Ok(result);
            }
        }

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

    /// List all prompts across all connected MCP servers.
    /// Returns tuples of (server_name, prompt).
    pub async fn list_all_prompts(&self) -> Result<Vec<(String, crate::types::McpPrompt)>> {
        let mut all = Vec::new();
        let mut servers = self.servers.write().await;

        for (name, client) in servers.iter_mut() {
            match client.list_prompts().await {
                Ok(prompts) => {
                    for prompt in prompts {
                        all.push((name.clone(), prompt));
                    }
                }
                Err(e) => {
                    warn!("Failed to list prompts from MCP server '{}': {}", name, e);
                }
            }
        }

        Ok(all)
    }

    /// Get a prompt from a specific MCP server.
    pub async fn get_prompt(
        &self,
        server_name: &str,
        prompt_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<Vec<crate::types::McpPromptMessage>> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{server_name}' not found"))?;

        client.get_prompt(prompt_name, arguments).await
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

    /// Number of running servers (including builtins).
    pub async fn server_count(&self) -> usize {
        let servers = self.servers.read().await;
        let builtins = self.builtin_servers.read().await;
        servers.len() + builtins.len()
    }

    /// Check if any servers are configured.
    pub async fn has_configs(&self) -> bool {
        let configs = self.configs.read().await;
        !configs.is_empty()
    }

    /// Check if a server name corresponds to a built-in in-process server.
    pub async fn is_builtin_server(&self, server_name: &str) -> bool {
        let builtins = self.builtin_servers.read().await;
        builtins.contains_key(server_name)
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
                let close_timeout = std::time::Duration::from_secs(5);
                if tokio::time::timeout(close_timeout, client.close()).await.is_err() {
                    warn!("MCP server '{}' close timed out after {}s", name, close_timeout.as_secs());
                }
            }
        }
    }

    /// Auto-reconnect dead servers using stored configs.
    ///
    /// Returns the names of servers that were successfully reconnected.
    pub async fn reconnect_dead_servers(&self) -> Vec<String> {
        // First identify dead servers
        let dead_names: Vec<String> = {
            let mut servers = self.servers.write().await;
            let mut dead = Vec::new();
            for (name, client) in servers.iter_mut() {
                if !client.is_alive() {
                    dead.push(name.clone());
                }
            }
            // Remove dead clients
            for name in &dead {
                if let Some(mut client) = servers.remove(name) {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        client.close(),
                    ).await;
                }
            }
            dead
        };

        if dead_names.is_empty() {
            return Vec::new();
        }

        // Find configs for dead servers and try to reconnect
        let configs = self.configs.read().await;
        let mut reconnected = Vec::new();

        for name in &dead_names {
            if let Some(config) = configs.iter().find(|c| &c.name == name) {
                info!("Attempting to reconnect MCP server '{}'", name);
                match McpClient::connect(config).await {
                    Ok(client) => {
                        let mut servers = self.servers.write().await;
                        servers.insert(name.clone(), client);
                        info!("MCP server '{}' reconnected successfully", name);
                        reconnected.push(name.clone());
                    }
                    Err(e) => {
                        warn!("Failed to reconnect MCP server '{}': {}", name, e);
                    }
                }
            } else {
                warn!("No config found for dead MCP server '{}', cannot reconnect", name);
            }
        }

        reconnected
    }

    /// Spawn a background health monitor that periodically checks server health
    /// and attempts to reconnect dead servers.
    ///
    /// Returns a `JoinHandle` for the background task. Drop the handle or
    /// abort it to stop monitoring.
    pub fn spawn_health_monitor(
        self: &Arc<Self>,
        interval: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let reconnected = manager.reconnect_dead_servers().await;
                if !reconnected.is_empty() {
                    info!("Health monitor reconnected {} servers: {:?}", reconnected.len(), reconnected);
                }
            }
        })
    }

    /// Subscribe to resource updates on a specific server.
    pub async fn subscribe_resource(&self, server_name: &str, uri: &str) -> Result<()> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{server_name}' not found"))?;
        client.subscribe_resource(uri).await
    }

    /// Unsubscribe from resource updates on a specific server.
    pub async fn unsubscribe_resource(&self, server_name: &str, uri: &str) -> Result<()> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{server_name}' not found"))?;
        client.unsubscribe_resource(uri).await
    }

    /// Send an elicitation request through a specific server.
    pub async fn create_elicitation(
        &self,
        server_name: &str,
        message: &str,
        requested_schema: serde_json::Value,
    ) -> Result<crate::types::ElicitationResponse> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{server_name}' not found"))?;
        client.create_elicitation(message, requested_schema).await
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

    // ── Additional registry tests ────────────────────────────────────────────

    #[tokio::test]
    async fn manager_load_replaces_configs() {
        let mgr = McpManager::new();
        let c1 = vec![McpServerConfig {
            name: "a".into(),
            command: "echo".into(),
            args: vec![],
            env: HashMap::new(),
        }];
        mgr.load_configs(c1).await;
        assert!(mgr.has_configs().await);

        let c2 = vec![];
        mgr.load_configs(c2).await;
        assert!(!mgr.has_configs().await);
    }

    #[tokio::test]
    async fn manager_server_count_tracks_correctly() {
        let mgr = McpManager::new();
        assert_eq!(mgr.server_count().await, 0);
        // Can't start real servers, but verify empty state
        let result = mgr.list_all_tools().await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn manager_stop_nonexistent_is_ok() {
        let mgr = McpManager::new();
        let result = mgr.stop_server("nonexistent").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn manager_call_tool_unknown_server() {
        let mgr = McpManager::new();
        let err = mgr.call_tool("mcp__unknown__readFile", serde_json::json!({})).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn manager_call_tool_invalid_name() {
        let mgr = McpManager::new();
        let err = mgr.call_tool("invalid_name", serde_json::json!({})).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn manager_list_tools_for_unknown_server() {
        let mgr = McpManager::new();
        let err = mgr.list_tools_for("missing").await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn load_mcp_configs_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join(".mcp.json");
        std::fs::write(&config_path, r#"{
            "mcpServers": {
                "fs": {
                    "command": "npx",
                    "args": ["-y", "@mcp/fs"],
                    "env": {"NODE_ENV": "production"}
                },
                "git": {
                    "command": "mcp-git"
                }
            }
        }"#).unwrap();

        let configs = load_mcp_configs(&config_path).unwrap();
        assert_eq!(configs.len(), 2);
        let fs = configs.iter().find(|c| c.name == "fs").unwrap();
        assert_eq!(fs.command, "npx");
        assert_eq!(fs.args, vec!["-y", "@mcp/fs"]);
        assert_eq!(fs.env.get("NODE_ENV").unwrap(), "production");

        let git = configs.iter().find(|c| c.name == "git").unwrap();
        assert_eq!(git.command, "mcp-git");
        assert!(git.args.is_empty());
    }

    #[test]
    fn load_mcp_configs_missing_command() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("mcp.json");
        std::fs::write(&config_path, r#"{
            "mcpServers": {
                "bad": {}
            }
        }"#).unwrap();

        let err = load_mcp_configs(&config_path);
        assert!(err.is_err());
    }

    #[test]
    fn load_mcp_configs_missing_servers_key() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("mcp.json");
        std::fs::write(&config_path, r#"{"version": 1}"#).unwrap();

        let err = load_mcp_configs(&config_path);
        assert!(err.is_err());
    }

    #[test]
    fn load_mcp_configs_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("mcp.json");
        std::fs::write(&config_path, "not json").unwrap();

        let err = load_mcp_configs(&config_path);
        assert!(err.is_err());
    }

    #[test]
    fn discover_mcp_configs_finds_project_level() {
        let dir = tempfile::tempdir().unwrap();
        let mcp_file = dir.path().join(".mcp.json");
        std::fs::write(&mcp_file, "{}").unwrap();

        let found = discover_mcp_configs(dir.path());
        assert!(found.iter().any(|p| p.file_name().unwrap() == ".mcp.json"));
    }

    #[test]
    fn discover_mcp_configs_finds_claude_dir() {
        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("mcp.json"), "{}").unwrap();

        let found = discover_mcp_configs(dir.path());
        assert!(!found.is_empty());
    }

    #[test]
    fn discover_mcp_configs_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let found = discover_mcp_configs(dir.path());
        // Should not find any project-level configs (home-level may exist)
        let project_configs: Vec<_> = found.iter()
            .filter(|p| p.starts_with(dir.path()))
            .collect();
        assert!(project_configs.is_empty());
    }

    #[test]
    fn format_and_parse_roundtrip() {
        let formatted = format_mcp_tool_name("my_server", "read_file");
        let (server, tool) = parse_mcp_tool_name(&formatted).unwrap();
        assert_eq!(server, "my_server");
        assert_eq!(tool, "read_file");
    }

    #[tokio::test]
    async fn reconnect_dead_servers_empty() {
        let mgr = McpManager::new();
        let reconnected = mgr.reconnect_dead_servers().await;
        assert!(reconnected.is_empty());
    }

    #[tokio::test]
    async fn subscribe_resource_unknown_server() {
        let mgr = McpManager::new();
        let err = mgr.subscribe_resource("nonexistent", "file:///x").await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn unsubscribe_resource_unknown_server() {
        let mgr = McpManager::new();
        let err = mgr.unsubscribe_resource("nonexistent", "file:///x").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn create_elicitation_unknown_server() {
        let mgr = McpManager::new();
        let err = mgr.create_elicitation("missing", "hi", serde_json::json!({})).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn health_monitor_can_be_aborted() {
        let mgr = Arc::new(McpManager::new());
        let handle = mgr.spawn_health_monitor(std::time::Duration::from_millis(50));
        // Let it run briefly
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        handle.abort();
        let result = handle.await;
        assert!(result.is_err()); // JoinError from abort
    }
}
