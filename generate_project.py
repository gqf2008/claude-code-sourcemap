#!/usr/bin/env python3
"""
Generate the complete claude-code-rs Rust workspace.
Run: python generate_project.py
Then: cd claude-code-rs && cargo check
"""
import os, sys

BASE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "claude-code-rs")

FILES = {}

# ============================================================
# Workspace Cargo.toml
# ============================================================
FILES["Cargo.toml"] = r'''[workspace]
members = [
    "crates/claude-core",
    "crates/claude-api",
    "crates/claude-tools",
    "crates/claude-agent",
    "crates/claude-cli",
]
resolver = "2"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
futures = "0.3"
async-stream = "0.3"
tokio-stream = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "2"
reqwest = { version = "0.12", features = ["json", "stream"] }
uuid = { version = "1", features = ["v4"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
clap = { version = "4", features = ["derive", "env"] }
rustyline = "14"
walkdir = "2"
glob = "0.3"
ignore = "0.4"
regex = "1"
chrono = { version = "0.4", features = ["serde"] }
dirs = "6"
'''

# ============================================================
# claude-core
# ============================================================
FILES["crates/claude-core/Cargo.toml"] = r'''[package]
name = "claude-core"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
async-trait = { workspace = true }
uuid = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
tokio = { workspace = true }
dirs = { workspace = true }
'''

FILES["crates/claude-core/src/lib.rs"] = r'''pub mod message;
pub mod tool;
pub mod permissions;
pub mod config;
'''

FILES["crates/claude-core/src/message.rs"] = r'''use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Vec<ToolResultContent>,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub uuid: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub uuid: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<StopReason>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessage {
    pub uuid: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    #[serde(rename = "system")]
    System(SystemMessage),
}

impl Message {
    pub fn uuid(&self) -> &str {
        match self {
            Message::User(m) => &m.uuid,
            Message::Assistant(m) => &m.uuid,
            Message::System(m) => &m.uuid,
        }
    }
}
'''

FILES["crates/claude-core/src/tool.rs"] = r'''use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::Value;
use crate::message::ToolResultContent;
use crate::permissions::{PermissionBehavior, PermissionMode, PermissionResult};

/// Simple abort signal using atomic boolean
#[derive(Clone)]
pub struct AbortSignal(Arc<AtomicBool>);

impl AbortSignal {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    pub fn abort(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_aborted(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

/// Runtime context available to every tool invocation
pub struct ToolContext {
    pub cwd: PathBuf,
    pub abort_signal: AbortSignal,
    pub permission_mode: PermissionMode,
    pub messages: Vec<crate::message::Message>,
}

/// Result of a tool execution
pub struct ToolResult {
    pub content: Vec<ToolResultContent>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResultContent::Text { text: text.into() }],
            is_error: false,
        }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResultContent::Text { text: text.into() }],
            is_error: true,
        }
    }
}

/// Core Tool trait — every tool must implement this
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult>;

    fn is_read_only(&self) -> bool {
        false
    }
    fn is_enabled(&self) -> bool {
        true
    }

    async fn check_permissions(&self, _input: &Value, context: &ToolContext) -> PermissionResult {
        match context.permission_mode {
            PermissionMode::BypassAll => PermissionResult::allow(),
            PermissionMode::AcceptEdits if self.is_read_only() => PermissionResult::allow(),
            PermissionMode::AcceptEdits => {
                PermissionResult::ask("Edit requires confirmation".into())
            }
            _ if self.is_read_only() => PermissionResult::allow(),
            _ => PermissionResult::ask(format!("Allow {} to run?", self.name())),
        }
    }
}

pub type DynTool = Arc<dyn Tool>;
'''

FILES["crates/claude-core/src/permissions.rs"] = r'''use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    BypassAll,
    Plan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone)]
pub struct PermissionResult {
    pub behavior: PermissionBehavior,
    pub reason: Option<String>,
}

impl PermissionResult {
    pub fn allow() -> Self {
        Self { behavior: PermissionBehavior::Allow, reason: None }
    }
    pub fn deny(reason: String) -> Self {
        Self { behavior: PermissionBehavior::Deny, reason: Some(reason) }
    }
    pub fn ask(reason: String) -> Self {
        Self { behavior: PermissionBehavior::Ask, reason: Some(reason) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool_name: String,
    pub pattern: Option<String>,
    pub behavior: PermissionBehavior,
}

/// Simple wildcard matcher (supports * glob)
pub fn matches_wildcard(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == value;
    }
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match value[pos..].find(part) {
            Some(idx) => {
                if i == 0 && idx != 0 {
                    return false;
                }
                pos += idx + part.len();
            }
            None => return false,
        }
    }
    true
}
'''

FILES["crates/claude-core/src/config.rs"] = r'''use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::permissions::PermissionRule;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_tools: Vec<String>,
    #[serde(default)]
    pub custom_system_prompt: Option<String>,
    #[serde(default)]
    pub append_system_prompt: Option<String>,
    #[serde(default)]
    pub permission_rules: Vec<PermissionRule>,
}

impl Settings {
    pub fn config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("claude"))
    }

    pub fn load() -> anyhow::Result<Self> {
        let config_dir = Self::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
        let settings_path = config_dir.join("settings.json");
        if settings_path.exists() {
            let content = std::fs::read_to_string(&settings_path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            Ok(Self::default())
        }
    }
}
'''

# ============================================================
# claude-api
# ============================================================
FILES["crates/claude-api/Cargo.toml"] = r'''[package]
name = "claude-api"
version = "0.1.0"
edition = "2021"

[dependencies]
claude-core = { path = "../claude-core" }
serde = { workspace = true }
serde_json = { workspace = true }
reqwest = { workspace = true }
tokio = { workspace = true }
futures = { workspace = true }
async-stream = { workspace = true }
tokio-stream = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
uuid = { workspace = true }
'''

FILES["crates/claude-api/src/lib.rs"] = r'''pub mod client;
pub mod types;
pub mod stream;
'''

