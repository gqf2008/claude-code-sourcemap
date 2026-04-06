//! End-to-end integration tests for query_stream using MockBackend.
//!
//! These tests verify the full agent loop: API call → stream processing →
//! tool execution → multi-turn → error recovery → budget enforcement.

use std::sync::Arc;

use claude_api::provider::MockBackend;
use claude_api::types::{ApiUsage, MessagesResponse, ResponseContentBlock};
use claude_core::message::{ContentBlock, Message, StopReason, UserMessage};

use super::{query_stream, AgentEvent, QueryConfig};

// ── Test helpers ─────────────────────────────────────────────────────────────

fn mock_text_response(text: &str) -> MessagesResponse {
    MessagesResponse {
        id: "msg_test".into(),
        response_type: "message".into(),
        role: "assistant".into(),
        content: vec![ResponseContentBlock::Text { text: text.into() }],
        model: "claude-sonnet-4-20250514".into(),
        stop_reason: Some("end_turn".into()),
        usage: ApiUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    }
}

fn mock_tool_response(tool_id: &str, tool_name: &str, input: serde_json::Value) -> MessagesResponse {
    MessagesResponse {
        id: "msg_tool".into(),
        response_type: "message".into(),
        role: "assistant".into(),
        content: vec![ResponseContentBlock::ToolUse {
            id: tool_id.into(),
            name: tool_name.into(),
            input,
        }],
        model: "claude-sonnet-4-20250514".into(),
        stop_reason: Some("tool_use".into()),
        usage: ApiUsage {
            input_tokens: 200,
            output_tokens: 100,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    }
}

fn make_stream_events(response: MessagesResponse) -> Vec<anyhow::Result<claude_api::types::StreamEvent>> {
    let usage = response.usage.clone();
    let content = response.content.clone();
    let stop_reason = response.stop_reason.clone();

    let mut events = Vec::new();
    events.push(Ok(claude_api::types::StreamEvent::MessageStart {
        message: response,
    }));

    for (idx, block) in content.iter().enumerate() {
        match block {
            ResponseContentBlock::Text { text } => {
                events.push(Ok(claude_api::types::StreamEvent::ContentBlockStart {
                    index: idx,
                    content_block: ResponseContentBlock::Text { text: String::new() },
                }));
                events.push(Ok(claude_api::types::StreamEvent::ContentBlockDelta {
                    index: idx,
                    delta: claude_api::types::DeltaBlock::TextDelta { text: text.clone() },
                }));
                events.push(Ok(claude_api::types::StreamEvent::ContentBlockStop { index: idx }));
            }
            ResponseContentBlock::ToolUse { id, name, input } => {
                events.push(Ok(claude_api::types::StreamEvent::ContentBlockStart {
                    index: idx,
                    content_block: ResponseContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::Value::Object(Default::default()),
                    },
                }));
                events.push(Ok(claude_api::types::StreamEvent::ContentBlockDelta {
                    index: idx,
                    delta: claude_api::types::DeltaBlock::InputJsonDelta {
                        partial_json: serde_json::to_string(input).unwrap(),
                    },
                }));
                events.push(Ok(claude_api::types::StreamEvent::ContentBlockStop { index: idx }));
            }
            ResponseContentBlock::Thinking { thinking } => {
                events.push(Ok(claude_api::types::StreamEvent::ContentBlockStart {
                    index: idx,
                    content_block: ResponseContentBlock::Thinking { thinking: String::new() },
                }));
                events.push(Ok(claude_api::types::StreamEvent::ContentBlockDelta {
                    index: idx,
                    delta: claude_api::types::DeltaBlock::ThinkingDelta { thinking: thinking.clone() },
                }));
                events.push(Ok(claude_api::types::StreamEvent::ContentBlockStop { index: idx }));
            }
        }
    }

    events.push(Ok(claude_api::types::StreamEvent::MessageDelta {
        delta: claude_api::types::MessageDeltaData {
            stop_reason: stop_reason.or(Some("end_turn".into())),
        },
        usage: Some(claude_api::types::DeltaUsage { output_tokens: usage.output_tokens }),
    }));

    events
}

