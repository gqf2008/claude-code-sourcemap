//! OpenAI Chat Completions format types.
//!
//! Internal struct definitions for serializing/deserializing OpenAI-format
//! requests and responses. All types are `pub(crate)` — only [super::OpenAIBackend]
//! is public.

use serde::{Deserialize, Serialize};
// ── OpenAI Request/Response Types ────────────────────────────────────────────

/// OpenAI Chat Completions request body.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChatCompletionRequest {
    pub(crate) model: String,
    pub(crate) messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tools: Option<Vec<ChatTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<serde_json::Value>,
    pub(crate) stream: bool,
}

/// A single message in the OpenAI chat format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatMessage {
    pub(crate) role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<ChatContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
}

/// Content can be a simple string or an array of content parts (multimodal).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ChatContent {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

/// A content part in multimodal messages (text or image).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ChatContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrlDetail },
}

/// Image URL detail for multimodal content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ImageUrlDetail {
    pub(crate) url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

/// Tool definition in OpenAI format (wraps a function definition).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChatTool {
    #[serde(rename = "type")]
    pub(crate) tool_type: String,
    pub(crate) function: ChatFunction,
}

/// Function definition within a tool.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChatFunction {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) parameters: serde_json::Value,
}

/// A tool call made by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatToolCall {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) call_type: String,
    pub(crate) function: ChatFunctionCall,
}

/// The function invocation within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatFunctionCall {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

/// OpenAI Chat Completions response (non-streaming).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ChatCompletionResponse {
    pub(crate) id: String,
    pub(crate) choices: Vec<ChatChoice>,
    pub(crate) model: String,
    pub(crate) usage: Option<ChatUsage>,
}

/// A single choice in the response.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ChatChoice {
    pub(crate) index: usize,
    pub(crate) message: ChatChoiceMessage,
    pub(crate) finish_reason: Option<String>,
}

/// The message in a choice (assistant's reply).
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ChatChoiceMessage {
    pub(crate) role: String,
    pub(crate) content: Option<String>,
    /// Reasoning/thinking content (DashScope/Qwen extension).
    pub(crate) reasoning_content: Option<String>,
    pub(crate) tool_calls: Option<Vec<ChatToolCall>>,
}

/// Token usage in OpenAI format.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ChatUsage {
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    #[serde(default)]
    pub(crate) total_tokens: u64,
}

/// Streaming chunk from OpenAI API.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ChatCompletionChunk {
    pub(crate) id: String,
    pub(crate) choices: Vec<ChunkChoice>,
    pub(crate) model: Option<String>,
    pub(crate) usage: Option<ChatUsage>,
}

/// A choice within a streaming chunk.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ChunkChoice {
    pub(crate) index: usize,
    pub(crate) delta: ChunkDelta,
    pub(crate) finish_reason: Option<String>,
}

/// Delta content in a streaming chunk.
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ChunkDelta {
    pub(crate) role: Option<String>,
    pub(crate) content: Option<String>,
    /// Reasoning/thinking content (DashScope/Qwen extension to OpenAI format).
    /// Maps to Anthropic's `ThinkingDelta` event.
    pub(crate) reasoning_content: Option<String>,
    pub(crate) tool_calls: Option<Vec<ChunkToolCall>>,
}

/// Tool call delta in a streaming chunk (may have partial data).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ChunkToolCall {
    pub(crate) index: usize,
    pub(crate) id: Option<String>,
    #[serde(rename = "type")]
    pub(crate) call_type: Option<String>,
    pub(crate) function: Option<ChunkFunctionCall>,
}

/// Partial function call data in a streaming chunk.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ChunkFunctionCall {
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<String>,
}
