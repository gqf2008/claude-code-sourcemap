//! MCP client — protocol-level operations over a transport.
//!
//! Implements the MCP lifecycle: initialize → list tools/resources → call tool → close.
//! Aligned with the Model Context Protocol specification.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use super::transport::StdioTransport;

// ── MCP protocol types ───────────────────────────────────────────────────────

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

/// Result of calling an MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    #[serde(default)]
    pub content: Vec<McpContent>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

impl McpToolResult {
    /// Extract text content from the result.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ── MCP Client ───────────────────────────────────────────────────────────────

/// Configuration for connecting to an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

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

        // Send initialize request
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

        // Parse server capabilities
        let capabilities: ServerCapabilities = serde_json::from_value(
            init_result.get("capabilities").cloned().unwrap_or(Value::Null),
        )
        .unwrap_or_default();

        let server_info: Option<ServerInfo> = init_result
            .get("serverInfo")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok());

        debug!(
            "MCP server '{}' initialized: {:?}, capabilities: tools={}, resources={}",
            config.name,
            server_info,
            capabilities.tools.is_some(),
            capabilities.resources.is_some(),
        );

        // Send initialized notification (signals we're ready)
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
            .with_context(|| format!("MCP tools/call '{}' failed", tool_name))?;

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
            result
                .get("resources")
                .cloned()
                .unwrap_or(Value::Array(vec![])),
        )
        .context("Failed to parse MCP resources list")?;

        Ok(resources)
    }

    /// Read a specific resource by URI.
    pub async fn read_resource(&mut self, uri: &str) -> Result<Vec<McpContent>> {
        let result = self
            .transport
            .request(
                "resources/read",
                Some(json!({ "uri": uri })),
            )
            .await
            .with_context(|| format!("MCP resources/read '{}' failed", uri))?;

        let contents: Vec<McpContent> = serde_json::from_value(
            result
                .get("contents")
                .cloned()
                .unwrap_or(Value::Array(vec![])),
        )
        .context("Failed to parse MCP resource contents")?;

        Ok(contents)
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

    /// Invalidate the tools cache (e.g., after server tools change notification).
    pub fn invalidate_tools_cache(&mut self) {
        self.tools_cache = None;
    }

    /// Handle a `notifications/tools/list_changed` notification from the server.
    /// Clears the tools cache so the next `list_tools()` call re-fetches.
    /// Returns true if the notification was relevant (tools changed), false otherwise.
    pub async fn handle_tool_list_changed(&mut self) -> Result<Vec<McpToolDef>> {
        info!("MCP server '{}': tool list changed notification received", self.server_name);
        self.tools_cache = None;
        self.list_tools().await
    }

    /// Persist large tool output to a file when it exceeds the size threshold.
    /// Returns the file path if persisted, or None if the output was small enough.
    ///
    /// Aligned with TS `processMCPResult()` — outputs >100KB are written to
    /// `~/.claude/mcp-outputs/{id}.txt` and replaced with a file path reference.
    pub fn persist_large_output(
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

        if std::fs::create_dir_all(&output_dir).is_err() {
            warn!("Failed to create MCP output directory: {}", output_dir.display());
            return None;
        }

        let id = uuid::Uuid::new_v4().simple().to_string();
        let filename = format!("{}-{}.txt", tool_name, &id[..8]);
        let path = output_dir.join(&filename);

        match std::fs::write(&path, &text) {
            Ok(()) => {
                info!(
                    "MCP large output persisted: {} ({} bytes) → {}",
                    tool_name,
                    text.len(),
                    path.display()
                );
                Some(path)
            }
            Err(e) => {
                warn!("Failed to persist MCP output: {}", e);
                None
            }
        }
    }
}

// ── Model Attribution ────────────────────────────────────────────────────────

/// Attribution metadata for tracking which model produced content.
/// Used to generate `Co-Authored-By` lines in commits and PR descriptions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attribution {
    /// Model identifier (e.g., "claude-sonnet-4-6")
    pub model_id: String,
    /// Human-readable model name
    pub model_name: String,
    /// Optional session URL for traceability
    pub session_url: Option<String>,
}

impl Attribution {
    /// Create attribution from a model ID.
    pub fn from_model(model_id: &str) -> Self {
        let model_name = claude_core::model::display_name_any(model_id).to_string();
        Self {
            model_id: model_id.to_string(),
            model_name,
            session_url: None,
        }
    }

    /// Generate a Co-Authored-By line for git commits.
    pub fn co_authored_by(&self) -> String {
        format!(
            "Co-Authored-By: {} <noreply@anthropic.com>",
            self.model_name
        )
    }

    /// Generate an attribution block for PR descriptions.
    pub fn pr_attribution_block(&self) -> String {
        let mut block = format!("---\n_Generated with {}._", self.model_name);
        if let Some(ref url) = self.session_url {
            block.push_str(&format!(" [Session]({})", url));
        }
        block
    }
}

