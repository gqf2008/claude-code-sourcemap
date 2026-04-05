use std::pin::Pin;
use std::sync::Arc;
use futures::Stream;
use tracing::warn;
use uuid::Uuid;

use claude_api::client::AnthropicClient;
use claude_api::types::*;
use claude_core::message::{
    AssistantMessage, ContentBlock, Message, StopReason, Usage, UserMessage,
};
use claude_core::tool::ToolContext;
use crate::executor::ToolExecutor;
use crate::hooks::{HookDecision, HookEvent, HookRegistry};
use crate::state::SharedState;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart { id: String, name: String },
    /// Emitted when tool input is fully parsed (at ContentBlockStop).
    ToolUseReady { id: String, name: String, input: serde_json::Value },
    ToolResult { id: String, is_error: bool, text: Option<String> },
    AssistantMessage(AssistantMessage),
    TurnComplete { stop_reason: StopReason },
    UsageUpdate(Usage),
    /// Max turns limit reached.
    MaxTurns { limit: u32 },
    Error(String),
}

pub struct QueryConfig {
    pub system_prompt: String,
    pub max_turns: u32,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub thinking: Option<claude_api::types::ThinkingConfig>,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            max_turns: 100,
            max_tokens: 16384,
            temperature: None,
            thinking: None,
        }
    }
}

