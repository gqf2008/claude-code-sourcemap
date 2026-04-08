//! Notification Formatter — aggregates Agent notifications into outbound messages.
//!
//! The Agent emits many fine-grained notifications (TextDelta, ThinkingDelta,
//! ToolUseStart, etc.). The formatter collects these into coherent
//! `OutboundMessage` objects suitable for platform delivery.

use claude_bus::events::AgentNotification;

use crate::message::{CodeBlock, OutboundMessage, ToolResult};

/// Accumulator for building an outbound message from a stream of notifications.
///
/// Call `push()` for each notification, then `finish()` to get the final message.
pub struct MessageFormatter {
    text: String,
    code_blocks: Vec<CodeBlock>,
    tool_results: Vec<ToolResult>,
    thinking: String,
    current_tool: Option<String>,
    is_streaming: bool,
}

impl MessageFormatter {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            code_blocks: vec![],
            tool_results: vec![],
            thinking: String::new(),
            current_tool: None,
            is_streaming: true,
        }
    }

    /// Process a notification, accumulating content.
    ///
    /// Returns `true` if the turn is complete (no more notifications expected).
    pub fn push(&mut self, notification: &AgentNotification) -> bool {
        match notification {
            AgentNotification::TextDelta { text } => {
                self.text.push_str(text);
                false
            }
            AgentNotification::ThinkingDelta { text } => {
                self.thinking.push_str(text);
                false
            }
            AgentNotification::ToolUseStart { tool_name, .. } => {
                self.current_tool = Some(tool_name.clone());
                false
            }
            AgentNotification::ToolUseComplete {
                tool_name,
                result_preview,
                is_error,
                ..
            } => {
                let summary = result_preview
                    .as_ref()
                    .map(|r| truncate(r, 200))
                    .unwrap_or_default();
                self.tool_results.push(ToolResult {
                    tool_name: tool_name.clone(),
                    summary,
                    success: !is_error,
                });
                self.current_tool = None;
                false
            }
            AgentNotification::TurnComplete { .. } => {
                self.is_streaming = false;
                true // Turn complete
            }
            AgentNotification::Error { message, .. } => {
                self.text.push_str(&format!("\n\n❌ Error: {}", message));
                self.is_streaming = false;
                true // Error ends the turn
            }
            _ => false, // Ignore other notification types
        }
    }

    /// Get a snapshot of the current message (for streaming updates).
    pub fn snapshot(&self) -> OutboundMessage {
        OutboundMessage {
            text: self.text.clone(),
            code_blocks: self.code_blocks.clone(),
            tool_results: self.tool_results.clone(),
            is_streaming: self.is_streaming,
            message_id: None,
        }
    }

    /// Finalize the message.
    pub fn finish(self) -> OutboundMessage {
        OutboundMessage {
            text: self.text,
            code_blocks: self.code_blocks,
            tool_results: self.tool_results,
            is_streaming: false,
            message_id: None,
        }
    }

    /// Whether the formatter has any content.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.tool_results.is_empty()
    }

    /// Whether a tool is currently executing.
    pub fn is_tool_running(&self) -> bool {
        self.current_tool.is_some()
    }

    /// Get the accumulated text so far.
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Default for MessageFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Truncate a string to max_len, adding "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_delta_accumulation() {
        let mut fmt = MessageFormatter::new();
        assert!(!fmt.push(&AgentNotification::TextDelta { text: "Hello ".into() }));
        assert!(!fmt.push(&AgentNotification::TextDelta { text: "world!".into() }));
        assert_eq!(fmt.text(), "Hello world!");
    }

    #[test]
    fn turn_complete_signals_done() {
        let mut fmt = MessageFormatter::new();
        fmt.push(&AgentNotification::TextDelta { text: "Done.".into() });
        let done = fmt.push(&AgentNotification::TurnComplete {
            turn: 1,
            stop_reason: "end_turn".into(),
            usage: claude_bus::events::UsageInfo::default(),
        });
        assert!(done);
    }

    #[test]
    fn tool_results_tracked() {
        let mut fmt = MessageFormatter::new();
        fmt.push(&AgentNotification::ToolUseStart {
            id: "t1".into(),
            tool_name: "FileRead".into(),
        });
        assert!(fmt.is_tool_running());

        fmt.push(&AgentNotification::ToolUseComplete {
            id: "t1".into(),
            tool_name: "FileRead".into(),
            result_preview: Some("contents of file.rs".into()),
            is_error: false,
        });
        assert!(!fmt.is_tool_running());

        let msg = fmt.finish();
        assert_eq!(msg.tool_results.len(), 1);
        assert_eq!(msg.tool_results[0].tool_name, "FileRead");
        assert!(msg.tool_results[0].success);
    }

    #[test]
    fn error_notification() {
        let mut fmt = MessageFormatter::new();
        let done = fmt.push(&AgentNotification::Error {
            code: claude_bus::events::ErrorCode::ApiError,
            message: "Rate limited".into(),
        });
        assert!(done);
        assert!(fmt.text().contains("❌ Error: Rate limited"));
    }

    #[test]
    fn snapshot_while_streaming() {
        let mut fmt = MessageFormatter::new();
        fmt.push(&AgentNotification::TextDelta { text: "In progress...".into() });
        let snap = fmt.snapshot();
        assert!(snap.is_streaming);
        assert_eq!(snap.text, "In progress...");
    }

    #[test]
    fn empty_formatter() {
        let fmt = MessageFormatter::new();
        assert!(fmt.is_empty());
        assert!(!fmt.is_tool_running());
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn thinking_delta_accumulated_separately() {
        let mut fmt = MessageFormatter::new();
        fmt.push(&AgentNotification::ThinkingDelta { text: "Let me think...".into() });
        // Thinking is not included in the main text
        assert!(fmt.text().is_empty());
        // But formatter is not considered empty since we have thinking content
        // Actually thinking doesn't count towards is_empty
        assert!(fmt.is_empty());
    }
}
