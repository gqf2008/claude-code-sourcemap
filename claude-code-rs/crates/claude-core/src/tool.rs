use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::Value;
use crate::message::ToolResultContent;
use crate::permissions::{PermissionMode, PermissionResult};

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
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

/// Runtime context available to every tool invocation
#[derive(Clone)]
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

/// Tool category for permission grouping and display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ToolCategory {
    /// File system operations: Read, Edit, Write, Glob, Grep, Ls
    FileSystem,
    /// Shell execution: Bash, PowerShell, REPL
    Shell,
    /// Web/network: WebFetch, WebSearch
    Web,
    /// Code intelligence: Lsp, ToolSearch
    Code,
    /// Agent/orchestration: AgentTool, Task*, SendMessage
    Agent,
    /// Session/config: Config, Plan, Context, Verify, Notebook
    Session,
    /// MCP integration
    Mcp,
    /// Git operations
    Git,
}

impl std::fmt::Display for ToolCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileSystem => write!(f, "filesystem"),
            Self::Shell => write!(f, "shell"),
            Self::Web => write!(f, "web"),
            Self::Code => write!(f, "code"),
            Self::Agent => write!(f, "agent"),
            Self::Session => write!(f, "session"),
            Self::Mcp => write!(f, "mcp"),
            Self::Git => write!(f, "git"),
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

    /// Tool category for permission grouping. Defaults to Session.
    fn category(&self) -> ToolCategory {
        ToolCategory::Session
    }

    fn is_read_only(&self) -> bool {
        false
    }

    /// If true, this tool can safely run in parallel with other concurrency-safe tools.
    /// Read-only tools are concurrency-safe; write tools are not.
    fn is_concurrency_safe(&self) -> bool {
        self.is_read_only()
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
