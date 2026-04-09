//! Format translation between Anthropic Messages API and `OpenAI` Chat Completions API.

use crate::types::{MessagesRequest, ApiMessage, ApiContentBlock, ToolResultContent, MessagesResponse, ResponseContentBlock, ApiUsage, StreamEvent, DeltaBlock, DeltaUsage, MessageDeltaData};
use super::types::{ChatCompletionRequest, ChatMessage, ChatContent, ChatTool, ChatFunction, ChatContentPart, ImageUrlDetail, ChatToolCall, ChatFunctionCall, ChatCompletionResponse, ChatCompletionChunk};
// ── Format Translation: Anthropic → OpenAI ───────────────────────────────────

/// Convert an Anthropic `MessagesRequest` into an `OpenAI` `ChatCompletionRequest`.
pub fn to_openai_request(req: &MessagesRequest) -> ChatCompletionRequest {
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

/// Convert a single Anthropic `ApiMessage` into one or more `OpenAI` `ChatMessage`s.
///
/// Anthropic puts everything in content blocks; `OpenAI` uses separate fields
/// (content, `tool_calls`) and separate messages for tool results.
pub fn convert_anthropic_message(msg: &ApiMessage, out: &mut Vec<ChatMessage>) {
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
                format!("[ERROR] {text}")
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

/// Convert an `OpenAI` `ChatCompletionResponse` into an Anthropic `MessagesResponse`.
pub fn from_openai_response(resp: ChatCompletionResponse) -> MessagesResponse {
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
                Some("tool_calls" | "function_call") => Some("tool_use".to_string()),
                Some("length") => Some("max_tokens".to_string()),
                Some("content_filter") => Some("end_turn".to_string()),
                other => other.map(std::string::ToString::to_string),
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

/// Tracks streaming state across multiple `OpenAI` chunks.
///
/// `OpenAI` streams are stateless chunks, but Anthropic's event model requires
/// matching `ContentBlockStart` / `ContentBlockStop` pairs. This struct tracks
/// which content blocks have been started so we can emit the right events.
pub struct OpenAIStreamState {
    /// Whether `MessageStart` has been emitted.
    pub(super) message_started: bool,
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
    /// Model name for the `MessageStart` event.
    model: String,
}

impl OpenAIStreamState {
    pub(super) fn new(model: impl Into<String>) -> Self {
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

    /// Process one `OpenAI` streaming chunk, returning Anthropic `StreamEvent`s.
    pub(super) fn process_chunk(&mut self, chunk: &ChatCompletionChunk) -> Vec<StreamEvent> {
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
                tool_indices.sort_unstable();
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

    /// Synthesize closing events if the stream ended without a `finish_reason`.
    pub(super) fn finalize(&mut self) -> Vec<StreamEvent> {
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
        tool_indices.sort_unstable();
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