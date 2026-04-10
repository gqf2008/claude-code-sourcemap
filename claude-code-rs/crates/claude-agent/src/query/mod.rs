mod helpers;

use std::pin::Pin;
use std::sync::Arc;
use futures::Stream;
use tracing::{info, warn};
use uuid::Uuid;

use claude_api::client::ApiClient;
use claude_api::types::*;
use claude_core::message::{
    AssistantMessage, ContentBlock, Message, StopReason, Usage, UserMessage,
};
use claude_core::tool::ToolContext;
use crate::compact::{AutoCompactState, compact_conversation, compact_context_message};
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
    /// Model context window size (for accurate percentage display).
    /// Overridden at runtime by engine builder from model capabilities.
    pub context_window: u64,
    /// Auto-compact state for proactive in-loop compaction.
    /// When `Some`, the query loop will trigger compaction when token usage
    /// approaches the context window limit (between tool-use turns).
    pub auto_compact_state: Option<Arc<tokio::sync::Mutex<AutoCompactState>>>,
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
            context_window: 200_000, // fallback; prefer runtime value from model capabilities
            auto_compact_state: None,
        }
    }
}

/// Core agent loop: send messages → process stream → execute tools → repeat
#[allow(clippy::too_many_arguments)]
pub fn query_stream(
    client: Arc<ApiClient>,
    executor: Arc<ToolExecutor>,
    state: SharedState,
    tool_context: ToolContext,
    config: QueryConfig,
    initial_messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    hooks: Arc<HookRegistry>,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
    query_stream_with_injection(
        client, executor, state, tool_context, config,
        initial_messages, tools, hooks, None,
    )
}