/// Core agent loop: send messages → process stream → execute tools → repeat
pub fn query_stream(
    client: Arc<AnthropicClient>,
    executor: Arc<ToolExecutor>,
    state: SharedState,
    tool_context: ToolContext,
    config: QueryConfig,
    initial_messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    hooks: Arc<HookRegistry>,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
    let stream = async_stream::stream! {
        let mut messages = initial_messages;
        let mut turn_count: u32 = 0;
        let mut stop_hook_retries: u32 = 0;
        const MAX_STOP_HOOK_RETRIES: u32 = 3;

        loop {
            // Check abort at the top of every turn
            if tool_context.abort_signal.is_aborted() {
                state.write().await.messages = messages.clone();
                yield AgentEvent::TurnComplete { stop_reason: claude_core::message::StopReason::EndTurn };
                break;
            }

            #[allow(unused_assignments)]
            let mut retried_this_turn = false;

            if turn_count >= config.max_turns {
                yield AgentEvent::MaxTurns { limit: config.max_turns };
                break;
            }

            let api_messages = messages_to_api(&messages);
            let system = if config.system_prompt.is_empty() {
                None
            } else {
                // Use prompt caching (ephemeral) on the system prompt to save
                // input tokens across turns — mirrors the TS implementation.
                Some(vec![SystemBlock {
                    block_type: "text".into(),
                    text: config.system_prompt.clone(),
                    cache_control: Some(CacheControl { control_type: "ephemeral".into() }),
                }])
            };

            let request = MessagesRequest {
                model: { state.read().await.model.clone() },
                max_tokens: config.max_tokens,
                messages: api_messages,
                system,
                tools: if tools.is_empty() { None } else { Some(tools.clone()) },
                stream: true,
                stop_sequences: None,
                temperature: config.temperature,
                top_p: None,
                thinking: config.thinking.clone(),
            };

            let event_stream = match client.messages_stream(&request).await {
                Ok(s) => s,
                Err(e) => {
                    // Retry once on transient errors (rate limit, server error)
                    let err_str = format!("{}", e);
                    let is_retryable = err_str.contains("rate")
                        || err_str.contains("529")
                        || err_str.contains("500")
                        || err_str.contains("503")
                        || err_str.contains("overloaded");
                    if is_retryable && !retried_this_turn && turn_count + 1 < config.max_turns {
                        #[allow(unused_assignments)]
                        { retried_this_turn = true; }
                        yield AgentEvent::TextDelta(format!("\n\x1b[33m[Retrying after API error: {}]\x1b[0m\n", err_str));
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        continue; // retry the turn
                    }
                    state.write().await.messages = messages.clone();
                    yield AgentEvent::Error(format!("API error: {}", e));
                    break;
                }
            };

            let mut assistant_text = String::new();
            let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();
            let mut current_tool_input = String::new();
            let mut current_tool_id = String::new();
            let mut current_tool_name = String::new();
            let mut stop_reason = None;
            let mut usage = None;

            use tokio_stream::StreamExt;
            let mut event_stream = event_stream;
            while let Some(event_result) = event_stream.next().await {
                match event_result {
                    Ok(event) => match event {
                        StreamEvent::ContentBlockStart { content_block, .. } => {
                            match &content_block {
                                ResponseContentBlock::Text { .. } => {
                                    // Text content arrives via ContentBlockDelta::TextDelta
                                }
                                ResponseContentBlock::ToolUse { id, name, .. } => {
                                    current_tool_id = id.clone();
                                    current_tool_name = name.clone();
                                    current_tool_input.clear();
                                    yield AgentEvent::ToolUseStart { id: id.clone(), name: name.clone() };
                                }
                                ResponseContentBlock::Thinking { thinking } => {
                                    yield AgentEvent::ThinkingDelta(thinking.clone());
                                }
                            }
                        }
                        StreamEvent::ContentBlockDelta { delta, .. } => match delta {
                            DeltaBlock::TextDelta { text } => {
                                assistant_text.push_str(&text);
                                yield AgentEvent::TextDelta(text);
                            }
                            DeltaBlock::InputJsonDelta { partial_json } => {
                                current_tool_input.push_str(&partial_json);
                            }
                            DeltaBlock::ThinkingDelta { thinking } => {
                                yield AgentEvent::ThinkingDelta(thinking);
                            }
                        },
                        StreamEvent::ContentBlockStop { .. } => {
                            if !current_tool_id.is_empty() {
                                let input: serde_json::Value = serde_json::from_str(&current_tool_input)
                                    .unwrap_or(serde_json::Value::Object(Default::default()));
                                yield AgentEvent::ToolUseReady {
                                    id: current_tool_id.clone(),
                                    name: current_tool_name.clone(),
                                    input: input.clone(),
                                };
                                assistant_blocks.push(ContentBlock::ToolUse {
                                    id: current_tool_id.clone(),
                                    name: current_tool_name.clone(),
                                    input: input.clone(),
                                });
                                tool_uses.push((current_tool_id.clone(), current_tool_name.clone(), input));
                                current_tool_id.clear();
                                current_tool_name.clear();
                                current_tool_input.clear();
                            }
                        }
                        StreamEvent::MessageDelta { delta, .. } => {
                            stop_reason = delta.stop_reason.as_deref().map(|r| match r {
                                "end_turn" => StopReason::EndTurn,
                                "tool_use" => StopReason::ToolUse,
                                "max_tokens" => StopReason::MaxTokens,
                                "stop_sequence" => StopReason::StopSequence,
                                other => {
                                    warn!("Unknown stop_reason from API: {}", other);
                                    StopReason::EndTurn
                                }
                            });
                        }
                        StreamEvent::MessageStart { message } => {
                            usage = Some(Usage {
                                input_tokens: message.usage.input_tokens,
                                output_tokens: message.usage.output_tokens,
                                cache_creation_input_tokens: message.usage.cache_creation_input_tokens,
                                cache_read_input_tokens: message.usage.cache_read_input_tokens,
                            });
                        }
                        StreamEvent::Error { error } => {
                            yield AgentEvent::Error(format!("{}: {}", error.error_type, error.message));
                            break;
                        }
                        _ => {}
                    },
                    Err(e) => {
                        state.write().await.messages = messages.clone();
                        yield AgentEvent::Error(format!("Stream error: {}", e));
                        break;
                    }
                }
            }

            // Ensure text block is present
            if !assistant_text.is_empty() && !assistant_blocks.iter().any(|b| matches!(b, ContentBlock::Text { .. })) {
                assistant_blocks.insert(0, ContentBlock::Text { text: assistant_text.clone() });
            }

            let assistant_msg = AssistantMessage {
                uuid: Uuid::new_v4().to_string(),
                content: assistant_blocks,
                stop_reason: stop_reason.clone(),
                usage: usage.clone(),
            };
            messages.push(Message::Assistant(assistant_msg.clone()));
            yield AgentEvent::AssistantMessage(assistant_msg);

            if let Some(ref u) = usage {
                let mut s = state.write().await;
                s.total_input_tokens = s.total_input_tokens.saturating_add(u.input_tokens);
                s.total_output_tokens = s.total_output_tokens.saturating_add(u.output_tokens);
                yield AgentEvent::UsageUpdate(u.clone());
            }

            let actual_stop = stop_reason.unwrap_or(StopReason::EndTurn);
            match actual_stop {
                StopReason::ToolUse if !tool_uses.is_empty() => {
                    let results: Vec<ContentBlock> = executor.execute_many(tool_uses, &tool_context).await;
                    let tool_result_msg = UserMessage {
                        uuid: Uuid::new_v4().to_string(),
                        content: results.clone(),
                    };
                    messages.push(Message::User(tool_result_msg));
                    for result in &results {
                        if let ContentBlock::ToolResult { tool_use_id, is_error, content } = result {
                            let result_text = content.first().and_then(|c| {
                                if let claude_core::message::ToolResultContent::Text { text } = c {
                                    Some(text.clone())
                                } else {
                                    None
                                }
                            });
                            yield AgentEvent::ToolResult { id: tool_use_id.clone(), is_error: *is_error, text: result_text };
                        }
                    }
                    turn_count += 1;
                    stop_hook_retries = 0; // reset on normal tool turn
                    { let mut s = state.write().await; s.turn_count = turn_count; }
                }
                other => {
                    // ── Stop hooks ───────────────────────────────────────────
                    if hooks.has_hooks(HookEvent::Stop) {
                        // Pass the last assistant text as context so hook scripts
                        // can inspect what Claude just said.
                        let last_text = if assistant_text.is_empty() { None } else { Some(assistant_text.clone()) };
                        let ctx = hooks.prompt_ctx(HookEvent::Stop, last_text);
                        match hooks.run(HookEvent::Stop, ctx).await {
                            HookDecision::FeedbackAndContinue { feedback } if stop_hook_retries < MAX_STOP_HOOK_RETRIES => {
                                stop_hook_retries += 1;
                                // exit 2: inject feedback as a new user message and loop
                                let feedback_msg = UserMessage {
                                    uuid: Uuid::new_v4().to_string(),
                                    content: vec![ContentBlock::Text { text: feedback.clone() }],
                                };
                                messages.push(Message::User(feedback_msg));
                                yield AgentEvent::TextDelta(format!("\n[Stop hook feedback ({}/{})]: {}\n", stop_hook_retries, MAX_STOP_HOOK_RETRIES, feedback));
                                turn_count += 1;
                                { let mut s = state.write().await; s.turn_count = turn_count; }
                                continue; // restart the query loop
                            }
                            HookDecision::FeedbackAndContinue { .. } => {
                                yield AgentEvent::TextDelta("\n[Stop hook retry limit reached — stopping]\n".to_string());
                            }
                            HookDecision::Block { reason } => {
                                // Non-zero exit: warn but still stop
                                yield AgentEvent::TextDelta(format!("\n[Stop hook warning]: {}\n", reason));
                            }
                            _ => {}
                        }
                    }

                    // Persist conversation history so the next submit() continues the session
                    state.write().await.messages = messages.clone();
                    yield AgentEvent::TurnComplete { stop_reason: other };
                    break;
                }
            }
        }
    };
    Box::pin(stream)
}

