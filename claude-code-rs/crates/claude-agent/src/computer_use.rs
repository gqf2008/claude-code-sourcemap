//! Computer Use tool bridge — wraps `claude-computer-use` MCP server
//! as `Tool` trait implementations for the agent's tool registry.
//!
//! Each CU tool is a thin wrapper that delegates to `ComputerUseMcpServer::call_tool()`.
//! The server is shared via `Arc` across all tools and holds the session lock.

use std::sync::Arc;

use async_trait::async_trait;
use claude_computer_use::ComputerUseMcpServer;
use claude_core::message::{ImageSource, ToolResultContent};
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::Value;
use tracing::info;

/// Shared CU server instance.
type SharedCuServer = Arc<ComputerUseMcpServer>;

/// Register all Computer Use tools into the given tool registry.
///
/// Acquires the session lock on first call. Returns `Err` if the lock
/// is already held by another session.
pub fn register_cu_tools(registry: &mut claude_tools::ToolRegistry) -> anyhow::Result<()> {
    let server = Arc::new(ComputerUseMcpServer::new()?);
    info!("Computer Use session lock acquired, registering 8 tools");

    for tool_def in server.list_tools() {
        let name = tool_def.name.clone();
        let description = tool_def.description.clone().unwrap_or_default();
        let schema = tool_def.input_schema.clone().unwrap_or(serde_json::json!({"type": "object"}));

        registry.register(CuToolBridge {
            server: server.clone(),
            tool_name: name,
            tool_description: description,
            tool_schema: schema,
        });
    }

    Ok(())
}

/// A single Computer Use tool exposed to the agent.
struct CuToolBridge {
    server: SharedCuServer,
    tool_name: String,
    tool_description: String,
    tool_schema: Value,
}

#[async_trait]
impl Tool for CuToolBridge {
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
        ToolCategory::ComputerUse
    }

    fn is_read_only(&self) -> bool {
        matches!(self.tool_name.as_str(), "screenshot" | "cursor_position")
    }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let mcp_result = self.server.call_tool(&self.tool_name, input);

        let mut content: Vec<ToolResultContent> = Vec::new();

        for c in &mcp_result.content {
            match c.content_type.as_str() {
                "text" => {
                    if let Some(text) = &c.text {
                        content.push(ToolResultContent::Text { text: text.clone() });
                    }
                }
                "image" => {
                    if let Some(data) = &c.data {
                        let media_type = c.mime_type.as_deref()
                            .unwrap_or("image/png")
                            .to_string();
                        content.push(ToolResultContent::Image {
                            source: ImageSource {
                                media_type,
                                data: data.clone(),
                            },
                        });
                    }
                }
                _ => {}
            }
        }

        if content.is_empty() {
            content.push(ToolResultContent::Text { text: String::new() });
        }

        Ok(ToolResult {
            content,
            is_error: mcp_result.is_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cu_tool_bridge_name_and_category() {
        let bridge = CuToolBridge {
            server: Arc::new(ComputerUseMcpServer::new().expect("session lock")),
            tool_name: "screenshot".into(),
            tool_description: "Take a screenshot".into(),
            tool_schema: serde_json::json!({"type": "object"}),
        };
        assert_eq!(bridge.name(), "screenshot");
        assert_eq!(bridge.category(), ToolCategory::ComputerUse);
        assert!(bridge.is_read_only());
    }

    #[test]
    fn cu_tool_bridge_click_not_readonly() {
        let bridge = CuToolBridge {
            server: Arc::new(ComputerUseMcpServer::new().expect("session lock")),
            tool_name: "click".into(),
            tool_description: "Click".into(),
            tool_schema: serde_json::json!({"type": "object"}),
        };
        assert!(!bridge.is_read_only());
    }
}
