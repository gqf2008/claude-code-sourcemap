//! Tests for QueryEngine and QueryEngineBuilder.

use super::*;

// ── QueryEngineBuilder ───────────────────────────────────────────

#[test]
fn test_builder_defaults() {
    let b = QueryEngineBuilder::new("test-key", "/tmp");
    assert_eq!(b.api_key, "test-key");
    assert_eq!(b.max_turns, 100);
    assert_eq!(b.max_tokens, 16384);
    assert!(b.model.is_none());
    assert!(b.system_prompt.is_empty());
    assert!(b.load_claude_md);
    assert!(b.load_memory);
    assert!(!b.coordinator_mode);
    assert!(b.allowed_tools.is_empty());
}

#[test]
fn test_builder_fluent_api() {
    let b = QueryEngineBuilder::new("key", "/tmp")
        .model("claude-haiku")
        .system_prompt("Hello")
        .max_turns(50)
        .max_tokens(8192)
        .compact_threshold(40_000)
        .coordinator_mode(true)
        .load_claude_md(false)
        .load_memory(false)
        .allowed_tools(vec!["Read".into(), "Bash".into()])
        .language(Some("中文".into()))
        .scratchpad_dir(Some("/tmp/scratchpad".into()));

    assert_eq!(b.model.as_deref(), Some("claude-haiku"));
    assert_eq!(b.system_prompt, "Hello");
    assert_eq!(b.max_turns, 50);
    assert_eq!(b.max_tokens, 8192);
    assert_eq!(b.compact_threshold, 40_000);
    assert!(b.coordinator_mode);
    assert!(!b.load_claude_md);
    assert!(!b.load_memory);
    assert_eq!(b.allowed_tools, vec!["Read", "Bash"]);
    assert_eq!(b.language.as_deref(), Some("中文"));
    assert_eq!(b.scratchpad_dir.as_deref(), Some("/tmp/scratchpad"));
}

#[test]
fn test_builder_thinking_config() {
    let b = QueryEngineBuilder::new("key", "/tmp")
        .thinking(Some(claude_api::types::ThinkingConfig {
            thinking_type: "enabled".into(),
            budget_tokens: Some(4096),
        }));

    let tc = b.thinking.as_ref().unwrap();
    assert_eq!(tc.thinking_type, "enabled");
    assert_eq!(tc.budget_tokens, Some(4096));
}

#[test]
fn test_builder_output_style() {
    let b = QueryEngineBuilder::new("key", "/tmp")
        .output_style("Concise".into(), "Be brief.".into());

    let (name, prompt) = b.output_style.as_ref().unwrap();
    assert_eq!(name, "Concise");
    assert_eq!(prompt, "Be brief.");
}

#[test]
fn test_builder_mcp_instructions() {
    let b = QueryEngineBuilder::new("key", "/tmp")
        .mcp_instructions(vec![
            ("github".into(), "Use GitHub MCP for repos".into()),
            ("slack".into(), "Use Slack MCP for messaging".into()),
        ]);

    assert_eq!(b.mcp_instructions.len(), 2);
    assert_eq!(b.mcp_instructions[0].0, "github");
}

fn build_test_engine() -> QueryEngine {
    QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .build()
}

#[test]
fn test_builder_build_creates_engine() {
    // Build with minimal config (no claude_md, no memory) to avoid FS access
    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .model("test-model")
        .max_turns(5)
        .build();

    assert_eq!(engine.cwd(), std::path::Path::new("/tmp"));
    assert!(!engine.is_coordinator());
    assert_eq!(engine.config.max_turns, 5);
}

#[test]
fn test_builder_build_coordinator_mode() {
    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .coordinator_mode(true)
        .build();

    assert!(engine.is_coordinator());
}

#[test]
fn test_engine_abort_signal() {
    let engine = build_test_engine();

    let signal = engine.abort_signal();
    assert!(!signal.is_aborted());
    engine.abort();
    assert!(signal.is_aborted());
}

// ── tool_definitions ─────────────────────────────────────────────

#[test]
fn test_tool_definitions_non_empty() {
    let engine = build_test_engine();

    let defs = engine.tool_definitions();
    assert!(!defs.is_empty(), "should have tool definitions");
}

#[test]
fn test_tool_definitions_last_has_cache_control() {
    let engine = build_test_engine();

    let defs = engine.tool_definitions();
    let last = defs.last().unwrap();
    assert!(last.cache_control.is_some(), "last tool def should have cache_control");
}

#[test]
fn test_tool_definitions_filtered_by_allowed_tools() {
    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .allowed_tools(vec!["Read".into(), "Write".into()])
        .build();

    let defs = engine.tool_definitions();
    assert!(defs.len() <= 3, "should only have allowed tools + DispatchAgent");
    for def in &defs {
        // DispatchAgent is always registered; Read/Write are the only allowed user tools
        assert!(
            def.name == "Read" || def.name == "Write" || def.name == "DispatchAgent",
            "unexpected tool: {}",
            def.name
        );
    }
}

