use std::sync::Arc;
use std::path::PathBuf;

use async_trait::async_trait;
use claude_api::client::ApiClient;
use claude_api::types::ToolDefinition;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio_stream::StreamExt;

use crate::coordinator::AgentTracker;
use crate::executor::ToolExecutor;
use crate::hooks::HookRegistry;
use crate::permissions::PermissionChecker;
use crate::query::{query_stream, query_stream_with_injection, AgentEvent, QueryConfig};
use crate::state::new_shared_state;
use claude_tools::ToolRegistry;

/// Shared map of cancellation tokens, keyed by agent ID.
pub type CancelTokenMap = Arc<tokio::sync::RwLock<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>>;

/// Shared map of agent message channels, keyed by agent ID.
pub type AgentChannelMap = Arc<tokio::sync::RwLock<std::collections::HashMap<String, tokio::sync::mpsc::UnboundedSender<String>>>>;

/// Configuration passed into the sub-agent.
pub struct SubAgentConfig {
    pub model: String,
    pub max_tokens: u32,
    pub cwd: PathBuf,
    pub system_prompt: String,
    pub max_turns: u32,
    /// Model context window size in tokens (used for context percentage warnings).
    pub context_window: u64,
}

/// Built-in agent type profiles aligned with the TS codebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentType {
    /// General-purpose sub-agent with full tool access (default).
    General,
    /// Fast exploration agent — read-only tools, lower turn limit.
    Explore,
    /// Planning agent — can read files and create/update tasks.
    Plan,
    /// Code review agent — read-only tools, focused on analysis.
    CodeReview,
    /// Verification agent — runs tests, checks correctness (read-only bias).
    Verification,
    /// Worker agent — spawned by coordinator, full tool access.
    Worker,
}

impl AgentType {
    fn from_str(s: &str) -> Self {
        match s {
            "explore" => Self::Explore,
            "plan" => Self::Plan,
            "code-review" | "code_review" | "review" => Self::CodeReview,
            "verification" | "verify" => Self::Verification,
            "worker" => Self::Worker,
            _ => Self::General,
        }
    }

    /// Human-readable description of when to use this agent type.
    pub fn when_to_use(&self) -> &'static str {
        match self {
            Self::General => "For implementation tasks requiring full tool access",
            Self::Explore => "For fast codebase investigation — read-only, lower cost",
            Self::Plan => "For task decomposition and planning without implementation",
            Self::CodeReview => "For analyzing code quality, bugs, and security concerns",
            Self::Verification => "For running tests and checking correctness of changes",
            Self::Worker => "Spawned by coordinator for delegated subtasks",
        }
    }

    /// Whether this agent should default to background execution.
    pub fn default_background(&self) -> bool {
        matches!(self, Self::Worker)
    }

    /// Whether this agent should run in isolated mode (reduced tool access).
    pub fn is_isolated(&self) -> bool {
        matches!(self, Self::Explore | Self::CodeReview | Self::Verification)
    }

    fn system_prompt(&self, base: &str) -> String {
        match self {
            Self::General | Self::Worker => base.to_string(),
            Self::Explore => format!(
                "{}\n\nYou are an exploration agent. Your job is to investigate the codebase \
                 and gather information. You should ONLY read files and search — do not modify \
                 anything. Be thorough but concise in your findings. Summarize what you discover.",
                base
            ),
            Self::Plan => format!(
                "{}\n\nYou are a planning agent. Analyze the request, break it down into \
                 actionable tasks using task_create, and identify dependencies between them. \
                 Read relevant code to inform your plan. Do not implement changes yourself.",
                base
            ),
            Self::CodeReview => format!(
                "{}\n\nYou are a code review agent. Analyze the code for bugs, style issues, \
                 security concerns, and potential improvements. Be specific about file paths \
                 and line numbers. Do not modify any files.",
                base
            ),
            Self::Verification => format!(
                "{}\n\nYou are a verification agent. Your job is to run tests, check builds, \
                 and verify correctness of changes. Report pass/fail status clearly. \
                 Do not modify source code — only run diagnostics.",
                base
            ),
        }
    }

    fn max_turns(&self, configured: u32) -> u32 {
        match self {
            Self::General | Self::Worker => configured.min(20),
            Self::Explore => configured.min(10),
            Self::Plan => configured.min(15),
            Self::CodeReview => configured.min(15),
            Self::Verification => configured.min(10),
        }
    }

    /// Returns true if this agent type should be restricted to read-only tools.
    fn read_only(&self) -> bool {
        matches!(self, Self::Explore | Self::CodeReview | Self::Verification)
    }

    /// Preferred model alias for this agent type.
    /// Returns `None` for "inherit" (use parent model).
    fn preferred_model(&self) -> Option<&'static str> {
        match self {
            Self::Explore => Some("haiku"),
            _ => None, // inherit parent model
        }
    }
}

