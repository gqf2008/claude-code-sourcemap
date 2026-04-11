//! Swarm tool bridge — wraps `SwarmMcpServer` as `Tool` trait implementations
//! for the agent's tool registry.
//!
//! Each swarm tool is a thin wrapper that delegates to `SwarmMcpServer::call_tool()`.
//! The server is shared via `Arc` across all tools.

use std::sync::Arc;

use async_trait::async_trait;
use clawed_core::message::ToolResultContent;
use clawed_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::Value;
use tracing::info;

use crate::SwarmMcpServer;

/// Shared swarm server instance.
type SharedSwarmServer = Arc<SwarmMcpServer>;

/// Register all Swarm tools into the given tool registry.
///
/// Registers:
/// 1. MCP bridge tools from `SwarmMcpServer` (actor-based, in-memory)
/// 2. Direct team management tools (disk-based, persistent)
pub fn register_swarm_tools(
    registry: &mut clawed_tools::ToolRegistry,
    default_model: &str,
    default_cwd: &str,
) {
    let server = Arc::new(SwarmMcpServer::new(
        default_model.to_string(),
        default_cwd.to_string(),
    ));
    let tool_count = server.list_tools().len();

    for tool_def in server.list_tools() {
        let name = tool_def.name.clone();
        let description = tool_def.description.clone().unwrap_or_default();
        let schema = tool_def
            .input_schema
            .clone()
            .unwrap_or(serde_json::json!({"type": "object"}));

        registry.register(SwarmToolBridge {
            server: server.clone(),
            tool_name: name,
            tool_description: description,
            tool_schema: schema,
        });
    }

    // Register disk-based team management tools
    registry.register(crate::TeamCreateTool);
    registry.register(crate::TeamDeleteTool);
    registry.register(crate::TeamStatusTool);

    info!(count = tool_count + 3, "Swarm tools registered");
}

/// A single swarm tool exposed to the agent.
struct SwarmToolBridge {
    server: SharedSwarmServer,
    tool_name: String,
    tool_description: String,
    tool_schema: Value,
}

#[async_trait]
impl Tool for SwarmToolBridge {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn input_schema(&self) -> Value {
        self.tool_schema.clone()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn is_read_only(&self) -> bool {
        matches!(
            self.tool_name.as_str(),
            "swarm_agent_status" | "swarm_team_status"
        )
    }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let mcp_result = self.server.call_tool(&self.tool_name, input).await;

        let mut content: Vec<ToolResultContent> = Vec::new();

        for c in &mcp_result.content {
            if let Some(text) = &c.text {
                content.push(ToolResultContent::Text { text: text.clone() });
            }
        }

        if content.is_empty() {
            content.push(ToolResultContent::Text {
                text: String::new(),
            });
        }

        Ok(ToolResult {
            content,
            is_error: mcp_result.is_error,
            structured_output: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swarm_tool_bridge_name_and_category() {
        let bridge = SwarmToolBridge {
            server: Arc::new(SwarmMcpServer::new("haiku".into(), "/tmp".into())),
            tool_name: "swarm_create_team".into(),
            tool_description: "Create a team".into(),
            tool_schema: serde_json::json!({"type": "object"}),
        };
        assert_eq!(bridge.name(), "swarm_create_team");
        assert_eq!(bridge.category(), ToolCategory::Agent);
        assert!(!bridge.is_read_only());
    }

    #[test]
    fn swarm_status_tools_are_readonly() {
        let server = Arc::new(SwarmMcpServer::new("haiku".into(), "/tmp".into()));
        let bridge1 = SwarmToolBridge {
            server: server.clone(),
            tool_name: "swarm_agent_status".into(),
            tool_description: "".into(),
            tool_schema: serde_json::json!({}),
        };
        let bridge2 = SwarmToolBridge {
            server,
            tool_name: "swarm_team_status".into(),
            tool_description: "".into(),
            tool_schema: serde_json::json!({}),
        };
        assert!(bridge1.is_read_only());
        assert!(bridge2.is_read_only());
    }

    #[tokio::test]
    async fn swarm_tool_bridge_call() {
        let server = Arc::new(SwarmMcpServer::new("haiku".into(), "/tmp".into()));
        let bridge = SwarmToolBridge {
            server,
            tool_name: "swarm_create_team".into(),
            tool_description: "Create a team".into(),
            tool_schema: serde_json::json!({"type": "object"}),
        };

        let ctx = ToolContext::default();
        let result = bridge
            .call(serde_json::json!({"name": "test-team"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
    }
}
