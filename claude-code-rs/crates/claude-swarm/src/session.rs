//! Lightweight agentic query session for swarm agents.
//!
//! Unlike `claude-agent`'s full `QueryEngine`, this session is purpose-built
//! for swarm `AgentActor`s:
//! - No hooks, compaction, or coordinator
//! - Direct `claude-api` calls with a built-in tool executor
//! - Conversation state stored in-actor (history Vec)
//!
//! # Circular dependency avoidance
//! `claude-swarm` cannot depend on `claude-agent` (which depends on `claude-swarm`).
//! This module reimplements the minimal agentic loop directly.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use futures::StreamExt;
use serde_json::Value;
use tracing::{debug, warn};
use uuid::Uuid;

use claude_api::client::ApiClient;
use claude_api::types::{
    ApiContentBlock, ApiMessage, DeltaBlock, MessagesRequest, ResponseContentBlock, StreamEvent,
    SystemBlock, ToolDefinition, ToolResultContent as ApiToolResultContent,
};
use claude_core::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent as CoreToolResultContent,
    UserMessage,
};
use claude_core::tool::{AbortSignal, ToolContext};
use claude_core::permissions::PermissionMode;
use claude_tools::ToolRegistry;

// ── SwarmSession ─────────────────────────────────────────────────────────

/// A stateful query session for a single swarm agent.
///
/// Wraps an API client and conversation history. Each call to
/// [`SwarmSession::submit`] runs one or more API turns (until the model
/// stops or max turns is reached), executing tool calls along the way.
pub struct SwarmSession {
    client: Arc<ApiClient>,
    registry: Arc<ToolRegistry>,
    history: Vec<Message>,
    model: String,
    system_prompt: String,
    cwd: String,
    max_turns: u32,
}

