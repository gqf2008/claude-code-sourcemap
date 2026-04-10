//! MCP server interface for the swarm network.
//!
//! Exposes swarm operations as MCP tools, following the same pattern
//! as `ComputerUseMcpServer` in `claude-computer-use`.

use std::sync::Arc;

use claude_mcp::types::{McpContent, McpToolDef, McpToolResult};
use serde_json::{json, Value};
use tracing::debug;

use crate::network::SwarmNetwork;

/// Server name used when registering with `McpManager`.
pub const SERVER_NAME: &str = "swarm";

/// Helper: create a text `McpContent`.
fn text_content(s: impl Into<String>) -> McpContent {
    McpContent {
        content_type: "text".into(),
        text: Some(s.into()),
        data: None,
        mime_type: None,
    }
}

/// Helper: create a successful `McpToolResult` with text.
fn ok_result(s: impl Into<String>) -> McpToolResult {
    McpToolResult {
        content: vec![text_content(s)],
        is_error: false,
    }
}

/// Helper: create an error `McpToolResult` with text.
fn err_result(s: impl Into<String>) -> McpToolResult {
    McpToolResult {
        content: vec![text_content(s)],
        is_error: true,
    }
}

/// In-process MCP server exposing swarm operations.
pub struct SwarmMcpServer {
    network: Arc<SwarmNetwork>,
}

impl SwarmMcpServer {
    /// Create a new swarm MCP server.
    pub fn new(default_model: String, default_cwd: String) -> Self {
        Self {
            network: Arc::new(SwarmNetwork::new(default_model, default_cwd)),
        }
    }

    /// Get a reference to the underlying network.
    pub fn network(&self) -> &SwarmNetwork {
        &self.network
    }

