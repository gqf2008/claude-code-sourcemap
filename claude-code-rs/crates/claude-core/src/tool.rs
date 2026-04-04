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