/// Common test setup: client + executor + state + tool_context + hooks
fn test_setup(mock: MockBackend) -> (
    Arc<claude_api::client::AnthropicClient>,
    Arc<crate::executor::ToolExecutor>,
    crate::state::SharedState,
    claude_core::tool::ToolContext,
    Arc<crate::hooks::HookRegistry>,
) {
    let client = Arc::new(
        claude_api::client::AnthropicClient::new("test-key")
            .with_backend(Box::new(mock)),
    );
    let registry = Arc::new(claude_tools::ToolRegistry::new());
    let perm = Arc::new(crate::permissions::PermissionChecker::new(
        claude_core::permissions::PermissionMode::Default,
        vec![],
    ));
    let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
    let state = crate::state::new_shared_state();
    let tool_context = claude_core::tool::ToolContext {
        cwd: std::env::temp_dir(),
        abort_signal: claude_core::tool::AbortSignal::new(),
        permission_mode: claude_core::permissions::PermissionMode::Default,
        messages: vec![],
    };
    let hooks = Arc::new(crate::hooks::HookRegistry::new());
    (client, executor, state, tool_context, hooks)
}

fn user_msg(text: &str) -> Vec<Message> {
    vec![Message::User(UserMessage {
        uuid: "u1".into(),
        content: vec![ContentBlock::Text { text: text.into() }],
    })]
}

async fn collect_events(
    client: Arc<claude_api::client::AnthropicClient>,
    executor: Arc<crate::executor::ToolExecutor>,
    state: crate::state::SharedState,
    tool_context: claude_core::tool::ToolContext,
    config: QueryConfig,
    messages: Vec<Message>,
    hooks: Arc<crate::hooks::HookRegistry>,
) -> Vec<AgentEvent> {
    let stream = query_stream(client, executor, state, tool_context, config, messages, vec![], hooks);
    tokio_stream::StreamExt::collect(stream).await
}

// ── E2E Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn e2e_single_turn_text_response() {
    let response = mock_text_response("Hello! How can I help?");
    let mock = MockBackend::new().with_stream_events(make_stream_events(response));
    let (client, executor, state, tool_context, hooks) = test_setup(mock);

    let config = QueryConfig {
        system_prompt: "You are helpful.".into(),
        max_turns: 5,
        ..QueryConfig::default()
    };

    let events = collect_events(client, executor, state.clone(), tool_context, config, user_msg("Hi"), hooks).await;

    let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Hello! How can I help?"));
    let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { stop_reason: StopReason::EndTurn }));
    let has_usage = events.iter().any(|e| matches!(e, AgentEvent::UsageUpdate(_)));
    assert!(has_text, "expected text delta");
    assert!(has_complete, "expected turn complete");
    assert!(has_usage, "expected usage update");

    let s = state.read().await;
    assert!(s.total_input_tokens > 0);
    assert!(s.total_output_tokens > 0);
    assert_eq!(s.messages.len(), 2);
}

#[tokio::test]
async fn e2e_max_turns_enforced() {
    let response = mock_text_response("Done.");
    let mock = MockBackend::new().with_stream_events(make_stream_events(response));
    let (client, executor, state, tool_context, hooks) = test_setup(mock);

    let config = QueryConfig {
        max_turns: 1,
        ..QueryConfig::default()
    };

    let events = collect_events(client, executor, state, tool_context, config, user_msg("Hi"), hooks).await;
    let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { .. }));
    assert!(has_complete, "should complete within max_turns");
}

#[tokio::test]
async fn e2e_abort_signal_stops_loop() {
    let response = mock_text_response("Should not appear");
    let mock = MockBackend::new().with_stream_events(make_stream_events(response));
    let (client, executor, state, tool_context, hooks) = test_setup(mock);

    tool_context.abort_signal.abort();
    let config = QueryConfig::default();

    let events = collect_events(client, executor, state, tool_context, config, user_msg("Hi"), hooks).await;

    let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(_)));
    let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { .. }));
    assert!(!has_text, "should not produce text when aborted");
    assert!(has_complete, "should produce TurnComplete on abort");
}

#[tokio::test]
async fn e2e_api_error_propagated() {
    let mock = MockBackend::new().with_stream_events(vec![
        Err(anyhow::anyhow!("authentication failed")),
    ]);
    let (client, executor, state, tool_context, hooks) = test_setup(mock);
    let config = QueryConfig::default();

    let events = collect_events(client, executor, state, tool_context, config, user_msg("Hi"), hooks).await;
    let has_error = events.iter().any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("Stream error")));
    assert!(has_error, "expected stream error event, got: {:?}", events);
}

