//! Token estimation — approximate token counts for messages and text.
//!
//! Claude uses a BPE tokenizer where average English text is ~4 chars/token.
//! Code is typically denser (~3.5 chars/token). We use 4.0 as a conservative
//! estimate, matching the TS implementation's heuristic approach.
//!
//! For precise counts, the Anthropic `countTokens` API endpoint should be used,
//! but this module provides fast local estimates for:
//!   - Pre-checking if messages fit within context windows
//!   - Triggering auto-compact before actual API calls
//!   - Displaying approximate token counts in the UI
//!
//! ## Hybrid counting
//!
//! The canonical way to measure context size is [`token_count_with_estimation`]:
//! use the last API response's real `Usage` token count, plus a rough estimate
//! for any messages appended since.  This avoids drift between estimated and
//! actual counts over long conversations.

use crate::message::{ContentBlock, Message, ToolResultContent, Usage};

/// Default bytes-per-token ratio for general text.
const DEFAULT_BYTES_PER_TOKEN: f64 = 4.0;

/// Bytes-per-token ratio for JSON content (many single-char tokens like `{`, `:`, `"`).
const JSON_BYTES_PER_TOKEN: f64 = 2.0;

/// Overhead tokens per message (role marker, formatting).
const MESSAGE_OVERHEAD: u64 = 4;

/// Overhead tokens per tool use block (function call scaffolding).
const TOOL_USE_OVERHEAD: u64 = 20;

/// Fixed token cost for images/documents regardless of size.
/// Based on Anthropic's vision pricing: width×height / 750, max ~2000×2000 = 5333,
/// but the TS code uses a flat 2000-token estimate.
const IMAGE_FIXED_TOKENS: u64 = 2_000;

/// Overhead added by the API for each tool *definition* in the request.
/// Covers the JSON schema preamble, parameter descriptions, etc.
pub const TOOL_DEFINITION_OVERHEAD: u64 = 500;

// ── Per-file-type ratio ─────────────────────────────────────────────────────

/// Return the bytes-per-token ratio for a given file extension.
///
/// JSON files have many single-character tokens (`{`, `}`, `:`, `,`, `"`)
/// and thus use roughly 2 bytes/token instead of the default 4.
pub fn bytes_per_token_for_extension(ext: &str) -> f64 {
    match ext.to_lowercase().as_str() {
        "json" | "jsonl" | "jsonc" => JSON_BYTES_PER_TOKEN,
        _ => DEFAULT_BYTES_PER_TOKEN,
    }
}

/// Estimate tokens for a string using the default ratio.
pub fn estimate_text_tokens(text: &str) -> u64 {
    estimate_text_tokens_with_ratio(text, DEFAULT_BYTES_PER_TOKEN)
}

/// Estimate tokens for file content using a file-extension-aware ratio.
pub fn estimate_file_tokens(content: &str, extension: &str) -> u64 {
    estimate_text_tokens_with_ratio(content, bytes_per_token_for_extension(extension))
}

/// Estimate tokens with an explicit bytes-per-token ratio.
fn estimate_text_tokens_with_ratio(text: &str, bytes_per_token: f64) -> u64 {
    if text.is_empty() {
        return 0;
    }
    (text.len() as f64 / bytes_per_token).ceil() as u64
}

/// Estimate tokens for a single content block.
fn estimate_block_tokens(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Text { text } => estimate_text_tokens(text),
        ContentBlock::ToolUse { name, input, .. } => {
            let input_str = serde_json::to_string(input).unwrap_or_default();
            TOOL_USE_OVERHEAD + estimate_text_tokens(name) + estimate_text_tokens(&input_str)
        }
        ContentBlock::ToolResult { content, .. } => {
            let mut tokens = TOOL_USE_OVERHEAD;
            for c in content {
                match c {
                    ToolResultContent::Text { text } => tokens += estimate_text_tokens(text),
                    ToolResultContent::Image { .. } => {
                        tokens += IMAGE_FIXED_TOKENS;
                    }
                }
            }
            tokens
        }
        ContentBlock::Thinking { thinking } => estimate_text_tokens(thinking),
    }
}