fn messages_to_api(messages: &[Message]) -> Vec<ApiMessage> {
    messages.iter().filter_map(|msg| match msg {
        Message::User(u) => Some(ApiMessage {
            role: "user".into(),
            content: u.content.iter().map(block_to_api).collect(),
        }),
        Message::Assistant(a) => Some(ApiMessage {
            role: "assistant".into(),
            content: a.content.iter().map(block_to_api).collect(),
        }),
        Message::System(_) => None,
    }).collect()
}

fn block_to_api(block: &ContentBlock) -> ApiContentBlock {
    match block {
        ContentBlock::Text { text } => ApiContentBlock::Text { text: text.clone() },
        ContentBlock::ToolUse { id, name, input } => ApiContentBlock::ToolUse {
            id: id.clone(), name: name.clone(), input: input.clone(),
        },
        ContentBlock::ToolResult { tool_use_id, content, is_error } => ApiContentBlock::ToolResult {
            tool_use_id: tool_use_id.clone(),
            content: content.iter().map(|c| match c {
                claude_core::message::ToolResultContent::Text { text } => {
                    claude_api::types::ToolResultContent::Text { text: text.clone() }
                }
                claude_core::message::ToolResultContent::Image { .. } => {
                    claude_api::types::ToolResultContent::Text { text: "[image]".into() }
                }
            }).collect(),
            is_error: *is_error,
        },
        ContentBlock::Thinking { thinking } => {
            ApiContentBlock::Text { text: format!("<thinking>{}</thinking>", thinking) }
        }
    }
}
