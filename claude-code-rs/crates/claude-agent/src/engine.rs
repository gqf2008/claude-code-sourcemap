use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use claude_api::client::AnthropicClient;
use claude_api::types::{CacheControl, ToolDefinition};
use claude_core::claude_md::load_claude_md;
use claude_core::config::HooksConfig;
use claude_core::memory::load_memories_for_prompt;
use claude_core::message::{ContentBlock, Message, UserMessage};
use claude_core::tool::{AbortSignal, ToolContext};
use claude_core::permissions::PermissionMode;
use claude_tools::ToolRegistry;
use tokio::sync::RwLock;

use crate::compact::{compact_conversation, compact_context_message, AUTO_COMPACT_THRESHOLD};
use crate::coordinator::{AgentTracker, SendMessageTool, TaskStopTool, TaskNotification};
use crate::dispatch_agent::{DispatchAgentTool, SubAgentConfig};
use crate::executor::ToolExecutor;
use crate::hooks::{HookDecision, HookEvent, HookRegistry};
use crate::permissions::PermissionChecker;
use crate::query::{query_stream, AgentEvent, QueryConfig};
use crate::state::{new_shared_state, SharedState};
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
}

pub struct QueryEngineBuilder {
    api_key: String,
    model: Option<String>,
    cwd: std::path::PathBuf,
    system_prompt: String,
    max_turns: u32,
    max_tokens: u32,
    permission_checker: PermissionChecker,
    hooks_config: HooksConfig,
    /// If true, scan and prepend CLAUDE.md files to the system prompt.
    load_claude_md: bool,
    /// If true, scan and prepend memory files to the system prompt.
    load_memory: bool,
    /// Token threshold for auto-compaction (0 = disabled).
    compact_threshold: u64,
    /// Enable coordinator (multi-agent orchestration) mode.
    coordinator_mode: bool,
    /// If non-empty, only these tools are available to the model.
    allowed_tools: Vec<String>,
    /// Extended thinking configuration.
    thinking: Option<claude_api::types::ThinkingConfig>,
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
            hooks_config: HooksConfig::default(),
            load_claude_md: true,
            load_memory: true,
            compact_threshold: AUTO_COMPACT_THRESHOLD,
            coordinator_mode: false,
            allowed_tools: Vec::new(),
            thinking: None,
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

    pub fn hooks_config(mut self, config: HooksConfig) -> Self {
        self.hooks_config = config;
        self
    }

    pub fn load_claude_md(mut self, enable: bool) -> Self {
        self.load_claude_md = enable;
        self
    }

    pub fn load_memory(mut self, enable: bool) -> Self {
        self.load_memory = enable;
        self
    }

    pub fn compact_threshold(mut self, tokens: u64) -> Self {
        self.compact_threshold = tokens;
        self
    }

    pub fn coordinator_mode(mut self, enable: bool) -> Self {
        self.coordinator_mode = enable;
        self
    }

    pub fn allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    pub fn thinking(mut self, config: Option<claude_api::types::ThinkingConfig>) -> Self {
        self.thinking = config;
        self
    }

