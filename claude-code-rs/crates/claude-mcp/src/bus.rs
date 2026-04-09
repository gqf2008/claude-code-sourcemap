//! MCP Bus Adapter — bridges `McpManager` to the `EventBus`.
//!
//! `McpBusAdapter` is used by the `AgentCoreAdapter` to handle MCP-related
//! `AgentRequest` variants and emit `AgentNotification`s for MCP lifecycle events.
//!
//! It is **not** a separate bus client — it's a service that the bus adapter
//! delegates to when processing MCP requests.

use std::collections::HashMap;

use anyhow::Result;
use tracing::{error, info};

use claude_bus::events::{AgentNotification, McpServerInfo};

use crate::registry::McpManager;
use crate::types::McpServerConfig;

/// Bridges MCP operations to the `EventBus` notification system.
///
/// The adapter owns an `McpManager` and provides methods that:
/// 1. Execute MCP operations (connect, disconnect, list)
/// 2. Return `AgentNotification`s to be emitted on the bus
pub struct McpBusAdapter {
    manager: McpManager,
}

impl McpBusAdapter {
    /// Create a new adapter wrapping an `McpManager`.
    #[must_use] 
    pub const fn new(manager: McpManager) -> Self {
        Self { manager }
    }

    /// Create with a pre-loaded set of configs.
    pub async fn with_configs(configs: Vec<McpServerConfig>) -> Self {
        let manager = McpManager::new();
        manager.load_configs(configs).await;
        Self { manager }
    }

    /// Connect to an MCP server, return notification to emit.
    pub async fn connect(
        &self,
        name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> AgentNotification {
        let config = McpServerConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: args.to_vec(),
            env: env.clone(),
        };

        match self.manager.start_server(&config).await {
            Ok(()) => {
                let tool_count = self
                    .manager
                    .list_all_tools()
                    .await
                    .map(|tools| {
                        tools
                            .iter()
                            .filter(|(prefixed, _)| prefixed.starts_with(&format!("mcp__{name}__")))
                            .count()
                    })
                    .unwrap_or(0);

                info!("MCP server '{}' connected with {} tools", name, tool_count);
                AgentNotification::McpServerConnected {
                    name: name.to_string(),
                    tool_count,
                }
            }
            Err(e) => {
                error!("Failed to connect MCP server '{}': {}", name, e);
                AgentNotification::McpServerError {
                    name: name.to_string(),
                    error: format!("{e:#}"),
                }
            }
        }
    }

    /// Disconnect an MCP server, return notification to emit.
    pub async fn disconnect(&self, name: &str) -> AgentNotification {
        match self.manager.stop_server(name).await {
            Ok(()) => {
                info!("MCP server '{}' disconnected", name);
                AgentNotification::McpServerDisconnected {
                    name: name.to_string(),
                }
            }
            Err(e) => {
                error!("Failed to disconnect MCP server '{}': {}", name, e);
                AgentNotification::McpServerError {
                    name: name.to_string(),
                    error: format!("{e:#}"),
                }
            }
        }
    }

    /// List all connected servers, return notification with server info.
    pub async fn list_servers(&self) -> AgentNotification {
        let server_names = self.manager.running_servers().await;
        let mut servers = Vec::new();

        for name in &server_names {
            let tool_count = self
                .manager
                .list_all_tools()
                .await
                .map(|tools| {
                    tools
                        .iter()
                        .filter(|(prefixed, _)| prefixed.starts_with(&format!("mcp__{name}__")))
                        .count()
                })
                .unwrap_or(0);

            servers.push(McpServerInfo {
                name: name.clone(),
                tool_count,
                connected: true,
            });
        }

        AgentNotification::McpServerList { servers }
    }

    /// Start all configured servers, return notifications for each.
    pub async fn start_all_configured(&self) -> Vec<AgentNotification> {
        let mut notifications = Vec::new();

        if let Err(e) = self.manager.start_all().await {
            notifications.push(AgentNotification::McpServerError {
                name: "*".to_string(),
                error: format!("Failed to start MCP servers: {e:#}"),
            });
            return notifications;
        }

        let server_names = self.manager.running_servers().await;
        for name in &server_names {
            let tool_count = self
                .manager
                .list_all_tools()
                .await
                .map(|tools| {
                    tools
                        .iter()
                        .filter(|(prefixed, _)| prefixed.starts_with(&format!("mcp__{name}__")))
                        .count()
                })
                .unwrap_or(0);

            notifications.push(AgentNotification::McpServerConnected {
                name: name.clone(),
                tool_count,
            });
        }

        notifications
    }

