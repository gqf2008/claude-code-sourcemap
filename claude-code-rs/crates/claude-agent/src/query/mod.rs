mod helpers;

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

use helpers::*;

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
    /// Per-turn token counts for budget tracking.
    TurnTokens { input_tokens: u64, output_tokens: u64 },
    /// Prompt is getting too large — may need compaction soon.
    ContextWarning { usage_pct: f64, message: String },
    /// Auto-compaction triggered.
    CompactStart,
    /// Compaction finished successfully.
    CompactComplete { summary_len: usize },
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
    /// Token budget for this query (0 = unlimited).
    pub token_budget: u64,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            max_turns: 100,
            max_tokens: 16384,
            temperature: None,
            thinking: None,
            token_budget: 0,
        }
    }
}

/// Core agent loop: send messages → process stream → execute tools → repeat
#[allow(clippy::too_many_arguments)]
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

        // ── Recovery state (aligned with TS query.ts) ────────────────────────
        let mut max_tokens_recovery_count: u32 = 0;
        const MAX_TOKENS_RECOVERY_LIMIT: u32 = 3;
        let mut effective_max_tokens = config.max_tokens;
        let mut has_attempted_reactive_compact = false;
        let mut consecutive_errors: u32 = 0;
        let mut retry_delay_ms: u64 = 1_000; // exponential backoff: 1s → 2s → 4s → … → 32s max

        // Look up model capabilities for smart max_tokens escalation
        let model_name = { state.read().await.model.clone() };
        let caps = claude_core::model::model_capabilities(&model_name);
        let escalated_max_tokens = caps.upper_max_output;

        loop {
            // Check abort at the top of every turn
            if tool_context.abort_signal.is_aborted() {
                state.write().await.messages = messages.clone();
                yield AgentEvent::TurnComplete { stop_reason: claude_core::message::StopReason::EndTurn };
                break;
            }

            if turn_count >= config.max_turns {
                yield AgentEvent::MaxTurns { limit: config.max_turns };
                break;
            }

            let api_messages = messages_to_api(&messages);
            let system = build_system_blocks(&config.system_prompt);

            let request = MessagesRequest {
                model: { state.read().await.model.clone() },
                max_tokens: effective_max_tokens,
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
                    let err_str = format!("{}", e);
                    consecutive_errors += 1;
                    state.write().await.record_error(error_category(&err_str));

                    match classify_api_error(&err_str, has_attempted_reactive_compact, consecutive_errors, retry_delay_ms) {
                        ApiErrorAction::ReactiveCompact => {
                            has_attempted_reactive_compact = true;
                            yield AgentEvent::TextDelta(
                                "\n\x1b[33m[Prompt too long — trimming context…]\x1b[0m\n".to_string()
                            );
                            // First pass: truncate large tool results
                            let truncated = crate::compact::truncate_large_tool_results(
                                &mut messages, crate::compact::MAX_TOOL_RESULT_CHARS / 2,
                            );
                            // Second pass: snip oldest message pairs, keep last 5
                            let snipped = crate::compact::snip_old_messages(&mut messages, 5);
                            if truncated + snipped > 0 {
                                yield AgentEvent::TextDelta(format!(
                                    "\x1b[33m[Trimmed {} tool result(s), snipped {} message(s)]\x1b[0m\n",
                                    truncated, snipped,
                                ));
                                continue;
                            }
                        }
                        ApiErrorAction::Retry { wait_ms } => {
                            if turn_count + 1 < config.max_turns {
                                yield AgentEvent::TextDelta(format!(
                                    "\n\x1b[33m[Retrying after API error ({}) in {}ms: {}]\x1b[0m\n",
                                    consecutive_errors, wait_ms, err_str
                                ));
                                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                                retry_delay_ms = (retry_delay_ms * 2).min(32_000);
                                continue;
                            }
                        }
                        ApiErrorAction::Fatal => {}
                    }
                    state.write().await.messages = messages.clone();
                    yield AgentEvent::Error(format!("API error: {}", e));
                    break;
                }
            };

            // Wrap the raw stream with idle watchdog
            let watchdog_config = claude_api::stream::StreamWatchdogConfig::from_env();
            let event_stream = claude_api::stream::with_idle_watchdog(event_stream, watchdog_config);

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
                                let input: serde_json::Value = match serde_json::from_str(&current_tool_input) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        tracing::warn!(
                                            "Malformed tool input JSON for {}: {} (raw: {}…)",
                                            current_tool_name,
                                            e,
                                            &current_tool_input[..current_tool_input.len().min(200)],
                                        );
                                        yield AgentEvent::TextDelta(format!(
                                            "\n\x1b[33m[Warning: malformed tool input for {}, using empty object]\x1b[0m\n",
                                            current_tool_name,
                                        ));
                                        serde_json::Value::Object(Default::default())
                                    }
                                };
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

            // ── PostSampling hook ────────────────────────────────────────────
            // Fires after model response, before tool execution. Allows
            // observation or modification of the assistant's output.
            if hooks.has_hooks(HookEvent::PostSampling) {
                let ctx = hooks.prompt_ctx(
                    HookEvent::PostSampling,
                    if assistant_text.is_empty() { None } else { Some(assistant_text.clone()) },
                );
                if let HookDecision::Block { reason } = hooks.run(HookEvent::PostSampling, ctx).await {
                    yield AgentEvent::Error(format!("[PostSampling hook blocked]: {}", reason));
                    state.write().await.messages = messages.clone();
                    break;
                }
            }

            // Successful API response — reset error tracking
            consecutive_errors = 0;
            retry_delay_ms = 1_000;

            if let Some(ref u) = usage {
                let mut s = state.write().await;
                s.total_input_tokens = s.total_input_tokens.saturating_add(u.input_tokens);
                s.total_output_tokens = s.total_output_tokens.saturating_add(u.output_tokens);
                s.total_cache_read_tokens = s.total_cache_read_tokens
                    .saturating_add(u.cache_read_input_tokens.unwrap_or(0));
                s.total_cache_creation_tokens = s.total_cache_creation_tokens
                    .saturating_add(u.cache_creation_input_tokens.unwrap_or(0));

                // Per-model usage tracking
                let model_name = s.model.clone();
                let cost = crate::cost::calculate_cost(&model_name, u);
                s.record_model_usage(
                    &model_name,
                    u.input_tokens,
                    u.output_tokens,
                    u.cache_read_input_tokens.unwrap_or(0),
                    u.cache_creation_input_tokens.unwrap_or(0),
                    cost,
                );
                drop(s);

                yield AgentEvent::UsageUpdate(u.clone());

                // Emit per-turn token event for budget tracking
                yield AgentEvent::TurnTokens {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                };

                // Context usage warning
                let total_input = { state.read().await.total_input_tokens };
                if let Some(warning_event) = build_context_warning(total_input) {
                    yield warning_event;
                }

                // Budget enforcement
                if config.token_budget > 0 {
                    let total_tokens = {
                        let s = state.read().await;
                        s.total_input_tokens + s.total_output_tokens
                    };
                    if total_tokens >= config.token_budget {
                        yield AgentEvent::Error(format!(
                            "Token budget exceeded ({}/{}) — stopping",
                            total_tokens, config.token_budget
                        ));
                        state.write().await.messages = messages.clone();
                        break;
                    }
                }
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
                    stop_hook_retries = 0;
                    { let mut s = state.write().await; s.turn_count = turn_count; }
                }

                StopReason::MaxTokens => {
                    // Strategy 1: Escalate max_tokens to model's upper limit
                    if effective_max_tokens < escalated_max_tokens {
                        effective_max_tokens = escalated_max_tokens;
                        yield AgentEvent::TextDelta(format!(
                            "\n\x1b[33m[Output truncated — escalating max_tokens to {}K]\x1b[0m\n",
                            escalated_max_tokens / 1000
                        ));
                        messages.push(Message::User(make_continuation_message(0, MAX_TOKENS_RECOVERY_LIMIT)));
                        turn_count += 1;
                        { let mut s = state.write().await; s.turn_count = turn_count; }
                        continue;
                    }

                    // Strategy 2: Multi-turn continuation (up to 3 attempts)
                    if max_tokens_recovery_count < MAX_TOKENS_RECOVERY_LIMIT {
                        max_tokens_recovery_count += 1;
                        yield AgentEvent::TextDelta(format!(
                            "\n\x1b[33m[Output truncated — recovery attempt {}/{}]\x1b[0m\n",
                            max_tokens_recovery_count, MAX_TOKENS_RECOVERY_LIMIT
                        ));
                        messages.push(Message::User(make_continuation_message(max_tokens_recovery_count, MAX_TOKENS_RECOVERY_LIMIT)));
                        turn_count += 1;
                        { let mut s = state.write().await; s.turn_count = turn_count; }
                        continue;
                    }

                    // Exhausted recovery
                    yield AgentEvent::TextDelta(
                        "\n\x1b[31m[Max output tokens recovery exhausted]\x1b[0m\n".to_string()
                    );
                    state.write().await.messages = messages.clone();
                    yield AgentEvent::TurnComplete { stop_reason: StopReason::MaxTokens };
                    break;
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
                                // Check max_turns before injecting feedback turn
                                if turn_count + 1 >= config.max_turns {
                                    yield AgentEvent::TextDelta("\n[Stop hook: at max turns — stopping]\n".to_string());
                                } else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::helpers::{
        build_system_blocks, classify_api_error, error_category,
        build_context_warning, make_continuation_message,
        messages_to_api, block_to_api, ApiErrorAction,
    };

    // ── classify_api_error ───────────────────────────────────────────────

    #[test]
    fn test_classify_prompt_too_long_triggers_compact() {
        let action = classify_api_error("prompt is too long", false, 0, 1000);
        assert!(matches!(action, ApiErrorAction::ReactiveCompact));
    }

    #[test]
    fn test_classify_prompt_too_long_already_compacted() {
        let action = classify_api_error("prompt is too long", true, 0, 1000);
        assert!(matches!(action, ApiErrorAction::Fatal));
    }

    #[test]
    fn test_classify_413_status() {
        let action = classify_api_error("HTTP 413 payload too large", false, 0, 1000);
        assert!(matches!(action, ApiErrorAction::ReactiveCompact));
    }

    #[test]
    fn test_classify_too_many_tokens() {
        let action = classify_api_error("too many tokens in request", false, 0, 1000);
        assert!(matches!(action, ApiErrorAction::ReactiveCompact));
    }

    #[test]
    fn test_classify_rate_limit_retryable() {
        let action = classify_api_error("rate limit exceeded", false, 1, 2000);
        assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 2000 }));
    }

    #[test]
    fn test_classify_529_overloaded() {
        let action = classify_api_error("529 service overloaded", false, 2, 5000);
        assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 5000 }));
    }

    #[test]
    fn test_classify_500_server_error() {
        let action = classify_api_error("500 internal server error", false, 0, 1000);
        assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 1000 }));
    }

    #[test]
    fn test_classify_503_service_unavailable() {
        let action = classify_api_error("503 service unavailable", false, 3, 3000);
        assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 3000 }));
    }

    #[test]
    fn test_classify_retry_after_header() {
        let action = classify_api_error("rate limited retry-after: 10", false, 1, 2000);
        assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 10000 }));
    }

    #[test]
    fn test_classify_max_consecutive_errors_exceeded() {
        let action = classify_api_error("rate limit", false, 6, 1000);
        assert!(matches!(action, ApiErrorAction::Fatal));
    }

    #[test]
    fn test_classify_unknown_error_fatal() {
        let action = classify_api_error("something unexpected happened", false, 0, 1000);
        assert!(matches!(action, ApiErrorAction::Fatal));
    }

    // ── error_category ───────────────────────────────────────────────────

    #[test]
    fn test_error_category_rate_limit() {
        assert_eq!(error_category("rate limit exceeded"), "rate_limit");
        assert_eq!(error_category("429 too many requests"), "rate_limit");
    }

    #[test]
    fn test_error_category_overloaded() {
        assert_eq!(error_category("overloaded"), "overloaded");
        assert_eq!(error_category("529 overloaded"), "overloaded");
    }

    #[test]
    fn test_error_category_server_error() {
        assert_eq!(error_category("500 internal"), "server_error");
        assert_eq!(error_category("503 unavailable"), "server_error");
    }

    #[test]
    fn test_error_category_generic() {
        assert_eq!(error_category("something else entirely"), "api_error");
    }

    // ── build_context_warning ────────────────────────────────────────────

    #[test]
    fn test_build_context_warning_normal() {
        let threshold = crate::compact::AUTO_COMPACT_THRESHOLD;
        let low = (threshold as f64 * 0.4) as u64;
        assert!(build_context_warning(low).is_none());
    }

    #[test]
    fn test_build_context_warning_warning_level() {
        let threshold = crate::compact::AUTO_COMPACT_THRESHOLD;
        let at_60 = (threshold as f64 * 0.60) as u64;
        let event = build_context_warning(at_60);
        assert!(event.is_some());
        if let Some(AgentEvent::ContextWarning { message, .. }) = event {
            assert!(message.contains("Approaching"));
        }
    }

    #[test]
    fn test_build_context_warning_critical() {
        let threshold = crate::compact::AUTO_COMPACT_THRESHOLD;
        let at_80 = (threshold as f64 * 0.80) as u64;
        let event = build_context_warning(at_80);
        assert!(event.is_some());
        if let Some(AgentEvent::ContextWarning { message, .. }) = event {
            assert!(message.contains("nearly full"));
        }
    }

    // ── make_continuation_message ────────────────────────────────────────

    #[test]
    fn test_continuation_first_attempt() {
        let msg = make_continuation_message(0, 3);
        let text = match &msg.content[0] {
            ContentBlock::Text { text } => text.clone(),
            _ => panic!("expected text block"),
        };
        assert!(text.contains("Resume directly"));
    }

    #[test]
    fn test_continuation_subsequent_attempt() {
        let msg = make_continuation_message(2, 5);
        let text = match &msg.content[0] {
            ContentBlock::Text { text } => text.clone(),
            _ => panic!("expected text block"),
        };
        assert!(text.contains("attempt 2/5"));
        assert!(text.contains("smaller pieces"));
    }

    // ── build_system_blocks ──────────────────────────────────────────────

    #[test]
    fn test_build_system_blocks_empty() {
        assert!(build_system_blocks("").is_none());
    }

    #[test]
    fn test_build_system_blocks_no_boundary() {
        let blocks = build_system_blocks("Hello world").unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "Hello world");
        assert!(blocks[0].cache_control.is_some());
    }

    #[test]
    fn test_build_system_blocks_with_boundary() {
        let boundary = crate::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
        let prompt = format!("Static part\n{}\nDynamic part", boundary);
        let blocks = build_system_blocks(&prompt).unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].text.contains("Static part"));
        assert!(blocks[1].text.contains("Dynamic part"));
        assert!(blocks[0].cache_control.is_some());
        assert_eq!(blocks[0].cache_control.as_ref().unwrap().control_type, "ephemeral");
        assert!(blocks[1].cache_control.is_none());
    }

    #[test]
    fn test_build_system_blocks_boundary_strips_marker() {
        let boundary = crate::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
        let prompt = format!("Static\n{}\nDynamic data", boundary);
        let blocks = build_system_blocks(&prompt).unwrap();
        assert!(!blocks[1].text.contains(boundary));
        assert!(blocks[1].text.contains("Dynamic data"));
    }

    // ── messages_to_api ──────────────────────────────────────────────────

    #[test]
    fn test_messages_to_api_converts_user_and_assistant() {
        let messages = vec![
            Message::User(UserMessage {
                uuid: "u1".into(),
                content: vec![ContentBlock::Text { text: "hello".into() }],
            }),
            Message::Assistant(AssistantMessage {
                uuid: "a1".into(),
                content: vec![ContentBlock::Text { text: "hi".into() }],
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            }),
        ];
        let api = messages_to_api(&messages);
        assert_eq!(api.len(), 2);
        assert_eq!(api[0].role, "user");
        assert_eq!(api[1].role, "assistant");
    }

    #[test]
    fn test_messages_to_api_skips_system() {
        let messages = vec![
            Message::System(claude_core::message::SystemMessage {
                uuid: "s1".into(),
                message: "system text".into(),
            }),
            Message::User(UserMessage {
                uuid: "u1".into(),
                content: vec![ContentBlock::Text { text: "hello".into() }],
            }),
        ];
        let api = messages_to_api(&messages);
        assert_eq!(api.len(), 1);
        assert_eq!(api[0].role, "user");
    }

    #[test]
    fn test_messages_to_api_cache_control_on_last_block() {
        let messages = vec![
            Message::User(UserMessage {
                uuid: "u1".into(),
                content: vec![ContentBlock::Text { text: "hello".into() }],
            }),
        ];
        let api = messages_to_api(&messages);
        match &api[0].content[0] {
            ApiContentBlock::Text { cache_control, .. } => {
                assert!(cache_control.is_some());
            }
            _ => panic!("expected Text block"),
        }
    }

    // ── block_to_api ─────────────────────────────────────────────────────

    #[test]
    fn test_block_to_api_text() {
        let block = ContentBlock::Text { text: "hello".into() };
        let api = block_to_api(&block);
        match api {
            ApiContentBlock::Text { text, cache_control } => {
                assert_eq!(text, "hello");
                assert!(cache_control.is_none());
            }
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn test_block_to_api_tool_use() {
        let block = ContentBlock::ToolUse {
            id: "t1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
        };
        let api = block_to_api(&block);
        match api {
            ApiContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "t1");
                assert_eq!(name, "Bash");
                assert_eq!(input["command"], "ls");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn test_block_to_api_thinking() {
        let block = ContentBlock::Thinking { thinking: "let me think...".into() };
        let api = block_to_api(&block);
        match api {
            ApiContentBlock::Text { text, .. } => {
                assert!(text.contains("<thinking>"));
                assert!(text.contains("let me think..."));
            }
            _ => panic!("expected Text for thinking"),
        }
    }

    // ── QueryConfig ──────────────────────────────────────────────────────

    #[test]
    fn test_query_config_defaults() {
        let cfg = QueryConfig::default();
        assert_eq!(cfg.max_turns, 100);
        assert_eq!(cfg.max_tokens, 16384);
        assert!(cfg.system_prompt.is_empty());
        assert_eq!(cfg.token_budget, 0);
    }

    // ── Integration tests with MockBackend ───────────────────────────────

    use claude_api::provider::MockBackend;
    use claude_api::types::{ApiUsage, MessagesResponse, ResponseContentBlock};

    fn mock_text_response(text: &str) -> MessagesResponse {
        MessagesResponse {
            id: "msg_test".into(),
            response_type: "message".into(),
            role: "assistant".into(),
            content: vec![ResponseContentBlock::Text { text: text.into() }],
            model: "claude-sonnet-4-20250514".into(),
            stop_reason: Some("end_turn".into()),
            usage: ApiUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        }
    }

    #[allow(dead_code)]
    fn mock_tool_response(tool_id: &str, tool_name: &str, input: serde_json::Value) -> MessagesResponse {
        MessagesResponse {
            id: "msg_tool".into(),
            response_type: "message".into(),
            role: "assistant".into(),
            content: vec![ResponseContentBlock::ToolUse {
                id: tool_id.into(),
                name: tool_name.into(),
                input,
            }],
            model: "claude-sonnet-4-20250514".into(),
            stop_reason: Some("tool_use".into()),
            usage: ApiUsage {
                input_tokens: 200,
                output_tokens: 100,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        }
    }

    fn make_stream_events(response: MessagesResponse) -> Vec<anyhow::Result<claude_api::types::StreamEvent>> {
        // Create the stream events that MockBackend will serve
        let usage = response.usage.clone();
        let content = response.content.clone();
        let stop_reason = response.stop_reason.clone();

        let mut events = Vec::new();
        events.push(Ok(claude_api::types::StreamEvent::MessageStart {
            message: response,
        }));

        for (idx, block) in content.iter().enumerate() {
            match block {
                ResponseContentBlock::Text { text } => {
                    events.push(Ok(claude_api::types::StreamEvent::ContentBlockStart {
                        index: idx,
                        content_block: ResponseContentBlock::Text { text: String::new() },
                    }));
                    events.push(Ok(claude_api::types::StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: claude_api::types::DeltaBlock::TextDelta { text: text.clone() },
                    }));
                    events.push(Ok(claude_api::types::StreamEvent::ContentBlockStop { index: idx }));
                }
                ResponseContentBlock::ToolUse { id, name, input } => {
                    events.push(Ok(claude_api::types::StreamEvent::ContentBlockStart {
                        index: idx,
                        content_block: ResponseContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: serde_json::Value::Object(Default::default()),
                        },
                    }));
                    events.push(Ok(claude_api::types::StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: claude_api::types::DeltaBlock::InputJsonDelta {
                            partial_json: serde_json::to_string(input).unwrap(),
                        },
                    }));
                    events.push(Ok(claude_api::types::StreamEvent::ContentBlockStop { index: idx }));
                }
                ResponseContentBlock::Thinking { thinking } => {
                    events.push(Ok(claude_api::types::StreamEvent::ContentBlockStart {
                        index: idx,
                        content_block: ResponseContentBlock::Thinking { thinking: String::new() },
                    }));
                    events.push(Ok(claude_api::types::StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: claude_api::types::DeltaBlock::ThinkingDelta { thinking: thinking.clone() },
                    }));
                    events.push(Ok(claude_api::types::StreamEvent::ContentBlockStop { index: idx }));
                }
            }
        }

        events.push(Ok(claude_api::types::StreamEvent::MessageDelta {
            delta: claude_api::types::MessageDeltaData {
                stop_reason: stop_reason.or(Some("end_turn".into())),
            },
            usage: Some(claude_api::types::DeltaUsage { output_tokens: usage.output_tokens }),
        }));

        events
    }

    #[tokio::test]
    async fn e2e_single_turn_text_response() {
        let response = mock_text_response("Hello! How can I help?");
        let mock = MockBackend::new()
            .with_stream_events(make_stream_events(response));

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let registry = Arc::new(claude_tools::ToolRegistry::new());
        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::Default,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::Default,
            messages: vec![],
        };
        let hooks = Arc::new(crate::hooks::HookRegistry::new());

        let config = QueryConfig {
            system_prompt: "You are helpful.".into(),
            max_turns: 5,
            ..QueryConfig::default()
        };

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        })];

        let stream = query_stream(
            client, executor, state.clone(), tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Should have: TextDelta("Hello! How can I help?"), AssistantMessage, UsageUpdate, TurnTokens, TurnComplete
        let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Hello! How can I help?"));
        let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { stop_reason: StopReason::EndTurn }));
        let has_usage = events.iter().any(|e| matches!(e, AgentEvent::UsageUpdate(_)));
        assert!(has_text, "expected text delta");
        assert!(has_complete, "expected turn complete");
        assert!(has_usage, "expected usage update");

        // State should reflect the turn
        let s = state.read().await;
        assert!(s.total_input_tokens > 0);
        assert!(s.total_output_tokens > 0);
        assert_eq!(s.messages.len(), 2); // user + assistant
    }

    #[tokio::test]
    async fn e2e_max_turns_enforced() {
        // Return end_turn with tool_use to trigger multi-turn, but set max_turns=1
        let response = mock_text_response("Done.");
        let mock = MockBackend::new()
            .with_stream_events(make_stream_events(response));

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let registry = Arc::new(claude_tools::ToolRegistry::new());
        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::Default,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::Default,
            messages: vec![],
        };
        let hooks = Arc::new(crate::hooks::HookRegistry::new());

        let config = QueryConfig {
            max_turns: 1,
            ..QueryConfig::default()
        };

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        })];

        let stream = query_stream(
            client, executor, state, tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Should complete successfully in 1 turn
        let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { .. }));
        assert!(has_complete, "should complete within max_turns");
    }

    #[tokio::test]
    async fn e2e_abort_signal_stops_loop() {
        let response = mock_text_response("Should not appear");
        let mock = MockBackend::new()
            .with_stream_events(make_stream_events(response));

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let registry = Arc::new(claude_tools::ToolRegistry::new());
        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::Default,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::Default,
            messages: vec![],
        };

        // Abort before the loop starts
        tool_context.abort_signal.abort();

        let hooks = Arc::new(crate::hooks::HookRegistry::new());
        let config = QueryConfig::default();

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        })];

        let stream = query_stream(
            client, executor, state, tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Should see TurnComplete but no TextDelta
        let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(_)));
        let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { .. }));
        assert!(!has_text, "should not produce text when aborted");
        assert!(has_complete, "should produce TurnComplete on abort");
    }

    #[tokio::test]
    async fn e2e_api_error_propagated() {
        // Queue a stream error
        let mock = MockBackend::new()
            .with_stream_events(vec![
                Err(anyhow::anyhow!("authentication failed")),
            ]);

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let registry = Arc::new(claude_tools::ToolRegistry::new());
        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::Default,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::Default,
            messages: vec![],
        };
        let hooks = Arc::new(crate::hooks::HookRegistry::new());
        let config = QueryConfig::default();

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        })];

        let stream = query_stream(
            client, executor, state, tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Should have an error event about stream error
        let has_error = events.iter().any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("Stream error")));
        assert!(has_error, "expected stream error event, got: {:?}", events);
    }

    #[tokio::test]
    async fn e2e_multi_turn_tool_execution() {
        // Turn 1: model returns tool_use → executor runs tool → sends result
        // Turn 2: model returns end_turn text
        let tool_response = mock_tool_response("tool_1", "echo_tool", serde_json::json!({"text": "hello"}));
        let text_response = mock_text_response("Done with tool execution.");

        let mock = MockBackend::new()
            .with_stream_events(make_stream_events(tool_response))
            .with_stream_events(make_stream_events(text_response));

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let registry = Arc::new(claude_tools::ToolRegistry::new());
        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::Default,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::Default,
            messages: vec![],
        };
        let hooks = Arc::new(crate::hooks::HookRegistry::new());
        let config = QueryConfig {
            max_turns: 5,
            ..QueryConfig::default()
        };

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "Use the echo tool".into() }],
        })];

        let stream = query_stream(
            client, executor, state.clone(), tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Should have tool_use start + ready events from turn 1
        let has_tool_start = events.iter().any(|e| matches!(e, AgentEvent::ToolUseStart { name, .. } if name == "echo_tool"));
        let has_tool_ready = events.iter().any(|e| matches!(e, AgentEvent::ToolUseReady { name, .. } if name == "echo_tool"));
        // Should have tool result (error since echo_tool isn't registered)
        let has_tool_result = events.iter().any(|e| matches!(e, AgentEvent::ToolResult { .. }));
        // Should have text from turn 2
        let has_final_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Done with tool execution."));
        // Should have 2 assistant messages (one per turn)
        let assistant_count = events.iter().filter(|e| matches!(e, AgentEvent::AssistantMessage(_))).count();

        assert!(has_tool_start, "expected ToolUseStart");
        assert!(has_tool_ready, "expected ToolUseReady");
        assert!(has_tool_result, "expected ToolResult");
        assert!(has_final_text, "expected final text delta");
        assert_eq!(assistant_count, 2, "expected 2 assistant messages (2 turns)");

        // State should show 2 turns
        let s = state.read().await;
        assert_eq!(s.turn_count, 1); // turn_count tracks tool turns only
        assert_eq!(s.messages.len(), 4); // user + assistant(tool) + user(result) + assistant(text)
    }

    #[tokio::test]
    async fn e2e_token_budget_enforced() {
        // Set a very tight token budget and verify it stops
        let response = mock_text_response("Hello!");
        let mock = MockBackend::new()
            .with_stream_events(make_stream_events(response));

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let registry = Arc::new(claude_tools::ToolRegistry::new());
        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::Default,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::Default,
            messages: vec![],
        };
        let hooks = Arc::new(crate::hooks::HookRegistry::new());
        let config = QueryConfig {
            token_budget: 1, // impossibly low budget
            ..QueryConfig::default()
        };

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        })];

        let stream = query_stream(
            client, executor, state, tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Should have the text delta (response processed) then budget error
        let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Hello!"));
        let has_budget_error = events.iter().any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("Token budget exceeded")));
        assert!(has_text, "expected text delta before budget stop");
        assert!(has_budget_error, "expected token budget exceeded error, got: {:?}", events);
    }

    #[tokio::test]
    async fn e2e_max_tokens_recovery_escalation() {
        // First response: stop_reason = max_tokens → should escalate and retry
        // Second response: normal end_turn
        let truncated = MessagesResponse {
            id: "msg_1".into(),
            response_type: "message".into(),
            role: "assistant".into(),
            content: vec![ResponseContentBlock::Text { text: "Partial output...".into() }],
            model: "claude-sonnet-4-20250514".into(),
            stop_reason: Some("max_tokens".into()),
            usage: ApiUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };
        let complete = mock_text_response("...complete!");

        let mock = MockBackend::new()
            .with_stream_events(make_stream_events(truncated))
            .with_stream_events(make_stream_events(complete));

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let registry = Arc::new(claude_tools::ToolRegistry::new());
        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::Default,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::Default,
            messages: vec![],
        };
        let hooks = Arc::new(crate::hooks::HookRegistry::new());
        let config = QueryConfig {
            max_turns: 10,
            ..QueryConfig::default()
        };

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "Write a long essay".into() }],
        })];

        let stream = query_stream(
            client, executor, state.clone(), tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Should have partial text from first turn
        let has_partial = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Partial output..."));
        // Should have escalation message
        let has_escalation = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("escalating max_tokens")));
        // Should have complete text from second turn
        let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "...complete!"));
        // Should end with TurnComplete
        let has_turn_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { stop_reason: StopReason::EndTurn }));

        assert!(has_partial, "expected partial text delta");
        assert!(has_escalation, "expected max_tokens escalation message");
        assert!(has_complete, "expected complete text delta");
        assert!(has_turn_complete, "expected TurnComplete at end");
    }

    #[tokio::test]
    async fn e2e_retry_on_overloaded_error() {
        // First call: overloaded error → retry
        // Second call: success
        let response = mock_text_response("Recovered!");
        let mock = MockBackend::new()
            .with_stream_error("overloaded: server is busy")
            .with_stream_events(make_stream_events(response));

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let registry = Arc::new(claude_tools::ToolRegistry::new());
        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::Default,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::Default,
            messages: vec![],
        };
        let hooks = Arc::new(crate::hooks::HookRegistry::new());
        let config = QueryConfig {
            max_turns: 5,
            ..QueryConfig::default()
        };

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        })];

        let stream = query_stream(
            client, executor, state, tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Should have retry message
        let has_retry = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("Retrying")));
        // Should have final text after retry
        let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Recovered!"));
        let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { stop_reason: StopReason::EndTurn }));

        assert!(has_retry, "expected retry message, got: {:?}", events);
        assert!(has_text, "expected recovered text");
        assert!(has_complete, "expected TurnComplete");
    }

    #[tokio::test]
    async fn e2e_fatal_error_stops_immediately() {
        // Queue a non-retryable error
        let mock = MockBackend::new()
            .with_stream_error("invalid_api_key: unauthorized");

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let registry = Arc::new(claude_tools::ToolRegistry::new());
        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::Default,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::Default,
            messages: vec![],
        };
        let hooks = Arc::new(crate::hooks::HookRegistry::new());
        let config = QueryConfig::default();

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        })];

        let stream = query_stream(
            client, executor, state, tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Should have fatal error, no retry
        let has_error = events.iter().any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("API error")));
        let has_retry = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("Retrying")));
        assert!(has_error, "expected API error, got: {:?}", events);
        assert!(!has_retry, "should NOT retry on fatal error");
    }

    #[tokio::test]
    async fn e2e_full_tool_round_trip_with_registered_tool() {
        // Register a mock echo tool that returns the input text
        struct EchoTool;
        #[async_trait::async_trait]
        impl claude_core::tool::Tool for EchoTool {
            fn name(&self) -> &str { "echo" }
            fn description(&self) -> &str { "Echo the input text" }
            fn input_schema(&self) -> serde_json::Value {
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" }
                    },
                    "required": ["text"]
                })
            }
            async fn call(&self, input: serde_json::Value, _ctx: &claude_core::tool::ToolContext) -> anyhow::Result<claude_core::tool::ToolResult> {
                let text = input["text"].as_str().unwrap_or("(empty)");
                Ok(claude_core::tool::ToolResult::text(format!("Echo: {}", text)))
            }
            fn is_read_only(&self) -> bool { true }
            fn category(&self) -> claude_core::tool::ToolCategory {
                claude_core::tool::ToolCategory::Session
            }
        }

        // Turn 1: tool_use("echo", {"text": "hello world"})
        // Turn 2: text response using tool result
        let tool_response = mock_tool_response("tool_1", "echo", serde_json::json!({"text": "hello world"}));
        let text_response = mock_text_response("The echo said: hello world");

        let mock = MockBackend::new()
            .with_stream_events(make_stream_events(tool_response))
            .with_stream_events(make_stream_events(text_response));

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let mut registry = claude_tools::ToolRegistry::new();
        registry.register(EchoTool);
        let registry = Arc::new(registry);

        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::BypassAll,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::BypassAll,
            messages: vec![],
        };
        let hooks = Arc::new(crate::hooks::HookRegistry::new());
        let config = QueryConfig {
            max_turns: 5,
            ..QueryConfig::default()
        };

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "Echo hello world".into() }],
        })];

        let stream = query_stream(
            client, executor, state.clone(), tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Verify full round-trip: tool start → ready → result → final text
        let has_tool_start = events.iter().any(|e| matches!(e, AgentEvent::ToolUseStart { name, .. } if name == "echo"));
        let has_tool_ready = events.iter().any(|e| matches!(e, AgentEvent::ToolUseReady { name, input, .. } if name == "echo" && input["text"] == "hello world"));

        // Tool result should contain "Echo: hello world" (NOT an error)
        let tool_result = events.iter().find_map(|e| {
            if let AgentEvent::ToolResult { id, is_error, text } = e {
                Some((id.clone(), *is_error, text.clone()))
            } else {
                None
            }
        });
        assert!(tool_result.is_some(), "expected tool result");
        let (id, is_error, text) = tool_result.unwrap();
        assert_eq!(id, "tool_1");
        assert!(!is_error, "tool should succeed");
        assert_eq!(text.as_deref(), Some("Echo: hello world"));

        // Final text from turn 2
        let has_final = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("echo said")));
        assert!(has_tool_start, "expected ToolUseStart");
        assert!(has_tool_ready, "expected ToolUseReady");
        assert!(has_final, "expected final text");

        // 4 messages: user + assistant(tool) + user(result) + assistant(text)
        let s = state.read().await;
        assert_eq!(s.messages.len(), 4);
    }

    #[tokio::test]
    async fn e2e_thinking_blocks_emitted() {
        // Model response with a thinking block followed by text
        let response = MessagesResponse {
            id: "msg_think".into(),
            response_type: "message".into(),
            role: "assistant".into(),
            content: vec![
                ResponseContentBlock::Thinking { thinking: "Let me think step by step...".into() },
                ResponseContentBlock::Text { text: "The answer is 42.".into() },
            ],
            model: "claude-sonnet-4-20250514".into(),
            stop_reason: Some("end_turn".into()),
            usage: ApiUsage {
                input_tokens: 100,
                output_tokens: 200,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };

        let mock = MockBackend::new()
            .with_stream_events(make_stream_events(response));

        let client = Arc::new(
            claude_api::client::AnthropicClient::new("test-key")
                .with_backend(Box::new(mock)),
        );
        let registry = Arc::new(claude_tools::ToolRegistry::new());
        let perm = Arc::new(crate::permissions::PermissionChecker::new(
            claude_core::permissions::PermissionMode::Default,
            vec![],
        ));
        let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
        let state = crate::state::new_shared_state();
        let tool_context = claude_core::tool::ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: claude_core::tool::AbortSignal::new(),
            permission_mode: claude_core::permissions::PermissionMode::Default,
            messages: vec![],
        };
        let hooks = Arc::new(crate::hooks::HookRegistry::new());
        let config = QueryConfig {
            thinking: Some(claude_api::types::ThinkingConfig {
                thinking_type: "enabled".into(),
                budget_tokens: Some(10000),
            }),
            ..QueryConfig::default()
        };

        let messages = vec![Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "What is the meaning of life?".into() }],
        })];

        let stream = query_stream(
            client, executor, state, tool_context,
            config, messages, vec![], hooks,
        );

        let events: Vec<AgentEvent> = tokio_stream::StreamExt::collect(stream).await;

        // Should have thinking delta
        let has_thinking = events.iter().any(|e| matches!(e, AgentEvent::ThinkingDelta(t) if t.contains("step by step")));
        // Should have text delta
        let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "The answer is 42."));
        // Should complete
        let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { stop_reason: StopReason::EndTurn }));

        assert!(has_thinking, "expected thinking delta, got: {:?}", events);
        assert!(has_text, "expected text delta");
        assert!(has_complete, "expected TurnComplete");
    }
}
