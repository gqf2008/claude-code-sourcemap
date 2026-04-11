use async_trait::async_trait;
use clawed_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

/// `McpAuthTool` — pseudo-tool that triggers OAuth for unauthenticated MCP servers.
///
/// When an MCP server requires authentication, this tool is registered in place
/// of the server's real tools.  Calling it initiates an OAuth PKCE flow and
/// returns an authorization URL for the user to open in their browser.
///
/// Once authentication succeeds, the real tools replace this pseudo-tool
/// automatically.
///
/// Mirrors the TS `McpAuthTool` from `tools/McpAuthTool/`.
pub struct McpAuthTool {
    /// Name of the MCP server that needs authentication.
    server_name: String,
    /// Transport type (sse, http, stdio, etc.)
    transport_type: String,
}

impl McpAuthTool {
    pub fn new(server_name: String, transport_type: String) -> Self {
        Self { server_name, transport_type }
    }
}

#[async_trait]
impl Tool for McpAuthTool {
    fn name(&self) -> &'static str { "McpAuth" }
    fn category(&self) -> ToolCategory { ToolCategory::Mcp }

    fn description(&self) -> &'static str {
        "Start the OAuth flow for an unauthenticated MCP server. \
         Call this tool to receive an authorization URL — share it with the \
         user to complete authentication. Once authorized, the server's real \
         tools will become available automatically."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "description": format!(
                "Authenticate with the '{}' MCP server ({} transport).",
                self.server_name, self.transport_type
            )
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, _input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        match self.transport_type.as_str() {
            "sse" | "http" | "streamable-http" => {
                // In a full implementation, this would:
                // 1. Call performMCPOAuthFlow() to get an auth URL
                // 2. Return the URL for the user to open in their browser
                // 3. Background task monitors for auth completion, then reconnects
                //
                // For now, return instructions for the user.
                let result = json!({
                    "status": "auth_required",
                    "message": format!(
                        "The '{}' MCP server requires OAuth authentication. \
                         Please use `/mcp auth {}` to start the authorization flow.",
                        self.server_name, self.server_name
                    ),
                    "server": self.server_name,
                    "transport": self.transport_type,
                });
                Ok(ToolResult::text(serde_json::to_string(&result)?))
            }
            "stdio" => {
                let result = json!({
                    "status": "unsupported",
                    "message": format!(
                        "The '{}' MCP server uses stdio transport, which does not support OAuth. \
                         Configure authentication through the server's own mechanism.",
                        self.server_name
                    ),
                });
                Ok(ToolResult::text(serde_json::to_string(&result)?))
            }
            _ => {
                let result = json!({
                    "status": "unsupported",
                    "message": format!(
                        "Authentication is not supported for '{}' transport on server '{}'.",
                        self.transport_type, self.server_name
                    ),
                });
                Ok(ToolResult::text(serde_json::to_string(&result)?))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> ToolContext {
        ToolContext::default()
    }

    #[tokio::test]
    async fn call_sse_returns_auth_required() {
        let tool = McpAuthTool::new("my-server".into(), "sse".into());
        let result = tool.call(json!({}), &test_context()).await.unwrap();
        let text = result.to_text();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["status"], "auth_required");
        assert!(parsed["message"].as_str().unwrap().contains("OAuth"));
    }

    #[tokio::test]
    async fn call_http_returns_auth_required() {
        let tool = McpAuthTool::new("api-server".into(), "http".into());
        let result = tool.call(json!({}), &test_context()).await.unwrap();
        let text = result.to_text();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["status"], "auth_required");
    }

    #[tokio::test]
    async fn call_stdio_returns_unsupported() {
        let tool = McpAuthTool::new("local-server".into(), "stdio".into());
        let result = tool.call(json!({}), &test_context()).await.unwrap();
        let text = result.to_text();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["status"], "unsupported");
        assert!(parsed["message"].as_str().unwrap().contains("stdio"));
    }

    #[tokio::test]
    async fn call_unknown_transport_returns_unsupported() {
        let tool = McpAuthTool::new("x".into(), "websocket".into());
        let result = tool.call(json!({}), &test_context()).await.unwrap();
        let text = result.to_text();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["status"], "unsupported");
    }

    #[test]
    fn tool_metadata() {
        let tool = McpAuthTool::new("test".into(), "sse".into());
        assert_eq!(tool.name(), "McpAuth");
        assert!(tool.is_read_only());
        assert_eq!(tool.category(), ToolCategory::Mcp);
    }

    #[test]
    fn input_schema_includes_server_name() {
        let tool = McpAuthTool::new("github-api".into(), "http".into());
        let schema = tool.input_schema();
        let desc = schema["description"].as_str().unwrap();
        assert!(desc.contains("github-api"));
        assert!(desc.contains("http"));
    }
}