/// Resolve a model alias ("haiku", "sonnet", "opus") to a concrete model name.
/// If `alias` is None or "inherit", returns the `parent_model` unchanged.
pub fn resolve_agent_model(alias: Option<&str>, parent_model: &str) -> String {
    match alias {
        None | Some("inherit") => parent_model.to_string(),
        Some(other) => {
            // Try alias resolution via model module
            claude_core::model::resolve_alias(other)
                .map(|s| s.to_string())
                .unwrap_or_else(|| other.to_string())
        }
    }
}

/// AgentTool — spawns a sub-agent to execute a given prompt.
///
/// The sub-agent runs its own query loop with isolated conversation and returns
/// its final text output. In coordinator mode, if `run_in_background` is true,
/// the agent is spawned via `tokio::spawn` and the tool returns immediately
/// with an `agent_id`. Results are delivered as `<task-notification>` XML via
/// the `AgentTracker`.
///
/// Aligned with TS `tools/AgentTool.ts` — this is the single "Agent" tool
/// that the model calls. There is no separate stub.
pub struct DispatchAgentTool {
    pub client: Arc<ApiClient>,
    pub registry: Arc<ToolRegistry>,
    pub permission_checker: Arc<PermissionChecker>,
    pub config: SubAgentConfig,
    /// Optional tracker for background agent execution (coordinator mode).
    pub agent_tracker: Option<AgentTracker>,
    /// Shared cancel tokens — used by TaskStop to abort background agents.
    pub cancel_tokens: Option<CancelTokenMap>,
    /// Shared agent message channels — used by SendMessage to deliver follow-ups.
    pub agent_channels: Option<AgentChannelMap>,
}

#[async_trait]
impl Tool for DispatchAgentTool {
    fn name(&self) -> &str { "Agent" }

    fn description(&self) -> &str {
        "Launch a sub-agent to accomplish an independent task. The sub-agent runs a full \
         agentic loop with its own conversation and tool permissions, then returns its \
         output. Use for parallel work, research, verification, or when isolation is needed. \
         The sub-agent cannot interact with the user.\n\n\
         Agent types:\n\
         - \"general\" (default): Full tool access, up to 20 turns\n\
         - \"explore\": Read-only, fast investigation, up to 10 turns\n\
         - \"plan\": Read + task management, up to 15 turns\n\
         - \"code-review\": Read-only code analysis, up to 15 turns\n\
         - \"verification\": Run tests and check correctness, up to 10 turns\n\
         - \"worker\": Full tool access, spawned by coordinator"
    }

