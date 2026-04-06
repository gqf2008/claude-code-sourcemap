//! Helper functions for the query stream loop.
//!
//! Extracted from `query.rs` for readability. All functions are `pub(super)`
//! so they can only be used by the parent `query` module.

use uuid::Uuid;

use claude_api::types::*;
use claude_core::message::{ContentBlock, Message, UserMessage};

use super::AgentEvent;

// ── System prompt ────────────────────────────────────────────────────────────

/// Build system prompt blocks with cache control and dynamic boundary splitting.
pub(super) fn build_system_blocks(system_prompt: &str) -> Option<Vec<SystemBlock>> {
    if system_prompt.is_empty() {
        return None;
    }
    let boundary = system_prompt.find(
        crate::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY
    );
    match boundary {
        Some(pos) => {
            let static_prefix = system_prompt[..pos].trim();
            let dynamic_suffix = system_prompt[pos..].trim();
            let dynamic_suffix = dynamic_suffix
                .strip_prefix(crate::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
                .unwrap_or(dynamic_suffix)
                .trim();
            let mut blocks = vec![SystemBlock {
                block_type: "text".into(),
                text: static_prefix.to_string(),
                cache_control: Some(CacheControl::ephemeral()),
            }];
            if !dynamic_suffix.is_empty() {
                blocks.push(SystemBlock {
                    block_type: "text".into(),
                    text: dynamic_suffix.to_string(),
                    cache_control: None,
                });
            }
            Some(blocks)
        }
        None => Some(vec![SystemBlock {
            block_type: "text".into(),
            text: system_prompt.to_string(),
            cache_control: Some(CacheControl::ephemeral()),
        }]),
    }
}

// ── Error classification ─────────────────────────────────────────────────────

/// What to do when an API error occurs.
pub(super) enum ApiErrorAction {
    /// Trigger reactive compaction (prompt too long).
    ReactiveCompact,
    /// Retry after a delay (transient error).
    Retry { wait_ms: u64 },
    /// Fatal error — give up.
    Fatal,
}

/// Classify an API error string and determine retry action.
pub(super) fn classify_api_error(
    err_str: &str,
    has_attempted_reactive_compact: bool,
    consecutive_errors: u32,
    retry_delay_ms: u64,
) -> ApiErrorAction {
    let is_prompt_too_long = err_str.contains("prompt is too long")
        || err_str.contains("413")
        || err_str.contains("too many tokens");
    if is_prompt_too_long && !has_attempted_reactive_compact {
        return ApiErrorAction::ReactiveCompact;
    }

    let is_retryable = err_str.contains("rate")
        || err_str.contains("529")
        || err_str.contains("500")
        || err_str.contains("503")
        || err_str.contains("overloaded");

    const MAX_CONSECUTIVE_ERRORS: u32 = 5;
    if is_retryable && consecutive_errors <= MAX_CONSECUTIVE_ERRORS {
        let wait_ms = if let Some(pos) = err_str.find("retry-after:") {
            let after = &err_str[pos + 12..];
            after.split_whitespace().next()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|secs| secs * 1000)
                .unwrap_or(retry_delay_ms)
        } else {
            retry_delay_ms
        };
        return ApiErrorAction::Retry { wait_ms };
    }

    ApiErrorAction::Fatal
}

/// Classify an error string into a tracking category.
pub(super) fn error_category(err_str: &str) -> &'static str {
    if err_str.contains("rate") || err_str.contains("429") {
        "rate_limit"
    } else if err_str.contains("overloaded") || err_str.contains("529") {
        "overloaded"
    } else if err_str.contains("500") || err_str.contains("503") {
        "server_error"
    } else {
        "api_error"
    }
}

// ── Context & recovery ───────────────────────────────────────────────────────

/// Build a context warning event if token usage is elevated.
pub(super) fn build_context_warning(total_input: u64) -> Option<AgentEvent> {
    let warning = crate::compact::calculate_token_warning(
        total_input,
        crate::compact::AUTO_COMPACT_THRESHOLD,
    );
    if warning == crate::compact::TokenWarningState::Normal {
        return None;
    }
    let pct = total_input as f64 / crate::compact::AUTO_COMPACT_THRESHOLD as f64;
    let msg = match warning {
        crate::compact::TokenWarningState::Warning =>
            "Approaching context limit — consider saving progress".to_string(),
        crate::compact::TokenWarningState::Critical =>
            "Context nearly full — auto-compaction may trigger soon".to_string(),
        crate::compact::TokenWarningState::Imminent =>
            "Context limit imminent — auto-compaction will trigger".to_string(),
        _ => return None,
    };
    Some(AgentEvent::ContextWarning { usage_pct: pct, message: msg })
}

/// Create a continuation message for max_tokens recovery.
pub(super) fn make_continuation_message(attempt: u32, limit: u32) -> UserMessage {
    let text = if attempt == 0 {
        "Output token limit hit. Resume directly — no apology, \
         no recap. Continue exactly where you left off.".to_string()
    } else {
        format!(
            "Output token limit hit again (attempt {}/{}). Continue where you left off. \
             Break remaining work into smaller pieces.",
            attempt, limit
        )
    };
    UserMessage {
        uuid: Uuid::new_v4().to_string(),
        content: vec![ContentBlock::Text { text }],
    }
}

// ── Message format conversion ────────────────────────────────────────────────

/// Convert internal messages to API format, adding cache breakpoints.
pub(super) fn messages_to_api(messages: &[Message]) -> Vec<ApiMessage> {
    let mut api_msgs: Vec<ApiMessage> = messages.iter().filter_map(|msg| match msg {
        Message::User(u) => Some(ApiMessage {
            role: "user".into(),
            content: u.content.iter().map(block_to_api).collect(),
        }),
        Message::Assistant(a) => Some(ApiMessage {
            role: "assistant".into(),
            content: a.content.iter().map(block_to_api).collect(),
        }),
        Message::System(_) => None,
    }).collect();

    // Cache breakpoint at conversation tail
    if let Some(last_msg) = api_msgs.last_mut() {
        if let Some(last_block) = last_msg.content.last_mut() {
            match last_block {
                ApiContentBlock::Text { cache_control, .. } => {
                    *cache_control = Some(CacheControl::ephemeral());
                }
                ApiContentBlock::ToolResult { cache_control, .. } => {
                    *cache_control = Some(CacheControl::ephemeral());
                }
                _ => {}
            }
        }
    }
    api_msgs
}

/// Convert a single content block to API format.
pub(super) fn block_to_api(block: &ContentBlock) -> ApiContentBlock {
    match block {
        ContentBlock::Text { text } => ApiContentBlock::Text { text: text.clone(), cache_control: None },
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
            cache_control: None,
        },
        ContentBlock::Thinking { thinking } => {
            ApiContentBlock::Text { text: format!("<thinking>{}</thinking>", thinking), cache_control: None }
        }
        ContentBlock::Image { source } => {
            ApiContentBlock::Image {
                source: claude_api::types::ImageSource {
                    source_type: "base64".into(),
                    media_type: source.media_type.clone(),
                    data: source.data.clone(),
                },
            }
        }
    }
}
