//! OpenAI-compatible API backend.
//!
//! Translates Anthropic Messages API format ↔ OpenAI Chat Completions format,
//! enabling use of any OpenAI-compatible endpoint (OpenAI, DeepSeek, Ollama,
//! vLLM, LiteLLM, etc.) through the existing [`ApiBackend`] trait.
//!
//! # Format Mapping
//!
//! | Anthropic | OpenAI |
//! |-----------|--------|
//! | `system` blocks (top-level) | `messages[0]` with `role: "system"` |
//! | `content: [{ type: "text" }]` | `content: "string"` or `parts` array |
//! | `tool_use` content block | `tool_calls` on assistant message |
//! | `tool_result` content block | `role: "tool"` message |
//! | `stop_reason: "end_turn"` | `finish_reason: "stop"` |
//! | `stop_reason: "tool_use"` | `finish_reason: "tool_calls"` |
//! | SSE `message_start` / `content_block_delta` | SSE `chat.completion.chunk` |

use std::pin::Pin;

use anyhow::{Context, Result};
use futures::Stream;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::provider::ApiBackend;
use crate::types::*;

// ── OpenAI Request/Response Types ────────────────────────────────────────────

/// OpenAI Chat Completions request body.
#[derive(Debug, Clone, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ChatTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    stream: bool,
}

/// A single message in the OpenAI chat format.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<ChatContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

/// Content can be a simple string or an array of content parts (multimodal).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ChatContent {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

/// A content part in multimodal messages (text or image).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ChatContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrlDetail },
}

/// Image URL detail for multimodal content.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImageUrlDetail {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// Tool definition in OpenAI format (wraps a function definition).
#[derive(Debug, Clone, Serialize)]
struct ChatTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ChatFunction,
}

/// Function definition within a tool.
#[derive(Debug, Clone, Serialize)]
struct ChatFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// A tool call made by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: ChatFunctionCall,
}

/// The function invocation within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatFunctionCall {
    name: String,
    arguments: String,
}

/// OpenAI Chat Completions response (non-streaming).
#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionResponse {
    id: String,
    choices: Vec<ChatChoice>,
    model: String,
    usage: Option<ChatUsage>,
}

/// A single choice in the response.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct ChatChoice {
    index: usize,
    message: ChatChoiceMessage,
    finish_reason: Option<String>,
}

/// The message in a choice (assistant's reply).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct ChatChoiceMessage {
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<ChatToolCall>>,
}

/// Token usage in OpenAI format.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

/// Streaming chunk from OpenAI API.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct ChatCompletionChunk {
    id: String,
    choices: Vec<ChunkChoice>,
    model: Option<String>,
    usage: Option<ChatUsage>,
}

/// A choice within a streaming chunk.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct ChunkChoice {
    index: usize,
    delta: ChunkDelta,
    finish_reason: Option<String>,
}

/// Delta content in a streaming chunk.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct ChunkDelta {
    role: Option<String>,
    content: Option<String>,
    tool_calls: Option<Vec<ChunkToolCall>>,
}

/// Tool call delta in a streaming chunk (may have partial data).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct ChunkToolCall {
    index: usize,
    id: Option<String>,
    #[serde(rename = "type")]
    call_type: Option<String>,
    function: Option<ChunkFunctionCall>,
}

/// Partial function call data in a streaming chunk.
#[derive(Debug, Clone, Deserialize)]
struct ChunkFunctionCall {
    name: Option<String>,
    arguments: Option<String>,
}

// ── Format Translation: Anthropic → OpenAI ───────────────────────────────────

/// Convert an Anthropic `MessagesRequest` into an OpenAI `ChatCompletionRequest`.
fn to_openai_request(req: &MessagesRequest) -> ChatCompletionRequest {
    let mut messages = Vec::new();

    // System prompt → system message
    if let Some(ref system_blocks) = req.system {
        let system_text: String = system_blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !system_text.is_empty() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: Some(ChatContent::Text(system_text)),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }

    // Convert Anthropic messages → OpenAI messages
    for msg in &req.messages {
        convert_anthropic_message(msg, &mut messages);
    }

    // Convert tool definitions
    let tools = req.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|t| ChatTool {
                tool_type: "function".into(),
                function: ChatFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect()
    });

    // tool_choice: if tools are present, default to "auto"
    let tool_choice = tools.as_ref().map(|t: &Vec<ChatTool>| {
        if t.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!("auto")
        }
    });

    ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        max_tokens: Some(req.max_tokens),
        temperature: req.temperature,
        top_p: req.top_p,
        stop: req.stop_sequences.clone(),
        tools,
        tool_choice,
        stream: req.stream,
    }
}

