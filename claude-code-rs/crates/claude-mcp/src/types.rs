//! MCP domain types — tools, resources, capabilities, and server configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Server capabilities ──────────────────────────────────────────────────────

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

// ── Tool definitions ─────────────────────────────────────────────────────────

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

// ── Resource definitions ─────────────────────────────────────────────────────

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

// ── Tool results ─────────────────────────────────────────────────────────────

/// Result of calling an MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    #[serde(default)]
    pub content: Vec<McpContent>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

impl McpToolResult {
    /// Extract concatenated text content from the result.
    #[must_use] 
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ── Server configuration ─────────────────────────────────────────────────────

/// Configuration for connecting to an MCP server via stdio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Configuration for connecting to an MCP server via SSE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSseConfig {
    /// SSE endpoint URL (e.g. "<https://mcp.example.com/sse>").
    pub url: String,
    /// Optional HTTP headers (for auth, etc.).
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

// ── Large output persistence ─────────────────────────────────────────────────

/// Persist large tool output to a file when it exceeds the size threshold.
/// Returns the file path if persisted, or None if the output was small enough.
///
/// Outputs >100KB are written to `~/.claude/mcp-outputs/{id}.txt`.
/// Uses `tokio::fs` for non-blocking I/O in async contexts.
pub async fn persist_large_output(
    tool_name: &str,
    result: &McpToolResult,
) -> Option<std::path::PathBuf> {
    const LARGE_OUTPUT_THRESHOLD: usize = 100 * 1024; // 100KB

    let text = result.text();
    if text.len() < LARGE_OUTPUT_THRESHOLD {
        return None;
    }

    let output_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude")
        .join("mcp-outputs");

    if tokio::fs::create_dir_all(&output_dir).await.is_err() {
        tracing::warn!("Failed to create MCP output directory: {}", output_dir.display());
        return None;
    }

    let id = uuid::Uuid::new_v4().simple().to_string();
    let filename = format!("{tool_name}-{}.txt", &id[..8]);
    let path = output_dir.join(&filename);

    match tokio::fs::write(&path, &text).await {
        Ok(()) => {
            tracing::info!(
                "MCP large output persisted: {tool_name} ({} bytes) → {}",
                text.len(), path.display()
            );
            Some(path)
        }
        Err(e) => {
            tracing::warn!("Failed to persist MCP output: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_text() {
        let result = McpToolResult {
            content: vec![
                McpContent {
                    content_type: "text".into(),
                    text: Some("Hello".into()),
                    data: None,
                    mime_type: None,
                },
                McpContent {
                    content_type: "text".into(),
                    text: Some("World".into()),
                    data: None,
                    mime_type: None,
                },
            ],
            is_error: false,
        };
        assert_eq!(result.text(), "Hello\nWorld");
    }

    #[test]
    fn tool_result_empty() {
        let result = McpToolResult {
            content: vec![],
            is_error: false,
        };
        assert_eq!(result.text(), "");
    }

    #[test]
    fn server_config_deserialize() {
        let json = r#"{"name":"test","command":"node","args":["server.js"],"env":{"PORT":"3000"}}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "test");
        assert_eq!(config.command, "node");
        assert_eq!(config.args, vec!["server.js"]);
        assert_eq!(config.env.get("PORT"), Some(&"3000".to_string()));
    }

    #[test]
    fn tool_annotations_deserialize() {
        let json = r#"{"name":"read","description":"Read a file","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}"#;
        let tool: McpToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "read");
        assert!(tool.annotations.as_ref().unwrap().read_only_hint.unwrap());
    }

    #[test]
    fn content_types() {
        let text = McpContent {
            content_type: "text".into(),
            text: Some("hello".into()),
            data: None,
            mime_type: None,
        };
        assert_eq!(text.text.unwrap(), "hello");

        let image = McpContent {
            content_type: "image".into(),
            text: None,
            data: Some("base64data".into()),
            mime_type: Some("image/png".into()),
        };
        assert!(image.text.is_none());
        assert_eq!(image.data.unwrap(), "base64data");
    }

    #[test]
    fn resource_deserialize() {
        let json = r#"{"uri":"file:///test.txt","name":"test","description":"A test file","mimeType":"text/plain"}"#;
        let res: McpResource = serde_json::from_str(json).unwrap();
        assert_eq!(res.uri, "file:///test.txt");
        assert_eq!(res.mime_type, Some("text/plain".to_string()));
    }

    #[test]
    fn sse_config_deserialize() {
        let json = r#"{"url":"https://example.com/sse","headers":{"Authorization":"Bearer tok"}}"#;
        let config: McpSseConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.url, "https://example.com/sse");
        assert_eq!(config.headers.get("Authorization").unwrap(), "Bearer tok");
    }

    #[test]
    fn sse_config_empty_headers() {
        let json = r#"{"url":"https://example.com/sse"}"#;
        let config: McpSseConfig = serde_json::from_str(json).unwrap();
        assert!(config.headers.is_empty());
    }

    #[test]
    fn server_capabilities_default() {
        let caps = ServerCapabilities::default();
        assert!(caps.tools.is_none());
        assert!(caps.resources.is_none());
    }

    #[tokio::test]
    async fn persist_small_output_returns_none() {
        let result = McpToolResult {
            content: vec![McpContent {
                content_type: "text".into(),
                text: Some("small output".into()),
                data: None,
                mime_type: None,
            }],
            is_error: false,
        };
        assert!(persist_large_output("test_tool", &result).await.is_none());
    }

    #[tokio::test]
    async fn persist_large_output_creates_file() {
        let large_text = "x".repeat(200_000);
        let result = McpToolResult {
            content: vec![McpContent {
                content_type: "text".into(),
                text: Some(large_text),
                data: None,
                mime_type: None,
            }],
            is_error: false,
        };
        let path = persist_large_output("test_tool", &result).await;
        assert!(path.is_some());
        let path = path.unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().contains("test_tool"));
        let _ = std::fs::remove_file(&path);
    }
}