FILES["crates/claude-api/src/types.rs"] = r'''use serde::{Deserialize, Serialize};

// ── Request types ──

#[derive(Debug, Clone, Serialize)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Vec<SystemBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub control_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
    pub role: String,
    pub content: Vec<ApiContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ApiContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Vec<ToolResultContent>,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(rename = "image")]
    Image { source: ImageSource },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContent {
    #[serde(rename = "text")]
    Text { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

// ── Response types ──

#[derive(Debug, Clone, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub response_type: String,
    pub role: String,
    pub content: Vec<ResponseContentBlock>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub usage: ApiUsage,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

// ── SSE Stream events ──

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: MessagesResponse },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: ResponseContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: usize, delta: DeltaBlock },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: MessageDeltaData,
        usage: Option<DeltaUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: ApiError },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum DeltaBlock {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageDeltaData {
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeltaUsage {
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}
'''

FILES["crates/claude-api/src/stream.rs"] = r'''use crate::types::StreamEvent;
use anyhow::Result;

/// Parse a single SSE data line into a StreamEvent
pub fn parse_sse_line(line: &str) -> Option<Result<StreamEvent>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return None;
    }
    if let Some(data) = line.strip_prefix("data: ") {
        if data == "[DONE]" {
            return None;
        }
        Some(
            serde_json::from_str(data)
                .map_err(|e| anyhow::anyhow!("Failed to parse SSE: {}", e)),
        )
    } else {
        None
    }
}
'''

FILES["crates/claude-api/src/client.rs"] = r'''use std::pin::Pin;
use anyhow::{Context, Result};
use futures::Stream;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use crate::types::*;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MODEL: &str = "claude-sonnet-4-20250514";

pub struct AnthropicClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    max_tokens: u32,
}

impl AnthropicClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            default_model: DEFAULT_MODEL.to_string(),
            max_tokens: 16384,
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.api_key).expect("Invalid API key"),
        );
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(API_VERSION),
        );
        headers
    }

    /// Send a non-streaming messages request
    pub async fn messages(&self, request: &MessagesRequest) -> Result<MessagesResponse> {
        let url = format!("{}/v1/messages", self.base_url);
        let response = self
            .http
            .post(&url)
            .headers(self.headers())
            .json(request)
            .send()
            .await
            .context("Failed to send API request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API error ({}): {}", status, body);
        }

        response.json().await.context("Failed to parse API response")
    }

    /// Send a streaming messages request, returns an async stream of events
    pub async fn messages_stream(
        &self,
        request: &MessagesRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let url = format!("{}/v1/messages", self.base_url);
        let mut req = request.clone();
        req.stream = true;

        let response = self
            .http
            .post(&url)
            .headers(self.headers())
            .json(&req)
            .send()
            .await
            .context("Failed to send streaming request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API error ({}): {}", status, body);
        }

        let stream = async_stream::stream! {
            use futures::StreamExt;
            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = byte_stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));
                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].to_string();
                            buffer = buffer[pos + 1..].to_string();
                            if let Some(event_result) = crate::stream::parse_sse_line(&line) {
                                yield event_result;
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(anyhow::anyhow!("Stream read error: {}", e));
                        return;
                    }
                }
            }
            if !buffer.trim().is_empty() {
                if let Some(event_result) = crate::stream::parse_sse_line(&buffer) {
                    yield event_result;
                }
            }
        };

        Ok(Box::pin(stream))
    }

    /// Convenience: build a MessagesRequest with defaults
    pub fn build_request(
        &self,
        messages: Vec<ApiMessage>,
        system: Option<Vec<SystemBlock>>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> MessagesRequest {
        MessagesRequest {
            model: self.default_model.clone(),
            max_tokens: self.max_tokens,
            messages,
            system,
            tools,
            stream: false,
            stop_sequences: None,
        }
    }
}
'''

# ============================================================
# claude-tools
# ============================================================
FILES["crates/claude-tools/Cargo.toml"] = r'''[package]
name = "claude-tools"
version = "0.1.0"
edition = "2021"

[dependencies]
claude-core = { path = "../claude-core" }
serde = { workspace = true }
serde_json = { workspace = true }
async-trait = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
walkdir = { workspace = true }
glob = { workspace = true }
ignore = { workspace = true }
regex = { workspace = true }
reqwest = { workspace = true }
tracing = { workspace = true }
'''

FILES["crates/claude-tools/src/lib.rs"] = r'''pub mod bash;
pub mod file_read;
pub mod file_edit;
pub mod file_write;
pub mod glob_tool;
pub mod grep;
pub mod web_fetch;
pub mod ask_user;

use std::collections::HashMap;
use std::sync::Arc;
use claude_core::tool::{DynTool, Tool};

pub struct ToolRegistry {
    tools: HashMap<String, DynTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        let name = tool.name().to_string();
        self.tools.insert(name, Arc::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<&DynTool> {
        self.tools.get(name)
    }

    pub fn all(&self) -> Vec<&DynTool> {
        self.tools.values().collect()
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// Create a registry pre-loaded with all built-in tools
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(bash::BashTool);
        registry.register(file_read::FileReadTool);
        registry.register(file_edit::FileEditTool);
        registry.register(file_write::FileWriteTool);
        registry.register(glob_tool::GlobTool);
        registry.register(grep::GrepTool);
        registry.register(web_fetch::WebFetchTool);
        registry.register(ask_user::AskUserTool);
        registry
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
'''

FILES["crates/claude-tools/src/bash.rs"] = r'''use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio::process::Command;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "Bash" }

    fn description(&self) -> &str {
        "Execute a shell command in the working directory."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to execute" },
                "timeout": { "type": "integer", "description": "Timeout in ms (default 120000)" }
            },
            "required": ["command"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command'"))?;
        let timeout_ms = input["timeout"].as_u64().unwrap_or(120_000);

        let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("bash", "-c") };

        let output = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            Command::new(shell)
                .arg(flag)
                .arg(command)
                .current_dir(&context.cwd)
                .output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Command timed out after {}ms", timeout_ms))?
        .map_err(|e| anyhow::anyhow!("Failed to execute: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut result = stdout.to_string();
        if !stderr.is_empty() {
            if !result.is_empty() { result.push('\n'); }
            result.push_str("STDERR:\n");
            result.push_str(&stderr);
        }

        if output.status.success() {
            Ok(ToolResult::text(if result.is_empty() { "(no output)".into() } else { result }))
        } else {
            Ok(ToolResult::error(format!(
                "Exit code {}\n{}",
                output.status.code().unwrap_or(-1),
                result
            )))
        }
    }
}
'''