impl SwarmSession {
    /// Create a new session.
    ///
    /// Reads `ANTHROPIC_API_KEY` from the environment. Returns `None` if not set.
    pub fn new(
        model: String,
        system_prompt: String,
        cwd: String,
        max_turns: u32,
    ) -> Option<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())?;

        let mut client = ApiClient::new(api_key);
        if let Ok(url) = std::env::var("ANTHROPIC_BASE_URL") {
            let url = url.trim().to_owned();
            if !url.is_empty() {
                client = client.with_base_url(url);
            }
        }

        let registry = ToolRegistry::with_defaults();
        Some(Self {
            client: Arc::new(client),
            registry: Arc::new(registry),
            history: Vec::new(),
            model,
            system_prompt,
            cwd,
            max_turns,
        })
    }

    /// Submit a user prompt and run the agentic loop until end_turn or max_turns.
    ///
    /// Returns the final text response from the model.
    pub async fn submit(&mut self, prompt: &str) -> Result<String> {
        self.history.push(Message::User(UserMessage {
            uuid: Uuid::new_v4().to_string(),
            content: vec![ContentBlock::Text { text: prompt.to_owned() }],
        }));

        // Build tool definitions from registry
        let tools: Vec<ToolDefinition> = self
            .registry
            .all()
            .iter()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
                cache_control: None,
            })
            .collect();

        let mut full_response = String::new();
        let mut turn = 0u32;

        loop {
            if turn >= self.max_turns {
                warn!(max_turns = self.max_turns, "Swarm agent hit max turns");
                break;
            }
            turn += 1;

            let api_messages: Vec<ApiMessage> =
                self.history.iter().map(core_msg_to_api).collect();

            let request = MessagesRequest {
                model: self.model.clone(),
                max_tokens: 4096,
                messages: api_messages,
                system: Some(vec![SystemBlock {
                    block_type: "text".to_string(),
                    text: self.system_prompt.clone(),
                    cache_control: None,
                }]),
                tools: if tools.is_empty() { None } else { Some(tools.clone()) },
                stream: true,
                ..Default::default()
            };

            let mut stream = self
                .client
                .messages_stream(&request)
                .await
                .context("Swarm agent API call failed")?;

            let mut text_buf = String::new();
            // (tool_use_id, tool_name, accumulated_json)
            let mut pending_tools: Vec<(String, String, String)> = Vec::new();
            let mut stop_reason: Option<String> = None;

            while let Some(ev) = stream.next().await {
                let event = ev.context("Swarm stream error")?;
                match event {
                    StreamEvent::ContentBlockStart {
                        content_block: ResponseContentBlock::ToolUse { id, name, .. },
                        ..
                    } => {
                        pending_tools.push((id, name, String::new()));
                    }
                    StreamEvent::ContentBlockStart { .. } => {}
                    StreamEvent::ContentBlockDelta { delta, .. } => match delta {
                        DeltaBlock::TextDelta { text } => text_buf.push_str(&text),
                        DeltaBlock::InputJsonDelta { partial_json } => {
                            if let Some(last) = pending_tools.last_mut() {
                                last.2.push_str(&partial_json);
                            }
                        }
                        _ => {}
                    },
                    StreamEvent::MessageDelta { delta, .. } => {
                        stop_reason = delta.stop_reason.clone();
                    }
                    StreamEvent::Error { error } => {
                        return Err(anyhow::anyhow!(
                            "API error: {} - {}",
                            error.error_type,
                            error.message
                        ));
                    }
                    _ => {}
                }
            }

            // Parse collected tool inputs
            let tool_uses: Vec<(String, String, Value)> = pending_tools
                .into_iter()
                .map(|(id, name, json)| {
                    let input = serde_json::from_str(&json)
                        .unwrap_or(Value::Object(Default::default()));
                    (id, name, input)
                })
                .collect();

            // Save assistant message to history
            let mut assistant_content: Vec<ContentBlock> = Vec::new();
            if !text_buf.is_empty() {
                full_response.push_str(&text_buf);
                assistant_content.push(ContentBlock::Text { text: text_buf.clone() });
            }
            for (id, name, input) in &tool_uses {
                assistant_content.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
            self.history.push(Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4().to_string(),
                content: assistant_content,
                stop_reason: None,
                usage: None,
            }));

            let is_tool_use = stop_reason.as_deref() == Some("tool_use");
            if tool_uses.is_empty() || !is_tool_use {
                break;
            }

            // Execute tool calls
            let mut tool_results: Vec<ContentBlock> = Vec::new();
            for (id, name, input) in tool_uses {
                debug!(tool = %name, "Swarm agent executing tool");
                let ctx = ToolContext {
                    cwd: std::path::PathBuf::from(&self.cwd),
                    abort_signal: AbortSignal::new(),
                    permission_mode: PermissionMode::Auto,
                    messages: self.history.clone(),
                };
                let result_block = match self.registry.get(&name) {
                    Some(tool) => match tool.call(input, &ctx).await {
                        Ok(result) => ContentBlock::ToolResult {
                            tool_use_id: id,
                            content: vec![CoreToolResultContent::Text {
                                text: result.to_text(),
                            }],
                            is_error: result.is_error,
                        },
                        Err(e) => ContentBlock::ToolResult {
                            tool_use_id: id,
                            content: vec![CoreToolResultContent::Text {
                                text: format!("Tool error: {e}"),
                            }],
                            is_error: true,
                        },
                    },
                    None => ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: vec![CoreToolResultContent::Text {
                            text: format!("Unknown tool: {name}"),
                        }],
                        is_error: true,
                    },
                };
                tool_results.push(result_block);
            }

            self.history.push(Message::User(UserMessage {
                uuid: Uuid::new_v4().to_string(),
                content: tool_results,
            }));
        }

        Ok(full_response)
    }

    /// Return the number of messages in this session's history.
    pub fn turn_count(&self) -> usize {
        self.history.len()
    }
}

// ── Message format conversion ─────────────────────────────────────────────

fn core_msg_to_api(msg: &Message) -> ApiMessage {
    match msg {
        Message::User(u) => ApiMessage {
            role: "user".to_string(),
            content: u.content.iter().map(core_block_to_api).collect(),
        },
        Message::Assistant(a) => ApiMessage {
            role: "assistant".to_string(),
            content: a.content.iter().map(core_block_to_api).collect(),
        },
        // System messages are injected via the `system` field, not as conversation messages
        Message::System(_) => ApiMessage {
            role: "user".to_string(),
            content: vec![],
        },
    }
}

fn core_block_to_api(block: &ContentBlock) -> ApiContentBlock {
    match block {
        ContentBlock::Text { text } => ApiContentBlock::Text {
            text: text.clone(),
            cache_control: None,
        },
        ContentBlock::ToolUse { id, name, input } => ApiContentBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        },
        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
            ApiContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content
                    .iter()
                    .filter_map(|c| match c {
                        CoreToolResultContent::Text { text } => {
                            Some(ApiToolResultContent::Text { text: text.clone() })
                        }
                        CoreToolResultContent::Image { .. } => None, // API only supports text in tool results
                    })
                    .collect(),
                is_error: *is_error,
                cache_control: None,
            }
        }
        ContentBlock::Image { source } => ApiContentBlock::Image {
            source: claude_api::types::ImageSource {
                source_type: "base64".to_string(),
                media_type: source.media_type.clone(),
                data: source.data.clone(),
            },
        },
        ContentBlock::Thinking { thinking } => ApiContentBlock::Text {
            text: format!("<thinking>{thinking}</thinking>"),
            cache_control: None,
        },
    }
}
