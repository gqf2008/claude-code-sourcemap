//! Unit tests for the query stream loop and helpers.

use super::*;
use super::helpers::{
    build_system_blocks, classify_api_error, error_category,
    build_context_warning, make_continuation_message,
    messages_to_api, block_to_api, ApiErrorAction,
};

// ── classify_api_error ───────────────────────────────────────────────

#[test]
fn test_classify_prompt_too_long_triggers_compact() {
    let action = classify_api_error("prompt is too long", false, 0, 1000);
    assert!(matches!(action, ApiErrorAction::ReactiveCompact));
}

#[test]
fn test_classify_prompt_too_long_already_compacted() {
    let action = classify_api_error("prompt is too long", true, 0, 1000);
    assert!(matches!(action, ApiErrorAction::Fatal));
}

#[test]
fn test_classify_413_status() {
    let action = classify_api_error("HTTP 413 payload too large", false, 0, 1000);
    assert!(matches!(action, ApiErrorAction::ReactiveCompact));
}

#[test]
fn test_classify_too_many_tokens() {
    let action = classify_api_error("too many tokens in request", false, 0, 1000);
    assert!(matches!(action, ApiErrorAction::ReactiveCompact));
}

#[test]
fn test_classify_rate_limit_retryable() {
    let action = classify_api_error("rate limit exceeded", false, 1, 2000);
    assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 2000 }));
}

#[test]
fn test_classify_529_overloaded() {
    let action = classify_api_error("529 service overloaded", false, 2, 5000);
    assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 5000 }));
}

#[test]
fn test_classify_500_server_error() {
    let action = classify_api_error("500 internal server error", false, 0, 1000);
    assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 1000 }));
}

#[test]
fn test_classify_503_service_unavailable() {
    let action = classify_api_error("503 service unavailable", false, 3, 3000);
    assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 3000 }));
}

#[test]
fn test_classify_retry_after_header() {
    let action = classify_api_error("rate limited retry-after: 10", false, 1, 2000);
    assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 10000 }));
}

#[test]
fn test_classify_max_consecutive_errors_exceeded() {
    let action = classify_api_error("rate limit", false, 6, 1000);
    assert!(matches!(action, ApiErrorAction::Fatal));
}

#[test]
fn test_classify_unknown_error_fatal() {
    let action = classify_api_error("something unexpected happened", false, 0, 1000);
    assert!(matches!(action, ApiErrorAction::Fatal));
}

// ── error_category ───────────────────────────────────────────────────

#[test]
fn test_error_category_rate_limit() {
    assert_eq!(error_category("rate limit exceeded"), "rate_limit");
    assert_eq!(error_category("429 too many requests"), "rate_limit");
}

#[test]
fn test_error_category_overloaded() {
    assert_eq!(error_category("overloaded"), "overloaded");
    assert_eq!(error_category("529 overloaded"), "overloaded");
}

#[test]
fn test_error_category_server_error() {
    assert_eq!(error_category("500 internal"), "server_error");
    assert_eq!(error_category("503 unavailable"), "server_error");
}

#[test]
fn test_error_category_generic() {
    assert_eq!(error_category("something else entirely"), "api_error");
}

// ── build_context_warning ────────────────────────────────────────────

const TEST_CONTEXT_WINDOW: u64 = 200_000;

#[test]
fn test_build_context_warning_normal() {
    // 40% of dynamic threshold (~167K * 0.4 = ~67K) should be normal
    assert!(build_context_warning(60_000, TEST_CONTEXT_WINDOW).is_none());
}

#[test]
fn test_build_context_warning_warning_level() {
    let threshold = crate::compact::get_auto_compact_threshold(TEST_CONTEXT_WINDOW);
    let at_60 = (threshold as f64 * 0.60) as u64;
    let result = build_context_warning(at_60, TEST_CONTEXT_WINDOW);
    assert!(result.is_some());
    if let Some((level, AgentEvent::ContextWarning { message, usage_pct, .. })) = result {
        assert_eq!(level, crate::compact::TokenWarningState::Warning);
        assert!(message.contains("Approaching"));
        assert!(usage_pct <= 1.0, "pct should be ≤ 100%, got {:.0}%", usage_pct * 100.0);
    }
}

#[test]
fn test_build_context_warning_critical() {
    let threshold = crate::compact::get_auto_compact_threshold(TEST_CONTEXT_WINDOW);
    let at_80 = (threshold as f64 * 0.80) as u64;
    let result = build_context_warning(at_80, TEST_CONTEXT_WINDOW);
    assert!(result.is_some());
    if let Some((level, AgentEvent::ContextWarning { message, usage_pct, .. })) = result {
        assert_eq!(level, crate::compact::TokenWarningState::Critical);
        assert!(message.contains("nearly full"));
        assert!(usage_pct <= 1.0, "pct should be ≤ 100%, got {:.0}%", usage_pct * 100.0);
    }
}

// ── make_continuation_message ────────────────────────────────────────

#[test]
fn test_continuation_first_attempt() {
    let msg = make_continuation_message(0, 3);
    let text = match &msg.content[0] {
        ContentBlock::Text { text } => text.clone(),
        _ => panic!("expected text block"),
    };
    assert!(text.contains("Resume directly"));
}

#[test]
fn test_continuation_subsequent_attempt() {
    let msg = make_continuation_message(2, 5);
    let text = match &msg.content[0] {
        ContentBlock::Text { text } => text.clone(),
        _ => panic!("expected text block"),
    };
    assert!(text.contains("attempt 2/5"));
    assert!(text.contains("smaller pieces"));
}