FILES["crates/claude-tools/src/file_read.rs"] = r'''use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::Path;

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str { "Read" }

    fn description(&self) -> &str {
        "Read file contents with optional line range. Also lists directories."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path to read" },
                "offset": { "type": "integer", "description": "Start line (0-indexed)" },
                "limit": { "type": "integer", "description": "Number of lines" }
            },
            "required": ["file_path"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'file_path'"))?;

        let path = resolve_path(file_path, &context.cwd);
        if !path.exists() {
            return Ok(ToolResult::error(format!("File not found: {}", path.display())));
        }
        if path.is_dir() {
            return read_directory(&path);
        }

        let content = tokio::fs::read_to_string(&path).await?;
        let lines: Vec<&str> = content.lines().collect();
        let offset = input["offset"].as_u64().unwrap_or(0) as usize;
        let limit = input["limit"].as_u64().map(|l| l as usize);
        let end = limit.map_or(lines.len(), |l| (offset + l).min(lines.len()));

        let selected: Vec<String> = lines[offset.min(lines.len())..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>4}  {}", offset + i + 1, line))
            .collect();

        Ok(ToolResult::text(selected.join("\n")))
    }
}

fn resolve_path(file_path: &str, cwd: &Path) -> std::path::PathBuf {
    let p = Path::new(file_path);
    if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
}

fn read_directory(path: &Path) -> anyhow::Result<ToolResult> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_dir() {
            entries.push(format!("  {}/", name));
        } else {
            entries.push(format!("  {}", name));
        }
    }
    entries.sort();
    Ok(ToolResult::text(entries.join("\n")))
}
'''

FILES["crates/claude-tools/src/file_edit.rs"] = r'''use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::Path;

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str { "Edit" }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact, unique string match with new content."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let file_path = input["file_path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'file_path'"))?;
        let old_string = input["old_string"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'old_string'"))?;
        let new_string = input["new_string"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'new_string'"))?;

        let path = if Path::new(file_path).is_absolute() {
            std::path::PathBuf::from(file_path)
        } else {
            context.cwd.join(file_path)
        };

        let content = tokio::fs::read_to_string(&path).await?;
        let count = content.matches(old_string).count();
        if count == 0 {
            return Ok(ToolResult::error("old_string not found in file."));
        }
        if count > 1 {
            return Ok(ToolResult::error(format!(
                "old_string found {} times — must be unique.", count
            )));
        }

        let new_content = content.replacen(old_string, new_string, 1);
        tokio::fs::write(&path, &new_content).await?;
        Ok(ToolResult::text(format!("Edited {}", path.display())))
    }
}
'''

FILES["crates/claude-tools/src/file_write.rs"] = r'''use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::path::Path;

pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str { "Write" }

    fn description(&self) -> &str {
        "Create a new file. Fails if the file already exists."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["file_path", "content"]
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let file_path = input["file_path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'file_path'"))?;
        let content = input["content"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'content'"))?;

        let path = if Path::new(file_path).is_absolute() {
            std::path::PathBuf::from(file_path)
        } else {
            context.cwd.join(file_path)
        };

        if path.exists() {
            return Ok(ToolResult::error(format!("File already exists: {}", path.display())));
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, content).await?;
        Ok(ToolResult::text(format!("Created {}", path.display())))
    }
}
'''

FILES["crates/claude-tools/src/glob_tool.rs"] = r'''use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str { "Glob" }

    fn description(&self) -> &str {
        "Find files matching a glob pattern."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "e.g. **/*.rs" },
                "path": { "type": "string", "description": "Search root (default: cwd)" }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let pattern = input["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'pattern'"))?;
        let search_dir = match input["path"].as_str() {
            Some(p) => {
                let pa = std::path::Path::new(p);
                if pa.is_absolute() { pa.to_path_buf() } else { context.cwd.join(pa) }
            }
            None => context.cwd.clone(),
        };
        let full = search_dir.join(pattern).to_string_lossy().to_string();
        let mut matches: Vec<String> = Vec::new();
        for entry in glob::glob(&full).map_err(|e| anyhow::anyhow!("Bad glob: {}", e))? {
            if let Ok(path) = entry {
                matches.push(path.display().to_string());
            }
        }
        matches.sort();
        if matches.is_empty() {
            Ok(ToolResult::text("No files matched."))
        } else {
            Ok(ToolResult::text(matches.join("\n")))
        }
    }
}
'''

FILES["crates/claude-tools/src/grep.rs"] = r'''use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use ignore::WalkBuilder;
use regex::Regex;
use std::fs;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "Grep" }

    fn description(&self) -> &str {
        "Search file contents with a regular expression."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string" },
                "path": { "type": "string" },
                "include": { "type": "string", "description": "File glob filter" }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let pattern = input["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'pattern'"))?;
        let search_path = match input["path"].as_str() {
            Some(p) => {
                let pa = std::path::Path::new(p);
                if pa.is_absolute() { pa.to_path_buf() } else { context.cwd.join(pa) }
            }
            None => context.cwd.clone(),
        };
        let regex = Regex::new(pattern).map_err(|e| anyhow::anyhow!("Bad regex: {}", e))?;
        let include_glob = input["include"].as_str();
        let mut results = Vec::new();
        let mut file_count = 0usize;
        const MAX_RESULTS: usize = 100;

        let walker = WalkBuilder::new(&search_path).hidden(true).git_ignore(true).build();
        'outer: for entry in walker.flatten() {
            if !entry.file_type().map_or(false, |ft| ft.is_file()) { continue; }
            let path = entry.path();
            if let Some(g) = include_glob {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !glob::Pattern::new(g).map_or(false, |p| p.matches(name)) { continue; }
            }
            let content = match fs::read_to_string(path) { Ok(c) => c, Err(_) => continue };
            let mut file_hits = Vec::new();
            for (num, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    file_hits.push(format!("  {}:{}: {}", path.display(), num + 1, line.trim()));
                    if results.len() + file_hits.len() >= MAX_RESULTS { results.extend(file_hits); break 'outer; }
                }
            }
            if !file_hits.is_empty() { file_count += 1; results.extend(file_hits); }
        }

        if results.is_empty() {
            Ok(ToolResult::text("No matches found."))
        } else {
            Ok(ToolResult::text(format!("Found matches in {} file(s):\n{}", file_count, results.join("\n"))))
        }
    }
}
'''

