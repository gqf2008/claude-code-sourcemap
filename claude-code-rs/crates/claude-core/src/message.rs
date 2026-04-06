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

/// A content block in a conversation message: text, tool call, tool result, or thinking.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
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