    fn to_auto_classifier_input(&self, input: &Value) -> Value {
        // Only pass agent type and description; strip the full prompt
        let agent_type = input.get("agent_type").cloned().unwrap_or(Value::Null);
        let desc = input.get("description").cloned().unwrap_or(Value::Null);
        json!({"Agent": {"type": agent_type, "description": desc}})
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task prompt for the sub-agent. Be specific and provide \
                                    all necessary context."
                },
                "description": {
                    "type": "string",
                    "description": "A short (3-5 word) description of what the agent will do. \
                                    Displayed in status UI."
                },
                "agent_type": {
                    "type": "string",
                    "enum": ["general", "explore", "plan", "code-review", "verification", "worker"],
                    "description": "The type of agent to launch. Determines available tools \
                                    and system prompt. Default: general."
                },
                "name": {
                    "type": "string",
                    "description": "Optional human-readable name for the agent, used for \
                                    identification in status output and SendMessage routing."
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of tool names available to the sub-agent. \
                                    Overrides agent_type defaults if provided."
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional system prompt override for the sub-agent."
                },
                "model": {
                    "type": "string",
                    "description": "Model alias for the sub-agent: 'haiku', 'sonnet', 'opus', \
                                    'inherit', or a concrete model name. Default: determined \
                                    by agent_type (explore=haiku, others=inherit parent model)."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "If true, the sub-agent runs in the background. Returns immediately \
                                    with an agent_id. Results are delivered as <task-notification>. \
                                    Default: false."
                }
            },
            "required": ["prompt"]
        })
    }

    // Sub-agents are read-only from the permission perspective (they ask themselves)
    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let prompt = input["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt'"))?
            .to_string();

        let agent_type = input["agent_type"]
            .as_str()
            .map(AgentType::from_str)
            .unwrap_or(AgentType::General);

        let allowed_tools: Option<Vec<String>> = input["allowed_tools"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect());

        let system_prompt = input["system_prompt"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| agent_type.system_prompt(&self.config.system_prompt));

        let run_in_background = input["run_in_background"]
            .as_bool()
            .unwrap_or(false)
            || self.agent_tracker.is_some(); // coordinator mode → always background

        let agent_name = input["name"].as_str().map(String::from);
        let agent_description = input["description"].as_str().map(String::from);

        // Build tool definitions for the sub-agent (optionally filtered)
        let all_tool_defs: Vec<ToolDefinition> = self.registry
            .all()
            .iter()
            .filter(|t| t.is_enabled())
            // Sub-agents cannot use interactive tools or spawn nested agents
            .filter(|t| !matches!(t.name(), "AskUserQuestion" | "Agent" | "SendMessage" | "TaskStop"))
            // Agent-type based filtering
            .filter(|t| {
                if let Some(ref allowed) = allowed_tools {
                    return allowed.contains(&t.name().to_string());
                }
                if agent_type.read_only() {
                    return t.is_read_only();
                }
                true
            })
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
                cache_control: None,
            })
            .collect();

        let executor = Arc::new(ToolExecutor::new(
            self.registry.clone(),
            self.permission_checker.clone(),
        ));
        let state = new_shared_state();

        // Resolve model: agent type preferred model → input model → parent model
        let agent_model = input["model"]
            .as_str()
            .map(|m| resolve_agent_model(Some(m), &self.config.model))
            .unwrap_or_else(|| {
                resolve_agent_model(agent_type.preferred_model(), &self.config.model)
            });

        {
            let mut s = state.write().await;
            s.model = agent_model.clone();
        }

        let tool_context = ToolContext {
            cwd: context.cwd.clone(),
            abort_signal: context.abort_signal.clone(),
            permission_mode: context.permission_mode,
            messages: Vec::new(),
        };

        // Bootstrap with the user prompt as the first message
        use uuid::Uuid;
        use claude_core::message::{ContentBlock, Message, UserMessage};
        let init_messages = vec![Message::User(UserMessage {
            uuid: Uuid::new_v4().to_string(),
            content: vec![ContentBlock::Text { text: prompt.clone() }],
        })];

        let query_config = QueryConfig {
            system_prompt,
            max_turns: agent_type.max_turns(self.config.max_turns),
            max_tokens: self.config.max_tokens,
            temperature: None,
            thinking: None,
            token_budget: 0,
            context_window: self.config.context_window,
            auto_compact_state: None, // sub-agents don't proactively compact
        };

        // Sub-agents run without user-defined hooks to avoid re-entrant side effects
        let no_hooks = Arc::new(HookRegistry::new());

        // ── Background execution (coordinator mode) ─────────────────────────
        if run_in_background {
            if let Some(ref tracker) = self.agent_tracker {
                let agent_id = format!("agent-{}", &Uuid::new_v4().to_string()[..8]);
                tracker.register(
                    &agent_id,
                    &prompt,
                    agent_name.as_deref(),
                    agent_description.as_deref(),
                ).await;

                // Create a CancellationToken so TaskStop can abort this agent
                let cancel_token = tokio_util::sync::CancellationToken::new();
                if let Some(ref tokens) = self.cancel_tokens {
                    tokens.write().await.insert(agent_id.clone(), cancel_token.clone());
                }

                // Override the abort signal with one linked to the cancel token
                let agent_abort = claude_core::tool::AbortSignal::new();
                let tool_context = ToolContext {
                    cwd: tool_context.cwd,
                    abort_signal: agent_abort.clone(),
                    permission_mode: tool_context.permission_mode,
                    messages: Vec::new(),
                };

                // Create message channel so SendMessage can deliver follow-ups
                let (msg_tx, msg_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                if let Some(ref channels) = self.agent_channels {
                    let mut ch = channels.write().await;
                    ch.insert(agent_id.clone(), msg_tx);
                    // Also register under human-readable name for name-based routing
                    if let Some(ref name) = agent_name {
                        let tx = ch.get(&agent_id).cloned();
                        if let Some(tx) = tx {
                            ch.insert(name.clone(), tx);
                        }
                    }
                }

                let client = self.client.clone();
                let tracker = tracker.clone();
                let agent_id_clone = agent_id.clone();
                let cancel_tokens = self.cancel_tokens.clone();
                let agent_channels = self.agent_channels.clone();
                let agent_name_clone = agent_name.clone();

                // Acquire concurrency permit — blocks if at max parallel agents
                let permit = match tracker.acquire_permit().await {
                    Ok(p) => p,
                    Err(_) => {
                        return Ok(ToolResult::error(
                            "Failed to acquire agent concurrency permit (semaphore closed)"
                        ));
                    }
                };

                tokio::spawn(async move {
                    // Hold permit for the lifetime of the agent — released on drop
                    let _permit = permit;
                    let mut stream = query_stream_with_injection(
                        client,
                        executor,
                        state,
                        tool_context,
                        query_config,
                        init_messages,
                        all_tool_defs,
                        no_hooks,
                        Some(msg_rx),
                    );

                    let mut output = String::new();
                    let mut tool_use_count: u32 = 0;
                    let mut total_tokens: u64 = 0;

                    loop {
                        tokio::select! {
                            _ = cancel_token.cancelled() => {
                                agent_abort.abort();
                                if tracker.is_running(&agent_id_clone).await {
                                    tracker.kill(&agent_id_clone).await;
                                }
                                break;
                            }
                            event = stream.next() => {
                                match event {
                                    Some(AgentEvent::TextDelta(text)) => output.push_str(&text),
                                    Some(AgentEvent::ToolUseStart { ref name, .. }) => {
                                        tool_use_count += 1;
                                        tracker.record_progress(
                                            &agent_id_clone,
                                            tool_use_count,
                                            total_tokens,
                                            Some(name.clone()),
                                        ).await;
                                    }
                                    Some(AgentEvent::UsageUpdate(u)) => {
                                        total_tokens += u.input_tokens + u.output_tokens;
                                        tracker.record_progress(
                                            &agent_id_clone,
                                            tool_use_count,
                                            total_tokens,
                                            None,
                                        ).await;
                                    }
                                    Some(AgentEvent::Error(e)) => {
                                        let error_with_context = if output.is_empty() {
                                            e
                                        } else {
                                            format!("Error after partial output:\n{}\n\nError: {}", output, e)
                                        };
                                        tracker.fail(&agent_id_clone, error_with_context).await;
                                        break;
                                    }
                                    None => {
                                        tracker
                                            .complete(&agent_id_clone, output, total_tokens, tool_use_count)
                                            .await;
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    // Clean up cancel token and agent channel
                    if let Some(ref tokens) = cancel_tokens {
                        tokens.write().await.remove(&agent_id_clone);
                    }
                    if let Some(ref channels) = agent_channels {
                        let mut ch = channels.write().await;
                        ch.remove(&agent_id_clone);
                        // Also remove name-based channel entry
                        if let Some(ref name) = agent_name_clone {
                            ch.remove(name);
                        }
                    }
                    tracker.remove(&agent_id_clone).await;
                });

                let desc = agent_description.as_deref().unwrap_or("background agent");
                let mut response = json!({
                    "status": "async_launched",
                    "agent_id": agent_id,
                    "description": desc,
                    "message": "Agent is running in the background. Results will be delivered as a <task-notification>."
                });
                if let Some(ref name) = agent_name {
                    response["name"] = json!(name);
                }
                return Ok(ToolResult::text(
                    serde_json::to_string_pretty(&response)
                        .unwrap_or_else(|_| r#"{"status":"async_launched"}"#.to_string()),
                ));
            }
        }

        // ── Synchronous execution (default) ─────────────────────────────────
        let mut stream = query_stream(
            self.client.clone(),
            executor,
            state,
            tool_context,
            query_config,
            init_messages,
            all_tool_defs,
            no_hooks,
        );

        // Collect all text output from the sub-agent
        let mut output = String::new();
        let mut error_msg: Option<String> = None;

        while let Some(event) = stream.next().await {
            match event {
                AgentEvent::TextDelta(text) => output.push_str(&text),
                AgentEvent::Error(e) => {
                    error_msg = Some(e);
                    break;
                }
                _ => {}
            }
        }

        if let Some(err) = error_msg {
            return Ok(ToolResult::error(format!("Sub-agent error: {}", err)));
        }

        if output.trim().is_empty() {
            Ok(ToolResult::text("Sub-agent completed with no text output."))
        } else {
            Ok(ToolResult::text(output))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── AgentType parsing ────────────────────────────────────────────────

    #[test]
    fn agent_type_from_str_general() {
        assert_eq!(AgentType::from_str("general"), AgentType::General);
        assert_eq!(AgentType::from_str("unknown"), AgentType::General);
        assert_eq!(AgentType::from_str(""), AgentType::General);
    }

    #[test]
    fn agent_type_from_str_explore() {
        assert_eq!(AgentType::from_str("explore"), AgentType::Explore);
    }

    #[test]
    fn agent_type_from_str_plan() {
        assert_eq!(AgentType::from_str("plan"), AgentType::Plan);
    }

    #[test]
    fn agent_type_from_str_code_review_variants() {
        assert_eq!(AgentType::from_str("code-review"), AgentType::CodeReview);
        assert_eq!(AgentType::from_str("code_review"), AgentType::CodeReview);
        assert_eq!(AgentType::from_str("review"), AgentType::CodeReview);
    }

    // ── AgentType properties ─────────────────────────────────────────────

    #[test]
    fn agent_type_read_only() {
        assert!(!AgentType::General.read_only());
        assert!(AgentType::Explore.read_only());
        assert!(!AgentType::Plan.read_only());
        assert!(AgentType::CodeReview.read_only());
    }

    #[test]
    fn agent_type_preferred_model() {
        assert_eq!(AgentType::Explore.preferred_model(), Some("haiku"));
        assert_eq!(AgentType::General.preferred_model(), None);
        assert_eq!(AgentType::Plan.preferred_model(), None);
        assert_eq!(AgentType::CodeReview.preferred_model(), None);
    }

    #[test]
    fn agent_type_max_turns_capped() {
        assert_eq!(AgentType::General.max_turns(100), 20);
        assert_eq!(AgentType::General.max_turns(5), 5);
        assert_eq!(AgentType::Explore.max_turns(100), 10);
        assert_eq!(AgentType::Explore.max_turns(3), 3);
        assert_eq!(AgentType::Plan.max_turns(100), 15);
        assert_eq!(AgentType::CodeReview.max_turns(15), 15);
    }

    #[test]
    fn agent_type_system_prompt_general_returns_base() {
        let base = "You are a helpful assistant.";
        assert_eq!(AgentType::General.system_prompt(base), base);
    }

    #[test]
    fn agent_type_system_prompt_explore_contains_keywords() {
        let prompt = AgentType::Explore.system_prompt("Base");
        assert!(prompt.contains("exploration agent"));
        assert!(prompt.contains("ONLY read"));
        assert!(prompt.starts_with("Base"));
    }

    #[test]
    fn agent_type_system_prompt_plan_contains_keywords() {
        let prompt = AgentType::Plan.system_prompt("Base");
        assert!(prompt.contains("planning agent"));
        assert!(prompt.contains("task_create"));
    }

    #[test]
    fn agent_type_system_prompt_code_review_contains_keywords() {
        let prompt = AgentType::CodeReview.system_prompt("Base");
        assert!(prompt.contains("code review agent"));
        assert!(prompt.contains("Do not modify"));
    }

    // ── resolve_agent_model ──────────────────────────────────────────────

    #[test]
    fn resolve_model_none_uses_parent() {
        assert_eq!(resolve_agent_model(None, "claude-sonnet-4-6"), "claude-sonnet-4-6");
    }

    #[test]
    fn resolve_model_inherit_uses_parent() {
        assert_eq!(resolve_agent_model(Some("inherit"), "claude-opus-4"), "claude-opus-4");
    }

    #[test]
    fn resolve_model_alias_resolves() {
        let model = resolve_agent_model(Some("haiku"), "claude-sonnet-4");
        assert!(model.contains("haiku"), "Expected haiku, got: {}", model);
    }

    #[test]
    fn resolve_model_unknown_alias_passes_through() {
        let model = resolve_agent_model(Some("custom-model-v1"), "parent");
        assert_eq!(model, "custom-model-v1");
    }

    // ── DispatchAgentTool metadata ───────────────────────────────────────

    fn make_tool() -> DispatchAgentTool {
        DispatchAgentTool {
            client: Arc::new(ApiClient::new("test-key")),
            registry: Arc::new(ToolRegistry::with_defaults()),
            permission_checker: Arc::new(PermissionChecker::new(
                claude_core::permissions::PermissionMode::BypassAll,
                Vec::new(),
            )),
            config: SubAgentConfig {
                model: "claude-sonnet-4".into(),
                max_tokens: 1024,
                cwd: PathBuf::from("."),
                system_prompt: "Test".into(),
                max_turns: 10,
                context_window: 200_000,
            },
            agent_tracker: None,
            cancel_tokens: None,
            agent_channels: None,
        }
    }

    #[test]
    fn dispatch_agent_tool_metadata() {
        let tool = make_tool();
        assert_eq!(tool.name(), "Agent");
        assert!(!tool.is_read_only());
        assert!(tool.description().contains("sub-agent"));
    }

    #[test]
    fn dispatch_agent_input_schema_has_all_properties() {
        let tool = make_tool();
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        let props = schema["properties"].as_object().unwrap();
        for key in &["prompt", "description", "agent_type", "name", "allowed_tools", "system_prompt", "model", "run_in_background"] {
            assert!(props.contains_key(*key), "Missing property: {}", key);
        }
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "prompt"));
    }
}