#[tokio::test]
async fn e2e_multi_turn_tool_execution() {
    let tool_response = mock_tool_response("tool_1", "echo_tool", serde_json::json!({"text": "hello"}));
    let text_response = mock_text_response("Done with tool execution.");

    let mock = MockBackend::new()
        .with_stream_events(make_stream_events(tool_response))
        .with_stream_events(make_stream_events(text_response));
    let (client, executor, state, tool_context, hooks) = test_setup(mock);

    let config = QueryConfig {
        max_turns: 5,
        ..QueryConfig::default()
    };

    let events = collect_events(client, executor, state.clone(), tool_context, config, user_msg("Use the echo tool"), hooks).await;

    let has_tool_start = events.iter().any(|e| matches!(e, AgentEvent::ToolUseStart { name, .. } if name == "echo_tool"));
    let has_tool_ready = events.iter().any(|e| matches!(e, AgentEvent::ToolUseReady { name, .. } if name == "echo_tool"));
    let has_tool_result = events.iter().any(|e| matches!(e, AgentEvent::ToolResult { .. }));
    let has_final_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Done with tool execution."));
    let assistant_count = events.iter().filter(|e| matches!(e, AgentEvent::AssistantMessage(_))).count();

    assert!(has_tool_start, "expected ToolUseStart");
    assert!(has_tool_ready, "expected ToolUseReady");
    assert!(has_tool_result, "expected ToolResult");
    assert!(has_final_text, "expected final text delta");
    assert_eq!(assistant_count, 2, "expected 2 assistant messages");

    let s = state.read().await;
    assert_eq!(s.turn_count, 1);
    assert_eq!(s.messages.len(), 4);
}

#[tokio::test]
async fn e2e_token_budget_enforced() {
    let response = mock_text_response("Hello!");
    let mock = MockBackend::new().with_stream_events(make_stream_events(response));
    let (client, executor, state, tool_context, hooks) = test_setup(mock);

    let config = QueryConfig {
        token_budget: 1,
        ..QueryConfig::default()
    };

    let events = collect_events(client, executor, state, tool_context, config, user_msg("Hi"), hooks).await;

    let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Hello!"));
    let has_budget_error = events.iter().any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("Token budget exceeded")));
    assert!(has_text, "expected text delta before budget stop");
    assert!(has_budget_error, "expected token budget exceeded error, got: {:?}", events);
}

#[tokio::test]
async fn e2e_max_tokens_recovery_escalation() {
    let truncated = MessagesResponse {
        id: "msg_1".into(),
        response_type: "message".into(),
        role: "assistant".into(),
        content: vec![ResponseContentBlock::Text { text: "Partial output...".into() }],
        model: "claude-sonnet-4-20250514".into(),
        stop_reason: Some("max_tokens".into()),
        usage: ApiUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    };
    let complete = mock_text_response("...complete!");

    let mock = MockBackend::new()
        .with_stream_events(make_stream_events(truncated))
        .with_stream_events(make_stream_events(complete));
    let (client, executor, state, tool_context, hooks) = test_setup(mock);

    let config = QueryConfig {
        max_turns: 10,
        ..QueryConfig::default()
    };

    let events = collect_events(client, executor, state.clone(), tool_context, config, user_msg("Write a long essay"), hooks).await;

    let has_partial = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Partial output..."));
    let has_escalation = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("escalating max_tokens")));
    let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "...complete!"));
    let has_turn_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { stop_reason: StopReason::EndTurn }));

    assert!(has_partial, "expected partial text delta");
    assert!(has_escalation, "expected max_tokens escalation message");
    assert!(has_complete, "expected complete text delta");
    assert!(has_turn_complete, "expected TurnComplete at end");
}

#[tokio::test]
async fn e2e_retry_on_overloaded_error() {
    let response = mock_text_response("Recovered!");
    let mock = MockBackend::new()
        .with_stream_error("overloaded: server is busy")
        .with_stream_events(make_stream_events(response));
    let (client, executor, state, tool_context, hooks) = test_setup(mock);

    let config = QueryConfig {
        max_turns: 5,
        ..QueryConfig::default()
    };

    let events = collect_events(client, executor, state, tool_context, config, user_msg("Hi"), hooks).await;

    let has_retry = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("Retrying")));
    let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Recovered!"));
    let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { stop_reason: StopReason::EndTurn }));

    assert!(has_retry, "expected retry message, got: {:?}", events);
    assert!(has_text, "expected recovered text");
    assert!(has_complete, "expected TurnComplete");
}

#[tokio::test]
async fn e2e_fatal_error_stops_immediately() {
    let mock = MockBackend::new().with_stream_error("invalid_api_key: unauthorized");
    let (client, executor, state, tool_context, hooks) = test_setup(mock);
    let config = QueryConfig::default();

    let events = collect_events(client, executor, state, tool_context, config, user_msg("Hi"), hooks).await;

    let has_error = events.iter().any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("API error")));
    let has_retry = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("Retrying")));
    assert!(has_error, "expected API error, got: {:?}", events);
    assert!(!has_retry, "should NOT retry on fatal error");
}

