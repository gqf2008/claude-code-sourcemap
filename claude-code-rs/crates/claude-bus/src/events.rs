//! Event, request, and notification types for the agent bus.
//!
//! All types are `Serialize + Deserialize` so they can be sent over JSON-RPC
//! when crossing process boundaries, or passed as Rust enums in-process.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Agent → UI notifications (broadcast) ─────────────────────────────────────

/// Events published by the Agent Core to all subscribed UI clients.
///
/// These flow in one direction: Agent → UI. The UI should never need to
/// acknowledge them (fire-and-forget / pub-sub semantics).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentNotification {
    // ── Streaming content ──

    /// Incremental text from the assistant response.
    TextDelta { text: String },

    /// Incremental thinking/reasoning text (extended thinking mode).
    ThinkingDelta { text: String },

    // ── Tool lifecycle ──

    /// A tool invocation has started (input may still be streaming).
    ToolUseStart {
        id: String,
        tool_name: String,
    },

    /// Tool input is fully available.
    ToolUseReady {
        id: String,
        tool_name: String,
        input: Value,
    },

    /// Tool execution completed (success or error).
    ToolUseComplete {
        id: String,
        tool_name: String,
        is_error: bool,
        /// Truncated result text for display (full result stays in conversation).
        result_preview: Option<String>,
    },

    // ── Turn lifecycle ──

    /// A new turn is starting (one API request-response cycle).
    TurnStart { turn: u32 },

    /// A turn has completed.
    TurnComplete {
        turn: u32,
        stop_reason: String,
        usage: UsageInfo,
    },

    /// The complete assistant message for this turn (for logging/display).
    AssistantMessage {
        turn: u32,
        text_blocks: Vec<String>,
    },

    // ── Session lifecycle ──

    /// Session has been initialized.
    SessionStart {
        session_id: String,
        model: String,
    },

    /// Session is ending (user exit, error, etc.).
    SessionEnd { reason: String },

    /// Session was saved to disk.
    SessionSaved { session_id: String },

    /// Session status response (for GetStatus request).
    SessionStatus {
        session_id: String,
        model: String,
        total_turns: u32,
        total_input_tokens: u64,
        total_output_tokens: u64,
        context_usage_pct: f64,
    },

    /// Conversation history was cleared.
    HistoryCleared,

    /// Model was changed (response to SetModel request).
    ModelChanged {
        model: String,
        display_name: String,
    },

    // ── Context management ──

    /// Context usage is getting high.
    ContextWarning { usage_pct: f64, message: String },

    /// Auto-compaction started.
    CompactStart,

    /// Compaction finished.
    CompactComplete { summary_len: usize },

    // ── Sub-agent lifecycle ──

    /// A sub-agent has been spawned.
    AgentSpawned {
        agent_id: String,
        name: Option<String>,
        agent_type: String,
        background: bool,
    },

    /// Progress update from a background sub-agent.
    AgentProgress {
        agent_id: String,
        text: String,
    },

    /// Sub-agent has completed.
    AgentComplete {
        agent_id: String,
        result: String,
        is_error: bool,
    },

    // ── MCP lifecycle ──

    /// An MCP server connected successfully.
    McpServerConnected { name: String, tool_count: usize },

    /// An MCP server disconnected (explicit or crash).
    McpServerDisconnected { name: String },

    /// An MCP server encountered an error.
    McpServerError { name: String, error: String },

    /// Response to McpListServers request.
    McpServerList { servers: Vec<McpServerInfo> },

    // ── Errors ──

    /// A non-fatal error occurred.
    Error { code: ErrorCode, message: String },
}

// ── UI → Agent requests (mpsc) ───────────────────────────────────────────────

/// Requests sent from the UI client to the Agent Core.
///
/// Each request is processed sequentially by the core's run loop.
/// Some requests produce a response, others are fire-and-forget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum AgentRequest {
    /// Submit a user message to the conversation.
    Submit {
        text: String,
        #[serde(default)]
        images: Vec<ImageAttachment>,
    },

    /// Abort the currently running operation (tool, API call, etc.).
    Abort,

    /// Respond to a permission request from the core.
    PermissionResponse {
        request_id: String,
        granted: bool,
        /// If true, remember this decision for the session.
        #[serde(default)]
        remember: bool,
    },

    /// Trigger manual compaction.
    Compact {
        instructions: Option<String>,
    },

    /// Switch the active model.
    SetModel { model: String },

    /// Execute a slash command (parsed by the core).
    SlashCommand { command: String },

    /// Send a follow-up message to a background sub-agent.
    SendAgentMessage {
        agent_id: String,
        message: String,
    },

    /// Cancel/stop a background sub-agent.
    StopAgent { agent_id: String },

    // ── MCP management ──

    /// Connect to an MCP server.
    McpConnect {
        name: String,
        command: String,
        args: Vec<String>,
        #[serde(default)]
        env: std::collections::HashMap<String, String>,
    },

    /// Disconnect an MCP server.
    McpDisconnect { name: String },

    /// List connected MCP servers (response via McpServerList notification).
    McpListServers,

    /// Graceful shutdown.
    Shutdown,

    /// Save the current session to disk.
    SaveSession,

    /// Query session status (response via SessionStatus notification).
    GetStatus,

    /// Clear the conversation history.
    ClearHistory,
}

// ── Permission request/response (bidirectional) ──────────────────────────────

/// Permission request from Agent Core → UI.
///
/// The core blocks until the UI responds with a [`PermissionResponse`].
/// Sent over a dedicated channel (not broadcast) since only one UI should respond.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Unique ID for correlating request/response.
    pub request_id: String,
    /// Name of the tool requesting permission.
    pub tool_name: String,
    /// Tool input parameters.
    pub input: Value,
    /// Risk assessment level.
    pub risk_level: RiskLevel,
    /// Human-readable description of what the tool wants to do.
    pub description: String,
}