    /// List available MCP tools.
    pub fn list_tools(&self) -> Vec<McpToolDef> {
        vec![
            McpToolDef {
                name: "swarm_create_team".into(),
                description: Some("Create a new agent team. Returns the team name.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Team name (alphanumeric and hyphens)"
                        }
                    },
                    "required": ["name"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "swarm_delete_team".into(),
                description: Some("Delete a team and terminate all its agents.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Team name to delete"
                        }
                    },
                    "required": ["name"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "swarm_spawn_agent".into(),
                description: Some("Spawn a new agent in a team.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "team_name": { "type": "string", "description": "Team to add agent to" },
                        "agent_name": { "type": "string", "description": "Name for the agent" },
                        "model": { "type": "string", "description": "Optional model override" },
                        "prompt": { "type": "string", "description": "System prompt for the agent" },
                        "cwd": { "type": "string", "description": "Working directory" }
                    },
                    "required": ["team_name", "agent_name"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "swarm_terminate_agent".into(),
                description: Some("Terminate a running agent by ID.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "team_name": { "type": "string" },
                        "agent_id": { "type": "string", "description": "Agent ID (format: name@team)" }
                    },
                    "required": ["team_name", "agent_id"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "swarm_send_message".into(),
                description: Some("Send a message to a specific agent and get its response.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "team_name": { "type": "string" },
                        "target_agent_id": { "type": "string" },
                        "message": { "type": "string" },
                        "from": { "type": "string", "description": "Sender agent ID" }
                    },
                    "required": ["team_name", "target_agent_id", "message"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "swarm_broadcast".into(),
                description: Some("Broadcast a message to all agents in a team (except sender).".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "team_name": { "type": "string" },
                        "message": { "type": "string" },
                        "from": { "type": "string", "description": "Sender agent ID" }
                    },
                    "required": ["team_name", "message", "from"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "swarm_agent_status".into(),
                description: Some("Get the status of a specific agent.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "team_name": { "type": "string" },
                        "agent_id": { "type": "string" }
                    },
                    "required": ["team_name", "agent_id"]
                })),
                annotations: None,
            },
            McpToolDef {
                name: "swarm_team_status".into(),
                description: Some("Get the status of all agents in a team.".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "team_name": { "type": "string" }
                    },
                    "required": ["team_name"]
                })),
                annotations: None,
            },
        ]
    }

    /// Call a tool by name with the given input.
    pub async fn call_tool(&self, name: &str, input: Value) -> McpToolResult {
        debug!(tool = %name, "Swarm MCP tool call");
        match name {
            "swarm_create_team" => self.handle_create_team(input).await,
            "swarm_delete_team" => self.handle_delete_team(input).await,
            "swarm_spawn_agent" => self.handle_spawn_agent(input).await,
            "swarm_terminate_agent" => self.handle_terminate_agent(input).await,
            "swarm_send_message" => self.handle_send_message(input).await,
            "swarm_broadcast" => self.handle_broadcast(input).await,
            "swarm_agent_status" => self.handle_agent_status(input).await,
            "swarm_team_status" => self.handle_team_status(input).await,
            _ => err_result(format!("Unknown tool: {name}")),
        }
    }

    async fn handle_create_team(&self, input: Value) -> McpToolResult {
        let name = input["name"].as_str().unwrap_or("");
        if name.is_empty() {
            return err_result("Missing required field: name");
        }
        match self.network.create_team(name).await {
            Ok(n) => ok_result(format!("Team '{n}' created successfully")),
            Err(e) => err_result(format!("Failed to create team: {e}")),
        }
    }

    async fn handle_delete_team(&self, input: Value) -> McpToolResult {
        let name = input["name"].as_str().unwrap_or("");
        match self.network.delete_team(name).await {
            Ok(()) => ok_result(format!("Team '{name}' deleted")),
            Err(e) => err_result(format!("Failed to delete team: {e}")),
        }
    }

    async fn handle_spawn_agent(&self, input: Value) -> McpToolResult {
        let team_name = input["team_name"].as_str().unwrap_or("");
        let agent_name = input["agent_name"].as_str().unwrap_or("");
        let model = input["model"].as_str().map(String::from);
        let prompt = input["prompt"].as_str().map(String::from);
        let cwd = input["cwd"].as_str().map(String::from);

        if team_name.is_empty() || agent_name.is_empty() {
            return err_result("Missing required fields: team_name, agent_name");
        }

        match self.network.spawn_agent(team_name, agent_name, model, prompt, cwd).await {
            Ok(result) if result.success => {
                ok_result(format!("Agent '{}' spawned in team '{team_name}'", result.agent_id))
            }
            Ok(result) => err_result(result.message),
            Err(e) => err_result(format!("Spawn error: {e}")),
        }
    }

    async fn handle_terminate_agent(&self, input: Value) -> McpToolResult {
        let team_name = input["team_name"].as_str().unwrap_or("");
        let agent_id = input["agent_id"].as_str().unwrap_or("");

        match self.network.terminate_agent(team_name, agent_id).await {
            Ok(r) => McpToolResult {
                content: vec![text_content(&r.message)],
                is_error: !r.success,
            },
            Err(e) => err_result(format!("Terminate error: {e}")),
        }
    }

    async fn handle_send_message(&self, input: Value) -> McpToolResult {
        let team_name = input["team_name"].as_str().unwrap_or("");
        let target = input["target_agent_id"].as_str().unwrap_or("");
        let message = input["message"].as_str().unwrap_or("");
        let from = input["from"].as_str();

        match self.network.send_message(team_name, target, message, from).await {
            Ok(r) if r.success => {
                let text = r.response.map(|r| r.text).unwrap_or_else(|| "No response".into());
                ok_result(text)
            }
            Ok(r) => err_result(r.error.unwrap_or_else(|| "Unknown error".into())),
            Err(e) => err_result(format!("Send error: {e}")),
        }
    }

    async fn handle_broadcast(&self, input: Value) -> McpToolResult {
        let team_name = input["team_name"].as_str().unwrap_or("");
        let message = input["message"].as_str().unwrap_or("");
        let from = input["from"].as_str().unwrap_or("");

        match self.network.broadcast(team_name, message, from).await {
            Ok(results) => {
                let ok = results.iter().filter(|r| r.success).count();
                let total = results.len();
                ok_result(format!("Broadcast delivered to {ok}/{total} agents"))
            }
            Err(e) => err_result(format!("Broadcast error: {e}")),
        }
    }

    async fn handle_agent_status(&self, input: Value) -> McpToolResult {
        let team_name = input["team_name"].as_str().unwrap_or("");
        let agent_id = input["agent_id"].as_str().unwrap_or("");

        match self.network.agent_status(team_name, agent_id).await {
            Ok(status) => ok_result(serde_json::to_string_pretty(&status).unwrap_or_default()),
            Err(e) => err_result(format!("Status error: {e}")),
        }
    }

    async fn handle_team_status(&self, input: Value) -> McpToolResult {
        let team_name = input["team_name"].as_str().unwrap_or("");

        match self.network.team_status(team_name).await {
            Ok(status) => ok_result(serde_json::to_string_pretty(&status).unwrap_or_default()),
            Err(e) => err_result(format!("Team status error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mcp_server_list_tools() {
        let server = SwarmMcpServer::new("claude-haiku".into(), "/tmp".into());
        let tools = server.list_tools();
        assert_eq!(tools.len(), 8);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"swarm_create_team"));
        assert!(names.contains(&"swarm_spawn_agent"));
        assert!(names.contains(&"swarm_send_message"));
        assert!(names.contains(&"swarm_broadcast"));
        assert!(names.contains(&"swarm_team_status"));
    }

    #[tokio::test]
    async fn mcp_server_create_team() {
        let server = SwarmMcpServer::new("claude-haiku".into(), "/tmp".into());
        let r = server.call_tool("swarm_create_team", json!({"name": "test"})).await;
        assert!(!r.is_error);
        assert!(r.text().contains("created"));
    }

    #[tokio::test]
    async fn mcp_server_full_workflow() {
        let server = SwarmMcpServer::new("claude-haiku".into(), "/tmp".into());

        // Create team
        let r = server.call_tool("swarm_create_team", json!({"name": "mcp-test"})).await;
        assert!(!r.is_error);

        // Spawn agent
        let r = server.call_tool("swarm_spawn_agent", json!({
            "team_name": "mcp-test",
            "agent_name": "worker",
            "prompt": "You are a worker"
        })).await;
        assert!(!r.is_error);

        // Send message
        let r = server.call_tool("swarm_send_message", json!({
            "team_name": "mcp-test",
            "target_agent_id": "worker@mcp-test",
            "message": "Hello worker"
        })).await;
        assert!(!r.is_error);
        // response text is the agent reply (may be error message in CI without API key)
        assert!(!r.text().is_empty());

        // Team status
        let r = server.call_tool("swarm_team_status", json!({"team_name": "mcp-test"})).await;
        assert!(!r.is_error);
        assert!(r.text().contains("mcp-test"));

        // Terminate
        let r = server.call_tool("swarm_terminate_agent", json!({
            "team_name": "mcp-test",
            "agent_id": "worker@mcp-test"
        })).await;
        assert!(!r.is_error);

        // Delete team
        let r = server.call_tool("swarm_delete_team", json!({"name": "mcp-test"})).await;
        assert!(!r.is_error);
    }

    #[tokio::test]
    async fn mcp_server_unknown_tool() {
        let server = SwarmMcpServer::new("claude-haiku".into(), "/tmp".into());
        let r = server.call_tool("nonexistent", json!({})).await;
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn mcp_server_missing_team() {
        let server = SwarmMcpServer::new("claude-haiku".into(), "/tmp".into());
        let r = server.call_tool("swarm_spawn_agent", json!({
            "team_name": "nope",
            "agent_name": "agent"
        })).await;
        assert!(r.is_error);
    }
}
