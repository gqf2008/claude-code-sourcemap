//! QueryEngine — the core agent orchestration engine.
//!
//! The engine drives the multi-turn agentic loop: accepting user prompts,
//! streaming API responses, executing tools, and managing conversation state.

mod builder;
pub use builder::QueryEngineBuilder;

use std::sync::Arc;

use claude_api::client::ApiClient;
use claude_api::types::{CacheControl, ToolDefinition};
use claude_core::message::{ContentBlock, Message, UserMessage};
use claude_core::tool::{AbortSignal, ToolContext};
use claude_tools::ToolRegistry;
use uuid::Uuid;

use crate::compact::{compact_conversation, compact_context_message, AutoCompactState};
use crate::coordinator::TaskNotification;
use crate::dispatch_agent::{AgentChannelMap, CancelTokenMap};
use crate::cost::CostTracker;
use crate::executor::ToolExecutor;
use crate::hooks::{HookDecision, HookEvent, HookRegistry};
use crate::query::{query_stream, AgentEvent, QueryConfig};
use crate::state::SharedState;
use crate::task_runner::{run_task, TaskProgress, TaskResult};

pub struct QueryEngine {
    client: Arc<ApiClient>,
    executor: Arc<ToolExecutor>,
    registry: Arc<ToolRegistry>,
    state: SharedState,
    config: QueryConfig,
    hooks: Arc<HookRegistry>,
    cwd: std::path::PathBuf,
    session_id: String,
    /// Timestamp when this session was created.
    created_at: chrono::DateTime<chrono::Utc>,
    compact_threshold: u64,
    /// Shared abort signal — call `.abort()` to cancel the running task.
    abort_signal: AbortSignal,
    /// Coordinator mode: receives task notifications from background agents.
    notification_rx: Option<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<TaskNotification>>>,
    /// Whether coordinator mode is active.
    coordinator_mode: bool,
    /// If non-empty, only expose these tools to the model.
    allowed_tools: Vec<String>,
    /// Tracks accumulated API usage costs per model.
    cost_tracker: CostTracker,
    /// Sub-agent cancellation tokens (coordinator mode only).
    cancel_tokens: Option<CancelTokenMap>,
    /// Sub-agent message channels (coordinator mode only).
    agent_channels: Option<AgentChannelMap>,
    /// Auto-compact state machine (circuit breaker, dynamic threshold).
    auto_compact: tokio::sync::Mutex<AutoCompactState>,
    /// Model context window size (for auto-compact threshold calculation).
    context_window: u64,
}

