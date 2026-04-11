use serde::{Deserialize, Serialize};

/// Why the model stopped generating: end of turn, tool use, token limit, or stop sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
}

/// Token usage for a single API turn (input, output, and cache token counts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

/// Base64-encoded image data with MIME type (e.g. `image/png`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    pub media_type: String,
    pub data: String,
}

/// A content block in a conversation message: text, image, tool call, tool result, or thinking.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Vec<ToolResultContent>,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
}

/// Content within a tool result — text or image.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
}

/// A message from the user, with a unique ID and content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub uuid: String,
    pub content: Vec<ContentBlock>,
}

/// A message from the assistant, with content, stop reason, and token usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub uuid: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<StopReason>,
    pub usage: Option<Usage>,
}

/// An internal system message (e.g. compaction notice, hook output).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessage {
    pub uuid: String,
    pub message: String,
}

/// A conversation message — either user, assistant, or system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    #[serde(rename = "system")]
    System(SystemMessage),
}

impl Message {
    pub fn uuid(&self) -> &str {
        match self {
            Message::User(m) => &m.uuid,
            Message::Assistant(m) => &m.uuid,
            Message::System(m) => &m.uuid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stop_reason_serde_roundtrip() {
        for reason in [StopReason::EndTurn, StopReason::ToolUse, StopReason::MaxTokens, StopReason::StopSequence] {
            let json = serde_json::to_string(&reason).unwrap();
            let back: StopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{:?}", reason), format!("{:?}", back));
        }
    }

    #[test]
    fn content_block_text_serde() {
        let block = ContentBlock::Text { text: "Hello".into() };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "Hello");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ContentBlock::Text { text } if text == "Hello"));
    }

    #[test]
    fn content_block_tool_use_serde() {
        let block = ContentBlock::ToolUse {
            id: "tu_1".into(),
            name: "FileRead".into(),
            input: json!({"path": "src/main.rs"}),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_use");
        assert_eq!(json["name"], "FileRead");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ContentBlock::ToolUse { name, .. } if name == "FileRead"));
    }

    #[test]
    fn content_block_tool_result_serde() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "tu_1".into(),
            content: vec![ToolResultContent::Text { text: "file contents".into() }],
            is_error: false,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["is_error"], false);
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ContentBlock::ToolResult { is_error: false, .. }));
    }

    #[test]
    fn content_block_thinking_serde() {
        let block = ContentBlock::Thinking { thinking: "Let me think...".into() };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "thinking");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ContentBlock::Thinking { thinking } if thinking.contains("think")));
    }

    #[test]
    fn content_block_image_serde() {
        let block = ContentBlock::Image {
            source: ImageSource {
                media_type: "image/png".into(),
                data: "iVBORw0KGgo=".into(),
            },
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["source"]["media_type"], "image/png");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        match back {
            ContentBlock::Image { source } => {
                assert_eq!(source.media_type, "image/png");
                assert_eq!(source.data, "iVBORw0KGgo=");
            }
            _ => panic!("Expected Image variant"),
        }
    }

    #[test]
    fn tool_result_content_image() {
        let content = ToolResultContent::Image {
            source: ImageSource {
                media_type: "image/png".into(),
                data: "iVBORw0KGgo=".into(),
            },
        };
        let json = serde_json::to_value(&content).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["source"]["media_type"], "image/png");
    }

    #[test]
    fn message_user_serde_and_uuid() {
        let msg = Message::User(UserMessage {
            uuid: "u-123".into(),
            content: vec![ContentBlock::Text { text: "Hello".into() }],
        });
        assert_eq!(msg.uuid(), "u-123");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "user");
        let back: Message = serde_json::from_value(json).unwrap();
        assert_eq!(back.uuid(), "u-123");
    }

    #[test]
    fn message_assistant_serde_and_uuid() {
        let msg = Message::Assistant(AssistantMessage {
            uuid: "a-456".into(),
            content: vec![ContentBlock::Text { text: "Hi".into() }],
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(10),
            }),
        });
        assert_eq!(msg.uuid(), "a-456");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "assistant");
        let back: Message = serde_json::from_value(json).unwrap();
        assert_eq!(back.uuid(), "a-456");
    }

    #[test]
    fn message_system_serde_and_uuid() {
        let msg = Message::System(SystemMessage {
            uuid: "s-789".into(),
            message: "Context compacted".into(),
        });
        assert_eq!(msg.uuid(), "s-789");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "system");
    }

    #[test]
    fn tool_result_is_error_defaults_false() {
        // When is_error is missing in JSON, it should default to false
        let json = json!({
            "type": "tool_result",
            "tool_use_id": "tu_1",
            "content": [{"type": "text", "text": "ok"}]
        });
        let block: ContentBlock = serde_json::from_value(json).unwrap();
        assert!(matches!(block, ContentBlock::ToolResult { is_error: false, .. }));
    }
}