/// Estimate tokens for a single message.
pub fn estimate_message_tokens(msg: &Message) -> u64 {
    let block_tokens = match msg {
        Message::User(u) => u.content.iter().map(estimate_block_tokens).sum::<u64>(),
        Message::Assistant(a) => a.content.iter().map(estimate_block_tokens).sum::<u64>(),
        Message::System(s) => estimate_text_tokens(&s.message),
    };
    block_tokens + MESSAGE_OVERHEAD
}

/// Estimate total tokens for a list of messages.
pub fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Estimate tokens for a system prompt string.
pub fn estimate_system_tokens(system: &str) -> u64 {
    estimate_text_tokens(system) + MESSAGE_OVERHEAD
}

/// Check if messages likely fit within a context window (with margin).
///
/// Returns `(fits, estimated_tokens)` where `fits` is true if the estimated
/// total is below `max_tokens * safety_margin`.
pub fn fits_in_context(
    system: &str,
    messages: &[Message],
    max_context: u64,
    safety_margin: f64,
) -> (bool, u64) {
    let system_tokens = estimate_system_tokens(system);
    let msg_tokens = estimate_messages_tokens(messages);
    let total = system_tokens + msg_tokens;
    let limit = (max_context as f64 * safety_margin) as u64;
    (total < limit, total)
}

/// Estimate the number of tool definition tokens for `n` tools.
///
/// Each tool definition schema costs roughly [`TOOL_DEFINITION_OVERHEAD`] tokens
/// in the API request.
pub fn estimate_tool_definition_tokens(tool_count: usize) -> u64 {
    tool_count as u64 * TOOL_DEFINITION_OVERHEAD
}

// ── Hybrid counting ─────────────────────────────────────────────────────────

/// Get total token count from a [`Usage`] struct (input + output + cache).
pub fn token_count_from_usage(usage: &Usage) -> u64 {
    usage.input_tokens
        + usage.output_tokens
        + usage.cache_creation_input_tokens.unwrap_or(0)
        + usage.cache_read_input_tokens.unwrap_or(0)
}

/// Canonical context-size measurement: use the last API response's real token
/// count plus a rough estimate for any messages appended since.
///
/// This avoids drift between estimated and actual counts over long conversations.
/// When multiple tool calls share the same assistant message ID, the function
/// walks back to the first sibling to avoid undercounting interleaved tool
/// results.
pub fn token_count_with_estimation(messages: &[Message]) -> u64 {
    // Walk backwards to find the most recent assistant message with Usage.
    let mut i = messages.len();
    while i > 0 {
        i -= 1;
        if let Message::Assistant(a) = &messages[i] {
            if let Some(ref usage) = a.usage {
                // Walk back past any earlier sibling records that share the
                // same message UUID (parallel tool calls split into separate
                // assistant records by the streaming layer).
                let anchor_uuid = &a.uuid;
                let mut start = i;
                if !anchor_uuid.is_empty() {
                    let mut j = i;
                    while j > 0 {
                        j -= 1;
                        if let Message::Assistant(prev) = &messages[j] {
                            if prev.uuid == *anchor_uuid {
                                start = j;
                            } else {
                                break;
                            }
                        }
                    }
                }
                let api_tokens = token_count_from_usage(usage);
                let tail_tokens = estimate_messages_tokens(&messages[start + 1..]);
                return api_tokens + tail_tokens;
            }
        }
    }
    // No Usage found — fall back to pure estimation.
    estimate_messages_tokens(messages)
}

// ── Tool result limiting ────────────────────────────────────────────────────

/// Default maximum tokens for a single tool result before truncation.
pub const DEFAULT_MAX_TOOL_RESULT_TOKENS: u64 = 30_000;