    pub fn build(self) -> QueryEngine {
        let mut client = AnthropicClient::new(self.api_key);
        if let Some(ref model) = self.model {
            client = client.with_model(model);
        }
        client = client.with_max_tokens(self.max_tokens);

        let client = Arc::new(client);
        let mut registry = ToolRegistry::with_defaults();
        let permission_checker = Arc::new(self.permission_checker);

        let model_name = self.model.clone().unwrap_or_else(|| "claude-sonnet-4-20250514".into());
        let system_prompt = if self.load_claude_md {
            let md = load_claude_md(&self.cwd);
            if md.is_empty() {
                self.system_prompt.clone()
            } else if self.system_prompt.is_empty() {
                md
            } else {
                format!("{}\n\n---\n\n{}", md, self.system_prompt)
            }
        } else {
            self.system_prompt.clone()
        };

        // Optionally prepend memory files
        let system_prompt = if self.load_memory {
            if let Some(mem) = load_memories_for_prompt(&self.cwd) {
                if system_prompt.is_empty() {
                    mem
                } else {
                    format!("{}\n\n---\n\n{}", mem, system_prompt)
                }
            } else {
                system_prompt
            }
        } else {
            system_prompt
        };

        let sub_registry = Arc::new(ToolRegistry::with_defaults());

        // ── Coordinator mode setup ───────────────────────────────────────────
        let (agent_tracker, notification_rx, coord_cancel_tokens, coord_agent_channels) = if self.coordinator_mode {
            let (tracker, rx) = AgentTracker::new();
            let agent_channels: Arc<RwLock<HashMap<String, tokio::sync::mpsc::UnboundedSender<String>>>> =
                Arc::new(RwLock::new(HashMap::new()));
            let cancel_tokens: Arc<RwLock<HashMap<String, tokio_util::sync::CancellationToken>>> =
                Arc::new(RwLock::new(HashMap::new()));

            // Register coordinator-only tools
            registry.register(SendMessageTool {
                tracker: tracker.clone(),
                agent_channels: agent_channels.clone(),
            });
            registry.register(TaskStopTool {
                tracker: tracker.clone(),
                cancel_tokens: cancel_tokens.clone(),
            });

            (
                Some(tracker),
                Some(tokio::sync::Mutex::new(rx)),
                Some(cancel_tokens),
                Some(agent_channels),
            )
        } else {
            (None, None, None, None)
        };

        let dispatch_tool = DispatchAgentTool {
            client: client.clone(),
            registry: sub_registry,
            permission_checker: permission_checker.clone(),
            config: SubAgentConfig {
                model: model_name.clone(),
                max_tokens: self.max_tokens,
                cwd: self.cwd.clone(),
                system_prompt: system_prompt.clone(),
                max_turns: self.max_turns,
            },
            agent_tracker,
            cancel_tokens: coord_cancel_tokens,
            agent_channels: coord_agent_channels,
        };
        registry.register(dispatch_tool);

        let registry = Arc::new(registry);

        let session_id = uuid::Uuid::new_v4().to_string();
        let hooks = Arc::new(HookRegistry::from_config(
            self.hooks_config,
            self.cwd.clone(),
            session_id.clone(),
        ));
        let executor = Arc::new(ToolExecutor::with_hooks(
            registry.clone(),
            permission_checker,
            hooks.clone(),
        ));

        let state = new_shared_state();
        {
            let mut s = state.blocking_write();
            s.model = model_name.clone();
        }
        let abort_signal = AbortSignal::new();

        QueryEngine {
            client,
            executor,
            registry,
            state,
            config: QueryConfig {
                system_prompt,
                max_turns: self.max_turns,
                max_tokens: self.max_tokens,
                temperature: None,
                thinking: self.thinking.clone(),
            },
            hooks,
            cwd: self.cwd,
            session_id,
            compact_threshold: self.compact_threshold,
            abort_signal,
            notification_rx,
            coordinator_mode: self.coordinator_mode,
            allowed_tools: self.allowed_tools,
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
            last.cache_control = Some(CacheControl { control_type: "ephemeral".into() });
        }
        defs
    }

    /// Submit a user message and get back a stream of AgentEvents.
    pub async fn submit(
        &self,
        user_prompt: impl Into<String>,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>> {
        let mut prompt_text: String = user_prompt.into();

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
            },
            messages,
            tools,
            self.hooks.clone(),
        )
    }

    pub fn state(&self) -> &SharedState {
        &self.state
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
    /// ```no_run
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

    /// Check if auto-compact should trigger (returns true if over threshold).
    ///
    /// Uses API-reported tokens when available; falls back to local estimation
    /// (e.g., when resuming a session before any API calls are made).
    pub async fn should_auto_compact(&self) -> bool {
        if self.compact_threshold == 0 {
            return false;
        }
        let s = self.state.read().await;
        if s.total_input_tokens > 0 {
            return s.total_input_tokens >= self.compact_threshold;
        }
        // No API-reported tokens yet — use local estimation
        let estimated = claude_core::token_estimation::estimate_messages_tokens(&s.messages)
            + claude_core::token_estimation::estimate_system_tokens(&self.config.system_prompt);
        estimated >= self.compact_threshold
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
