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
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct ChatChoiceMessage {
    role: String,
    content: Option<String>,
    /// Reasoning/thinking content (DashScope/Qwen extension).
    reasoning_content: Option<String>,
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
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct ChunkDelta {
    role: Option<String>,
    content: Option<String>,
    /// Reasoning/thinking content (DashScope/Qwen extension to OpenAI format).
    /// Maps to Anthropic's `ThinkingDelta` event.
    reasoning_content: Option<String>,
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

            // Reasoning/thinking content (DashScope/Qwen extension)
            if let Some(reasoning) = c.message.reasoning_content {
                if !reasoning.is_empty() {
                    blocks.push(ResponseContentBlock::Thinking { thinking: reasoning });
                }
            }

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

/// Tracks streaming state across multiple OpenAI chunks.
///
/// OpenAI streams are stateless chunks, but Anthropic's event model requires
/// matching `ContentBlockStart` / `ContentBlockStop` pairs. This struct tracks
/// which content blocks have been started so we can emit the right events.
struct OpenAIStreamState {
    /// Whether `MessageStart` has been emitted.
    message_started: bool,
    /// Whether thinking `ContentBlockStart` (index 0) has been emitted.
    thinking_block_started: bool,
    /// Whether text `ContentBlockStart` has been emitted.
    text_block_started: bool,
    /// Index for the next content block (thinking takes 0 if present, text follows).
    next_block_index: usize,
    /// The index used for the text content block.
    text_block_index: usize,
    /// Set of tool call indices that have received `ContentBlockStart`.
    tool_blocks_started: std::collections::HashSet<usize>,
    /// Model name for the MessageStart event.
    model: String,
}

impl OpenAIStreamState {
    fn new(model: impl Into<String>) -> Self {
        Self {
            message_started: false,
            thinking_block_started: false,
            text_block_started: false,
            next_block_index: 0,
            text_block_index: 0,
            tool_blocks_started: std::collections::HashSet::new(),
            model: model.into(),
        }
    }

    /// Process one OpenAI streaming chunk, returning Anthropic `StreamEvent`s.
    fn process_chunk(&mut self, chunk: &ChatCompletionChunk) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        // First chunk → MessageStart
        if !self.message_started {
            self.message_started = true;
            events.push(StreamEvent::MessageStart {
                message: MessagesResponse {
                    id: chunk.id.clone(),
                    response_type: "message".into(),
                    role: "assistant".into(),
                    content: Vec::new(),
                    model: self.model.clone(),
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
            // Reasoning/thinking delta (DashScope/Qwen extension)
            if let Some(ref reasoning) = choice.delta.reasoning_content {
                if !reasoning.is_empty() {
                    if !self.thinking_block_started {
                        self.thinking_block_started = true;
                        let idx = self.next_block_index;
                        self.next_block_index += 1;
                        events.push(StreamEvent::ContentBlockStart {
                            index: idx,
                            content_block: ResponseContentBlock::Thinking {
                                thinking: String::new(),
                            },
                        });
                    }
                    events.push(StreamEvent::ContentBlockDelta {
                        index: 0, // thinking is always block 0
                        delta: DeltaBlock::ThinkingDelta {
                            thinking: reasoning.clone(),
                        },
                    });
                }
            }

            // Text delta — ensure ContentBlockStart is emitted first
            if let Some(ref text) = choice.delta.content {
                if !text.is_empty() {
                    // Close thinking block before starting text block
                    if self.thinking_block_started && !self.text_block_started {
                        events.push(StreamEvent::ContentBlockStop { index: 0 });
                    }
                    if !self.text_block_started {
                        self.text_block_started = true;
                        self.text_block_index = self.next_block_index;
                        self.next_block_index += 1;
                        events.push(StreamEvent::ContentBlockStart {
                            index: self.text_block_index,
                            content_block: ResponseContentBlock::Text {
                                text: String::new(),
                            },
                        });
                    }
                    events.push(StreamEvent::ContentBlockDelta {
                        index: self.text_block_index,
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
                        let block_index = self.next_block_index + tc.index;

                        // New tool call start → ContentBlockStart (only once per index)
                        if tc.id.is_some() && !self.tool_blocks_started.contains(&tc.index) {
                            self.tool_blocks_started.insert(tc.index);
                            let name = func.name.clone().unwrap_or_default();
                            events.push(StreamEvent::ContentBlockStart {
                                index: block_index,
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
                                    index: block_index,
                                    delta: DeltaBlock::InputJsonDelta {
                                        partial_json: args.clone(),
                                    },
                                });
                            }
                        }
                    }
                }
            }

            // Finish reason → close open blocks, then MessageDelta + MessageStop
            if let Some(ref reason) = choice.finish_reason {
                // Close thinking block if still open (not already closed by text start)
                if self.thinking_block_started && !self.text_block_started {
                    events.push(StreamEvent::ContentBlockStop { index: 0 });
                }
                // Emit ContentBlockStop for text block
                if self.text_block_started {
                    events.push(StreamEvent::ContentBlockStop { index: self.text_block_index });
                }
                let mut tool_indices: Vec<usize> =
                    self.tool_blocks_started.iter().copied().collect();
                tool_indices.sort();
                for idx in tool_indices {
                    events.push(StreamEvent::ContentBlockStop { index: self.next_block_index + idx });
                }

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

    /// Synthesize closing events if the stream ended without a finish_reason.
    fn finalize(&mut self) -> Vec<StreamEvent> {
        if !self.message_started {
            return Vec::new();
        }

        let mut events = Vec::new();

        // Close any open content blocks
        if self.thinking_block_started && !self.text_block_started {
            events.push(StreamEvent::ContentBlockStop { index: 0 });
            self.thinking_block_started = false;
        }
        if self.text_block_started {
            events.push(StreamEvent::ContentBlockStop { index: self.text_block_index });
            self.text_block_started = false;
        }
        let mut tool_indices: Vec<usize> =
            self.tool_blocks_started.iter().copied().collect();
        tool_indices.sort();
        for idx in tool_indices {
            events.push(StreamEvent::ContentBlockStop { index: self.next_block_index + idx });
        }
        self.tool_blocks_started.clear();

        events.push(StreamEvent::MessageDelta {
            delta: MessageDeltaData {
                stop_reason: Some("end_turn".to_string()),
            },
            usage: None,
        });
        events.push(StreamEvent::MessageStop);

        events
    }
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
    /// If a URL ending in `/v1` is provided, the suffix is automatically stripped
    /// to avoid double-prefixing (e.g. `https://example.com/v1` → `https://example.com`).
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let mut url = base_url.into();
        // Strip trailing /v1 to prevent double-path: /v1/v1/chat/completions
        let trimmed = url.trim_end_matches('/');
        if let Some(prefix) = trimmed.strip_suffix("/v1") {
            url = prefix.to_string();
        }
        Self {
            api_key: api_key.into(),
            base_url: url,
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
        // Map Anthropic canonical model names to common defaults per provider.
        // Users should override with --model for specific provider models.
        if canonical.starts_with("claude-") {
            warn!(
                provider = %self.provider,
                model = canonical,
                "Anthropic model name passed to {} provider; override with --model",
                self.provider
            );
        }
        canonical.to_string()
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
            let mut state = OpenAIStreamState::new(&model);
            let mut got_done = false;

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
                                got_done = true;
                                break;
                            }

                            match serde_json::from_str::<ChatCompletionChunk>(data) {
                                Ok(chunk) => {
                                    let events = state.process_chunk(&chunk);
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

            // If the stream ended without a proper finish_reason, synthesize closing events
            if state.message_started && !got_done {
                let closing = state.finalize();
                for event in closing {
                    yield Ok(event);
                }
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

    // ── Backend construction ──

    #[test]
    fn base_url_strips_trailing_v1() {
        let b = OpenAIBackend::new("key", "https://example.com/v1");
        assert_eq!(b.base_url(), "https://example.com");

        let b2 = OpenAIBackend::new("key", "https://example.com/v1/");
        assert_eq!(b2.base_url(), "https://example.com");

        // Should NOT strip /v1 from the middle
        let b3 = OpenAIBackend::new("key", "https://example.com/v1beta");
        assert_eq!(b3.base_url(), "https://example.com/v1beta");

        // No /v1 — unchanged
        let b4 = OpenAIBackend::new("key", "https://example.com");
        assert_eq!(b4.base_url(), "https://example.com");
    }

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
                    ..Default::default()
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
                    ..Default::default()
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
                    ..Default::default()
                },
                finish_reason: Some("length".into()),
            }],
            model: "gpt-4o".into(),
            usage: None,
        };

        let resp = from_openai_response(openai_resp);
        assert_eq!(resp.stop_reason.as_deref(), Some("max_tokens"));
    }

    // ── Streaming conversion (OpenAIStreamState) ──

    #[test]
    fn first_chunk_emits_message_start_and_content_block_start() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".into()),
                    content: Some("Hi".into()),
                    tool_calls: None,
                ..Default::default() },
                finish_reason: None,
            }],
            model: Some("gpt-4o".into()),
            usage: None,
        };

        let mut state = OpenAIStreamState::new("gpt-4o");
        let events = state.process_chunk(&chunk);
        // MessageStart + ContentBlockStart(text) + TextDelta
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], StreamEvent::MessageStart { .. }));
        assert!(matches!(events[1], StreamEvent::ContentBlockStart { index: 0, .. }));
        match &events[2] {
            StreamEvent::ContentBlockDelta {
                delta: DeltaBlock::TextDelta { text },
                ..
            } => assert_eq!(text, "Hi"),
            _ => panic!("Expected TextDelta"),
        }
    }

    #[test]
    fn subsequent_chunk_no_duplicate_block_start() {
        let chunk1 = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".into()),
                    content: Some("Hello".into()),
                    tool_calls: None,
                ..Default::default() },
                finish_reason: None,
            }],
            model: Some("gpt-4o".into()),
            usage: None,
        };

        let chunk2 = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: Some(" world".into()),
                    tool_calls: None,
                ..Default::default() },
                finish_reason: None,
            }],
            model: None,
            usage: None,
        };

        let mut state = OpenAIStreamState::new("gpt-4o");
        let _ = state.process_chunk(&chunk1);
        let events = state.process_chunk(&chunk2);
        // Second chunk: only TextDelta (no MessageStart or ContentBlockStart)
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ContentBlockDelta {
                delta: DeltaBlock::TextDelta { text },
                ..
            } => assert_eq!(text, " world"),
            _ => panic!("Expected TextDelta"),
        }
    }

    #[test]
    fn finish_reason_emits_block_stop_and_message_stop() {
        let chunk1 = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".into()),
                    content: Some("Hi".into()),
                    tool_calls: None,
                ..Default::default() },
                finish_reason: None,
            }],
            model: Some("gpt-4o".into()),
            usage: None,
        };

        let chunk2 = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: None,
                    tool_calls: None,
                ..Default::default() },
                finish_reason: Some("stop".into()),
            }],
            model: None,
            usage: None,
        };

        let mut state = OpenAIStreamState::new("gpt-4o");
        let _ = state.process_chunk(&chunk1);
        let events = state.process_chunk(&chunk2);
        // ContentBlockStop(0) + MessageDelta + MessageStop
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], StreamEvent::ContentBlockStop { index: 0 }));
        match &events[1] {
            StreamEvent::MessageDelta { delta, .. } => {
                assert_eq!(delta.stop_reason.as_deref(), Some("end_turn"));
            }
            _ => panic!("Expected MessageDelta"),
        }
        assert!(matches!(events[2], StreamEvent::MessageStop));
    }

    #[test]
    fn tool_call_stream_emits_start_delta_stop() {
        let chunk1 = ChatCompletionChunk {
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
                    ..Default::default()
                },
                finish_reason: None,
            }],
            model: None,
            usage: None,
        };

        let chunk2 = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: None,
                    tool_calls: None,
                ..Default::default() },
                finish_reason: Some("tool_calls".into()),
            }],
            model: None,
            usage: None,
        };

        let mut state = OpenAIStreamState::new("gpt-4o");
        let events1 = state.process_chunk(&chunk1);
        assert_eq!(events1.len(), 3); // MessageStart + ContentBlockStart + InputJsonDelta
        assert!(matches!(events1[0], StreamEvent::MessageStart { .. }));
        assert!(matches!(events1[1], StreamEvent::ContentBlockStart { index: 0, .. }));
        match &events1[2] {
            StreamEvent::ContentBlockDelta {
                delta: DeltaBlock::InputJsonDelta { partial_json },
                ..
            } => assert_eq!(partial_json, r#"{"com"#),
            _ => panic!("Expected InputJsonDelta"),
        }

        let events2 = state.process_chunk(&chunk2);
        // ContentBlockStop(0) + MessageDelta + MessageStop
        assert_eq!(events2.len(), 3);
        assert!(matches!(events2[0], StreamEvent::ContentBlockStop { index: 0 }));
        match &events2[1] {
            StreamEvent::MessageDelta { delta, .. } => {
                assert_eq!(delta.stop_reason.as_deref(), Some("tool_use"));
            }
            _ => panic!("Expected MessageDelta"),
        }
        assert!(matches!(events2[2], StreamEvent::MessageStop));
    }

    #[test]
    fn finalize_synthesizes_closing_events() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-stream".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".into()),
                    content: Some("partial".into()),
                    tool_calls: None,
                ..Default::default() },
                finish_reason: None,
            }],
            model: Some("gpt-4o".into()),
            usage: None,
        };

        let mut state = OpenAIStreamState::new("gpt-4o");
        let _ = state.process_chunk(&chunk);

        // Stream ends abruptly (no finish_reason)
        let closing = state.finalize();
        // ContentBlockStop(0) + MessageDelta(end_turn) + MessageStop
        assert_eq!(closing.len(), 3);
        assert!(matches!(closing[0], StreamEvent::ContentBlockStop { index: 0 }));
        assert!(matches!(closing[1], StreamEvent::MessageDelta { .. }));
        assert!(matches!(closing[2], StreamEvent::MessageStop));
    }

    #[test]
    fn mixed_text_and_tools_stream() {
        let mut state = OpenAIStreamState::new("gpt-4o");

        // Chunk 1: text content
        let c1 = ChatCompletionChunk {
            id: "chatcmpl-mix".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".into()),
                    content: Some("Let me read that.".into()),
                    tool_calls: None,
                ..Default::default() },
                finish_reason: None,
            }],
            model: Some("gpt-4o".into()),
            usage: None,
        };
        let events = state.process_chunk(&c1);
        assert_eq!(events.len(), 3); // MessageStart + ContentBlockStart + TextDelta

        // Chunk 2: tool call start
        let c2 = ChatCompletionChunk {
            id: "chatcmpl-mix".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![ChunkToolCall {
                        index: 0,
                        id: Some("call_001".into()),
                        call_type: Some("function".into()),
                        function: Some(ChunkFunctionCall {
                            name: Some("read_file".into()),
                            arguments: Some(r#"{"path":"test.txt"}"#.into()),
                        }),
                    }]),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            model: None,
            usage: None,
        };        let events = state.process_chunk(&c2);
        assert_eq!(events.len(), 2); // ContentBlockStart(1) + InputJsonDelta

        // Chunk 3: finish
        let c3 = ChatCompletionChunk {
            id: "chatcmpl-mix".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: None,
                    tool_calls: None,
                ..Default::default() },
                finish_reason: Some("tool_calls".into()),
            }],
            model: None,
            usage: None,
        };
        let events = state.process_chunk(&c3);
        // ContentBlockStop(0) + ContentBlockStop(1) + MessageDelta + MessageStop
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], StreamEvent::ContentBlockStop { index: 0 }));
        assert!(matches!(events[1], StreamEvent::ContentBlockStop { index: 1 }));
        assert!(matches!(events[2], StreamEvent::MessageDelta { .. }));
        assert!(matches!(events[3], StreamEvent::MessageStop));
    }

    #[test]
    fn reasoning_content_emits_thinking_events() {
        // Chunk 1: reasoning/thinking delta
        let chunk1 = ChatCompletionChunk {
            id: "chatcmpl-reason".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".into()),
                    reasoning_content: Some("Let me think...".into()),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            model: Some("qwen3.6-plus".into()),
            usage: None,
        };

        let mut state = OpenAIStreamState::new("qwen3.6-plus");
        let events = state.process_chunk(&chunk1);
        // MessageStart + ContentBlockStart(thinking) + ThinkingDelta
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], StreamEvent::MessageStart { .. }));
        assert!(matches!(events[1], StreamEvent::ContentBlockStart {
            index: 0,
            content_block: ResponseContentBlock::Thinking { .. },
        }));
        match &events[2] {
            StreamEvent::ContentBlockDelta {
                index: 0,
                delta: DeltaBlock::ThinkingDelta { thinking },
            } => assert_eq!(thinking, "Let me think..."),
            _ => panic!("Expected ThinkingDelta"),
        }

        // Chunk 2: text content (should close thinking, start text at index 1)
        let chunk2 = ChatCompletionChunk {
            id: "chatcmpl-reason".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    content: Some("The answer is 42.".into()),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            model: None,
            usage: None,
        };

        let events = state.process_chunk(&chunk2);
        // ContentBlockStop(0) + ContentBlockStart(1, text) + TextDelta
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], StreamEvent::ContentBlockStop { index: 0 }));
        assert!(matches!(events[1], StreamEvent::ContentBlockStart {
            index: 1,
            content_block: ResponseContentBlock::Text { .. },
        }));
        match &events[2] {
            StreamEvent::ContentBlockDelta {
                index: 1,
                delta: DeltaBlock::TextDelta { text },
            } => assert_eq!(text, "The answer is 42."),
            _ => panic!("Expected TextDelta at index 1"),
        }

        // Chunk 3: finish
        let chunk3 = ChatCompletionChunk {
            id: "chatcmpl-reason".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta::default(),
                finish_reason: Some("stop".into()),
            }],
            model: None,
            usage: None,
        };

        let events = state.process_chunk(&chunk3);
        // ContentBlockStop(1) + MessageDelta + MessageStop
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], StreamEvent::ContentBlockStop { index: 1 }));
        assert!(matches!(events[1], StreamEvent::MessageDelta { .. }));
        assert!(matches!(events[2], StreamEvent::MessageStop));
    }

    #[test]
    fn from_openai_response_with_reasoning_content() {
        let openai_resp = ChatCompletionResponse {
            id: "chatcmpl-reason".into(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatChoiceMessage {
                    role: "assistant".into(),
                    content: Some("42".into()),
                    reasoning_content: Some("I need to calculate...".into()),
                    ..Default::default()
                },
                finish_reason: Some("stop".into()),
            }],
            model: "qwen3.6-plus".into(),
            usage: None,
        };

        let resp = from_openai_response(openai_resp);
        assert_eq!(resp.content.len(), 2);
        // Thinking block comes first
        match &resp.content[0] {
            ResponseContentBlock::Thinking { thinking } => {
                assert_eq!(thinking, "I need to calculate...");
            }
            _ => panic!("Expected Thinking block"),
        }
        // Then text block
        match &resp.content[1] {
            ResponseContentBlock::Text { text } => {
                assert_eq!(text, "42");
            }
            _ => panic!("Expected Text block"),
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