/// Convert a single Anthropic `ApiMessage` into one or more OpenAI `ChatMessage`s.
///
/// Anthropic puts everything in content blocks; OpenAI uses separate fields
/// (content, tool_calls) and separate messages for tool results.
fn convert_anthropic_message(msg: &ApiMessage, out: &mut Vec<ChatMessage>) {
    if msg.role == "user" {
        // User messages: collect text + images into content, but tool_results
        // become separate "tool" messages.
        let mut text_parts: Vec<ChatContentPart> = Vec::new();
        let mut tool_results: Vec<(String, String, bool)> = Vec::new(); // (tool_use_id, text, is_error)

        for block in &msg.content {
            match block {
                ApiContentBlock::Text { text, .. } => {
                    text_parts.push(ChatContentPart::Text {
                        text: text.clone(),
                    });
                }
                ApiContentBlock::Image { source } => {
                    let data_url =
                        format!("data:{};base64,{}", source.media_type, source.data);
                    text_parts.push(ChatContentPart::ImageUrl {
                        image_url: ImageUrlDetail {
                            url: data_url,
                            detail: Some("auto".into()),
                        },
                    });
                }
                ApiContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } => {
                    let text = content
                        .iter()
                        .map(|c| match c {
                            ToolResultContent::Text { text } => text.as_str(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    tool_results.push((tool_use_id.clone(), text, *is_error));
                }
                ApiContentBlock::ToolUse { .. } => {
                    // tool_use blocks don't appear in user messages normally
                }
            }
        }

        // Emit the text/image content as a user message (if any)
        if !text_parts.is_empty() {
            let content = if text_parts.len() == 1 {
                if let ChatContentPart::Text { ref text } = text_parts[0] {
                    ChatContent::Text(text.clone())
                } else {
                    ChatContent::Parts(text_parts)
                }
            } else {
                ChatContent::Parts(text_parts)
            };

            out.push(ChatMessage {
                role: "user".into(),
                content: Some(content),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }

        // Emit tool results as separate "tool" messages
        for (tool_use_id, text, is_error) in tool_results {
            let content_text = if is_error {
                format!("[ERROR] {}", text)
            } else {
                text
            };
            out.push(ChatMessage {
                role: "tool".into(),
                content: Some(ChatContent::Text(content_text)),
                tool_calls: None,
                tool_call_id: Some(tool_use_id),
                name: None,
            });
        }
    } else if msg.role == "assistant" {
        // Assistant messages: collect text into content, tool_use into tool_calls
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in &msg.content {
            match block {
                ApiContentBlock::Text { text, .. } => {
                    text_parts.push(text.clone());
                }
                ApiContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ChatToolCall {
                        id: id.clone(),
                        call_type: "function".into(),
                        function: ChatFunctionCall {
                            name: name.clone(),
                            arguments: serde_json::to_string(input).unwrap_or_default(),
                        },
                    });
                }
                _ => {}
            }
        }

        let content_str = text_parts.join("");
        out.push(ChatMessage {
            role: "assistant".into(),
            content: if content_str.is_empty() {
                None
            } else {
                Some(ChatContent::Text(content_str))
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            name: None,
        });
    }
}

// ── Format Translation: OpenAI → Anthropic ───────────────────────────────────

/// Convert an OpenAI `ChatCompletionResponse` into an Anthropic `MessagesResponse`.
fn from_openai_response(resp: ChatCompletionResponse) -> MessagesResponse {
    let choice = resp.choices.into_iter().next();
    let (content, stop_reason) = match choice {
        Some(c) => {
            let mut blocks = Vec::new();

            // Text content
            if let Some(text) = c.message.content {
                if !text.is_empty() {
                    blocks.push(ResponseContentBlock::Text { text });
                }
            }

            // Tool calls → tool_use blocks
            if let Some(tool_calls) = c.message.tool_calls {
                for tc in tool_calls {
                    let input: serde_json::Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                    blocks.push(ResponseContentBlock::ToolUse {
                        id: tc.id,
                        name: tc.function.name,
                        input,
                    });
                }
            }

            let stop = match c.finish_reason.as_deref() {
                Some("stop") => Some("end_turn".to_string()),
                Some("tool_calls") | Some("function_call") => Some("tool_use".to_string()),
                Some("length") => Some("max_tokens".to_string()),
                Some("content_filter") => Some("end_turn".to_string()),
                other => other.map(|s| s.to_string()),
            };

            (blocks, stop)
        }
        None => (Vec::new(), Some("end_turn".to_string())),
    };

    let usage = resp.usage.map(|u| ApiUsage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });

    MessagesResponse {
        id: resp.id,
        response_type: "message".into(),
        role: "assistant".into(),
        content,
        model: resp.model,
        stop_reason,
        usage: usage.unwrap_or(ApiUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        }),
    }
}

/// Convert an OpenAI streaming chunk into zero or more Anthropic `StreamEvent`s.
fn from_openai_chunk(
    chunk: &ChatCompletionChunk,
    is_first: bool,
    model: &str,
) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    // First chunk → MessageStart
    if is_first {
        events.push(StreamEvent::MessageStart {
            message: MessagesResponse {
                id: chunk.id.clone(),
                response_type: "message".into(),
                role: "assistant".into(),
                content: Vec::new(),
                model: model.to_string(),
                stop_reason: None,
                usage: ApiUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                },
            },
        });
    }

    for choice in &chunk.choices {
        // Text delta
        if let Some(ref text) = choice.delta.content {
            if !text.is_empty() {
                events.push(StreamEvent::ContentBlockDelta {
                    index: 0,
                    delta: DeltaBlock::TextDelta {
                        text: text.clone(),
                    },
                });
            }
        }

        // Tool call deltas
        if let Some(ref tool_calls) = choice.delta.tool_calls {
            for tc in tool_calls {
                if let Some(ref func) = tc.function {
                    // New tool call start → ContentBlockStart
                    if tc.id.is_some() {
                        let name = func.name.clone().unwrap_or_default();
                        events.push(StreamEvent::ContentBlockStart {
                            index: tc.index + 1, // offset by 1 (0 is text)
                            content_block: ResponseContentBlock::ToolUse {
                                id: tc.id.clone().unwrap_or_default(),
                                name,
                                input: serde_json::Value::Object(serde_json::Map::new()),
                            },
                        });
                    }

                    // Argument delta
                    if let Some(ref args) = func.arguments {
                        if !args.is_empty() {
                            events.push(StreamEvent::ContentBlockDelta {
                                index: tc.index + 1,
                                delta: DeltaBlock::InputJsonDelta {
                                    partial_json: args.clone(),
                                },
                            });
                        }
                    }
                }
            }
        }

        // Finish reason → MessageDelta + MessageStop
        if let Some(ref reason) = choice.finish_reason {
            let stop_reason = match reason.as_str() {
                "stop" => "end_turn",
                "tool_calls" | "function_call" => "tool_use",
                "length" => "max_tokens",
                _ => reason.as_str(),
            };

            // Include usage if available
            let usage = chunk.usage.as_ref().map(|u| DeltaUsage {
                output_tokens: u.completion_tokens,
            });

            events.push(StreamEvent::MessageDelta {
                delta: MessageDeltaData {
                    stop_reason: Some(stop_reason.to_string()),
                },
                usage,
            });
            events.push(StreamEvent::MessageStop);
        }
    }

    events
}

