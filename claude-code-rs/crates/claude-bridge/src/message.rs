//! Message types — platform-agnostic inbound/outbound message representations.
//!
//! These types bridge the gap between platform-specific message formats
//! and the Agent's `AgentRequest`/`AgentNotification` types.

use serde::{Deserialize, Serialize};

/// Platform-agnostic channel identifier.
///
/// Wraps platform-specific channel/chat/group IDs with platform tagging.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChannelId {
    /// Platform name (e.g., "feishu", "telegram").
    pub platform: String,
    /// Platform-specific channel/chat ID.
    pub channel: String,
}

impl ChannelId {
    pub fn new(platform: impl Into<String>, channel: impl Into<String>) -> Self {
        Self {
            platform: platform.into(),
            channel: channel.into(),
        }
    }
}

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.platform, self.channel)
    }
}

/// Information about the message sender.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderInfo {
    /// Platform-specific user ID.
    pub user_id: String,
    /// Display name (may be empty).
    pub display_name: String,
    /// Optional avatar URL.
    pub avatar_url: Option<String>,
}

impl SenderInfo {
    pub fn new(user_id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            display_name: display_name.into(),
            avatar_url: None,
        }
    }
}

/// Attachment in an inbound message (images, files).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// MIME type (e.g., "image/png", "application/pdf").
    pub mime_type: String,
    /// File name.
    pub name: String,
    /// Download URL (platform-specific, may require auth).
    pub url: String,
    /// File size in bytes (if known).
    pub size: Option<u64>,
}

/// Inbound message from a platform user → Agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    /// Source channel.
    pub channel_id: ChannelId,
    /// Sender information.
    pub sender: SenderInfo,
    /// Message text content.
    pub text: String,
    /// Attached files/images.
    pub attachments: Vec<Attachment>,
    /// Platform-specific message ID (for threading/replies).
    pub message_id: Option<String>,
    /// If this is a reply, the parent message ID.
    pub reply_to: Option<String>,
    /// Raw platform-specific event payload (for advanced use).
    pub raw: Option<serde_json::Value>,
}

impl InboundMessage {
    /// Create a simple text message.
    pub fn text(
        channel_id: ChannelId,
        sender: SenderInfo,
        text: impl Into<String>,
    ) -> Self {
        Self {
            channel_id,
            sender,
            text: text.into(),
            attachments: vec![],
            message_id: None,
            reply_to: None,
            raw: None,
        }
    }
}

/// A code block in an outbound message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeBlock {
    /// Programming language (for syntax highlighting).
    pub language: Option<String>,
    /// Code content.
    pub code: String,
}

/// Tool execution result summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Tool name.
    pub tool_name: String,
    /// Brief summary of what the tool did.
    pub summary: String,
    /// Whether the tool succeeded.
    pub success: bool,
}

/// Outbound message from Agent → platform user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// Main text content (markdown).
    pub text: String,
    /// Extracted code blocks (for special rendering).
    pub code_blocks: Vec<CodeBlock>,
    /// Tool execution summaries.
    pub tool_results: Vec<ToolResult>,
    /// Whether this is a streaming update (platform may use edit instead of new msg).
    pub is_streaming: bool,
    /// Platform-specific message ID (set after first send, used for updates).
    pub message_id: Option<String>,
}

impl OutboundMessage {
    /// Create a simple text reply.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            code_blocks: vec![],
            tool_results: vec![],
            is_streaming: false,
            message_id: None,
        }
    }

    /// Create a streaming reply (will be updated as more content arrives).
    pub fn streaming(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            code_blocks: vec![],
            tool_results: vec![],
            is_streaming: true,
            message_id: None,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_id_display() {
        let id = ChannelId::new("feishu", "oc_abc123");
        assert_eq!(id.to_string(), "feishu:oc_abc123");
    }

    #[test]
    fn channel_id_eq() {
        let a = ChannelId::new("telegram", "123");
        let b = ChannelId::new("telegram", "123");
        let c = ChannelId::new("telegram", "456");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn channel_id_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ChannelId::new("feishu", "a"));
        set.insert(ChannelId::new("feishu", "a"));
        set.insert(ChannelId::new("telegram", "a"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn inbound_message_text() {
        let msg = InboundMessage::text(
            ChannelId::new("feishu", "ch1"),
            SenderInfo::new("u1", "Alice"),
            "Hello, Agent!",
        );
        assert_eq!(msg.text, "Hello, Agent!");
        assert!(msg.attachments.is_empty());
        assert!(msg.reply_to.is_none());
    }

    #[test]
    fn outbound_message_text() {
        let msg = OutboundMessage::text("Here is your answer.");
        assert!(!msg.is_streaming);
        assert!(msg.code_blocks.is_empty());
    }

    #[test]
    fn outbound_message_streaming() {
        let msg = OutboundMessage::streaming("Working on it...");
        assert!(msg.is_streaming);
    }

    #[test]
    fn sender_info_creation() {
        let sender = SenderInfo::new("user123", "Bob");
        assert_eq!(sender.user_id, "user123");
        assert_eq!(sender.display_name, "Bob");
        assert!(sender.avatar_url.is_none());
    }

    #[test]
    fn channel_id_serde_roundtrip() {
        let id = ChannelId::new("feishu", "oc_abc");
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: ChannelId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn inbound_message_serde_roundtrip() {
        let msg = InboundMessage::text(
            ChannelId::new("telegram", "chat42"),
            SenderInfo::new("u1", "Alice"),
            "Hello!",
        );
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: InboundMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.text, "Hello!");
        assert_eq!(deserialized.channel_id.platform, "telegram");
    }

    #[test]
    fn outbound_with_code_blocks() {
        let msg = OutboundMessage {
            text: "Here's the code:".into(),
            code_blocks: vec![
                CodeBlock { language: Some("rust".into()), code: "fn main() {}".into() },
            ],
            tool_results: vec![
                ToolResult { tool_name: "FileRead".into(), summary: "Read config.rs".into(), success: true },
            ],
            is_streaming: false,
            message_id: None,
        };
        assert_eq!(msg.code_blocks.len(), 1);
        assert_eq!(msg.tool_results.len(), 1);
        assert!(msg.tool_results[0].success);
    }
}