FILES["crates/claude-tools/src/web_fetch.rs"] = r'''use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str { "WebFetch" }
    fn description(&self) -> &str { "Fetch a URL and return its text content." }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string" },
                "max_length": { "type": "integer", "description": "Max chars (default 5000)" }
            },
            "required": ["url"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let url = input["url"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'url'"))?;
        let max_len = input["max_length"].as_u64().unwrap_or(5000) as usize;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let resp = client.get(url).send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        let truncated = if body.len() > max_len {
            format!("{}...\n[Truncated {}/{}]", &body[..max_len], max_len, body.len())
        } else { body };
        if status.is_success() { Ok(ToolResult::text(truncated)) }
        else { Ok(ToolResult::error(format!("HTTP {}: {}", status, truncated))) }
    }
}
'''

FILES["crates/claude-tools/src/ask_user.rs"] = r'''use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::io::{self, Write};

pub struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str { "AskUser" }
    fn description(&self) -> &str { "Ask the user a question and wait for a response." }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "question": { "type": "string" } },
            "required": ["question"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let question = input["question"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'question'"))?;
        println!("\n\x1b[33m? {}\x1b[0m", question);
        print!("> ");
        io::stdout().flush()?;
        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
        Ok(ToolResult::text(response.trim().to_string()))
    }
}
'''

# ============================================================
# claude-agent
# ============================================================
FILES["crates/claude-agent/Cargo.toml"] = r'''[package]
name = "claude-agent"
version = "0.1.0"
edition = "2021"

[dependencies]
claude-core = { path = "../claude-core" }
claude-api = { path = "../claude-api" }
claude-tools = { path = "../claude-tools" }
serde = { workspace = true }
serde_json = { workspace = true }
async-trait = { workspace = true }
tokio = { workspace = true }
futures = { workspace = true }
async-stream = { workspace = true }
tokio-stream = { workspace = true }
anyhow = { workspace = true }
thiserror = { workspace = true }
uuid = { workspace = true }
tracing = { workspace = true }
'''

FILES["crates/claude-agent/src/lib.rs"] = r'''pub mod engine;
pub mod query;
pub mod executor;
pub mod state;
pub mod hooks;
pub mod permissions;
'''

FILES["crates/claude-agent/src/state.rs"] = r'''use std::sync::Arc;
use tokio::sync::RwLock;
use claude_core::permissions::PermissionMode;
use claude_core::message::Message;

#[derive(Debug, Clone)]
pub struct AppState {
    pub model: String,
    pub permission_mode: PermissionMode,
    pub verbose: bool,
    pub messages: Vec<Message>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub turn_count: u32,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            permission_mode: PermissionMode::Default,
            verbose: false,
            messages: Vec::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            turn_count: 0,
        }
    }
}

pub type SharedState = Arc<RwLock<AppState>>;

pub fn new_shared_state() -> SharedState {
    Arc::new(RwLock::new(AppState::default()))
}
'''

FILES["crates/claude-agent/src/hooks.rs"] = r'''use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    SessionStart,
}

#[derive(Debug, Clone)]
pub struct HookContext {
    pub event: HookEvent,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_output: Option<String>,
}

#[async_trait]
pub trait Hook: Send + Sync {
    fn event(&self) -> HookEvent;
    async fn execute(&self, context: &HookContext) -> anyhow::Result<HookResult>;
}

#[derive(Debug, Clone)]
pub enum HookResult {
    Continue,
    Block { reason: String },
    Modify { new_input: Value },
}

pub struct HookRegistry {
    hooks: Vec<Box<dyn Hook>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    pub fn register(&mut self, hook: impl Hook + 'static) {
        self.hooks.push(Box::new(hook));
    }

    pub async fn run_hooks(&self, context: &HookContext) -> anyhow::Result<HookResult> {
        for hook in &self.hooks {
            if hook.event() == context.event {
                let result = hook.execute(context).await?;
                match &result {
                    HookResult::Block { .. } | HookResult::Modify { .. } => return Ok(result),
                    HookResult::Continue => continue,
                }
            }
        }
        Ok(HookResult::Continue)
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}
'''

FILES["crates/claude-agent/src/permissions.rs"] = r'''use claude_core::permissions::{PermissionBehavior, PermissionMode, PermissionResult, PermissionRule};
use claude_core::tool::Tool;
use serde_json::Value;
use std::io::{self, Write};

pub struct PermissionChecker {
    rules: Vec<PermissionRule>,
    mode: PermissionMode,
}

impl PermissionChecker {
    pub fn new(mode: PermissionMode, rules: Vec<PermissionRule>) -> Self {
        Self { rules, mode }
    }

    pub async fn check(&self, tool: &dyn Tool, _input: &Value) -> PermissionResult {
        if self.mode == PermissionMode::BypassAll {
            return PermissionResult::allow();
        }
        if self.mode == PermissionMode::Plan && !tool.is_read_only() {
            return PermissionResult::deny("Plan mode: writes not allowed".into());
        }
        for rule in &self.rules {
            if rule.tool_name == tool.name() || rule.tool_name == "*" {
                match rule.behavior {
                    PermissionBehavior::Allow => return PermissionResult::allow(),
                    PermissionBehavior::Deny => {
                        return PermissionResult::deny(format!("'{}' denied by rule", tool.name()));
                    }
                    PermissionBehavior::Ask => {}
                }
            }
        }
        if tool.is_read_only() {
            return PermissionResult::allow();
        }
        PermissionResult::ask(format!("Allow {} ?", tool.name()))
    }

    /// Interactive terminal permission prompt
    pub fn prompt_user(tool_name: &str, description: &str) -> bool {
        print!("\n\x1b[33m⚠  {} wants to: {}\n   Allow? [y/N]: \x1b[0m", tool_name, description);
        io::stdout().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
    }
}
'''