// ── Structured GitDiffResult ─────────────────────────────────────────────────

/// Structured representation of a git diff output.
/// Replaces raw string passing with parsed, size-limited diff data.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitDiffResult {
    /// Total files changed
    pub files_changed: usize,
    /// Total insertions
    pub insertions: usize,
    /// Total deletions
    pub deletions: usize,
    /// Per-file statistics
    pub file_stats: Vec<GitFileStat>,
    /// Whether the diff was truncated
    pub truncated: bool,
    /// Truncation reason if applicable
    pub truncation_reason: Option<String>,
}

/// Per-file diff statistics from `git diff --numstat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitFileStat {
    pub file: String,
    pub insertions: usize,
    pub deletions: usize,
    /// Whether this file's diff was too large and was omitted
    pub content_omitted: bool,
}

impl GitDiffResult {
    /// Diff size limits aligned with TS `diff.ts`
    const MAX_FILES: usize = 50;
    const MAX_TOTAL_BYTES: usize = 1_000_000;  // 1MB
    const MAX_LINES_PER_FILE: usize = 400;

    /// Parse output of `git diff --numstat` into structured stats.
    pub fn parse_numstat(numstat_output: &str) -> Self {
        let mut result = GitDiffResult::default();

        for line in numstat_output.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() != 3 {
                continue;
            }

            let ins = parts[0].parse::<usize>().unwrap_or(0);
            let del = parts[1].parse::<usize>().unwrap_or(0);
            let file = parts[2].to_string();

            result.insertions += ins;
            result.deletions += del;

            if result.file_stats.len() >= Self::MAX_FILES {
                result.truncated = true;
                result.truncation_reason = Some(format!(
                    "Exceeded {} file limit", Self::MAX_FILES
                ));
                break;
            }

            result.file_stats.push(GitFileStat {
                file,
                insertions: ins,
                deletions: del,
                content_omitted: ins + del > Self::MAX_LINES_PER_FILE,
            });
        }