    /// Stop all running servers, return notifications.
    pub async fn stop_all(&self) -> Vec<AgentNotification> {
        let names = self.manager.running_servers().await;
        let mut notifications = Vec::new();

        if let Err(e) = self.manager.stop_all().await {
            notifications.push(AgentNotification::McpServerError {
                name: "*".to_string(),
                error: format!("Failed to stop MCP servers: {e:#}"),
            });
        } else {
            for name in names {
                notifications.push(AgentNotification::McpServerDisconnected { name });
            }
        }

        notifications
    }

    /// Run health check, clean up dead servers, return notifications for any removed.
    pub async fn health_check(&self) -> Vec<AgentNotification> {
        let before = self.manager.running_servers().await;
        self.manager.cleanup_dead_servers().await;
        let after = self.manager.running_servers().await;

        let mut notifications = Vec::new();
        for name in &before {
            if !after.contains(name) {
                notifications.push(AgentNotification::McpServerDisconnected {
                    name: name.clone(),
                });
            }
        }

        notifications
    }

    /// Get a reference to the underlying `McpManager`.
    #[must_use] 
    pub const fn manager(&self) -> &McpManager {
        &self.manager
    }

    /// Load new configs into the manager.
    pub async fn load_configs(&self, configs: Vec<McpServerConfig>) {
        self.manager.load_configs(configs).await;
    }

    /// Discover and load configs from standard locations.
    pub async fn discover_and_load(&self, cwd: &std::path::Path) -> Result<Vec<AgentNotification>> {
        let config_paths = crate::discover_mcp_configs(cwd);
        let mut all_configs = Vec::new();

        for path in &config_paths {
            match crate::load_mcp_configs(path) {
                Ok(configs) => all_configs.extend(configs),
                Err(e) => {
                    error!("Failed to load MCP configs from {}: {}", path.display(), e);
                }
            }
        }

        if all_configs.is_empty() {
            return Ok(Vec::new());
        }

        self.manager.load_configs(all_configs).await;
        let notifications = self.start_all_configured().await;
        Ok(notifications)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn adapter_new() {
        let adapter = McpBusAdapter::new(McpManager::new());
        let list = adapter.list_servers().await;
        match list {
            AgentNotification::McpServerList { servers } => {
                assert!(servers.is_empty());
            }
            other => panic!("Expected McpServerList, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn adapter_with_configs() {
        let configs = vec![McpServerConfig {
            name: "test".to_string(),
            command: "echo".to_string(),
            args: vec![],
            env: HashMap::new(),
        }];
        let adapter = McpBusAdapter::with_configs(configs).await;
        assert!(adapter.manager().has_configs().await);
    }

    #[tokio::test]
    async fn connect_nonexistent_returns_error() {
        let adapter = McpBusAdapter::new(McpManager::new());
        let notification = adapter
            .connect("fake", "nonexistent_command_12345", &[], &HashMap::new())
            .await;

        match notification {
            AgentNotification::McpServerError { name, error } => {
                assert_eq!(name, "fake");
                assert!(!error.is_empty());
            }
            other => panic!("Expected McpServerError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnect_nonexistent_succeeds() {
        let adapter = McpBusAdapter::new(McpManager::new());
        // stop_server on nonexistent is a no-op (returns Ok)
        let notification = adapter.disconnect("nonexistent").await;
        match notification {
            AgentNotification::McpServerDisconnected { name } => {
                assert_eq!(name, "nonexistent");
            }
            other => panic!("Expected McpServerDisconnected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn health_check_empty() {
        let adapter = McpBusAdapter::new(McpManager::new());
        let notifications = adapter.health_check().await;
        assert!(notifications.is_empty());
    }

    #[tokio::test]
    async fn stop_all_empty() {
        let adapter = McpBusAdapter::new(McpManager::new());
        let notifications = adapter.stop_all().await;
        assert!(notifications.is_empty());
    }

    #[tokio::test]
    async fn start_all_configured_no_configs() {
        let adapter = McpBusAdapter::new(McpManager::new());
        let notifications = adapter.start_all_configured().await;
        assert!(notifications.is_empty());
    }

    #[tokio::test]
    async fn discover_and_load_no_configs() {
        let adapter = McpBusAdapter::new(McpManager::new());
        let dir = std::env::temp_dir().join("mcp_test_no_configs");
        std::fs::create_dir_all(&dir).ok();
        let notifications = adapter.discover_and_load(&dir).await.unwrap();
        assert!(notifications.is_empty());
    }

    #[tokio::test]
    async fn load_configs_updates_manager() {
        let adapter = McpBusAdapter::new(McpManager::new());
        assert!(!adapter.manager().has_configs().await);

        adapter
            .load_configs(vec![McpServerConfig {
                name: "test".to_string(),
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
            }])
            .await;

        assert!(adapter.manager().has_configs().await);
    }
}