FILES["crates/claude-agent/src/executor.rs"] = r'''use std::sync::Arc;
use claude_core::tool::ToolContext;
use claude_core::message::{ContentBlock, ToolResultContent};
use claude_core::permissions::PermissionBehavior;
use claude_tools::ToolRegistry;
use serde_json::Value;
use tracing::{debug, warn};
use crate::permissions::PermissionChecker;

pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    permission_checker: Arc<PermissionChecker>,
}

impl ToolExecutor {
    pub fn new(registry: Arc<ToolRegistry>, permission_checker: Arc<PermissionChecker>) -> Self {
        Self { registry, permission_checker }
    }

    pub async fn execute(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        input: Value,
        context: &ToolContext,
    ) -> ContentBlock {
        let tool = match self.registry.get(tool_name) {
            Some(t) => t,
            None => {
                return ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    content: vec![ToolResultContent::Text { text: format!("Unknown tool: {}", tool_name) }],
                    is_error: true,
                };
            }
        };

        let perm = self.permission_checker.check(tool.as_ref(), &input).await;
        match perm.behavior {
            PermissionBehavior::Deny => {
                return ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    content: vec![ToolResultContent::Text {
                        text: perm.reason.unwrap_or_else(|| "Permission denied".into()),
                    }],
                    is_error: true,
                };
            }
            PermissionBehavior::Ask => {
                let desc = format!("{}: {}", tool_name, serde_json::to_string(&input).unwrap_or_default());
                if !PermissionChecker::prompt_user(tool_name, &desc) {
                    return ContentBlock::ToolResult {
                        tool_use_id: tool_use_id.to_string(),
                        content: vec![ToolResultContent::Text { text: "User denied permission".into() }],
                        is_error: true,
                    };
                }
            }
            PermissionBehavior::Allow => {}
        }

        debug!("Executing tool: {}", tool_name);
        match tool.call(input, context).await {
            Ok(result) => ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: result.content,
                is_error: result.is_error,
            },
            Err(e) => {
                warn!("Tool {} failed: {}", tool_name, e);
                ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    content: vec![ToolResultContent::Text { text: format!("Tool error: {}", e) }],
                    is_error: true,
                }
            }
        }
    }

    pub async fn execute_many(
        &self,
        tool_uses: Vec<(String, String, Value)>,
        context: &ToolContext,
    ) -> Vec<ContentBlock> {
        let mut results = Vec::new();
        for (id, name, input) in tool_uses {
            results.push(self.execute(&id, &name, input, context).await);
        }
        results
    }
}
'''

FILES["crates/claude-agent/src/query.rs"] = r'''use std::pin::Pin;
use std::sync::Arc;
use futures::Stream;
use uuid::Uuid;

use claude_api::client::AnthropicClient;
use claude_api::types::*;
use claude_core::message::{
    AssistantMessage, ContentBlock, Message, StopReason, Usage, UserMessage,
};
use claude_core::tool::{AbortSignal, ToolContext};
use crate::executor::ToolExecutor;
use crate::state::SharedState;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart { id: String, name: String },
    ToolResult { id: String, is_error: bool },
    AssistantMessage(AssistantMessage),
    TurnComplete { stop_reason: StopReason },
    UsageUpdate(Usage),
    Error(String),
}

pub struct QueryConfig {
    pub system_prompt: String,
    pub max_turns: u32,
    pub max_tokens: u32,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            max_turns: 100,
            max_tokens: 16384,
        }
    }
}

/// Core agent loop: send messages → process stream → execute tools → repeat
pub fn query_stream(
    client: Arc<AnthropicClient>,
    executor: Arc<ToolExecutor>,
    state: SharedState,
    tool_context: ToolContext,
    config: QueryConfig,
    initial_messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
    let stream = async_stream::stream! {
        let mut messages = initial_messages;
        let mut turn_count: u32 = 0;

        loop {
            if turn_count >= config.max_turns {
                yield AgentEvent::Error(format!("Max turns ({}) reached", config.max_turns));
                break;
            }

            let api_messages = messages_to_api(&messages);
            let system = if config.system_prompt.is_empty() {
                None
            } else {
                Some(vec![SystemBlock {
                    block_type: "text".into(),
                    text: config.system_prompt.clone(),
                    cache_control: None,
                }])
            };

            let request = MessagesRequest {
                model: { state.read().await.model.clone() },
                max_tokens: config.max_tokens,
                messages: api_messages,
                system,
                tools: if tools.is_empty() { None } else { Some(tools.clone()) },
                stream: true,
                stop_sequences: None,
            };

            let event_stream = match client.messages_stream(&request).await {
                Ok(s) => s,
                Err(e) => { yield AgentEvent::Error(format!("API error: {}", e)); break; }
            };

            let mut assistant_text = String::new();
            let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();
            let mut current_tool_input = String::new();
            let mut current_tool_id = String::new();
            let mut current_tool_name = String::new();
            let mut stop_reason = None;
            let mut usage = None;

            use tokio_stream::StreamExt;
            let mut event_stream = event_stream;
            while let Some(event_result) = event_stream.next().await {
                match event_result {
                    Ok(event) => match event {
                        StreamEvent::ContentBlockStart { content_block, .. } => {
                            match &content_block {
                                ResponseContentBlock::Text { text } => {
                                    assistant_text.push_str(text);
                                    yield AgentEvent::TextDelta(text.clone());
                                }
                                ResponseContentBlock::ToolUse { id, name, .. } => {
                                    current_tool_id = id.clone();
                                    current_tool_name = name.clone();
                                    current_tool_input.clear();
                                    yield AgentEvent::ToolUseStart { id: id.clone(), name: name.clone() };
                                }
                                ResponseContentBlock::Thinking { thinking } => {
                                    yield AgentEvent::ThinkingDelta(thinking.clone());
                                }
                            }
                        }
                        StreamEvent::ContentBlockDelta { delta, .. } => match delta {
                            DeltaBlock::TextDelta { text } => {
                                assistant_text.push_str(&text);
                                yield AgentEvent::TextDelta(text);
                            }
                            DeltaBlock::InputJsonDelta { partial_json } => {
                                current_tool_input.push_str(&partial_json);
                            }
                            DeltaBlock::ThinkingDelta { thinking } => {
                                yield AgentEvent::ThinkingDelta(thinking);
                            }
                        },
                        StreamEvent::ContentBlockStop { .. } => {
                            if !current_tool_id.is_empty() {
                                let input: serde_json::Value = serde_json::from_str(&current_tool_input)
                                    .unwrap_or(serde_json::Value::Object(Default::default()));
                                assistant_blocks.push(ContentBlock::ToolUse {
                                    id: current_tool_id.clone(),
                                    name: current_tool_name.clone(),
                                    input: input.clone(),
                                });
                                tool_uses.push((current_tool_id.clone(), current_tool_name.clone(), input));
                                current_tool_id.clear();
                                current_tool_name.clear();
                                current_tool_input.clear();
                            }
                        }
                        StreamEvent::MessageDelta { delta, .. } => {
                            stop_reason = delta.stop_reason.as_deref().map(|r| match r {
                                "end_turn" => StopReason::EndTurn,
                                "tool_use" => StopReason::ToolUse,
                                "max_tokens" => StopReason::MaxTokens,
                                _ => StopReason::EndTurn,
                            });
                        }
                        StreamEvent::MessageStart { message } => {
                            usage = Some(Usage {
                                input_tokens: message.usage.input_tokens,
                                output_tokens: message.usage.output_tokens,
                                cache_creation_input_tokens: message.usage.cache_creation_input_tokens,
                                cache_read_input_tokens: message.usage.cache_read_input_tokens,
                            });
                        }
                        StreamEvent::Error { error } => {
                            yield AgentEvent::Error(format!("{}: {}", error.error_type, error.message));
                            break;
                        }
                        _ => {}
                    },
                    Err(e) => { yield AgentEvent::Error(format!("Stream error: {}", e)); break; }
                }
            }

            // Ensure text block is present
            if !assistant_text.is_empty() && !assistant_blocks.iter().any(|b| matches!(b, ContentBlock::Text { .. })) {
                assistant_blocks.insert(0, ContentBlock::Text { text: assistant_text.clone() });
            }

            let assistant_msg = AssistantMessage {
                uuid: Uuid::new_v4().to_string(),
                content: assistant_blocks,
                stop_reason: stop_reason.clone(),
                usage: usage.clone(),
            };
            messages.push(Message::Assistant(assistant_msg.clone()));
            yield AgentEvent::AssistantMessage(assistant_msg);

            if let Some(ref u) = usage {
                let mut s = state.write().await;
                s.total_input_tokens += u.input_tokens;
                s.total_output_tokens += u.output_tokens;
                yield AgentEvent::UsageUpdate(u.clone());
            }

            let actual_stop = stop_reason.unwrap_or(StopReason::EndTurn);
            match actual_stop {
                StopReason::ToolUse if !tool_uses.is_empty() => {
                    let results = executor.execute_many(tool_uses, &tool_context).await;
                    let tool_result_msg = UserMessage {
                        uuid: Uuid::new_v4().to_string(),
                        content: results.clone(),
                    };
                    messages.push(Message::User(tool_result_msg));
                    for result in &results {
                        if let ContentBlock::ToolResult { tool_use_id, is_error, .. } = result {
                            yield AgentEvent::ToolResult { id: tool_use_id.clone(), is_error: *is_error };
                        }
                    }
                    turn_count += 1;
                    { let mut s = state.write().await; s.turn_count = turn_count; }
                }
                other => {
                    yield AgentEvent::TurnComplete { stop_reason: other };
                    break;
                }
            }
        }
    };
    Box::pin(stream)
}

fn messages_to_api(messages: &[Message]) -> Vec<ApiMessage> {
    messages.iter().filter_map(|msg| match msg {
        Message::User(u) => Some(ApiMessage {
            role: "user".into(),
            content: u.content.iter().map(block_to_api).collect(),
        }),
        Message::Assistant(a) => Some(ApiMessage {
            role: "assistant".into(),
            content: a.content.iter().map(block_to_api).collect(),
        }),
        Message::System(_) => None,
    }).collect()
}

fn block_to_api(block: &ContentBlock) -> ApiContentBlock {
    match block {
        ContentBlock::Text { text } => ApiContentBlock::Text { text: text.clone() },
        ContentBlock::ToolUse { id, name, input } => ApiContentBlock::ToolUse {
            id: id.clone(), name: name.clone(), input: input.clone(),
        },
        ContentBlock::ToolResult { tool_use_id, content, is_error } => ApiContentBlock::ToolResult {
            tool_use_id: tool_use_id.clone(),
            content: content.iter().map(|c| match c {
                claude_core::message::ToolResultContent::Text { text } => {
                    claude_api::types::ToolResultContent::Text { text: text.clone() }
                }
                claude_core::message::ToolResultContent::Image { .. } => {
                    claude_api::types::ToolResultContent::Text { text: "[image]".into() }
                }
            }).collect(),
            is_error: *is_error,
        },
        ContentBlock::Thinking { thinking } => {
            ApiContentBlock::Text { text: format!("<thinking>{}</thinking>", thinking) }
        }
    }
}
'''