/// Permission response from UI → Agent Core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub request_id: String,
    pub granted: bool,
    /// Remember this decision for the rest of the session.
    pub remember: bool,
}

// ── Supporting types ─────────────────────────────────────────────────────────

/// Token/cost usage information for a turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
}

/// Image attachment in a submit request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageAttachment {
    /// Base64-encoded image data.
    pub data: String,
    /// MIME type (e.g. "image/png").
    pub media_type: String,
}

/// MCP server status info (returned in McpServerList notification).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub tool_count: usize,
    pub connected: bool,
}

/// Risk level for permission requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// Error codes for agent notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// API returned an error (rate limit, auth, etc.)
    ApiError,
    /// Tool execution failed.
    ToolError,
    /// Context window exceeded.
    ContextOverflow,
    /// Network/connection issue.
    NetworkError,
    /// Permission denied (auto-deny in strict mode).
    PermissionDenied,
    /// Internal error (bug, panic, etc.)
    InternalError,
}

// ── Display implementations ──────────────────────────────────────────────────

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiError => write!(f, "api_error"),
            Self::ToolError => write!(f, "tool_error"),
            Self::ContextOverflow => write!(f, "context_overflow"),
            Self::NetworkError => write!(f, "network_error"),
            Self::PermissionDenied => write!(f, "permission_denied"),
            Self::InternalError => write!(f, "internal_error"),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_serialization_roundtrip() {
        let events = vec![
            AgentNotification::TextDelta { text: "Hello".into() },
            AgentNotification::ToolUseStart {
                id: "tu_1".into(),
                tool_name: "FileRead".into(),
            },
            AgentNotification::ToolUseComplete {
                id: "tu_1".into(),
                tool_name: "FileRead".into(),
                is_error: false,
                result_preview: Some("file content...".into()),
            },
            AgentNotification::TurnComplete {
                turn: 1,
                stop_reason: "end_turn".into(),
                usage: UsageInfo {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 80,
                    cache_creation_tokens: 20,
                },
            },
            AgentNotification::AgentSpawned {
                agent_id: "agent-abc".into(),
                name: Some("reviewer".into()),
                agent_type: "explore".into(),
                background: true,
            },
            AgentNotification::Error {
                code: ErrorCode::ApiError,
                message: "Rate limited".into(),
            },
            AgentNotification::McpServerConnected {
                name: "github".into(),
                tool_count: 5,
            },
            AgentNotification::McpServerDisconnected {
                name: "github".into(),
            },
            AgentNotification::McpServerError {
                name: "github".into(),
                error: "connection lost".into(),
            },
            AgentNotification::McpServerList {
                servers: vec![McpServerInfo {
                    name: "test".into(),
                    tool_count: 3,
                    connected: true,
                }],
            },
            AgentNotification::HistoryCleared,
            AgentNotification::ModelChanged {
                model: "claude-sonnet-4-20250514".into(),
                display_name: "Claude Sonnet 4".into(),
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let back: AgentNotification = serde_json::from_str(&json).unwrap();
            // Verify type tag is present
            assert!(json.contains("\"type\""));
            // Verify roundtrip produces valid JSON
            let json2 = serde_json::to_string(&back).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn request_serialization_roundtrip() {
        let requests = vec![
            AgentRequest::Submit {
                text: "Fix the bug".into(),
                images: vec![],
            },
            AgentRequest::Abort,
            AgentRequest::PermissionResponse {
                request_id: "pr_1".into(),
                granted: true,
                remember: false,
            },
            AgentRequest::SetModel { model: "opus".into() },
            AgentRequest::SlashCommand { command: "/help".into() },
            AgentRequest::SendAgentMessage {
                agent_id: "agent-1".into(),
                message: "focus on tests".into(),
            },
            AgentRequest::StopAgent { agent_id: "agent-1".into() },
            AgentRequest::McpConnect {
                name: "fs".into(),
                command: "npx".into(),
                args: vec!["-y".into(), "fs-server".into()],
                env: std::collections::HashMap::new(),
            },
            AgentRequest::McpDisconnect { name: "fs".into() },
            AgentRequest::McpListServers,
            AgentRequest::Shutdown,
            AgentRequest::ClearHistory,
        ];

        for req in &requests {
            let json = serde_json::to_string(req).unwrap();
            let back: AgentRequest = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&back).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn permission_request_serialization() {
        let req = PermissionRequest {
            request_id: "pr_1".into(),
            tool_name: "Bash".into(),
            input: serde_json::json!({ "command": "rm -rf node_modules" }),
            risk_level: RiskLevel::High,
            description: "Execute shell command".into(),
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"risk_level\":\"high\""));
        let back: PermissionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.request_id, "pr_1");
        assert_eq!(back.risk_level, RiskLevel::High);
    }

    #[test]
    fn error_code_display() {
        assert_eq!(ErrorCode::ApiError.to_string(), "api_error");
        assert_eq!(ErrorCode::ToolError.to_string(), "tool_error");
        assert_eq!(ErrorCode::ContextOverflow.to_string(), "context_overflow");
    }

    #[test]
    fn risk_level_display() {
        assert_eq!(RiskLevel::Low.to_string(), "low");
        assert_eq!(RiskLevel::Medium.to_string(), "medium");
        assert_eq!(RiskLevel::High.to_string(), "high");
    }

    #[test]
    fn usage_info_default() {
        let usage = UsageInfo::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_creation_tokens, 0);
    }
}
