use super::types::*;
use super::translate::*;
use super::*;
use crate::types::*;
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