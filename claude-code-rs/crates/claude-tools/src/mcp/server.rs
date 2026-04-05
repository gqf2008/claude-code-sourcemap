//! MCP server manager — tracks connected servers and provides dynamic tool proxy.
//!
//! The `McpManager` connects to multiple MCP servers, discovers their tools,
//! and creates `DynTool` proxies that can be registered in the ToolRegistry.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{info, warn};

use super::client::{McpClient, McpServerConfig, McpToolDef, McpToolResult};

// ── Server Manager ───────────────────────────────────────────────────────────

/// Manages multiple MCP server connections.
pub struct McpManager {
    servers: Arc<RwLock<HashMap<String, McpClient>>>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Connect to an MCP server and register it.
    pub async fn connect_server(&self, config: &McpServerConfig) -> Result<()> {
        let client = McpClient::connect(config).await?;
        self.servers
            .write()
            .await
            .insert(config.name.clone(), client);
        Ok(())
    }

    /// Disconnect a specific server.
    pub async fn disconnect_server(&self, name: &str) -> Result<()> {
        let mut servers = self.servers.write().await;
        if let Some(mut client) = servers.remove(name) {
            client.close().await?;
        }
        Ok(())
    }

    /// Disconnect all servers.
    pub async fn disconnect_all(&self) -> Result<()> {
        let mut servers = self.servers.write().await;
        for (name, mut client) in servers.drain() {
            if let Err(e) = client.close().await {
                warn!("Error disconnecting MCP server '{}': {}", name, e);
            }
        }
        Ok(())
    }

    /// List all connected server names.
    pub async fn server_names(&self) -> Vec<String> {
        self.servers.read().await.keys().cloned().collect()
    }

    /// Get tools from all connected servers, with fully-qualified names.
    pub async fn all_tools(&self) -> Result<Vec<(String, McpToolDef)>> {
        let mut all = Vec::new();
        let mut servers = self.servers.write().await;
        for (server_name, client) in servers.iter_mut() {
            match client.list_tools().await {
                Ok(tools) => {
                    for tool in tools {
                        all.push((server_name.clone(), tool));
                    }
                }
                Err(e) => {
                    warn!("Failed to list tools from MCP server '{}': {}", server_name, e);
                }
            }
        }
        Ok(all)
    }

    /// Call a tool on a specific server.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<McpToolResult> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{}' not connected", server_name))?;
        client.call_tool(tool_name, arguments).await
    }

    /// List resources from a specific server.
    pub async fn list_resources(&self, server_name: &str) -> Result<Vec<super::client::McpResource>> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{}' not connected", server_name))?;
        client.list_resources().await
    }

    /// Read a resource from a specific server.
    pub async fn read_resource(
        &self,
        server_name: &str,
        uri: &str,
    ) -> Result<Vec<super::client::McpContent>> {
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .with_context(|| format!("MCP server '{}' not connected", server_name))?;
        client.read_resource(uri).await
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tool Name Convention ─────────────────────────────────────────────────────

/// Build a fully-qualified MCP tool name: `mcp__<server>__<tool>`.
///
/// This matches the TS convention: `mcp__${sanitize(server)}__${sanitize(tool)}`.
pub fn build_mcp_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "mcp__{}__{}",
        sanitize_name(server_name),
        sanitize_name(tool_name)
    )
}

/// Parse a fully-qualified MCP tool name back into (server, tool).
pub fn parse_mcp_tool_name(qualified: &str) -> Option<(&str, &str)> {
    let rest = qualified.strip_prefix("mcp__")?;
    let sep_idx = rest.find("__")?;
    let server = &rest[..sep_idx];
    let tool = &rest[sep_idx + 2..];
    if tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

/// Sanitize a name for use in qualified tool names (replace non-alphanumeric with _).
fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

// ── MCP Config Loading ───────────────────────────────────────────────────────

/// Load MCP server configs from a `.mcp.json` file.
pub fn load_mcp_configs(path: &std::path::Path) -> Result<Vec<McpServerConfig>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read MCP config: {}", path.display()))?;

    let parsed: Value = serde_json::from_str(&content)
        .with_context(|| format!("Invalid JSON in MCP config: {}", path.display()))?;

    let servers = parsed
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .context("Missing 'mcpServers' in MCP config")?;

    let mut configs = Vec::new();
    for (name, config) in servers {
        let command = config["command"]
            .as_str()
            .with_context(|| format!("Missing 'command' for MCP server '{}'", name))?
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
pub fn discover_mcp_configs(cwd: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();

    // Project-level: <cwd>/.mcp.json
    let project = cwd.join(".mcp.json");
    if project.exists() {
        paths.push(project);
    }

    // User-level: ~/.claude/.mcp.json
    if let Some(home) = dirs::home_dir() {
        let user = home.join(".claude").join(".mcp.json");
        if user.exists() {
            paths.push(user);
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_mcp_tool_name() {
        assert_eq!(
            build_mcp_tool_name("my-server", "read_file"),
            "mcp__my-server__read_file"
        );
        assert_eq!(
            build_mcp_tool_name("test server", "do.thing"),
            "mcp__test_server__do_thing"
        );
    }

    #[test]
    fn test_parse_mcp_tool_name() {
        assert_eq!(
            parse_mcp_tool_name("mcp__my-server__read_file"),
            Some(("my-server", "read_file"))
        );
        assert_eq!(parse_mcp_tool_name("not_mcp_tool"), None);
        assert_eq!(parse_mcp_tool_name("mcp__server__"), None);
    }

    #[test]
    fn test_parse_roundtrip() {
        let server = "github";
        let tool = "create_issue";
        let qualified = build_mcp_tool_name(server, tool);
        let (s, t) = parse_mcp_tool_name(&qualified).unwrap();
        assert_eq!(s, server);
        assert_eq!(t, tool);
    }

    #[test]
    fn test_load_mcp_configs_parsing() {
        let tmp = std::env::temp_dir().join("test_mcp_config.json");
        std::fs::write(
            &tmp,
            r#"{
                "mcpServers": {
                    "github": {
                        "command": "npx",
                        "args": ["-y", "@github/mcp-server"],
                        "env": {"GH_TOKEN": "test123"}
                    },
                    "filesystem": {
                        "command": "mcp-fs",
                        "args": ["/home/user"]
                    }
                }
            }"#,
        )
        .unwrap();

        let configs = load_mcp_configs(&tmp).unwrap();
        assert_eq!(configs.len(), 2);

        let github = configs.iter().find(|c| c.name == "github").unwrap();
        assert_eq!(github.command, "npx");
        assert_eq!(github.args, vec!["-y", "@github/mcp-server"]);
        assert_eq!(github.env.get("GH_TOKEN"), Some(&"test123".to_string()));

        let fs = configs.iter().find(|c| c.name == "filesystem").unwrap();
        assert_eq!(fs.command, "mcp-fs");
        assert_eq!(fs.args, vec!["/home/user"]);
        assert!(fs.env.is_empty());

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_sanitize_name() {
        assert_eq!(sanitize_name("hello-world"), "hello-world");
        assert_eq!(sanitize_name("my server"), "my_server");
        assert_eq!(sanitize_name("a/b.c"), "a_b_c");
    }
}
