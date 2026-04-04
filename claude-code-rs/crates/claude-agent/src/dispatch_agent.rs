use std::sync::Arc;
use std::path::PathBuf;

use async_trait::async_trait;
use claude_api::client::AnthropicClient;
use claude_api::types::ToolDefinition;
use claude_core::tool::{AbortSignal, Tool, ToolContext, ToolResult};
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
         can be parallelised or isolated. The sub-agent cannot interact with the user and \
         cannot use tools that require user interaction."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task prompt for the sub-agent."
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of tool names available to the sub-agent. \
                                    If omitted, all non-interactive tools are available."
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

        let allowed_tools: Option<Vec<String>> = input["allowed_tools"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect());

        let system_prompt = input["system_prompt"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| self.config.system_prompt.clone());

        // Build tool definitions for the sub-agent (optionally filtered)
        let all_tool_defs: Vec<ToolDefinition> = self.registry
            .all()
            .iter()
            .filter(|t| t.is_enabled())
            // Sub-agents cannot use interactive tools or nested dispatch_agent
            .filter(|t| !matches!(t.name(), "AskUserQuestion" | "dispatch_agent"))
            .filter(|t| {
                if let Some(ref allowed) = allowed_tools {
                    allowed.contains(&t.name().to_string())
                } else {
                    true
                }
            })
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
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
            abort_signal: AbortSignal::new(),
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
            max_turns: self.config.max_turns.min(20), // cap sub-agent turns
            max_tokens: self.config.max_tokens,
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