// ── build_system_blocks ──────────────────────────────────────────────

#[test]
fn test_build_system_blocks_empty() {
    assert!(build_system_blocks("", false).is_none());
}

#[test]
fn test_build_system_blocks_no_boundary() {
    let blocks = build_system_blocks("Hello world", false).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "Hello world");
    assert!(blocks[0].cache_control.is_some());
}

#[test]
fn test_build_system_blocks_with_boundary() {
    let boundary = crate::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
    let prompt = format!("Static part\n{}\nDynamic part", boundary);
    let blocks = build_system_blocks(&prompt, false).unwrap();
    assert_eq!(blocks.len(), 2);
    assert!(blocks[0].text.contains("Static part"));
    assert!(blocks[1].text.contains("Dynamic part"));
    assert!(blocks[0].cache_control.is_some());
    assert_eq!(blocks[0].cache_control.as_ref().unwrap().control_type, "ephemeral");
    assert!(blocks[1].cache_control.is_none());
}

#[test]
fn test_build_system_blocks_boundary_strips_marker() {
    let boundary = crate::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
    let prompt = format!("Static\n{}\nDynamic data", boundary);
    let blocks = build_system_blocks(&prompt, false).unwrap();
    assert!(!blocks[1].text.contains(boundary));
    assert!(blocks[1].text.contains("Dynamic data"));
}

#[test]
fn test_build_system_blocks_skip_cache() {
    let blocks = build_system_blocks("Hello world", true).unwrap();
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].cache_control.is_none());
}

// ── messages_to_api ──────────────────────────────────────────────────

#[test]
fn test_messages_to_api_converts_user_and_assistant() {
    let messages = vec![
        Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "hello".into() }],
        }),
        Message::Assistant(AssistantMessage {
            uuid: "a1".into(),
            content: vec![ContentBlock::Text { text: "hi".into() }],
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        }),
    ];
    let api = messages_to_api(&messages, false);
    assert_eq!(api.len(), 2);
    assert_eq!(api[0].role, "user");
    assert_eq!(api[1].role, "assistant");
}

#[test]
fn test_messages_to_api_skips_system() {
    let messages = vec![
        Message::System(claude_core::message::SystemMessage {
            uuid: "s1".into(),
            message: "system text".into(),
        }),
        Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "hello".into() }],
        }),
    ];
    let api = messages_to_api(&messages, false);
    assert_eq!(api.len(), 1);
    assert_eq!(api[0].role, "user");
}

#[test]
fn test_messages_to_api_cache_control_on_last_block() {
    let messages = vec![
        Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "hello".into() }],
        }),
    ];
    let api = messages_to_api(&messages, false);
    match &api[0].content[0] {
        ApiContentBlock::Text { cache_control, .. } => {
            assert!(cache_control.is_some());
        }
        _ => panic!("expected Text block"),
    }
}

#[test]
fn test_messages_to_api_skip_cache() {
    let messages = vec![
        Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text { text: "hello".into() }],
        }),
    ];
    let api = messages_to_api(&messages, true);
    match &api[0].content[0] {
        ApiContentBlock::Text { cache_control, .. } => {
            assert!(cache_control.is_none(), "cache_control should be None when skip_cache=true");
        }
        _ => panic!("expected Text block"),
    }
}

// ── block_to_api ─────────────────────────────────────────────────────

#[test]
fn test_block_to_api_text() {
    let block = ContentBlock::Text { text: "hello".into() };
    let api = block_to_api(&block);
    match api {
        ApiContentBlock::Text { text, cache_control } => {
            assert_eq!(text, "hello");
            assert!(cache_control.is_none());
        }
        _ => panic!("expected Text"),
    }
}

#[test]
fn test_block_to_api_tool_use() {
    let block = ContentBlock::ToolUse {
        id: "t1".into(),
        name: "Bash".into(),
        input: serde_json::json!({"command": "ls"}),
    };
    let api = block_to_api(&block);
    match api {
        ApiContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "t1");
            assert_eq!(name, "Bash");
            assert_eq!(input["command"], "ls");
        }
        _ => panic!("expected ToolUse"),
    }
}

#[test]
fn test_block_to_api_thinking() {
    let block = ContentBlock::Thinking { thinking: "let me think...".into() };
    let api = block_to_api(&block);
    match api {
        ApiContentBlock::Text { text, .. } => {
            assert!(text.contains("<thinking>"));
            assert!(text.contains("let me think..."));
        }
        _ => panic!("expected Text for thinking"),
    }
}

#[test]
fn test_block_to_api_image() {
    let block = ContentBlock::Image {
        source: claude_core::message::ImageSource {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
    };
    let api = block_to_api(&block);
    match api {
        ApiContentBlock::Image { source } => {
            assert_eq!(source.source_type, "base64");
            assert_eq!(source.media_type, "image/png");
            assert_eq!(source.data, "iVBORw0KGgo=");
        }
        _ => panic!("expected Image for image block"),
    }
}

// ── QueryConfig ──────────────────────────────────────────────────────

#[test]
fn test_query_config_defaults() {
    let cfg = QueryConfig::default();
    assert_eq!(cfg.max_turns, 100);
    assert_eq!(cfg.max_tokens, 16384);
    assert!(cfg.system_prompt.is_empty());
    assert_eq!(cfg.token_budget, 0);
}