#[tokio::test]
async fn e2e_full_tool_round_trip_with_registered_tool() {
    struct EchoTool;
    #[async_trait::async_trait]
    impl claude_core::tool::Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "Echo the input text" }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            })
        }
        async fn call(&self, input: serde_json::Value, _ctx: &claude_core::tool::ToolContext) -> anyhow::Result<claude_core::tool::ToolResult> {
            let text = input["text"].as_str().unwrap_or("(empty)");
            Ok(claude_core::tool::ToolResult::text(format!("Echo: {}", text)))
        }
        fn is_read_only(&self) -> bool { true }
        fn category(&self) -> claude_core::tool::ToolCategory { claude_core::tool::ToolCategory::Session }
    }

    let tool_response = mock_tool_response("tool_1", "echo", serde_json::json!({"text": "hello world"}));
    let text_response = mock_text_response("The echo said: hello world");

    let mock = MockBackend::new()
        .with_stream_events(make_stream_events(tool_response))
        .with_stream_events(make_stream_events(text_response));

    let client = Arc::new(
        claude_api::client::AnthropicClient::new("test-key")
            .with_backend(Box::new(mock)),
    );
    let mut registry = claude_tools::ToolRegistry::new();
    registry.register(EchoTool);
    let registry = Arc::new(registry);
    let perm = Arc::new(crate::permissions::PermissionChecker::new(
        claude_core::permissions::PermissionMode::BypassAll,
        vec![],
    ));
    let executor = Arc::new(crate::executor::ToolExecutor::new(registry, perm));
    let state = crate::state::new_shared_state();
    let tool_context = claude_core::tool::ToolContext {
        cwd: std::env::temp_dir(),
        abort_signal: claude_core::tool::AbortSignal::new(),
        permission_mode: claude_core::permissions::PermissionMode::BypassAll,
        messages: vec![],
    };
    let hooks = Arc::new(crate::hooks::HookRegistry::new());
    let config = QueryConfig { max_turns: 5, ..QueryConfig::default() };

    let events = collect_events(client, executor, state.clone(), tool_context, config, user_msg("Echo hello world"), hooks).await;

    let has_tool_start = events.iter().any(|e| matches!(e, AgentEvent::ToolUseStart { name, .. } if name == "echo"));
    let has_tool_ready = events.iter().any(|e| matches!(e, AgentEvent::ToolUseReady { name, input, .. } if name == "echo" && input["text"] == "hello world"));

    let tool_result = events.iter().find_map(|e| {
        if let AgentEvent::ToolResult { id, is_error, text } = e {
            Some((id.clone(), *is_error, text.clone()))
        } else {
            None
        }
    });
    assert!(tool_result.is_some(), "expected tool result");
    let (id, is_error, text) = tool_result.unwrap();
    assert_eq!(id, "tool_1");
    assert!(!is_error, "tool should succeed");
    assert_eq!(text.as_deref(), Some("Echo: hello world"));

    let has_final = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("echo said")));
    assert!(has_tool_start, "expected ToolUseStart");
    assert!(has_tool_ready, "expected ToolUseReady");
    assert!(has_final, "expected final text");

    let s = state.read().await;
    assert_eq!(s.messages.len(), 4);
}

#[tokio::test]
async fn e2e_thinking_blocks_emitted() {
    let response = MessagesResponse {
        id: "msg_think".into(),
        response_type: "message".into(),
        role: "assistant".into(),
        content: vec![
            ResponseContentBlock::Thinking { thinking: "Let me think step by step...".into() },
            ResponseContentBlock::Text { text: "The answer is 42.".into() },
        ],
        model: "claude-sonnet-4-20250514".into(),
        stop_reason: Some("end_turn".into()),
        usage: ApiUsage {
            input_tokens: 100,
            output_tokens: 200,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    };

    let mock = MockBackend::new().with_stream_events(make_stream_events(response));
    let (client, executor, state, tool_context, hooks) = test_setup(mock);

    let config = QueryConfig {
        thinking: Some(claude_api::types::ThinkingConfig {
            thinking_type: "enabled".into(),
            budget_tokens: Some(10000),
        }),
        ..QueryConfig::default()
    };

    let events = collect_events(client, executor, state, tool_context, config, user_msg("What is the meaning of life?"), hooks).await;

    let has_thinking = events.iter().any(|e| matches!(e, AgentEvent::ThinkingDelta(t) if t.contains("step by step")));
    let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "The answer is 42."));
    let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete { stop_reason: StopReason::EndTurn }));

    assert!(has_thinking, "expected thinking delta, got: {:?}", events);
    assert!(has_text, "expected text delta");
    assert!(has_complete, "expected TurnComplete");
}
