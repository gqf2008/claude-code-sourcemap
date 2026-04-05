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

use crate::message::{ContentBlock, Message, ToolResultContent};

/// Average characters per token (conservative estimate).
const CHARS_PER_TOKEN: f64 = 4.0;

/// Overhead tokens per message (role marker, formatting).
const MESSAGE_OVERHEAD: u64 = 4;

/// Overhead tokens per tool use block (function call scaffolding).
const TOOL_USE_OVERHEAD: u64 = 20;

/// Estimate tokens for a string.
pub fn estimate_text_tokens(text: &str) -> u64 {
    (text.len() as f64 / CHARS_PER_TOKEN).ceil() as u64
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
                    ToolResultContent::Image { source } => {
                        // Images are roughly 1 token per 750 pixels, but we estimate
                        // from base64 data size: ~0.75 bytes/pixel * 4/3 base64 overhead
                        tokens += (source.data.len() as u64) / 100;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_text_tokens() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("hello"), 2); // 5/4 = 1.25 → ceil = 2
        assert_eq!(estimate_text_tokens("a".repeat(100).as_str()), 25);
    }

    #[test]
    fn test_estimate_message_tokens() {
        let msg = Message::System(crate::message::SystemMessage {
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
            Message::User(crate::message::UserMessage {
                uuid: "u1".into(),
                content: vec![ContentBlock::Text { text: "Hello".into() }],
            }),
        ];
        let (fits, _) = fits_in_context(system, &messages, 200_000, 0.9);
        assert!(fits);
    }
}
