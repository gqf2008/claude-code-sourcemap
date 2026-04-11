//! `AgentEngine` trait — the abstract interface for the agent core.
//!
//! Consumers (CLI, RPC, Bridge) should program against this trait rather than
//! the concrete `QueryEngine` type whenever possible.  This enables:
//!
//! 1. Testing with mock engines
//! 2. Future alternative engine implementations
//! 3. Explicit documentation of the public contract

use std::pin::Pin;

use async_trait::async_trait;
use clawed_api::types::ThinkingConfig;
use clawed_core::message::ContentBlock;
use clawed_core::tool::AbortSignal;
use futures::Stream;

use crate::cost::CostTracker;
use crate::query::AgentEvent;
use crate::task_runner::{TaskProgress, TaskResult};

/// The core engine contract.
///
/// All methods are `&self` — the engine manages interior mutability internally.
#[async_trait]
pub trait AgentEngine: Send + Sync {
    // ── Submission ───────────────────────────────────────────────────────────

    /// Submit a text prompt and receive a stream of agent events.
    async fn submit(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;

    /// Submit a mixed-content prompt (text + images).
    async fn submit_with_content(
        &self,
        content: Vec<ContentBlock>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;

    // ── Abort / cancel ──────────────────────────────────────────────────────

    /// Abort the current task.
    fn abort(&self);

    /// Get a clone of the abort signal for external cancellation.
    fn abort_signal(&self) -> AbortSignal;

    // ── Identity / metadata ─────────────────────────────────────────────────

    /// Unique session identifier.
    fn session_id(&self) -> &str;

    /// Working directory.
    fn cwd(&self) -> &std::path::Path;

    /// Current model name.
    async fn model(&self) -> String;

    /// Set the model for subsequent requests.
    async fn set_model(&self, model: &str);

    /// Whether this engine is in coordinator (multi-agent) mode.
    fn is_coordinator(&self) -> bool;

    // ── Tool / registry info ────────────────────────────────────────────────

    /// Number of registered tools.
    fn tool_count(&self) -> usize;

    /// `(name, description, is_enabled)` for every registered tool.
    fn tool_list(&self) -> Vec<(String, String, bool)>;

    // ── Cost & usage ────────────────────────────────────────────────────────

    /// Reference to the cost tracker.
    fn cost_tracker(&self) -> &CostTracker;

    /// Context window usage percentage (0–100), or `None` if unknown.
    async fn context_usage_percent(&self) -> Option<u8>;

    // ── Compaction ──────────────────────────────────────────────────────────

    /// Check if auto-compaction should trigger.
    async fn should_auto_compact(&self) -> bool;

    /// Run explicit compaction. Returns the summary on success.
    async fn compact(
        &self,
        trigger: &str,
        custom_instructions: Option<&str>,
    ) -> anyhow::Result<String>;

    /// Record a successful auto-compact (resets circuit breaker).
    async fn record_compact_success(&self);

    /// Record a failed auto-compact attempt.
    async fn record_compact_failure(&self);

    // ── History manipulation ────────────────────────────────────────────────

    /// Clear conversation history.
    async fn clear_history(&self);

    /// Undo the last `n` turns. Returns `(removed, remaining_messages)`.
    async fn rewind_turns(&self, n: usize) -> (usize, usize);

    /// Get the last user prompt text (for /retry).
    async fn last_user_prompt(&self) -> Option<String>;

    /// Pop the last user+assistant turn and return the user prompt.
    async fn pop_last_turn(&self) -> Option<String>;

    // ── Session persistence ─────────────────────────────────────────────────

    /// Save the current session to disk.
    async fn save_session(&self) -> anyhow::Result<()>;

    /// Restore a session. Returns the session title.
    async fn restore_session(&self, session_id: &str) -> anyhow::Result<String>;

    /// Rename the current session.
    async fn rename_session(&self, name: &str) -> anyhow::Result<()>;

    // ── Thinking / cache control ────────────────────────────────────────────

    /// Get the current thinking configuration.
    fn thinking_config(&self) -> Option<ThinkingConfig>;

    /// Override thinking at runtime.
    fn set_thinking(&self, config: Option<ThinkingConfig>);

    /// Request that the next API call skips prompt caching.
    fn set_break_cache(&self);

    // ── Multi-agent coordination ────────────────────────────────────────────

    /// Drain pending task notifications from background agents.
    async fn drain_notifications(&self) -> Vec<clawed_core::message::Message>;

    /// Send a message to a running background agent.
    async fn send_to_agent(&self, agent_id: &str, message: &str) -> anyhow::Result<()>;

    /// Cancel a running background agent.
    async fn cancel_agent(&self, agent_id: &str) -> anyhow::Result<()>;

    // ── System prompt ───────────────────────────────────────────────────────

    /// Update the CLAUDE.md portion of the system prompt.
    async fn update_system_prompt_context(&self, claude_md: &str);

    // ── Hooks lifecycle ─────────────────────────────────────────────────────

    /// Run SessionStart hooks. Returns optional context to prepend.
    async fn run_session_start(&self) -> Option<String>;

    // ── Task runner ─────────────────────────────────────────────────────────

    /// Run a task autonomously to completion.
    async fn run_task(
        &self,
        task: &str,
        on_progress: Box<dyn FnMut(TaskProgress) + Send>,
    ) -> TaskResult;
}