FILES["crates/claude-agent/src/engine.rs"] = r'''use std::sync::Arc;
use uuid::Uuid;

use claude_api::client::AnthropicClient;
use claude_api::types::ToolDefinition;
use claude_core::message::{ContentBlock, Message, UserMessage};
use claude_core::tool::{AbortSignal, ToolContext};
use claude_core::permissions::PermissionMode;
use claude_tools::ToolRegistry;

use crate::executor::ToolExecutor;
use crate::hooks::HookRegistry;
use crate::permissions::PermissionChecker;
use crate::query::{query_stream, AgentEvent, QueryConfig};
use crate::state::{new_shared_state, SharedState};

pub struct QueryEngine {
    client: Arc<AnthropicClient>,
    executor: Arc<ToolExecutor>,
    registry: Arc<ToolRegistry>,
    state: SharedState,
    config: QueryConfig,
    #[allow(dead_code)]
    hooks: Arc<HookRegistry>,
    cwd: std::path::PathBuf,
}

pub struct QueryEngineBuilder {
    api_key: String,
    model: Option<String>,
    cwd: std::path::PathBuf,
    system_prompt: String,
    max_turns: u32,
    max_tokens: u32,
    permission_checker: PermissionChecker,
    hooks: HookRegistry,
}

impl QueryEngineBuilder {
    pub fn new(api_key: impl Into<String>, cwd: impl Into<std::path::PathBuf>) -> Self {
        Self {
            api_key: api_key.into(),
            model: None,
            cwd: cwd.into(),
            system_prompt: String::new(),
            max_turns: 100,
            max_tokens: 16384,
            permission_checker: PermissionChecker::new(PermissionMode::Default, Vec::new()),
            hooks: HookRegistry::new(),
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn max_turns(mut self, max: u32) -> Self {
        self.max_turns = max;
        self
    }

    #[allow(dead_code)]
    pub fn max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = max;
        self
    }

    pub fn permission_checker(mut self, checker: PermissionChecker) -> Self {
        self.permission_checker = checker;
        self
    }

    pub fn build(self) -> QueryEngine {
        let mut client = AnthropicClient::new(self.api_key);
        if let Some(ref model) = self.model {
            client = client.with_model(model);
        }
        client = client.with_max_tokens(self.max_tokens);

        let client = Arc::new(client);
        let registry = Arc::new(ToolRegistry::with_defaults());
        let permission_checker = Arc::new(self.permission_checker);
        let executor = Arc::new(ToolExecutor::new(registry.clone(), permission_checker));

        let state = new_shared_state();

        QueryEngine {
            client,
            executor,
            registry,
            state,
            config: QueryConfig {
                system_prompt: self.system_prompt,
                max_turns: self.max_turns,
                max_tokens: self.max_tokens,
            },
            hooks: Arc::new(self.hooks),
            cwd: self.cwd,
        }
    }
}

impl QueryEngine {
    pub fn builder(
        api_key: impl Into<String>,
        cwd: impl Into<std::path::PathBuf>,
    ) -> QueryEngineBuilder {
        QueryEngineBuilder::new(api_key, cwd)
    }

    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.registry
            .all()
            .iter()
            .filter(|t| t.is_enabled())
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    /// Submit a user message and get back a stream of AgentEvents
    pub fn submit(
        &self,
        user_prompt: impl Into<String>,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>> {
        let user_msg = UserMessage {
            uuid: Uuid::new_v4().to_string(),
            content: vec![ContentBlock::Text { text: user_prompt.into() }],
        };
        let messages = vec![Message::User(user_msg)];
        let tools = self.tool_definitions();
        let tool_context = ToolContext {
            cwd: self.cwd.clone(),
            abort_signal: AbortSignal::new(),
            permission_mode: PermissionMode::Default,
            messages: Vec::new(),
        };

        query_stream(
            self.client.clone(),
            self.executor.clone(),
            self.state.clone(),
            tool_context,
            QueryConfig {
                system_prompt: self.config.system_prompt.clone(),
                max_turns: self.config.max_turns,
                max_tokens: self.config.max_tokens,
            },
            messages,
            tools,
        )
    }

    pub fn state(&self) -> &SharedState {
        &self.state
    }
}
'''

