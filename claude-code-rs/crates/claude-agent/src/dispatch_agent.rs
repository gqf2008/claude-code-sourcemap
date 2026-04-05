use std::sync::Arc;
use std::path::PathBuf;

use async_trait::async_trait;
use claude_api::client::AnthropicClient;
use claude_api::types::ToolDefinition;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio_stream::StreamExt;

use crate::coordinator::AgentTracker;
use crate::executor::ToolExecutor;
use crate::hooks::HookRegistry;
use crate::permissions::PermissionChecker;
use crate::query::{query_stream, AgentEvent, QueryConfig};
use crate::state::new_shared_state;
use claude_tools::ToolRegistry;

/// Configuration passed into the sub-agent.
pub struct SubAgentConfig {
    pub model: String,
    pub max_tokens: u32,
    pub cwd: PathBuf,
    pub system_prompt: String,
    pub max_turns: u32,
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
}

impl AgentType {
    fn from_str(s: &str) -> Self {
        match s {
            "explore" => Self::Explore,
            "plan" => Self::Plan,
            "code-review" | "code_review" | "review" => Self::CodeReview,
            _ => Self::General,
        }
    }

    fn system_prompt(&self, base: &str) -> String {
        match self {
            Self::General => base.to_string(),
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
        }
    }

    fn max_turns(&self, configured: u32) -> u32 {
        match self {
            Self::General => configured.min(20),
            Self::Explore => configured.min(10),
            Self::Plan => configured.min(15),
            Self::CodeReview => configured.min(15),
        }
    }

    /// Returns true if this agent type should be restricted to read-only tools.
    fn read_only(&self) -> bool {
        matches!(self, Self::Explore | Self::CodeReview)
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

/// A tool that spawns a sub-agent to execute a given prompt.
/// The sub-agent runs its own query loop and returns its final text output.
///
/// In coordinator mode, if `run_in_background` is true, the agent is spawned
/// via `tokio::spawn` and the tool returns immediately with an `agent_id`.
/// Results are delivered as `<task-notification>` XML via the `AgentTracker`.
pub struct DispatchAgentTool {
    pub client: Arc<AnthropicClient>,
    pub registry: Arc<ToolRegistry>,
    pub permission_checker: Arc<PermissionChecker>,
    pub config: SubAgentConfig,
    /// Optional tracker for background agent execution (coordinator mode).
    pub agent_tracker: Option<AgentTracker>,
    /// Shared cancel tokens — used by TaskStop to abort background agents.
    pub cancel_tokens: Option<Arc<tokio::sync::RwLock<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>>>,
    /// Shared agent message channels — used by SendMessage to deliver follow-ups.
    pub agent_channels: Option<Arc<tokio::sync::RwLock<std::collections::HashMap<String, tokio::sync::mpsc::UnboundedSender<String>>>>>,
}

#[async_trait]
impl Tool for DispatchAgentTool {
    fn name(&self) -> &str { "dispatch_agent" }

    fn description(&self) -> &str {
        "Launch a sub-agent to accomplish an independent task. The sub-agent runs a full \
         agentic loop with tools and returns its output when done. Use this for tasks that \
         can be parallelised or isolated. The sub-agent cannot interact with the user.\n\n\
         Agent types:\n\
         - \"general\" (default): Full tool access, up to 20 turns\n\
         - \"explore\": Read-only, fast investigation, up to 10 turns\n\
         - \"plan\": Read + task management, up to 15 turns\n\
         - \"code-review\": Read-only code analysis, up to 15 turns"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task prompt for the sub-agent."
                },
                "agent_type": {
                    "type": "string",
                    "enum": ["general", "explore", "plan", "code-review"],
                    "description": "The type of agent to launch. Determines available tools \
                                    and system prompt. Default: general."
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
                                    with an agent_id that can be checked with task_get. Default: false."
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

        // Build tool definitions for the sub-agent (optionally filtered)
        let all_tool_defs: Vec<ToolDefinition> = self.registry
            .all()
            .iter()
            .filter(|t| t.is_enabled())
            // Sub-agents cannot use interactive tools or nested dispatch_agent
            .filter(|t| !matches!(t.name(), "AskUserQuestion" | "dispatch_agent" | "SendMessage" | "TaskStop"))
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
        };

        // Sub-agents run without user-defined hooks to avoid re-entrant side effects
        let no_hooks = Arc::new(HookRegistry::new());

        // ── Background execution (coordinator mode) ─────────────────────────
        if run_in_background {
            if let Some(ref tracker) = self.agent_tracker {
                let agent_id = format!("agent-{}", &Uuid::new_v4().to_string()[..8]);
                tracker.register(&agent_id, &prompt).await;

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
                    channels.write().await.insert(agent_id.clone(), msg_tx);
                }

                let client = self.client.clone();
                let tracker = tracker.clone();
                let agent_id_clone = agent_id.clone();
                let cancel_tokens = self.cancel_tokens.clone();
                let agent_channels = self.agent_channels.clone();

                tokio::spawn(async move {
                    let mut stream = query_stream(
                        client,
                        executor,
                        state,
                        tool_context,
                        query_config,
                        init_messages,
                        all_tool_defs,
                        no_hooks,
                    );

                    let mut output = String::new();
                    let mut tool_use_count: u32 = 0;
                    let mut total_tokens: u64 = 0;

                    // Note: msg_rx is kept alive so SendMessage doesn't get "channel closed"
                    // errors, but follow-up messages are not injected mid-stream because
                    // query_stream uses a local messages vec. This is a known limitation;
                    // the TS implementation has more complex plumbing for live injection.
                    // For now, received messages are dropped after the stream completes.
                    let _msg_rx = msg_rx;

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
                                    Some(AgentEvent::ToolUseStart { .. }) => tool_use_count += 1,
                                    Some(AgentEvent::UsageUpdate(u)) => {
                                        total_tokens += u.input_tokens + u.output_tokens;
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
                        channels.write().await.remove(&agent_id_clone);
                    }
                    tracker.remove(&agent_id_clone).await;
                });

                return Ok(ToolResult::text(
                    serde_json::to_string_pretty(&json!({
                        "status": "async_launched",
                        "agent_id": agent_id,
                        "message": "Agent is running in the background. Results will be delivered as a <task-notification>."
                    }))
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