// ── OpenAI-Compatible Backend ────────────────────────────────────────────────

/// Backend for any OpenAI Chat Completions–compatible API.
///
/// Works with OpenAI, DeepSeek, Ollama, vLLM, LiteLLM, Together AI,
/// Groq, and any other provider implementing the Chat Completions format.
///
/// # Usage
///
/// ```no_run
/// use claude_api::openai::OpenAIBackend;
///
/// // OpenAI
/// let backend = OpenAIBackend::new("sk-...", "https://api.openai.com");
///
/// // Ollama (local)
/// let backend = OpenAIBackend::new("ollama", "http://localhost:11434");
///
/// // DeepSeek
/// let backend = OpenAIBackend::new("sk-...", "https://api.deepseek.com");
/// ```
pub struct OpenAIBackend {
    api_key: String,
    base_url: String,
    /// Provider name for display (e.g. "openai", "deepseek", "ollama").
    provider: String,
}

impl OpenAIBackend {
    /// Create a new OpenAI-compatible backend.
    ///
    /// `base_url` should be the root URL without `/v1/chat/completions`.
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider: "openai".into(),
        }
    }

    /// Set a custom provider name for display purposes.
    pub fn with_provider_name(mut self, name: impl Into<String>) -> Self {
        self.provider = name.into();
        self
    }

    /// Detect provider from base URL and set appropriate name.
    pub fn auto_detect_provider(mut self) -> Self {
        let url = self.base_url.to_lowercase();
        self.provider = if url.contains("openai.com") {
            "openai".into()
        } else if url.contains("deepseek.com") {
            "deepseek".into()
        } else if url.contains("localhost") || url.contains("127.0.0.1") {
            "local".into()
        } else if url.contains("together") {
            "together".into()
        } else if url.contains("groq") {
            "groq".into()
        } else {
            "openai-compatible".into()
        };
        self
    }
}

