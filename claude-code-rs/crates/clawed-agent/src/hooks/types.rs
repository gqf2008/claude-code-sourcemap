//! Hook system types: events, context, and decisions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Public event enum ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    StopFailure,
    UserPromptSubmit,
    SessionStart,
    SessionEnd,
    Setup,
    PreCompact,
    PostCompact,
    SubagentStart,
    SubagentStop,
    Notification,
    /// Fired after model sampling, before tool execution.
    PostSampling,
    // ── New events (TS parity) ──
    /// Permission request shown to user.
    PermissionRequest,
    /// Permission denied by user or rule.
    PermissionDenied,
    /// CLAUDE.md / instructions loaded or changed.
    InstructionsLoaded,
    /// Working directory changed.
    CwdChanged,
    /// Watched file changed on disk.
    FileChanged,
    /// Configuration settings changed.
    ConfigChange,
    /// Task created (task management).
    TaskCreated,
    /// Task completed.
    TaskCompleted,
    // ── Additional events (TS parity) ──
    /// Teammate agent is idle/waiting for messages.
    TeammateIdle,
    /// User elicitation prompt presented.
    Elicitation,
    /// User responded to an elicitation.
    ElicitationResult,
    /// Git worktree created for isolated work.
    WorktreeCreate,
    /// Git worktree removed after cleanup.
    WorktreeRemove,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::Stop => "Stop",
            Self::StopFailure => "StopFailure",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::Setup => "Setup",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::SubagentStart => "SubagentStart",
            Self::SubagentStop => "SubagentStop",
            Self::Notification => "Notification",
            Self::PostSampling => "PostSampling",
            Self::PermissionRequest => "PermissionRequest",
            Self::PermissionDenied => "PermissionDenied",
            Self::InstructionsLoaded => "InstructionsLoaded",
            Self::CwdChanged => "CwdChanged",
            Self::FileChanged => "FileChanged",
            Self::ConfigChange => "ConfigChange",
            Self::TaskCreated => "TaskCreated",
            Self::TaskCompleted => "TaskCompleted",
            Self::TeammateIdle => "TeammateIdle",
            Self::Elicitation => "Elicitation",
            Self::ElicitationResult => "ElicitationResult",
            Self::WorktreeCreate => "WorktreeCreate",
            Self::WorktreeRemove => "WorktreeRemove",
        }
    }
}

// ── Context passed to every hook invocation ──────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HookContext {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Compact trigger: "manual" or "auto"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Post-compact summary text
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Agent ID for subagent events
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub cwd: String,
    pub session_id: String,
}

// ── Hook decision returned to caller ────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum HookDecision {
    /// Proceed normally.
    Continue,
    /// Block the action; reason shown to Claude.
    Block { reason: String },
    /// (Stop hooks only) inject `feedback` as a new user message and loop.
    FeedbackAndContinue { feedback: String },
    /// Append extra text to the current payload (prompt / system prompt).
    AppendContext { text: String },
    /// Replace tool input with a new value.
    ModifyInput { new_input: Value },
}

// ── Optional JSON response hook scripts can emit on stdout ──────────────────

#[derive(Debug, Deserialize)]
pub(super) struct HookJsonResponse {
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
}