/// Like [`query_stream`] but accepts an optional channel for mid-stream message
/// injection. Messages received on `inject_rx` are appended as user-role
/// messages at the start of each turn, enabling SendMessage follow-ups.
#[allow(clippy::too_many_arguments)]
pub fn query_stream_with_injection(
    client: Arc<ApiClient>,
    executor: Arc<ToolExecutor>,
    state: SharedState,
    tool_context: ToolContext,
    config: QueryConfig,
    initial_messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    hooks: Arc<HookRegistry>,
    inject_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
    let stream = async_stream::stream! {
        let mut messages = initial_messages;
        let mut tool_context = tool_context;
        let mut turn_count: u32 = 0;
        let mut stop_hook_retries: u32 = 0;
        const MAX_STOP_HOOK_RETRIES: u32 = 3;
        let mut inject_rx = inject_rx;

        // ── Recovery state (aligned with TS query.ts) ────────────────────────
        let mut max_tokens_recovery_count: u32 = 0;
        const MAX_TOKENS_RECOVERY_LIMIT: u32 = 3;
        let mut effective_max_tokens = config.max_tokens;
        let mut has_attempted_reactive_compact = false;
        let mut consecutive_errors: u32 = 0;
        let mut retry_delay_ms: u64 = 1_000; // exponential backoff: 1s → 2s → 4s → … → 32s max

        // Track last emitted warning level to avoid spamming the same warning every turn
        let mut last_warning_level: Option<crate::compact::TokenWarningState> = None;

        // Look up model capabilities for smart max_tokens escalation
        let model_name = { state.read().await.model.clone() };
        let caps = claude_core::model::model_capabilities(&model_name);
        let escalated_max_tokens = caps.upper_max_output;

        loop {
            // Drain any externally injected messages (from SendMessage)
            if let Some(ref mut rx) = inject_rx {
                while let Ok(msg_text) = rx.try_recv() {
                    messages.push(Message::User(UserMessage {
                        uuid: Uuid::new_v4().to_string(),
                        content: vec![ContentBlock::Text {
                            text: format!("[Follow-up message from coordinator]\n{}", msg_text),
                        }],
                    }));
                }
            }

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
            // Inject plan mode context into system prompt when active
            let effective_system = if tool_context.permission_mode == claude_core::permissions::PermissionMode::Plan {
                format!(
                    "{}\n\n<plan_mode>\nYou are currently in PLAN MODE. Only read-only tools are available.\n\
                     Focus on exploration and planning. When your plan is ready, call ExitPlanMode with the plan.\n\
                     Do NOT attempt to use file write, edit, or shell tools — they will be rejected.\n</plan_mode>",
                    config.system_prompt
                )
            } else {
                config.system_prompt.clone()
            };
            let system = build_system_blocks(&effective_system);

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
                            }
                            // Always retry after reactive compact attempt
                            continue;
                        }
                        ApiErrorAction::Retry { wait_ms } => {
                            if turn_count + 1 < config.max_turns {
                                let jittered = with_jitter(wait_ms, consecutive_errors);
                                yield AgentEvent::TextDelta(format!(
                                    "\n\x1b[33m[Retrying after API error ({}) in {}ms: {}]\x1b[0m\n",
                                    consecutive_errors, jittered, err_str
                                ));
                                tokio::time::sleep(std::time::Duration::from_millis(jittered)).await;
                                retry_delay_ms = (retry_delay_ms * 2).min(32_000);
                                continue;
                            }
                        }
                        ApiErrorAction::Fatal => {}
                    }
                    state.write().await.messages = messages.clone();
                    yield AgentEvent::Error(format!("API error: {:#}", e));
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
            let mut should_retry_turn = false;

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
                            DeltaBlock::SignatureDelta { .. } => {
                                // Safely ignored — signature verification is not user-facing
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
                        let err_str = format!("{}", e);
                        let is_timeout = err_str.contains("idle timeout")
                            || err_str.contains("stall timeout")
                            || err_str.contains("timed out")
                            || err_str.contains("connection reset");

                        if is_timeout {
                            consecutive_errors += 1;
                            state.write().await.record_error("stream_timeout");
                            if consecutive_errors <= 3 {
                                let wait_ms = with_jitter(retry_delay_ms.min(8_000), consecutive_errors);
                                yield AgentEvent::TextDelta(format!(
                                    "\n\x1b[33m[Stream timeout — retrying ({}/3) in {}ms]\x1b[0m\n",
                                    consecutive_errors, wait_ms,
                                ));
                                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                                retry_delay_ms = (retry_delay_ms * 2).min(32_000);
                                // Don't push partial assistant message; retry the entire turn
                                should_retry_turn = true;
                                break; // break inner while loop
                            }
                        }

                        state.write().await.messages = messages.clone();
                        yield AgentEvent::Error(format!("Stream error: {}", e));
                        break;
                    }
                }
            }

            // If a stream timeout triggered a retry, skip message processing
            // and go straight to the next outer-loop iteration (re-call API).
            if should_retry_turn {
                continue;
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

                // Context usage warning (only emit when level escalates)
                let total_input = { state.read().await.total_input_tokens };
                if let Some((level, warning_event)) = build_context_warning(total_input, config.context_window) {
                    if last_warning_level != Some(level) {
                        last_warning_level = Some(level);
                        yield warning_event;
                    }
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
                    // Capture tool names for plan mode transitions before execution
                    let called_tool_names: Vec<String> = tool_uses.iter().map(|(_, name, _)| name.clone()).collect();

                    // Snapshot messages into tool_context so tools like ContextTool can inspect them
                    tool_context.messages = messages.clone();
                    let results: Vec<ContentBlock> = executor.execute_many(tool_uses, &tool_context).await;
                    let tool_result_msg = UserMessage {
                        uuid: Uuid::new_v4().to_string(),
                        content: results.clone(),
                    };
                    messages.push(Message::User(tool_result_msg));

                    // Check for plan mode transitions from successful tool results
                    for (idx, result) in results.iter().enumerate() {
                        if let ContentBlock::ToolResult { tool_use_id, is_error, content } = result {
                            let result_text = content.first().and_then(|c| {
                                if let claude_core::message::ToolResultContent::Text { text } = c {
                                    Some(text.clone())
                                } else {
                                    None
                                }
                            });

                            // Apply plan mode transitions on successful tool calls
                            if !is_error {
                                if let Some(name) = called_tool_names.get(idx) {
                                    if name == "EnterPlanMode" {
                                        let mut s = state.write().await;
                                        s.enter_plan_mode();
                                        tool_context.permission_mode = claude_core::permissions::PermissionMode::Plan;
                                        info!("Plan mode activated via EnterPlanMode tool");
                                    } else if name == "ExitPlanMode" {
                                        let mut s = state.write().await;
                                        let restored = s.exit_plan_mode();
                                        tool_context.permission_mode = restored;
                                        info!("Plan mode deactivated via ExitPlanMode tool, restored to {:?}", restored);
                                    }
                                }
                            }

                            yield AgentEvent::ToolResult { id: tool_use_id.clone(), is_error: *is_error, text: result_text };
                        }
                    }
                    turn_count += 1;
                    stop_hook_retries = 0;
                    { let mut s = state.write().await; s.turn_count = turn_count; }

                    // ── Proactive auto-compact (between tool-use turns) ──────
                    // Like TS query.ts: check should_auto_compact at each turn
                    // boundary and compact before next API call to avoid hitting
                    // context limits mid-conversation.
                    if let Some(ref ac_state) = config.auto_compact_state {
                        let current_tokens = claude_core::token_estimation::token_count_with_estimation(&messages)
                            + claude_core::token_estimation::estimate_system_tokens(&config.system_prompt);
                        let should_compact = {
                            let ac = ac_state.lock().await;
                            ac.should_auto_compact(current_tokens, config.context_window)
                        };
                        if should_compact {
                            yield AgentEvent::CompactStart;
                            let model = { state.read().await.model.clone() };
                            match compact_conversation(&client, &messages, &model, None).await {
                                Ok(summary) => {
                                    let summary_len = summary.len();
                                    let context_msg = compact_context_message(&summary, None);
                                    messages = vec![Message::User(UserMessage {
                                        uuid: Uuid::new_v4().to_string(),
                                        content: vec![ContentBlock::Text { text: context_msg }],
                                    })];
                                    // Re-estimate tokens so warning system stays accurate
                                    let new_tokens = claude_core::token_estimation::token_count_with_estimation(&messages)
                                        + claude_core::token_estimation::estimate_system_tokens(&config.system_prompt);
                                    {
                                        let mut s = state.write().await;
                                        s.messages = messages.clone();
                                        s.total_input_tokens = new_tokens;
                                        s.total_output_tokens = 0;
                                    }
                                    ac_state.lock().await.record_success();
                                    last_warning_level = None; // reset after compaction
                                    has_attempted_reactive_compact = false;
                                    info!("Proactive auto-compact succeeded (summary {} chars)", summary_len);
                                    yield AgentEvent::CompactComplete { summary_len };
                                }
                                Err(e) => {
                                    ac_state.lock().await.record_failure();
                                    warn!("Proactive auto-compact failed: {}", e);
                                    yield AgentEvent::TextDelta(format!(
                                        "\n\x1b[33m[Auto-compact failed: {} — continuing…]\x1b[0m\n", e
                                    ));
                                }
                            }
                        }
                    }
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
mod tests;

#[cfg(test)]
mod e2e_tests;

