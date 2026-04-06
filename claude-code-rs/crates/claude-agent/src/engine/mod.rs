//! QueryEngine — the core agent orchestration engine.
//!
//! The engine drives the multi-turn agentic loop: accepting user prompts,
//! streaming API responses, executing tools, and managing conversation state.

mod builder;
pub use builder::QueryEngineBuilder;

use std::sync::Arc;

use claude_api::client::AnthropicClient;
use claude_api::types::{CacheControl, ToolDefinition};
use claude_core::message::{ContentBlock, Message, UserMessage};
use claude_core::tool::{AbortSignal, ToolContext};
use claude_tools::ToolRegistry;
use uuid::Uuid;

use crate::compact::{compact_conversation, compact_context_message, AutoCompactState};
use crate::coordinator::TaskNotification;
use crate::cost::CostTracker;
use crate::executor::ToolExecutor;
use crate::hooks::{HookDecision, HookEvent, HookRegistry};
use crate::query::{query_stream, AgentEvent, QueryConfig};
use crate::state::SharedState;
use crate::task_runner::{run_task, TaskProgress, TaskResult};

pub struct QueryEngine {
    client: Arc<AnthropicClient>,
    executor: Arc<ToolExecutor>,
    registry: Arc<ToolRegistry>,
    state: SharedState,
    config: QueryConfig,
    hooks: Arc<HookRegistry>,
    cwd: std::path::PathBuf,
    session_id: String,
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
            QueryConfig {
                system_prompt: self.config.system_prompt.clone(),
                max_turns: self.config.max_turns,
                max_tokens: self.config.max_tokens,
                temperature: self.config.temperature,
                thinking: self.config.thinking.clone(),
                token_budget: self.config.token_budget,
            },
            messages,
            tools,
            self.hooks.clone(),
        )
    }

    pub fn state(&self) -> &SharedState {
        &self.state
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
    /// Uses the `AutoCompactState` circuit breaker and model-specific context
    /// window when available; falls back to the simple fixed threshold for
    /// legacy callers that set a custom `compact_threshold`.
    pub async fn should_auto_compact(&self) -> bool {
        if self.compact_threshold == 0 {
            return false;
        }
        let s = self.state.read().await;
        let current_tokens = if s.total_input_tokens > 0 {
            s.total_input_tokens
        } else {
            claude_core::token_estimation::estimate_messages_tokens(&s.messages)
                + claude_core::token_estimation::estimate_system_tokens(&self.config.system_prompt)
        };
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

    /// Clear conversation history and reset token counters.
    pub async fn clear_history(&self) {
        let mut s = self.state.write().await;
        s.messages.clear();
        s.turn_count = 0;
        s.total_input_tokens = 0;
        s.total_output_tokens = 0;
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
            created_at: chrono::Utc::now(), // approximate
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
        };
        save_session(&snapshot)
    }

    /// Restore a session from disk, replacing current state.
    pub async fn restore_session(&self, session_id: &str) -> anyhow::Result<String> {
        use claude_core::session::load_session;
        let snap = load_session(session_id)?;
        let title = snap.title.clone();
        {
            let mut s = self.state.write().await;
            s.messages = snap.messages;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── QueryEngineBuilder ───────────────────────────────────────────

    #[test]
    fn test_builder_defaults() {
        let b = QueryEngineBuilder::new("test-key", "/tmp");
        assert_eq!(b.api_key, "test-key");
        assert_eq!(b.max_turns, 100);
        assert_eq!(b.max_tokens, 16384);
        assert!(b.model.is_none());
        assert!(b.system_prompt.is_empty());
        assert!(b.load_claude_md);
        assert!(b.load_memory);
        assert!(!b.coordinator_mode);
        assert!(b.allowed_tools.is_empty());
    }

    #[test]
    fn test_builder_fluent_api() {
        let b = QueryEngineBuilder::new("key", "/tmp")
            .model("claude-haiku")
            .system_prompt("Hello")
            .max_turns(50)
            .max_tokens(8192)
            .compact_threshold(40_000)
            .coordinator_mode(true)
            .load_claude_md(false)
            .load_memory(false)
            .allowed_tools(vec!["Read".into(), "Bash".into()])
            .language(Some("中文".into()))
            .scratchpad_dir(Some("/tmp/scratchpad".into()));

        assert_eq!(b.model.as_deref(), Some("claude-haiku"));
        assert_eq!(b.system_prompt, "Hello");
        assert_eq!(b.max_turns, 50);
        assert_eq!(b.max_tokens, 8192);
        assert_eq!(b.compact_threshold, 40_000);
        assert!(b.coordinator_mode);
        assert!(!b.load_claude_md);
        assert!(!b.load_memory);
        assert_eq!(b.allowed_tools, vec!["Read", "Bash"]);
        assert_eq!(b.language.as_deref(), Some("中文"));
        assert_eq!(b.scratchpad_dir.as_deref(), Some("/tmp/scratchpad"));
    }

    #[test]
    fn test_builder_thinking_config() {
        let b = QueryEngineBuilder::new("key", "/tmp")
            .thinking(Some(claude_api::types::ThinkingConfig {
                thinking_type: "enabled".into(),
                budget_tokens: Some(4096),
            }));

        let tc = b.thinking.as_ref().unwrap();
        assert_eq!(tc.thinking_type, "enabled");
        assert_eq!(tc.budget_tokens, Some(4096));
    }

    #[test]
    fn test_builder_output_style() {
        let b = QueryEngineBuilder::new("key", "/tmp")
            .output_style("Concise".into(), "Be brief.".into());

        let (name, prompt) = b.output_style.as_ref().unwrap();
        assert_eq!(name, "Concise");
        assert_eq!(prompt, "Be brief.");
    }

    #[test]
    fn test_builder_mcp_instructions() {
        let b = QueryEngineBuilder::new("key", "/tmp")
            .mcp_instructions(vec![
                ("github".into(), "Use GitHub MCP for repos".into()),
                ("slack".into(), "Use Slack MCP for messaging".into()),
            ]);

        assert_eq!(b.mcp_instructions.len(), 2);
        assert_eq!(b.mcp_instructions[0].0, "github");
    }

    fn build_test_engine() -> QueryEngine {
        QueryEngineBuilder::new("fake-key", "/tmp")
            .load_claude_md(false)
            .load_memory(false)
            .build()
    }

    #[test]
    fn test_builder_build_creates_engine() {
        // Build with minimal config (no claude_md, no memory) to avoid FS access
        let engine = QueryEngineBuilder::new("fake-key", "/tmp")
            .load_claude_md(false)
            .load_memory(false)
            .model("test-model")
            .max_turns(5)
            .build();

        assert_eq!(engine.cwd(), std::path::Path::new("/tmp"));
        assert!(!engine.is_coordinator());
        assert_eq!(engine.config.max_turns, 5);
    }

    #[test]
    fn test_builder_build_coordinator_mode() {
        let engine = QueryEngineBuilder::new("fake-key", "/tmp")
            .load_claude_md(false)
            .load_memory(false)
            .coordinator_mode(true)
            .build();

        assert!(engine.is_coordinator());
    }

    #[test]
    fn test_engine_abort_signal() {
        let engine = build_test_engine();

        let signal = engine.abort_signal();
        assert!(!signal.is_aborted());
        engine.abort();
        assert!(signal.is_aborted());
    }

    // ── tool_definitions ─────────────────────────────────────────────

    #[test]
    fn test_tool_definitions_non_empty() {
        let engine = build_test_engine();

        let defs = engine.tool_definitions();
        assert!(!defs.is_empty(), "should have tool definitions");
    }

    #[test]
    fn test_tool_definitions_last_has_cache_control() {
        let engine = build_test_engine();

        let defs = engine.tool_definitions();
        let last = defs.last().unwrap();
        assert!(last.cache_control.is_some(), "last tool def should have cache_control");
    }

    #[test]
    fn test_tool_definitions_filtered_by_allowed_tools() {
        let engine = QueryEngineBuilder::new("fake-key", "/tmp")
            .load_claude_md(false)
            .load_memory(false)
            .allowed_tools(vec!["Read".into(), "Write".into()])
            .build();

        let defs = engine.tool_definitions();
        assert!(defs.len() <= 3, "should only have allowed tools + DispatchAgent");
        for def in &defs {
            // DispatchAgent is always registered; Read/Write are the only allowed user tools
            assert!(
                def.name == "Read" || def.name == "Write" || def.name == "DispatchAgent",
                "unexpected tool: {}",
                def.name
            );
        }
    }

    // ── should_auto_compact ──────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_auto_compact_disabled_when_zero() {
        let engine = QueryEngineBuilder::new("fake-key", "/tmp")
            .load_claude_md(false)
            .load_memory(false)
            .compact_threshold(0)
            .build();

        assert!(!engine.should_auto_compact().await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_auto_compact_not_triggered_when_empty() {
        let engine = build_test_engine();

        // Empty conversation → token count is tiny → no auto-compact
        assert!(!engine.should_auto_compact().await);
    }

    // ── drain_notifications ──────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_drain_notifications_empty_when_not_coordinator() {
        let engine = build_test_engine();

        let msgs = engine.drain_notifications().await;
        assert!(msgs.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_drain_notifications_coordinator() {
        let engine = QueryEngineBuilder::new("fake-key", "/tmp")
            .load_claude_md(false)
            .load_memory(false)
            .coordinator_mode(true)
            .build();

        // No notifications sent → drain returns empty
        let msgs = engine.drain_notifications().await;
        assert!(msgs.is_empty());
    }

    // ── run_session_start ────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_run_session_start_no_hooks() {
        let engine = build_test_engine();

        // No hooks configured → returns None
        let result = engine.run_session_start().await;
        assert!(result.is_none());
    }

    // ── submit empty prompt ──────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_submit_empty_prompt_returns_error() {
        use tokio_stream::StreamExt as _;

        let engine = build_test_engine();
        let mut stream = engine.submit("").await;
        let first = stream.next().await;
        match first {
            Some(AgentEvent::Error(msg)) => {
                assert!(msg.contains("empty"), "expected empty-prompt error, got: {msg}");
            }
            other => panic!("expected Error event, got: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_submit_whitespace_prompt_returns_error() {
        use tokio_stream::StreamExt as _;

        let engine = build_test_engine();
        let mut stream = engine.submit("   \n\t  ").await;
        let first = stream.next().await;
        match first {
            Some(AgentEvent::Error(msg)) => {
                assert!(msg.contains("empty"), "expected empty-prompt error, got: {msg}");
            }
            other => panic!("expected Error event, got: {other:?}"),
        }
    }
}