// ── should_auto_compact ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_auto_compact_disabled_when_zero() {
    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .compact_threshold(0)
        .build();

    assert!(!engine.should_auto_compact().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_auto_compact_not_triggered_when_empty() {
    let engine = build_test_engine();

    // Empty conversation → token count is tiny → no auto-compact
    assert!(!engine.should_auto_compact().await);
}

// ── drain_notifications ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_drain_notifications_empty_when_not_coordinator() {
    let engine = build_test_engine();

    let msgs = engine.drain_notifications().await;
    assert!(msgs.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_drain_notifications_coordinator() {
    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .coordinator_mode(true)
        .build();

    // No notifications sent → drain returns empty
    let msgs = engine.drain_notifications().await;
    assert!(msgs.is_empty());
}

// ── run_session_start ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_run_session_start_no_hooks() {
    let engine = build_test_engine();

    // No hooks configured → returns None
    let result = engine.run_session_start().await;
    assert!(result.is_none());
}

// ── submit empty prompt ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_submit_empty_prompt_returns_error() {
    use tokio_stream::StreamExt as _;

    let engine = build_test_engine();
    let mut stream = engine.submit("").await;
    let first = stream.next().await;
    match first {
        Some(AgentEvent::Error(msg)) => {
            assert!(msg.contains("empty"), "expected empty-prompt error, got: {msg}");
        }
        other => panic!("expected Error event, got: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_submit_whitespace_prompt_returns_error() {
    use tokio_stream::StreamExt as _;

    let engine = build_test_engine();
    let mut stream = engine.submit("   \n\t  ").await;
    let first = stream.next().await;
    match first {
        Some(AgentEvent::Error(msg)) => {
            assert!(msg.contains("empty"), "expected empty-prompt error, got: {msg}");
        }
        other => panic!("expected Error event, got: {other:?}"),
    }
}

// ── builder: system prompt assembly ──────────────────────────────

#[test]
fn test_builder_append_system_prompt() {
    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .system_prompt("Base prompt.")
        .append_system_prompt(Some("Extra instructions.".into()))
        .build();

    assert!(
        engine.config.system_prompt.contains("Base prompt."),
        "should contain base prompt"
    );
    assert!(
        engine.config.system_prompt.contains("Extra instructions."),
        "should contain appended prompt"
    );
}

#[test]
fn test_builder_append_empty_no_change() {
    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .system_prompt("Base prompt.")
        .append_system_prompt(Some(String::new()))
        .build();

    // Empty append should not add trailing newlines
    assert!(!engine.config.system_prompt.ends_with("\n\n"));
}

#[test]
fn test_builder_thinking_config_propagated() {
    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .thinking(Some(claude_api::types::ThinkingConfig {
            thinking_type: "enabled".into(),
            budget_tokens: Some(20_000),
        }))
        .build();

    let tc = engine.config.thinking.as_ref().expect("thinking should be set");
    assert_eq!(tc.thinking_type, "enabled");
    assert_eq!(tc.budget_tokens, Some(20_000));
}

#[test]
fn test_builder_cost_tracker_starts_at_zero() {
    let engine = build_test_engine();
    assert!((engine.cost_tracker().total_usd() - 0.0).abs() < f64::EPSILON);
}

#[test]
fn test_builder_tool_count_includes_dispatch() {
    let engine = build_test_engine();
    // Should have all default tools + DispatchAgent
    assert!(engine.tool_count() > 10, "expected many tools, got {}", engine.tool_count());
}

#[test]
fn test_builder_context_window_default_model() {
    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .build();

    // Default model is sonnet → 200K context window
    assert!(engine.context_window >= 200_000,
        "expected ≥200K context, got {}", engine.context_window);
}

#[test]
fn test_builder_hooks_config_applied() {
    use claude_core::config::{HooksConfig, HookCommandDef, HookRule};

    let mut hooks = HooksConfig::default();
    hooks.pre_tool_use = vec![HookRule {
        matcher: Some("Bash".into()),
        hooks: vec![HookCommandDef {
            hook_type: "command".into(),
            command: "echo hello".into(),
            timeout_ms: None,
        }],
    }];

    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .hooks_config(hooks)
        .build();

    // The hooks registry should have at least 1 rule
    assert!(engine.hooks.has_hooks(crate::hooks::HookEvent::PreToolUse));
}

// ── context_usage_percent ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_context_usage_zero_window_returns_none() {
    let mut engine = build_test_engine();
    engine.context_window = 0;
    assert!(engine.context_usage_percent().await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_context_usage_empty_conversation() {
    let engine = build_test_engine();
    // Empty conversation → very low usage
    if let Some(pct) = engine.context_usage_percent().await {
        assert!(pct < 5, "expected < 5%, got {}%", pct);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_context_usage_with_messages() {
    let engine = QueryEngineBuilder::new("fake-key", "/tmp")
        .load_claude_md(false)
        .load_memory(false)
        .build();

    // Add a large user message
    {
        let mut state = engine.state().write().await;
        let big_text = "word ".repeat(10_000);
        state.messages.push(claude_core::message::Message::User(
            claude_core::message::UserMessage {
                uuid: "test-big".into(),
                content: vec![claude_core::message::ContentBlock::Text { text: big_text }],
            }
        ));
    }

    let pct = engine.context_usage_percent().await.unwrap();
    assert!(pct > 0, "should have non-zero usage with a large message");
}