impl QueryEngine {
    pub fn builder(
        api_key: impl Into<String>,
        cwd: impl Into<std::path::PathBuf>,
    ) -> QueryEngineBuilder {
        QueryEngineBuilder::new(api_key, cwd)
    }

    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut defs: Vec<ToolDefinition> = self.registry
            .all()
            .iter()
            .filter(|t| t.is_enabled())
            .filter(|t| {
                self.allowed_tools.is_empty()
                    || self.allowed_tools.iter().any(|a| a.eq_ignore_ascii_case(t.name()))
            })
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
                cache_control: None,
            })
            .collect();

        // Enable prompt caching on the last tool definition (mirrors TS behavior)
        if let Some(last) = defs.last_mut() {
            last.cache_control = Some(CacheControl::ephemeral());
        }
        defs
    }

    /// Submit a user message and get back a stream of AgentEvents.
    pub async fn submit(
        &self,
        user_prompt: impl Into<String>,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>> {
        let mut prompt_text: String = user_prompt.into();

        // ── Empty prompt validation ──────────────────────────────────────────
        if prompt_text.trim().is_empty() {
            let err_stream = async_stream::stream! {
                yield AgentEvent::Error("Prompt cannot be empty".to_string());
            };
            return Box::pin(err_stream);
        }

        // ── UserPromptSubmit hook ────────────────────────────────────────────
        if self.hooks.has_hooks(HookEvent::UserPromptSubmit) {
            let ctx = self.hooks.prompt_ctx(HookEvent::UserPromptSubmit, Some(prompt_text.clone()));
            match self.hooks.run(HookEvent::UserPromptSubmit, ctx).await {
                HookDecision::Block { reason } => {
                    // Block: return a stream with just the error
                    let err_stream = async_stream::stream! {
                        yield AgentEvent::Error(format!("[UserPromptSubmit hook blocked]: {}", reason));
                    };
                    return Box::pin(err_stream);
                }
                HookDecision::AppendContext { text } => {
                    prompt_text = format!("{}\n\n{}", prompt_text, text);
                }
                _ => {}
            }
        }

        let (permission_mode, mut messages) = {
            let s = self.state.read().await;
            (s.permission_mode, s.messages.clone())
        };

        let user_msg = UserMessage {
            uuid: Uuid::new_v4().to_string(),
            content: vec![ContentBlock::Text { text: prompt_text }],
        };
        messages.push(Message::User(user_msg));

        let tools = self.tool_definitions();
        let tool_context = ToolContext {
            cwd: self.cwd.clone(),
            abort_signal: self.abort_signal.clone(),
            permission_mode,
            messages: Vec::new(),
        };

        query_stream(
            self.client.clone(),
            self.executor.clone(),
            self.state.clone(),
            tool_context,
            self.build_query_config(),
            messages,
            tools,
            self.hooks.clone(),
        )
    }

    /// Submit a user message with mixed content blocks (text + images).
    ///
    /// Use this when the user attaches images via `@path/to/image.png` syntax.
    /// The content blocks should be pre-built (text blocks for text, image blocks
    /// for attached images).
    pub async fn submit_with_content(
        &self,
        content: Vec<ContentBlock>,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>> {
        if content.is_empty() {
            let err_stream = async_stream::stream! {
                yield AgentEvent::Error("Prompt cannot be empty".to_string());
            };
            return Box::pin(err_stream);
        }

        // Run UserPromptSubmit hook with text from first text block
        let text_preview: String = content.iter().filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        }).collect::<Vec<_>>().join("\n");

        let mut final_content = content;

        if self.hooks.has_hooks(HookEvent::UserPromptSubmit) {
            let ctx = self.hooks.prompt_ctx(HookEvent::UserPromptSubmit, Some(text_preview));
            match self.hooks.run(HookEvent::UserPromptSubmit, ctx).await {
                HookDecision::Block { reason } => {
                    let err_stream = async_stream::stream! {
                        yield AgentEvent::Error(format!("[UserPromptSubmit hook blocked]: {}", reason));
                    };
                    return Box::pin(err_stream);
                }
                HookDecision::AppendContext { text } => {
                    final_content.push(ContentBlock::Text { text });
                }
                _ => {}
            }
        }

        let (permission_mode, mut messages) = {
            let s = self.state.read().await;
            (s.permission_mode, s.messages.clone())
        };

        let user_msg = UserMessage {
            uuid: Uuid::new_v4().to_string(),
            content: final_content,
        };
        messages.push(Message::User(user_msg));

        let tools = self.tool_definitions();
        let tool_context = ToolContext {
            cwd: self.cwd.clone(),
            abort_signal: self.abort_signal.clone(),
            permission_mode,
            messages: Vec::new(),
        };

        query_stream(
            self.client.clone(),
            self.executor.clone(),
            self.state.clone(),
            tool_context,
            self.build_query_config(),
            messages,
            tools,
            self.hooks.clone(),
        )
    }

    pub fn state(&self) -> &SharedState {
        &self.state
    }

    /// Build a `QueryConfig` with shared auto-compact state from this engine.
    fn build_query_config(&self) -> QueryConfig {
        QueryConfig {
            system_prompt: self.config.system_prompt.clone(),
            max_turns: self.config.max_turns,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            thinking: self.config.thinking.clone(),
            token_budget: self.config.token_budget,
            context_window: self.context_window,
            auto_compact_state: Some(Arc::new(tokio::sync::Mutex::new(
                AutoCompactState::new(),
            ))),
        }
    }

    /// Get the cost tracker for displaying usage stats.
    pub fn cost_tracker(&self) -> &CostTracker {
        &self.cost_tracker
    }

    /// Number of tools registered in the tool registry.
    pub fn tool_count(&self) -> usize {
        self.registry.len()
    }

    /// Whether this engine is in coordinator (multi-agent) mode.
    pub fn is_coordinator(&self) -> bool {
        self.coordinator_mode
    }

    /// Drain any pending task notifications from background agents.
    /// Returns them as user-role messages containing `<task-notification>` XML.
    /// Call this between turns in the REPL to inject notifications into the conversation.
    pub async fn drain_notifications(&self) -> Vec<Message> {
        let rx = match &self.notification_rx {
            Some(rx) => rx,
            None => return Vec::new(),
        };
        let mut rx = rx.lock().await;
        let mut messages = Vec::new();
        while let Ok(notification) = rx.try_recv() {
            messages.push(notification.to_message());
        }
        messages
    }

    /// Get a clone of the abort signal so callers can cancel the running task.
    /// Call `.abort()` on the returned signal to interrupt tool execution and
    /// stop the agent loop at the next opportunity.
    pub fn abort_signal(&self) -> AbortSignal {
        self.abort_signal.clone()
    }

    /// Abort the current task (equivalent to Ctrl-C in the TS implementation).
    pub fn abort(&self) {
        self.abort_signal.abort();
    }

    /// Send a message to a running background sub-agent.
    ///
    /// Returns an error if not in coordinator mode or if the agent is not found.
    pub async fn send_to_agent(&self, agent_id: &str, message: &str) -> anyhow::Result<()> {
        let channels = self.agent_channels.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not in coordinator mode"))?;
        let channels = channels.read().await;
        let tx = channels.get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found or not running", agent_id))?;
        tx.send(message.to_string())
            .map_err(|_| anyhow::anyhow!("Agent '{}' channel closed", agent_id))?;
        Ok(())
    }

    /// Cancel a running background sub-agent.
    ///
    /// Returns an error if not in coordinator mode or if the agent is not found.
    pub async fn cancel_agent(&self, agent_id: &str) -> anyhow::Result<()> {
        let tokens = self.cancel_tokens.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not in coordinator mode"))?;
        let tokens = tokens.read().await;
        let token = tokens.get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found or not running", agent_id))?;
        token.cancel();
        Ok(())
    }

    /// Return tool names and descriptions for the ListTools bus request.
    pub fn tool_list(&self) -> Vec<(String, String, bool)> {
        self.registry
            .all()
            .iter()
            .map(|t| (t.name().to_string(), t.description().to_string(), t.is_enabled()))
            .collect()
    }

    /// Access the hook registry (for firing lifecycle events from task_runner, etc.)
    pub(crate) fn hooks(&self) -> &Arc<HookRegistry> {
        &self.hooks
    }

    /// Run a task autonomously to completion, streaming progress events.
    ///
    /// This is the primary entry point for non-interactive / programmatic use.
    /// It drives the full multi-turn agentic loop (planning → tool execution →
    /// verification → delivery) and returns a structured `TaskResult`.
    ///
    /// # Arguments
    /// - `task` — natural-language task description
    /// - `on_progress` — callback invoked for each `TaskProgress` event
    ///
    /// # Example
    /// ```rust,ignore
    /// let result = engine.run_task("Add a README.md with project description", |p| {
    ///     if let TaskProgress::Text(t) = p { print!("{}", t); }
    /// }).await;
    /// println!("Done in {} turns: {}", result.turns, result.reason);
    /// ```
    pub async fn run_task<F>(&self, task: &str, on_progress: F) -> TaskResult
    where
        F: FnMut(TaskProgress) + Send,
    {
        run_task(self, task, on_progress).await
    }

    /// Return the session ID (used by hooks).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Run SessionStart hooks — call once at startup.
    pub async fn run_session_start(&self) -> Option<String> {
        if !self.hooks.has_hooks(HookEvent::SessionStart) {
            return None;
        }
        let ctx = self.hooks.prompt_ctx(HookEvent::SessionStart, None);
        match self.hooks.run(HookEvent::SessionStart, ctx).await {
            HookDecision::AppendContext { text } => Some(text),
            _ => None,
        }
    }

    /// Compact the current conversation history.
    ///
    /// Fires PreCompact hooks (which can block or append custom instructions),
    /// calls Claude to summarise the conversation, replaces the history with a
    /// single system context message, then fires PostCompact hooks.
    ///
    /// Returns `Ok(summary)` on success, `Err` if the conversation is empty or
    /// the PreCompact hook blocked the operation.
    pub async fn compact(&self, trigger: &str, custom_instructions: Option<&str>) -> anyhow::Result<String> {
        let messages = {
            let s = self.state.read().await;
            s.messages.clone()
        };

        if messages.is_empty() {
            anyhow::bail!("Nothing to compact — conversation is empty.");
        }

        // ── PreCompact hook ──────────────────────────────────────────────────
        let mut extra_instructions = custom_instructions.map(|s| s.to_string());
        if self.hooks.has_hooks(HookEvent::PreCompact) {
            let ctx = self.hooks.compact_ctx(HookEvent::PreCompact, trigger, None);
            match self.hooks.run(HookEvent::PreCompact, ctx).await {
                HookDecision::Block { reason } => {
                    anyhow::bail!("Compaction blocked by PreCompact hook: {}", reason);
                }
                HookDecision::AppendContext { text } => {
                    extra_instructions = Some(match extra_instructions {
                        Some(existing) => format!("{}\n\n{}", existing, text),
                        None => text,
                    });
                }
                _ => {}
            }
        }

        // ── Call Claude for summary ──────────────────────────────────────────
        let model = { self.state.read().await.model.clone() };
        let summary = compact_conversation(
            &self.client,
            &messages,
            &model,
            extra_instructions.as_deref(),
        )
        .await?;

        // ── Replace conversation history ─────────────────────────────────────
        let context_msg = compact_context_message(&summary, None);
        {
            let mut s = self.state.write().await;
            s.messages = vec![Message::User(UserMessage {
                uuid: Uuid::new_v4().to_string(),
                content: vec![ContentBlock::Text { text: context_msg }],
            })];
            s.total_input_tokens = 0;
            s.total_output_tokens = 0;
        }

        // ── PostCompact hook ─────────────────────────────────────────────────
        if self.hooks.has_hooks(HookEvent::PostCompact) {
            let ctx = self.hooks.compact_ctx(
                HookEvent::PostCompact,
                trigger,
                Some(summary.clone()),
            );
            // Fire-and-forget
            let _ = self.hooks.run(HookEvent::PostCompact, ctx).await;
        }

        Ok(summary)
    }

    /// Check if auto-compact should trigger.
    ///
    /// Uses hybrid token counting: last API response's real token count plus
    /// rough estimation for messages added since.  Falls back to the simple
    /// fixed threshold for legacy callers that set a custom `compact_threshold`.
    pub async fn should_auto_compact(&self) -> bool {
        if self.compact_threshold == 0 {
            return false;
        }
        let s = self.state.read().await;
        // Hybrid counting: prefer API-reported usage + rough tail estimate
        let current_tokens = claude_core::token_estimation::token_count_with_estimation(&s.messages)
            + claude_core::token_estimation::estimate_system_tokens(&self.config.system_prompt);
        drop(s);

        let ac = self.auto_compact.lock().await;
        if self.context_window > 0 {
            ac.should_auto_compact(current_tokens, self.context_window)
        } else {
            // Fallback to simple threshold
            current_tokens >= self.compact_threshold
        }
    }

    /// Record a successful auto-compact (resets the circuit breaker).
    pub async fn record_compact_success(&self) {
        self.auto_compact.lock().await.record_success();
    }

    /// Record a failed auto-compact attempt (increments circuit breaker counter).
    pub async fn record_compact_failure(&self) {
        self.auto_compact.lock().await.record_failure();
    }

    /// Get the current context window usage as a percentage (0–100).
    /// Returns None if context window is unknown (0).
    pub async fn context_usage_percent(&self) -> Option<u8> {
        if self.context_window == 0 {
            return None;
        }
        let s = self.state.read().await;
        let current = claude_core::token_estimation::token_count_with_estimation(&s.messages)
            + claude_core::token_estimation::estimate_system_tokens(&self.config.system_prompt);
        let pct = (current as f64 / self.context_window as f64 * 100.0).min(100.0) as u8;
        Some(pct)
    }

    /// Clear conversation history and reset token counters.
    pub async fn clear_history(&self) {
        let mut s = self.state.write().await;
        s.messages.clear();
        s.turn_count = 0;
        s.total_input_tokens = 0;
        s.total_output_tokens = 0;
    }

    /// Get the last user message text from conversation history (for /retry).
    ///
    /// Returns `None` if no user messages exist.
    pub async fn last_user_prompt(&self) -> Option<String> {
        let s = self.state.read().await;
        s.messages.iter().rev().find_map(|msg| {
            if let Message::User(u) = msg {
                u.content.iter().find_map(|b| {
                    if let claude_core::message::ContentBlock::Text { text } = b {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        })
    }

    /// Undo the last assistant turn and return the user prompt that preceded it.
    ///
    /// Removes both the last assistant message and the last user message from history.
    /// Used by `/retry` to resend the last user prompt.
    pub async fn pop_last_turn(&self) -> Option<String> {
        let prompt = self.last_user_prompt().await?;
        let mut s = self.state.write().await;

        // Pop messages from the end until we've removed the last assistant + user pair
        let mut removed_assistant = false;
        while let Some(last) = s.messages.last() {
            match last {
                Message::Assistant(_) if !removed_assistant => {
                    s.messages.pop();
                    removed_assistant = true;
                }
                Message::User(_) if removed_assistant => {
                    s.messages.pop();
                    break;
                }
                _ if removed_assistant => {
                    break; // stop if we hit a non-user message
                }
                _ => {
                    s.messages.pop(); // skip tool result messages etc.
                }
            }
        }
        if s.turn_count > 0 {
            s.turn_count -= 1;
        }

        Some(prompt)
    }

    // ── Session persistence ──────────────────────────────────────────────────

    /// Save the current session to disk.
    pub async fn save_session(&self) -> anyhow::Result<()> {
        use claude_core::session::*;
        let s = self.state.read().await;
        let snapshot = SessionSnapshot {
            id: self.session_id.clone(),
            title: title_from_messages(&s.messages),
            model: s.model.clone(),
            cwd: self.cwd.to_string_lossy().to_string(),
            created_at: self.created_at,
            updated_at: chrono::Utc::now(),
            turn_count: s.turn_count,
            input_tokens: s.total_input_tokens,
            output_tokens: s.total_output_tokens,
            model_usage: s.model_usage.iter().map(|(k, v)| {
                (k.clone(), SessionModelUsage {
                    input_tokens: v.input_tokens,
                    output_tokens: v.output_tokens,
                    cache_read_tokens: v.cache_read_tokens,
                    cache_creation_tokens: v.cache_creation_tokens,
                    api_calls: v.api_calls,
                    cost_usd: v.cost_usd,
                })
            }).collect(),
            total_cost_usd: s.model_usage.values().map(|u| u.cost_usd).sum(),
            messages: s.messages.clone(),
            git_branch: None,
            custom_title: None,
            ai_title: None,
            summary: None,
            last_prompt: None,
        };
        save_session(&snapshot)
    }

    /// Restore a session from disk, replacing current state.
    /// Applies message sanitization to fix orphaned thinking blocks,
    /// unresolved tool references, and other artifacts from interrupted sessions.
    pub async fn restore_session(&self, session_id: &str) -> anyhow::Result<String> {
        use claude_core::session::load_session;
        use claude_core::message_sanitize::sanitize_messages;
        let snap = load_session(session_id)?;
        let title = snap.title.clone();
        let (sanitized_messages, report) = sanitize_messages(snap.messages);
        if report.has_changes() {
            tracing::info!("Session restore {}: {}", session_id, report.summary());
        }
        {
            let mut s = self.state.write().await;
            s.messages = sanitized_messages;
            s.model = snap.model;
            s.turn_count = snap.turn_count;
            s.total_input_tokens = snap.input_tokens;
            s.total_output_tokens = snap.output_tokens;
        }
        // Reset abort signal for new session
        self.abort_signal.reset();
        Ok(title)
    }

    /// Get the working directory.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// Update the CLAUDE.md portion of the system prompt (for /reload-context).
    pub async fn update_system_prompt_context(&self, claude_md: &str) {
        let mut s = self.state.write().await;
        // Store the refreshed CLAUDE.md content for next query_stream call.
        // The system prompt is rebuilt each query from config, so we just
        // note that context was reloaded.
        s.context_reloaded = true;
        s.claude_md_content = claude_md.to_string();
    }
}

#[cfg(test)]
mod tests;