/// Truncate a tool result string if it exceeds `max_tokens` estimated tokens.
///
/// Keeps the first and last portions of the output, inserting a marker in the
/// middle. Returns the original string unchanged if within limits.
pub fn limit_tool_result(output: &str, max_tokens: u64) -> String {
    let estimated = estimate_text_tokens(output);
    if estimated <= max_tokens {
        return output.to_string();
    }

    // Convert token limit back to approximate byte budget
    let max_bytes = (max_tokens as f64 * DEFAULT_BYTES_PER_TOKEN) as usize;
    if output.len() <= max_bytes {
        return output.to_string();
    }

    let keep = max_bytes / 2;

    // Find safe char boundaries
    let mut first_end = keep;
    while first_end > 0 && !output.is_char_boundary(first_end) {
        first_end -= 1;
    }
    let mut last_start = output.len().saturating_sub(keep);
    while last_start < output.len() && !output.is_char_boundary(last_start) {
        last_start += 1;
    }

    let skipped_bytes = output.len() - first_end - (output.len() - last_start);
    let skipped_tokens = estimate_text_tokens(&output[first_end..last_start]);
    format!(
        "{}\n\n... [truncated ~{} tokens ({} bytes)] ...\n\n{}",
        &output[..first_end],
        skipped_tokens,
        skipped_bytes,
        &output[last_start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{AssistantMessage, ImageSource, SystemMessage, UserMessage};

    #[test]
    fn test_estimate_text_tokens() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("hello"), 2); // 5/4 = 1.25 → ceil = 2
        assert_eq!(estimate_text_tokens("a".repeat(100).as_str()), 25);
    }

    #[test]
    fn test_estimate_message_tokens() {
        let msg = Message::System(SystemMessage {
            uuid: "test".into(),
            message: "You are a helpful assistant.".into(),
        });
        let tokens = estimate_message_tokens(&msg);
        // 28 chars / 4 = 7 + 4 overhead = 11
        assert_eq!(tokens, 11);
    }

    #[test]
    fn test_fits_in_context() {
        let system = "System prompt";
        let messages = vec![
            Message::User(UserMessage {
                uuid: "u1".into(),
                content: vec![ContentBlock::Text { text: "Hello".into() }],
            }),
        ];
        let (fits, _) = fits_in_context(system, &messages, 200_000, 0.9);
        assert!(fits);
    }

    // ── P24 new tests ───────────────────────────────────────────────────

    #[test]
    fn file_type_ratio_json() {
        assert_eq!(bytes_per_token_for_extension("json"), 2.0);
        assert_eq!(bytes_per_token_for_extension("jsonl"), 2.0);
        assert_eq!(bytes_per_token_for_extension("JSONC"), 2.0);
        assert_eq!(bytes_per_token_for_extension("rs"), 4.0);
        assert_eq!(bytes_per_token_for_extension("py"), 4.0);
        assert_eq!(bytes_per_token_for_extension(""), 4.0);
    }

    #[test]
    fn estimate_file_tokens_json_denser() {
        let content = "a".repeat(100);
        let json_tokens = estimate_file_tokens(&content, "json");
        let rust_tokens = estimate_file_tokens(&content, "rs");
        // JSON: 100/2 = 50, Rust: 100/4 = 25
        assert_eq!(json_tokens, 50);
        assert_eq!(rust_tokens, 25);
    }

    #[test]
    fn image_block_fixed_tokens() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: vec![ToolResultContent::Image {
                source: ImageSource {
                    media_type: "image/png".into(),
                    data: "tiny".into(),
                },
            }],
            is_error: false,
        };
        let tokens = estimate_block_tokens(&block);
        // TOOL_USE_OVERHEAD(20) + IMAGE_FIXED_TOKENS(2000) = 2020
        assert_eq!(tokens, TOOL_USE_OVERHEAD + IMAGE_FIXED_TOKENS);
    }

    #[test]
    fn tool_definition_overhead() {
        assert_eq!(estimate_tool_definition_tokens(0), 0);
        assert_eq!(estimate_tool_definition_tokens(1), 500);
        assert_eq!(estimate_tool_definition_tokens(10), 5_000);
    }

    #[test]
    fn token_count_from_usage_all_fields() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(10),
            cache_read_input_tokens: Some(5),
        };
        assert_eq!(token_count_from_usage(&usage), 165);
    }

    #[test]
    fn token_count_from_usage_no_cache() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        assert_eq!(token_count_from_usage(&usage), 150);
    }

    #[test]
    fn hybrid_counting_no_usage_falls_back() {
        let messages = vec![
            Message::User(UserMessage {
                uuid: "u1".into(),
                content: vec![ContentBlock::Text { text: "Hello world".into() }],
            }),
        ];
        let hybrid = token_count_with_estimation(&messages);
        let pure = estimate_messages_tokens(&messages);
        assert_eq!(hybrid, pure);
    }

    #[test]
    fn hybrid_counting_uses_api_usage() {
        let messages = vec![
            Message::User(UserMessage {
                uuid: "u1".into(),
                content: vec![ContentBlock::Text { text: "Hello".into() }],
            }),
            Message::Assistant(AssistantMessage {
                uuid: "a1".into(),
                content: vec![ContentBlock::Text { text: "Hi there".into() }],
                stop_reason: None,
                usage: Some(Usage {
                    input_tokens: 1000,
                    output_tokens: 200,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                }),
            }),
            Message::User(UserMessage {
                uuid: "u2".into(),
                content: vec![ContentBlock::Text { text: "Follow up".into() }],
            }),
        ];
        let hybrid = token_count_with_estimation(&messages);
        // API: 1000+200=1200, tail (u2): "Follow up" = ceil(9/4)=3 + 4 overhead = 7
        assert_eq!(hybrid, 1207);
    }

    #[test]
    fn hybrid_counting_walks_back_siblings() {
        // Simulate parallel tool calls: two assistant messages with same UUID
        let messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: "shared".into(),
                content: vec![ContentBlock::Text { text: "tool1".into() }],
                stop_reason: None,
                usage: None,
            }),
            Message::User(UserMessage {
                uuid: "tool-result-1".into(),
                content: vec![ContentBlock::Text { text: "result1".into() }],
            }),
            Message::Assistant(AssistantMessage {
                uuid: "shared".into(),
                content: vec![ContentBlock::Text { text: "tool2".into() }],
                stop_reason: None,
                usage: Some(Usage {
                    input_tokens: 5000,
                    output_tokens: 500,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                }),
            }),
            Message::User(UserMessage {
                uuid: "u3".into(),
                content: vec![ContentBlock::Text { text: "next".into() }],
            }),
        ];
        let hybrid = token_count_with_estimation(&messages);
        // Walks back to first "shared" (index 0), tail = messages[1..] estimated
        // API tokens = 5500
        // tail = result1 msg + tool2 msg + next msg
        let tail = estimate_messages_tokens(&messages[1..]);
        assert_eq!(hybrid, 5500 + tail);
    }

    #[test]
    fn limit_tool_result_within_limit() {
        let short = "Hello world";
        assert_eq!(limit_tool_result(short, 100), short);
    }

    #[test]
    fn limit_tool_result_truncates_large() {
        let large = "x".repeat(200_000); // 200KB → ~50K tokens
        let limited = limit_tool_result(&large, 1_000); // limit to 1K tokens
        assert!(limited.len() < large.len());
        assert!(limited.contains("[truncated"));
        assert!(limited.contains("tokens"));
        // Starts and ends with original content
        assert!(limited.starts_with("xxx"));
        assert!(limited.ends_with("xxx"));
    }

    #[test]
    fn limit_tool_result_preserves_char_boundaries() {
        // Multi-byte chars: 你好 is 3 bytes each in UTF-8
        let content = "你好".repeat(50_000); // ~300KB
        let limited = limit_tool_result(&content, 1_000);
        // Should not panic and should produce valid UTF-8
        assert!(limited.len() < content.len());
        assert!(limited.contains("[truncated"));
    }

    #[test]
    fn estimate_text_tokens_with_ratio_custom() {
        let text = "a".repeat(100);
        assert_eq!(estimate_text_tokens_with_ratio(&text, 2.0), 50);
        assert_eq!(estimate_text_tokens_with_ratio(&text, 5.0), 20);
    }

    #[test]
    fn empty_messages_hybrid_returns_zero() {
        assert_eq!(token_count_with_estimation(&[]), 0);
    }
}