# ============================================================
# claude-cli
# ============================================================
FILES["crates/claude-cli/Cargo.toml"] = r'''[package]
name = "claude-cli"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "claude"
path = "src/main.rs"

[dependencies]
claude-core = { path = "../claude-core" }
claude-api = { path = "../claude-api" }
claude-tools = { path = "../claude-tools" }
claude-agent = { path = "../claude-agent" }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
futures = { workspace = true }
tokio-stream = { workspace = true }
anyhow = { workspace = true }
clap = { workspace = true }
rustyline = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
dirs = { workspace = true }
'''

FILES["crates/claude-cli/src/main.rs"] = r'''mod config;
mod repl;
mod commands;
mod output;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "claude", version, about = "Claude Code — AI coding assistant (Rust)")]
struct Cli {
    /// Initial prompt (non-interactive mode)
    prompt: Option<String>,

    /// API key (or set ANTHROPIC_API_KEY)
    #[arg(long, env = "ANTHROPIC_API_KEY")]
    api_key: Option<String>,

    /// Model
    #[arg(long, short, default_value = "claude-sonnet-4-20250514")]
    model: String,

    /// Permission mode: default | bypass | acceptEdits | plan
    #[arg(long, default_value = "default")]
    permission_mode: String,

    /// Custom system prompt
    #[arg(long)]
    system_prompt: Option<String>,

    /// Working directory
    #[arg(long, short = 'd')]
    cwd: Option<String>,

    /// Max conversation turns
    #[arg(long, default_value = "100")]
    max_turns: u32,

    /// Verbose output
    #[arg(long, short)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { EnvFilter::new("debug") } else { EnvFilter::new("warn") };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let settings = config::load_settings()?;

    let api_key = cli.api_key.or(settings.api_key).ok_or_else(|| {
        anyhow::anyhow!("API key required. Set ANTHROPIC_API_KEY or use --api-key.")
    })?;

    let cwd = match cli.cwd {
        Some(dir) => std::path::PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let system_prompt = config::build_system_prompt(
        cli.system_prompt.as_deref(),
        settings.custom_system_prompt.as_deref(),
        settings.append_system_prompt.as_deref(),
    );

    let permission_mode = config::parse_permission_mode(&cli.permission_mode);

    let engine = claude_agent::engine::QueryEngine::builder(api_key, &cwd)
        .model(&cli.model)
        .system_prompt(system_prompt)
        .max_turns(cli.max_turns)
        .permission_checker(claude_agent::permissions::PermissionChecker::new(
            permission_mode,
            settings.permission_rules,
        ))
        .build();

    if let Some(prompt) = cli.prompt {
        output::run_single(&engine, &prompt).await?;
    } else {
        repl::run(engine).await?;
    }

    Ok(())
}
'''

FILES["crates/claude-cli/src/config.rs"] = r'''use claude_core::config::Settings;
use claude_core::permissions::PermissionMode;

pub fn load_settings() -> anyhow::Result<Settings> {
    Settings::load()
}

pub fn parse_permission_mode(mode: &str) -> PermissionMode {
    match mode {
        "bypass" | "bypassPermissions" => PermissionMode::BypassAll,
        "acceptEdits" | "accept-edits" => PermissionMode::AcceptEdits,
        "plan" => PermissionMode::Plan,
        _ => PermissionMode::Default,
    }
}

pub fn build_system_prompt(
    cli_prompt: Option<&str>,
    settings_prompt: Option<&str>,
    append_prompt: Option<&str>,
) -> String {
    let base = cli_prompt.or(settings_prompt).unwrap_or(DEFAULT_SYSTEM_PROMPT);
    match append_prompt {
        Some(extra) => format!("{}\n\n{}", base, extra),
        None => base.to_string(),
    }
}

const DEFAULT_SYSTEM_PROMPT: &str = r#"You are Claude, an AI assistant made by Anthropic, running as a CLI coding agent.
You have access to tools for reading, writing, and searching files, executing shell commands, and more.

Key guidelines:
- Read files before editing them
- Make precise, surgical edits
- Use Bash for complex operations
- Verify changes after making them
- Be concise in responses"#;
'''

