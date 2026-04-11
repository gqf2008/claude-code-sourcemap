//! Helper functions for the query stream loop.
//!
//! Extracted from `query.rs` for readability. All functions are `pub(super)`
//! so they can only be used by the parent `query` module.

use uuid::Uuid;

use clawed_api::types::*;
use clawed_core::message::{ContentBlock, Message, UserMessage};

use super::AgentEvent;

// ── System prompt ────────────────────────────────────────────────────────────

/// Build system prompt blocks with cache control and dynamic boundary splitting.
///
/// When `skip_cache` is true, all `cache_control` fields are set to `None`
/// to force a prompt cache miss (used by `/break-cache`).
pub(super) fn build_system_blocks(system_prompt: &str, skip_cache: bool) -> Option<Vec<SystemBlock>> {
    if system_prompt.is_empty() {
        return None;
    }
    let cc = if skip_cache { None } else { Some(CacheControl::ephemeral()) };
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
                cache_control: cc.clone(),
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
            cache_control: cc,
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

/// Add jitter to a delay value (±25%) to prevent thundering herd.
/// Uses a simple deterministic pseudo-random based on the attempt count
/// to avoid pulling in a random number generator.
pub(super) fn with_jitter(base_ms: u64, attempt: u32) -> u64 {
    // Pseudo-random factor based on attempt: varies between 0.75 and 1.25
    let factor_pct = 75 + ((attempt as u64 * 37 + 13) % 51); // 75..125
    base_ms * factor_pct / 100
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
/// Returns `(warning_level, event)` — caller should deduplicate by level.
pub(super) fn build_context_warning(
    total_input: u64,
    context_window: u64,
) -> Option<(crate::compact::TokenWarningState, AgentEvent)> {
    let threshold = crate::compact::get_auto_compact_threshold(context_window);
    let warning = crate::compact::calculate_token_warning(total_input, threshold);
    if warning == crate::compact::TokenWarningState::Normal {
        return None;
    }
    let pct = if context_window > 0 {
        (total_input as f64 / context_window as f64).min(1.0)
    } else {
        0.0
    };
    let msg = match warning {
        crate::compact::TokenWarningState::Warning =>
            "Approaching context limit — consider saving progress".to_string(),
        crate::compact::TokenWarningState::Critical =>
            "Context nearly full — auto-compaction may trigger soon".to_string(),
        crate::compact::TokenWarningState::Imminent =>
            "Context limit imminent — auto-compaction will trigger".to_string(),
        _ => return None,
    };
    Some((warning, AgentEvent::ContextWarning { usage_pct: pct, message: msg }))
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
///
/// When `skip_cache` is true, no cache_control markers are added (for `/break-cache`).
pub(super) fn messages_to_api(messages: &[Message], skip_cache: bool) -> Vec<ApiMessage> {
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

    // Cache breakpoint at conversation tail (skipped when break-cache is active)
    if !skip_cache {
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
                clawed_core::message::ToolResultContent::Text { text } => {
                    clawed_api::types::ToolResultContent::Text { text: text.clone() }
                }
                clawed_core::message::ToolResultContent::Image { .. } => {
                    clawed_api::types::ToolResultContent::Text { text: "[image]".into() }
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
                source: clawed_api::types::ImageSource {
                    source_type: "base64".into(),
                    media_type: source.media_type.clone(),
                    data: source.data.clone(),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── with_jitter ────────────────────────────────────────────────────

    #[test]
    fn jitter_stays_within_bounds() {
        for attempt in 0..20 {
            let result = with_jitter(1000, attempt);
            assert!(result >= 750, "attempt {}: {} < 750", attempt, result);
            assert!(result <= 1250, "attempt {}: {} > 1250", attempt, result);
        }
    }

    #[test]
    fn jitter_varies_across_attempts() {
        let v1 = with_jitter(1000, 1);
        let v2 = with_jitter(1000, 2);
        let v3 = with_jitter(1000, 3);
        assert!(v1 != v2 || v2 != v3, "jitter should vary: {}, {}, {}", v1, v2, v3);
    }

    #[test]
    fn jitter_zero_base() {
        assert_eq!(with_jitter(0, 1), 0);
    }

    // ── classify_api_error ─────────────────────────────────────────────

    #[test]
    fn classify_rate_limit_retries() {
        let action = classify_api_error("rate limit exceeded", false, 1, 1000);
        assert!(matches!(action, ApiErrorAction::Retry { .. }));
    }

    #[test]
    fn classify_prompt_too_long_compacts() {
        let action = classify_api_error("prompt is too long", false, 1, 1000);
        assert!(matches!(action, ApiErrorAction::ReactiveCompact));
    }

    #[test]
    fn classify_prompt_too_long_after_compact_is_fatal() {
        let action = classify_api_error("prompt is too long", true, 1, 1000);
        assert!(matches!(action, ApiErrorAction::Fatal));
    }

    #[test]
    fn classify_too_many_errors_is_fatal() {
        let action = classify_api_error("rate limit", false, 6, 1000);
        assert!(matches!(action, ApiErrorAction::Fatal));
    }

    // ── error_category ─────────────────────────────────────────────────

    #[test]
    fn error_category_rate_limit() {
        assert_eq!(error_category("429 rate limit"), "rate_limit");
    }

    #[test]
    fn error_category_overloaded() {
        assert_eq!(error_category("529 overloaded"), "overloaded");
    }

    #[test]
    fn error_category_server() {
        assert_eq!(error_category("500 internal server error"), "server_error");
    }

    #[test]
    fn error_category_generic() {
        assert_eq!(error_category("something else"), "api_error");
    }
}