        result.files_changed = result.file_stats.len();
        result
    }

    /// Run `git diff --numstat` in the given directory and parse results.
    pub fn from_git(cwd: &std::path::Path, args: &[&str]) -> Result<Self> {
        let mut cmd_args = vec!["diff", "--numstat"];
        cmd_args.extend_from_slice(args);

        let output = std::process::Command::new("git")
            .args(&cmd_args)
            .current_dir(cwd)
            .output()
            .context("Failed to run git diff --numstat")?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git diff --numstat failed: {}", err.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        let mut result = Self::parse_numstat(&stdout);

        // Check total size
        let total_lines: usize = result.file_stats.iter()
            .map(|f| f.insertions + f.deletions)
            .sum();
        if total_lines > Self::MAX_TOTAL_BYTES / 80 {
            result.truncated = true;
            result.truncation_reason = Some("Diff too large".to_string());
        }

        Ok(result)
    }

    /// Format as a compact summary string.
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} file(s) changed, {} insertion(s)(+), {} deletion(s)(-)",
            self.files_changed, self.insertions, self.deletions
        );
        if self.truncated {
            if let Some(ref reason) = self.truncation_reason {
                out.push_str(&format!(" [truncated: {}]", reason));
            }
        }
        out
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_tool_result_text() {
        let result = McpToolResult {
            content: vec![
                McpContent {
                    content_type: "text".to_string(),
                    text: Some("Hello".to_string()),
                    data: None,
                    mime_type: None,
                },
                McpContent {
                    content_type: "text".to_string(),
                    text: Some("World".to_string()),
                    data: None,
                    mime_type: None,
                },
            ],
            is_error: false,
        };
        assert_eq!(result.text(), "Hello\nWorld");
    }

    #[test]
    fn test_mcp_tool_result_empty() {
        let result = McpToolResult {
            content: vec![],
            is_error: false,
        };
        assert_eq!(result.text(), "");
    }

    #[test]
    fn test_server_config_deserialize() {
        let json = r#"{"name":"test","command":"node","args":["server.js"],"env":{"PORT":"3000"}}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "test");
        assert_eq!(config.command, "node");
        assert_eq!(config.args, vec!["server.js"]);
        assert_eq!(config.env.get("PORT"), Some(&"3000".to_string()));
    }

    #[test]
    fn test_tool_annotations_deserialize() {
        let json = r#"{"name":"read","description":"Read a file","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}"#;
        let tool: McpToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "read");
        assert!(tool.annotations.as_ref().unwrap().read_only_hint.unwrap());
    }

    #[test]
    fn test_mcp_content_types() {
        let text_content = McpContent {
            content_type: "text".to_string(),
            text: Some("hello".to_string()),
            data: None,
            mime_type: None,
        };
        assert_eq!(text_content.text.unwrap(), "hello");

        let image_content = McpContent {
            content_type: "image".to_string(),
            text: None,
            data: Some("base64data".to_string()),
            mime_type: Some("image/png".to_string()),
        };
        assert!(image_content.text.is_none());
        assert_eq!(image_content.data.unwrap(), "base64data");
    }

    // ── Attribution tests ────────────────────────────────────────────

    #[test]
    fn attribution_from_model() {
        let attr = Attribution::from_model("claude-sonnet-4-6");
        assert_eq!(attr.model_id, "claude-sonnet-4-6");
        assert!(!attr.model_name.is_empty());
        assert!(attr.session_url.is_none());
    }

    #[test]
    fn attribution_co_authored_by() {
        let attr = Attribution::from_model("claude-sonnet-4-6");
        let line = attr.co_authored_by();
        assert!(line.starts_with("Co-Authored-By:"));
        assert!(line.contains("noreply@anthropic.com"));
    }

    #[test]
    fn attribution_pr_block() {
        let mut attr = Attribution::from_model("claude-opus-4-6");
        attr.session_url = Some("https://example.com/session/123".into());
        let block = attr.pr_attribution_block();
        assert!(block.contains("Generated with"));
        assert!(block.contains("[Session]"));
    }

    #[test]
    fn attribution_pr_block_no_url() {
        let attr = Attribution::from_model("claude-haiku-4-5");
        let block = attr.pr_attribution_block();
        assert!(block.contains("Generated with"));
        assert!(!block.contains("[Session]"));
    }

    // ── GitDiffResult tests ──────────────────────────────────────────

    #[test]
    fn parse_numstat_basic() {
        let input = "10\t5\tsrc/main.rs\n3\t1\tREADME.md\n";
        let result = GitDiffResult::parse_numstat(input);
        assert_eq!(result.files_changed, 2);
        assert_eq!(result.insertions, 13);
        assert_eq!(result.deletions, 6);
        assert_eq!(result.file_stats.len(), 2);
        assert_eq!(result.file_stats[0].file, "src/main.rs");
        assert!(!result.truncated);
    }

    #[test]
    fn parse_numstat_empty() {
        let result = GitDiffResult::parse_numstat("");
        assert_eq!(result.files_changed, 0);
        assert_eq!(result.insertions, 0);
        assert_eq!(result.deletions, 0);
    }

    #[test]
    fn parse_numstat_large_file_omitted() {
        // File with 500 insertions > MAX_LINES_PER_FILE (400)
        let input = "500\t10\tlarge_file.rs\n";
        let result = GitDiffResult::parse_numstat(input);
        assert!(result.file_stats[0].content_omitted);
    }

    #[test]
    fn parse_numstat_truncation_at_limit() {
        // Generate 55 files (exceeds MAX_FILES=50)
        let mut input = String::new();
        for i in 0..55 {
            input.push_str(&format!("1\t0\tfile{}.rs\n", i));
        }
        let result = GitDiffResult::parse_numstat(&input);
        assert_eq!(result.file_stats.len(), 50);
        assert!(result.truncated);
        assert!(result.truncation_reason.unwrap().contains("50"));
    }

    #[test]
    fn diff_summary_format() {
        let result = GitDiffResult {
            files_changed: 3,
            insertions: 42,
            deletions: 10,
            file_stats: vec![],
            truncated: false,
            truncation_reason: None,
        };
        let summary = result.summary();
        assert!(summary.contains("3 file(s)"));
        assert!(summary.contains("42 insertion(s)"));
        assert!(summary.contains("10 deletion(s)"));
    }

    #[test]
    fn diff_summary_truncated() {
        let result = GitDiffResult {
            files_changed: 50,
            insertions: 1000,
            deletions: 500,
            file_stats: vec![],
            truncated: true,
            truncation_reason: Some("Exceeded 50 file limit".into()),
        };
        let summary = result.summary();
        assert!(summary.contains("[truncated:"));
    }

    // ── Large output persistence tests ───────────────────────────────

    #[test]
    fn persist_small_output_returns_none() {
        let result = McpToolResult {
            content: vec![McpContent {
                content_type: "text".into(),
                text: Some("small output".into()),
                data: None,
                mime_type: None,
            }],
            is_error: false,
        };
        assert!(McpClient::persist_large_output("test_tool", &result).is_none());
    }

    #[test]
    fn persist_large_output_creates_file() {
        let large_text = "x".repeat(200_000); // 200KB
        let result = McpToolResult {
            content: vec![McpContent {
                content_type: "text".into(),
                text: Some(large_text),
                data: None,
                mime_type: None,
            }],
            is_error: false,
        };
        let path = McpClient::persist_large_output("test_tool", &result);
        assert!(path.is_some());
        let path = path.unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().contains("test_tool"));
        // Cleanup
        let _ = std::fs::remove_file(&path);
    }
}