FILES["crates/claude-cli/src/commands.rs"] = r'''pub enum SlashCommand {
    Help,
    Clear,
    Model(String),
    Compact,
    Cost,
    Exit,
    Unknown(String),
}

impl SlashCommand {
    pub fn parse(input: &str) -> Option<Self> {
        let input = input.trim();
        if !input.starts_with('/') { return None; }
        let parts: Vec<&str> = input[1..].splitn(2, ' ').collect();
        let cmd = parts[0].to_lowercase();
        let args = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
        Some(match cmd.as_str() {
            "help" | "?" => Self::Help,
            "clear" => Self::Clear,
            "model" => Self::Model(args),
            "compact" => Self::Compact,
            "cost" => Self::Cost,
            "exit" | "quit" => Self::Exit,
            _ => Self::Unknown(cmd),
        })
    }

    pub fn execute(&self) -> CommandResult {
        match self {
            Self::Help => CommandResult::Print(HELP_TEXT.to_string()),
            Self::Clear => CommandResult::ClearHistory,
            Self::Model(name) if name.is_empty() => {
                CommandResult::Print("Usage: /model <name>".to_string())
            }
            Self::Model(name) => CommandResult::SetModel(name.clone()),
            Self::Compact => CommandResult::Print("Compact not yet implemented.".to_string()),
            Self::Cost => CommandResult::ShowCost,
            Self::Exit => CommandResult::Exit,
            Self::Unknown(cmd) => {
                CommandResult::Print(format!("Unknown command: /{}. Type /help.", cmd))
            }
        }
    }
}

pub enum CommandResult {
    Print(String),
    ClearHistory,
    SetModel(String),
    ShowCost,
    Exit,
}

const HELP_TEXT: &str = "\
Available commands:
  /help     Show this help
  /clear    Clear conversation history
  /model    Switch model (e.g. /model claude-sonnet-4-20250514)
  /compact  Compact conversation history
  /cost     Show token usage and cost
  /exit     Exit the CLI";
'''

FILES["crates/claude-cli/src/output.rs"] = r'''use claude_agent::engine::QueryEngine;
use claude_agent::query::AgentEvent;
use tokio_stream::StreamExt;

pub async fn print_stream(
    mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>>,
) -> anyhow::Result<()> {
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TextDelta(text) => {
                print!("{}", text);
                use std::io::Write;
                std::io::stdout().flush().ok();
            }
            AgentEvent::ThinkingDelta(_) => {}
            AgentEvent::ToolUseStart { name, .. } => {
                println!("\n\x1b[36m⚙ Tool: {}\x1b[0m", name);
            }
            AgentEvent::ToolResult { is_error, .. } => {
                if is_error {
                    println!("\x1b[31m✗ Tool failed\x1b[0m");
                } else {
                    println!("\x1b[32m✓ Done\x1b[0m");
                }
            }
            AgentEvent::AssistantMessage(_) => {}
            AgentEvent::TurnComplete { .. } => { println!(); }
            AgentEvent::UsageUpdate(u) => {
                tracing::debug!("Tokens: in={}, out={}", u.input_tokens, u.output_tokens);
            }
            AgentEvent::Error(msg) => {
                eprintln!("\x1b[31mError: {}\x1b[0m", msg);
            }
        }
    }
    Ok(())
}

pub async fn run_single(engine: &QueryEngine, prompt: &str) -> anyhow::Result<()> {
    let stream = engine.submit(prompt);
    print_stream(stream).await
}
'''

FILES["crates/claude-cli/src/repl.rs"] = r'''use claude_agent::engine::QueryEngine;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::commands::{CommandResult, SlashCommand};
use crate::output::print_stream;

pub async fn run(engine: QueryEngine) -> anyhow::Result<()> {
    println!("\x1b[1;34m╭─────────────────────────────────╮\x1b[0m");
    println!("\x1b[1;34m│      Claude Code (Rust)         │\x1b[0m");
    println!("\x1b[1;34m│  Type /help for commands        │\x1b[0m");
    println!("\x1b[1;34m│  Type /exit to quit             │\x1b[0m");
    println!("\x1b[1;34m╰─────────────────────────────────╯\x1b[0m\n");

    let mut rl = DefaultEditor::new()?;

    loop {
        let readline = rl.readline("\x1b[1;32m> \x1b[0m");
        match readline {
            Ok(line) => {
                let input = line.trim();
                if input.is_empty() { continue; }
                let _ = rl.add_history_entry(input);

                if let Some(cmd) = SlashCommand::parse(input) {
                    match cmd.execute() {
                        CommandResult::Print(text) => println!("{}", text),
                        CommandResult::Exit => { println!("Goodbye!"); break; }
                        CommandResult::ClearHistory => { println!("History cleared."); }
                        CommandResult::SetModel(model) => {
                            let state = engine.state();
                            let mut s = state.write().await;
                            s.model = model.clone();
                            println!("Model set to: {}", model);
                        }
                        CommandResult::ShowCost => {
                            let state = engine.state();
                            let s = state.read().await;
                            println!(
                                "Tokens: input={}, output={}, turns={}",
                                s.total_input_tokens, s.total_output_tokens, s.turn_count
                            );
                        }
                    }
                    continue;
                }

                let stream = engine.submit(input);
                if let Err(e) = print_stream(stream).await {
                    eprintln!("\x1b[31mError: {}\x1b[0m", e);
                }
            }
            Err(ReadlineError::Interrupted) => { println!("^C"); continue; }
            Err(ReadlineError::Eof) => { println!("Goodbye!"); break; }
            Err(err) => { eprintln!("Error: {:?}", err); break; }
        }
    }
    Ok(())
}
'''

# ============================================================
# Generate all files
# ============================================================
def main():
    count = 0
    for rel_path, content in FILES.items():
        full_path = os.path.join(BASE, rel_path.replace("/", os.sep))
        os.makedirs(os.path.dirname(full_path), exist_ok=True)
        with open(full_path, "w", encoding="utf-8", newline="\n") as f:
            # Strip leading newline from raw strings
            text = content
            if text.startswith("\n"):
                text = text[1:]
            f.write(text)
        count += 1
        print(f"  [{count:2d}] {rel_path}")

    print(f"\n✅ Generated {count} files in {BASE}")
    print("\nNext steps:")
    print(f"  cd {BASE}")
    print("  cargo check")

if __name__ == "__main__":
    main()