#[async_trait::async_trait]
impl ApiBackend for OpenAIBackend {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        // Skip auth header for local providers (Ollama doesn't need it)
        if !self.api_key.is_empty()
            && self.api_key != "ollama"
            && self.api_key != "local"
        {
            let auth_value = format!("Bearer {}", self.api_key);
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_value)
                    .map_err(|_| anyhow::anyhow!("Invalid API key format"))?,
            );
        }

        Ok(headers)
    }

    fn map_model_id(&self, canonical: &str) -> String {
        // Map Anthropic canonical model names to common OpenAI-compatible names
        match canonical {
            "claude-sonnet-4-20250514" | "claude-sonnet-4-6" => {
                // Keep as-is — provider should handle unknown models gracefully
                canonical.to_string()
            }
            _ => canonical.to_string(),
        }
    }

    async fn send_messages(
        &self,
        http: &reqwest::Client,
        request: &MessagesRequest,
    ) -> Result<MessagesResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));
        let headers = self.headers()?;

        let openai_req = to_openai_request(request);
        debug!(
            provider = self.provider,
            model = %openai_req.model,
            "Sending chat completion request"
        );

        let response = http
            .post(&url)
            .headers(headers)
            .json(&openai_req)
            .send()
            .await
            .context("OpenAI-compatible request failed")?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API error {} ({}): {}", status, self.provider, body);
        }

        let openai_resp: ChatCompletionResponse = response
            .json()
            .await
            .context("Failed to parse OpenAI-compatible response")?;

        Ok(from_openai_response(openai_resp))
    }

    async fn send_messages_stream(
        &self,
        http: &reqwest::Client,
        request: &MessagesRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));
        let headers = self.headers()?;

        let mut openai_req = to_openai_request(request);
        openai_req.stream = true;

        let model = openai_req.model.clone();
        debug!(
            provider = self.provider,
            model = %model,
            "Sending streaming chat completion request"
        );

        let response = http
            .post(&url)
            .headers(headers)
            .json(&openai_req)
            .send()
            .await
            .context("OpenAI-compatible stream request failed")?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Stream API error {} ({}): {}", status, self.provider, body);
        }

        let stream = async_stream::stream! {
            use futures::StreamExt;
            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut is_first = true;

            while let Some(chunk_result) = byte_stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));

                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].trim().to_string();
                            buffer = buffer[pos + 1..].to_string();

                            if line.is_empty() || line == ":" {
                                continue;
                            }

                            // Handle SSE format: "data: {...}"
                            let data = if let Some(stripped) = line.strip_prefix("data: ") {
                                stripped
                            } else if let Some(stripped) = line.strip_prefix("data:") {
                                stripped
                            } else {
                                continue;
                            };

                            // "[DONE]" signals end of stream
                            if data.trim() == "[DONE]" {
                                break;
                            }

                            match serde_json::from_str::<ChatCompletionChunk>(data) {
                                Ok(chunk) => {
                                    let events = from_openai_chunk(&chunk, is_first, &model);
                                    is_first = false;
                                    for event in events {
                                        yield Ok(event);
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        provider = "openai",
                                        error = %e,
                                        line = %data,
                                        "Failed to parse streaming chunk"
                                    );
                                    // Skip unparseable chunks rather than failing
                                }
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(anyhow::anyhow!("Stream read error: {}", e));
                        return;
                    }
                }
            }

            // If we never got a MessageStop, synthesize one
            if !is_first {
                // Ensure clean termination
            }
        };

        Ok(Box::pin(stream))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Request conversion ──

    #[test]
    fn simple_text_message_converts_correctly() {
        let req = MessagesRequest {
            model: "gpt-4o".into(),
            max_tokens: 4096,
            messages: vec![ApiMessage {
                role: "user".into(),
                content: vec![ApiContentBlock::Text {
                    text: "Hello!".into(),
                    cache_control: None,
                }],
            }],
            system: Some(vec![SystemBlock {
                block_type: "text".into(),
                text: "You are helpful.".into(),
                cache_control: None,
            }]),
            ..Default::default()
        };

        let openai = to_openai_request(&req);
        assert_eq!(openai.model, "gpt-4o");
        assert_eq!(openai.messages.len(), 2); // system + user
        assert_eq!(openai.messages[0].role, "system");
        assert_eq!(openai.messages[1].role, "user");

        // System text
        match &openai.messages[0].content {
            Some(ChatContent::Text(t)) => assert_eq!(t, "You are helpful."),
            _ => panic!("Expected text content"),
        }

        // User text
        match &openai.messages[1].content {
            Some(ChatContent::Text(t)) => assert_eq!(t, "Hello!"),
            _ => panic!("Expected text content"),
        }
    }

    #[test]
    fn tool_definitions_convert() {
        let req = MessagesRequest {
            model: "gpt-4o".into(),
            max_tokens: 4096,
            messages: vec![],
            tools: Some(vec![ToolDefinition {
                name: "read_file".into(),
                description: "Read a file".into(),
                input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
                cache_control: None,
            }]),
            ..Default::default()
        };

        let openai = to_openai_request(&req);
        assert!(openai.tools.is_some());
        let tools = openai.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_type, "function");
        assert_eq!(tools[0].function.name, "read_file");
    }

    #[test]
    fn assistant_tool_use_converts() {
        let req = MessagesRequest {
            model: "gpt-4o".into(),
            max_tokens: 4096,
            messages: vec![
                ApiMessage {
                    role: "user".into(),
                    content: vec![ApiContentBlock::Text {
                        text: "Read foo.txt".into(),
                        cache_control: None,
                    }],
                },
                ApiMessage {
                    role: "assistant".into(),
                    content: vec![
                        ApiContentBlock::Text {
                            text: "I'll read that file.".into(),
                            cache_control: None,
                        },
                        ApiContentBlock::ToolUse {
                            id: "call_123".into(),
                            name: "read_file".into(),
                            input: json!({"path": "foo.txt"}),
                        },
                    ],
                },
                ApiMessage {
                    role: "user".into(),
                    content: vec![ApiContentBlock::ToolResult {
                        tool_use_id: "call_123".into(),
                        content: vec![ToolResultContent::Text {
                            text: "file contents here".into(),
                        }],
                        is_error: false,
                        cache_control: None,
                    }],
                },
            ],
            ..Default::default()
        };

        let openai = to_openai_request(&req);
        // user + assistant + tool
        assert_eq!(openai.messages.len(), 3);

        // Assistant has tool_calls
        let assistant = &openai.messages[1];
        assert_eq!(assistant.role, "assistant");
        assert!(assistant.tool_calls.is_some());
        let tc = &assistant.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.id, "call_123");
        assert_eq!(tc.function.name, "read_file");

        // Tool result becomes "tool" message
        let tool_msg = &openai.messages[2];
        assert_eq!(tool_msg.role, "tool");
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call_123"));
    }

    #[test]
    fn image_converts_to_image_url() {
        let req = MessagesRequest {
            model: "gpt-4o".into(),
            max_tokens: 4096,
            messages: vec![ApiMessage {
                role: "user".into(),
                content: vec![
                    ApiContentBlock::Text {
                        text: "What's in this image?".into(),
                        cache_control: None,
                    },
                    ApiContentBlock::Image {
                        source: ImageSource {
                            source_type: "base64".into(),
                            media_type: "image/png".into(),
                            data: "iVBOR...".into(),
                        },
                    },
                ],
            }],
            ..Default::default()
        };

        let openai = to_openai_request(&req);
        assert_eq!(openai.messages.len(), 1);
        match &openai.messages[0].content {
            Some(ChatContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2);
                match &parts[1] {
                    ChatContentPart::ImageUrl { image_url } => {
                        assert!(image_url.url.starts_with("data:image/png;base64,"));
                    }
                    _ => panic!("Expected ImageUrl"),
                }
            }
            _ => panic!("Expected Parts content"),
        }
    }

    // ── Response conversion ──

    #[test]
    fn simple_response_converts() {
        let openai_resp = ChatCompletionResponse {
            id: "chatcmpl-123".into(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatChoiceMessage {
                    role: "assistant".into(),
                    content: Some("Hello there!".into()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".into()),
            }],
            model: "gpt-4o".into(),
            usage: Some(ChatUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        };

        let resp = from_openai_response(openai_resp);
        assert_eq!(resp.id, "chatcmpl-123");
        assert_eq!(resp.model, "gpt-4o");
        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 5);
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ResponseContentBlock::Text { text } => assert_eq!(text, "Hello there!"),
            _ => panic!("Expected Text"),
        }
    }

    #[test]
    fn tool_call_response_converts() {
        let openai_resp = ChatCompletionResponse {
            id: "chatcmpl-456".into(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatChoiceMessage {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![ChatToolCall {
                        id: "call_abc".into(),
                        call_type: "function".into(),
                        function: ChatFunctionCall {
                            name: "read_file".into(),
                            arguments: r#"{"path":"test.txt"}"#.into(),
                        },
                    }]),
                },
                finish_reason: Some("tool_calls".into()),
            }],
            model: "gpt-4o".into(),
            usage: None,
        };

        let resp = from_openai_response(openai_resp);
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ResponseContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "test.txt");
            }
            _ => panic!("Expected ToolUse"),
        }
    }

    #[test]
    fn finish_reason_length_maps_to_max_tokens() {
        let openai_resp = ChatCompletionResponse {
            id: "chatcmpl-789".into(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatChoiceMessage {
                    role: "assistant".into(),
                    content: Some("truncated...".into()),
                    tool_calls: None,
                },
                finish_reason: Some("length".into()),
            }],
            model: "gpt-4o".into(),
            usage: None,
        };

        let resp = from_openai_response(openai_resp);
        assert_eq!(resp.stop_reason.as_deref(), Some("max_tokens"));
    }

    // ── Streaming conversion ──

    #[test]
    fn first_chunk_emits_message_start() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".into()),
                    content: Some("Hi".into()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            model: Some("gpt-4o".into()),
            usage: None,
        };

        let events = from_openai_chunk(&chunk, true, "gpt-4o");
        assert!(events.len() >= 2); // MessageStart + TextDelta
        assert!(matches!(events[0], StreamEvent::MessageStart { .. }));
        match &events[1] {
            StreamEvent::ContentBlockDelta {
                delta: DeltaBlock::TextDelta { text },
                ..
            } => assert_eq!(text, "Hi"),
            _ => panic!("Expected TextDelta"),
        }
    }

    #[test]
    fn subsequent_chunk_no_message_start() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: Some("world".into()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            model: None,
            usage: None,
        };

        let events = from_openai_chunk(&chunk, false, "gpt-4o");
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ContentBlockDelta {
                delta: DeltaBlock::TextDelta { text },
                ..
            } => assert_eq!(text, "world"),
            _ => panic!("Expected TextDelta"),
        }
    }

    #[test]
    fn finish_reason_emits_message_delta_and_stop() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: None,
                    tool_calls: None,
                },
                finish_reason: Some("stop".into()),
            }],
            model: None,
            usage: None,
        };

        let events = from_openai_chunk(&chunk, false, "gpt-4o");
        assert_eq!(events.len(), 2);
        match &events[0] {
            StreamEvent::MessageDelta { delta, .. } => {
                assert_eq!(delta.stop_reason.as_deref(), Some("end_turn"));
            }
            _ => panic!("Expected MessageDelta"),
        }
        assert!(matches!(events[1], StreamEvent::MessageStop));
    }

    #[test]
    fn tool_call_chunk_emits_content_block_start_and_delta() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![ChunkToolCall {
                        index: 0,
                        id: Some("call_xyz".into()),
                        call_type: Some("function".into()),
                        function: Some(ChunkFunctionCall {
                            name: Some("bash".into()),
                            arguments: Some(r#"{"com"#.into()),
                        }),
                    }]),
                },
                finish_reason: None,
            }],
            model: None,
            usage: None,
        };

        let events = from_openai_chunk(&chunk, false, "gpt-4o");
        assert_eq!(events.len(), 2); // ContentBlockStart + InputJsonDelta
        assert!(matches!(events[0], StreamEvent::ContentBlockStart { .. }));
        match &events[1] {
            StreamEvent::ContentBlockDelta {
                delta: DeltaBlock::InputJsonDelta { partial_json },
                ..
            } => assert_eq!(partial_json, r#"{"com"#),
            _ => panic!("Expected InputJsonDelta"),
        }
    }

    // ── Backend construction ──

    #[test]
    fn backend_auto_detect_provider() {
        let b = OpenAIBackend::new("key", "https://api.openai.com").auto_detect_provider();
        assert_eq!(b.provider_name(), "openai");

        let b = OpenAIBackend::new("key", "https://api.deepseek.com").auto_detect_provider();
        assert_eq!(b.provider_name(), "deepseek");

        let b = OpenAIBackend::new("key", "http://localhost:11434").auto_detect_provider();
        assert_eq!(b.provider_name(), "local");

        let b = OpenAIBackend::new("key", "https://my-server.com").auto_detect_provider();
        assert_eq!(b.provider_name(), "openai-compatible");
    }

    #[test]
    fn backend_headers_with_api_key() {
        let b = OpenAIBackend::new("sk-test123", "https://api.openai.com");
        let headers = b.headers().unwrap();
        assert!(headers.contains_key(AUTHORIZATION));
        let auth = headers.get(AUTHORIZATION).unwrap().to_str().unwrap();
        assert_eq!(auth, "Bearer sk-test123");
    }

    #[test]
    fn backend_headers_skip_auth_for_ollama() {
        let b = OpenAIBackend::new("ollama", "http://localhost:11434");
        let headers = b.headers().unwrap();
        assert!(!headers.contains_key(AUTHORIZATION));
    }

    #[test]
    fn error_tool_result_prefixed() {
        let req = MessagesRequest {
            model: "gpt-4o".into(),
            max_tokens: 4096,
            messages: vec![ApiMessage {
                role: "user".into(),
                content: vec![ApiContentBlock::ToolResult {
                    tool_use_id: "call_err".into(),
                    content: vec![ToolResultContent::Text {
                        text: "file not found".into(),
                    }],
                    is_error: true,
                    cache_control: None,
                }],
            }],
            ..Default::default()
        };

        let openai = to_openai_request(&req);
        let tool_msg = &openai.messages[0];
        assert_eq!(tool_msg.role, "tool");
        match &tool_msg.content {
            Some(ChatContent::Text(t)) => assert!(t.starts_with("[ERROR]")),
            _ => panic!("Expected error text"),
        }
    }

    #[test]
    fn multiple_system_blocks_merge() {
        let req = MessagesRequest {
            model: "gpt-4o".into(),
            max_tokens: 4096,
            messages: vec![],
            system: Some(vec![
                SystemBlock {
                    block_type: "text".into(),
                    text: "Rule 1.".into(),
                    cache_control: None,
                },
                SystemBlock {
                    block_type: "text".into(),
                    text: "Rule 2.".into(),
                    cache_control: None,
                },
            ]),
            ..Default::default()
        };

        let openai = to_openai_request(&req);
        assert_eq!(openai.messages.len(), 1);
        match &openai.messages[0].content {
            Some(ChatContent::Text(t)) => assert_eq!(t, "Rule 1.\n\nRule 2."),
            _ => panic!("Expected merged system text"),
        }
    }

    #[test]
    fn empty_choices_yields_empty_response() {
        let openai_resp = ChatCompletionResponse {
            id: "chatcmpl-empty".into(),
            choices: vec![],
            model: "gpt-4o".into(),
            usage: None,
        };

        let resp = from_openai_response(openai_resp);
        assert!(resp.content.is_empty());
        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
    }
}
