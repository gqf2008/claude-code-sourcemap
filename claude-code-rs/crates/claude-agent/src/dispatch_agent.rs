use std::sync::Arc;
use std::path::PathBuf;

use async_trait::async_trait;
use claude_api::client::AnthropicClient;
use claude_api::types::ToolDefinition;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio_stream::StreamExt;

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
}

/// A tool that spawns a sub-agent to execute a given prompt.
/// The sub-agent runs its own query loop and returns its final text output.
pub struct DispatchAgentTool {
    pub client: Arc<AnthropicClient>,
    pub registry: Arc<ToolRegistry>,
    pub permission_checker: Arc<PermissionChecker>,
    pub config: SubAgentConfig,
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

        // Build tool definitions for the sub-agent (optionally filtered)
        let all_tool_defs: Vec<ToolDefinition> = self.registry
            .all()
            .iter()
            .filter(|t| t.is_enabled())
            // Sub-agents cannot use interactive tools or nested dispatch_agent
            .filter(|t| !matches!(t.name(), "AskUserQuestion" | "dispatch_agent"))
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
        // Inherit permission mode from parent
        {
            let mut s = state.write().await;
            s.model = self.config.model.clone();
        }

        let tool_context = ToolContext {
            cwd: context.cwd.clone(),
            abort_signal: context.abort_signal.clone(), // inherit parent's abort signal
            permission_mode: context.permission_mode,
            messages: Vec::new(),
        };

        // Bootstrap with the user prompt as the first message
        use uuid::Uuid;
        use claude_core::message::{ContentBlock, Message, UserMessage};
        let init_messages = vec![Message::User(UserMessage {
            uuid: Uuid::new_v4().to_string(),
            content: vec![ContentBlock::Text { text: prompt }],
        })];

        let query_config = QueryConfig {
            system_prompt,
            max_turns: agent_type.max_turns(self.config.max_turns),
            max_tokens: self.config.max_tokens,
            temperature: None,
            thinking: None,
        };

        // Sub-agents run without user-defined hooks to avoid re-entrant hook side effects
        let no_hooks = Arc::new(HookRegistry::new());

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
