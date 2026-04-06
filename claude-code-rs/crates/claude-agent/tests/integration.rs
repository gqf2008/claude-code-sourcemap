//! Integration tests for the claude-agent crate.
//!
//! These tests verify the engine builder, tool definitions, session persistence,
//! coordinator setup, and system prompt assembly — everything except actual
//! Anthropic API calls.

use claude_agent::coordinator::AgentTracker;
use claude_agent::cost::CostTracker;
use claude_agent::engine::QueryEngineBuilder;
use claude_agent::permissions::PermissionChecker;
use claude_agent::state::new_shared_state;
use claude_agent::system_prompt::{build_system_prompt_ext, DynamicSections};
use claude_core::message::{ContentBlock, Message, UserMessage};
use claude_core::permissions::PermissionMode;

// ── Engine Builder ───────────────────────────────────────────────────────────

#[test]
fn test_engine_builder_defaults() {
    let engine = QueryEngineBuilder::new("test-key", std::env::temp_dir())
        .load_claude_md(false)
        .load_memory(false)
        .build();

    assert!(!engine.is_coordinator());
    assert!(!engine.session_id().is_empty());
}

#[test]
fn test_engine_builder_coordinator_mode() {
    let engine = QueryEngineBuilder::new("test-key", std::env::temp_dir())
        .coordinator_mode(true)
        .load_claude_md(false)
        .load_memory(false)
        .build();

    assert!(engine.is_coordinator());
}

#[test]
fn test_engine_builder_with_custom_model() {
    let engine = QueryEngineBuilder::new("test-key", std::env::temp_dir())
        .model("claude-haiku-3-20240307")
        .load_claude_md(false)
        .load_memory(false)
        .build();

    // Engine should be built without panicking
    assert!(!engine.session_id().is_empty());
}

#[test]
fn test_engine_builder_with_allowed_tools() {
    let engine = QueryEngineBuilder::new("test-key", std::env::temp_dir())
        .allowed_tools(vec!["Bash".to_string(), "Read".to_string()])
        .load_claude_md(false)
        .load_memory(false)
        .build();

    // Engine should be built without panicking
    assert!(!engine.session_id().is_empty());
}

// ── System Prompt Assembly ───────────────────────────────────────────────────

#[test]
fn test_system_prompt_default() {
    let prompt = build_system_prompt_ext(
        &std::env::temp_dir(),
        "claude-sonnet-4-20250514",
        &["Bash".to_string(), "Read".to_string()],
        "",
        "",
        &DynamicSections::default(),
    );
    // Should contain key sections
    assert!(prompt.text.contains("IMPORTANT"), "Should contain guidelines");
    assert!(!prompt.text.is_empty());
}

#[test]
fn test_system_prompt_with_language() {
    let dynamic = DynamicSections {
        language: Some("中文"),
        ..Default::default()
    };
    let prompt = build_system_prompt_ext(
        &std::env::temp_dir(),
        "claude-sonnet-4-20250514",
        &["Bash".to_string()],
        "",
        "",
        &dynamic,
    );
    assert!(prompt.text.contains("中文"), "Should contain language preference");
}

#[test]
fn test_system_prompt_with_claude_md() {
    let prompt = build_system_prompt_ext(
        &std::env::temp_dir(),
        "claude-sonnet-4-20250514",
        &[],
        "Always use TypeScript",
        "",
        &DynamicSections::default(),
    );
    assert!(prompt.text.contains("Always use TypeScript"), "Should contain CLAUDE.md content");
}

#[test]
fn test_system_prompt_with_memory() {
    let prompt = build_system_prompt_ext(
        &std::env::temp_dir(),
        "claude-sonnet-4-20250514",
        &[],
        "",
        "Use PostgreSQL for the database",
        &DynamicSections::default(),
    );
    assert!(prompt.text.contains("PostgreSQL"), "Should contain memory content");
}

// ── State & Session Persistence ──────────────────────────────────────────────

#[tokio::test]
async fn test_shared_state_read_write() {
    let state = new_shared_state();

    {
        let mut s = state.write().await;
        s.model = "claude-sonnet-4-20250514".to_string();
        s.turn_count = 5;
        s.total_input_tokens = 1000;
        s.total_output_tokens = 200;
    }

    let s = state.read().await;
    assert_eq!(s.model, "claude-sonnet-4-20250514");
    assert_eq!(s.turn_count, 5);
    assert_eq!(s.total_input_tokens, 1000);
    assert_eq!(s.total_output_tokens, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_session_save_and_load() {
    let engine = tokio::task::spawn_blocking(|| {
        QueryEngineBuilder::new("test-key", std::env::temp_dir())
            .load_claude_md(false)
            .load_memory(false)
            .build()
    }).await.unwrap();

    // Add some messages to the state
    {
        let mut s = engine.state().write().await;
        s.messages.push(Message::User(UserMessage {
            uuid: "test-uuid-1".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello, Claude".to_string(),
            }],
        }));
        s.turn_count = 1;
    }

    // Save
    let save_result = engine.save_session().await;
    assert!(save_result.is_ok(), "Session save should succeed: {:?}", save_result.err());

    // Load into a new engine
    let sid = engine.session_id().to_string();
    let engine2 = tokio::task::spawn_blocking(|| {
        QueryEngineBuilder::new("test-key", std::env::temp_dir())
            .load_claude_md(false)
            .load_memory(false)
            .build()
    }).await.unwrap();

    let title = engine2.restore_session(&sid).await;
    assert!(title.is_ok(), "Session restore should succeed: {:?}", title.err());

    // Verify restored state
    let s = engine2.state().read().await;
    assert_eq!(s.messages.len(), 1);
    assert_eq!(s.turn_count, 1);

    // Cleanup
    let _ = claude_core::session::delete_session(&sid);
}

// ── Coordinator ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_coordinator_agent_lifecycle() {
    let (tracker, mut rx) = AgentTracker::new();

    // Register an agent
    tracker.register("agent-1", "Do some work").await;
    assert!(tracker.is_running("agent-1").await);

    // Complete the agent
    tracker
        .complete("agent-1", "All done!".to_string(), 500, 3)
        .await;
    assert!(!tracker.is_running("agent-1").await);

    // Check notification was sent
    let notif = rx.try_recv().unwrap();
    assert_eq!(notif.agent_id, "agent-1");
    assert_eq!(notif.total_tokens, 500);
    assert_eq!(notif.tool_uses, 3);
    assert!(notif.to_xml().contains("<task-id>agent-1</task-id>"));
    assert!(notif.to_xml().contains("<status>completed</status>"));
}

#[tokio::test]
async fn test_coordinator_agent_failure() {
    let (tracker, mut rx) = AgentTracker::new();

    tracker.register("agent-2", "Try something").await;
    tracker.fail("agent-2", "API error".to_string()).await;

    let notif = rx.try_recv().unwrap();
    assert!(notif.to_xml().contains("<status>failed</status>"));
    assert!(notif.to_xml().contains("API error"));
}

#[tokio::test]
async fn test_coordinator_notification_to_message() {
    let (tracker, mut rx) = AgentTracker::new();

    tracker.register("agent-3", "Work").await;
    tracker
        .complete("agent-3", "Result here".to_string(), 100, 1)
        .await;

    let notif = rx.try_recv().unwrap();
    let msg = notif.to_message();
    match msg {
        Message::User(um) => {
            let text = match &um.content[0] {
                ContentBlock::Text { text } => text.clone(),
                _ => panic!("Expected text content"),
            };
            assert!(text.contains("<task-notification>"));
            assert!(text.contains("Result here"));
        }
        _ => panic!("Expected User message"),
    }
}

// ── Cost Tracking ────────────────────────────────────────────────────────────

#[test]
fn test_cost_tracker_accumulation() {
    let tracker = CostTracker::new();

    let usage1 = claude_core::message::Usage {
        input_tokens: 1000,
        output_tokens: 500,
        cache_read_input_tokens: Some(100),
        cache_creation_input_tokens: Some(200),
    };
    tracker.add("claude-sonnet-4-20250514", &usage1);

    let usage2 = claude_core::message::Usage {
        input_tokens: 2000,
        output_tokens: 1000,
        cache_read_input_tokens: Some(300),
        cache_creation_input_tokens: Some(400),
    };
    tracker.add("claude-sonnet-4-20250514", &usage2);

    let total = tracker.total_usd();
    assert!(total > 0.0, "Total cost should be positive");

    let report = tracker.format_summary(3000, 1500, 2);
    assert!(!report.is_empty(), "Report should not be empty");
    assert!(report.contains("3.0K"), "Should show formatted input tokens, got: {report}");
}

// ── Engine Clear History ─────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_engine_clear_history() {
    let engine = tokio::task::spawn_blocking(|| {
        QueryEngineBuilder::new("test-key", std::env::temp_dir())
            .load_claude_md(false)
            .load_memory(false)
            .build()
    }).await.unwrap();

    // Add state
    {
        let mut s = engine.state().write().await;
        s.messages.push(Message::User(UserMessage {
            uuid: "uuid".to_string(),
            content: vec![ContentBlock::Text { text: "test".to_string() }],
        }));
        s.turn_count = 3;
        s.total_input_tokens = 5000;
    }

    engine.clear_history().await;

    let s = engine.state().read().await;
    assert!(s.messages.is_empty());
    assert_eq!(s.turn_count, 0);
    assert_eq!(s.total_input_tokens, 0);
}

// ── Abort Signal ─────────────────────────────────────────────────────────────

#[test]
fn test_abort_signal() {
    let engine = QueryEngineBuilder::new("test-key", std::env::temp_dir())
        .load_claude_md(false)
        .load_memory(false)
        .build();

    let signal = engine.abort_signal();
    assert!(!signal.is_aborted());

    engine.abort();
    assert!(signal.is_aborted());
}

// ── Permission Checker ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_permission_checker_bypass_all() {
    use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
    use async_trait::async_trait;

    struct DummyTool;
    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str { "Bash" }
        fn category(&self) -> ToolCategory { ToolCategory::Shell }
        fn description(&self) -> &str { "dummy" }
        fn input_schema(&self) -> serde_json::Value { serde_json::json!({"type":"object"}) }
        fn is_read_only(&self) -> bool { false }
        async fn call(&self, _: serde_json::Value, _: &ToolContext) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::text("ok"))
        }
    }

    let checker = PermissionChecker::new(PermissionMode::BypassAll, Vec::new());
    let result = checker.check(&DummyTool, &serde_json::json!({"command": "ls"})).await;
    assert!(
        matches!(result.behavior, claude_core::permissions::PermissionBehavior::Allow),
        "BypassAll should allow everything"
    );
}

#[tokio::test]
async fn test_permission_checker_plan_denies_writes() {
    use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
    use async_trait::async_trait;

    struct WriteTool;
    #[async_trait]
    impl Tool for WriteTool {
        fn name(&self) -> &str { "Write" }
        fn category(&self) -> ToolCategory { ToolCategory::FileSystem }
        fn description(&self) -> &str { "write" }
        fn input_schema(&self) -> serde_json::Value { serde_json::json!({"type":"object"}) }
        fn is_read_only(&self) -> bool { false }
        async fn call(&self, _: serde_json::Value, _: &ToolContext) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::text("ok"))
        }
    }

    let checker = PermissionChecker::new(PermissionMode::Plan, Vec::new());
    let result = checker.check(&WriteTool, &serde_json::json!({})).await;
    assert!(
        matches!(result.behavior, claude_core::permissions::PermissionBehavior::Deny),
        "Plan mode should deny write tools"
    );
}

// ── MCP Tool Name Convention ─────────────────────────────────────────────────

#[test]
fn test_mcp_tool_name_roundtrip() {
    use claude_tools::mcp::{build_mcp_tool_name, parse_mcp_tool_name};

    let name = build_mcp_tool_name("github", "create_issue");
    assert_eq!(name, "mcp__github__create_issue");

    let (server, tool) = parse_mcp_tool_name(&name).unwrap();
    assert_eq!(server, "github");
    assert_eq!(tool, "create_issue");
}

// ── Auto-Compact ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_auto_compact_threshold() {
    let engine = tokio::task::spawn_blocking(|| {
        QueryEngineBuilder::new("test-key", std::env::temp_dir())
            .compact_threshold(1000)
            .load_claude_md(false)
            .load_memory(false)
            .build()
    }).await.unwrap();

    // With 0 messages, should not trigger
    assert!(!engine.should_auto_compact().await);

    // Inject an assistant message with high Usage token count so hybrid
    // counting picks it up (it uses token_count_with_estimation which reads
    // the last assistant Usage).
    {
        let mut s = engine.state().write().await;
        s.messages.push(claude_core::message::Message::Assistant(
            claude_core::message::AssistantMessage {
                uuid: "compact-test".into(),
                content: vec![claude_core::message::ContentBlock::Text {
                    text: "test".into(),
                }],
                stop_reason: None,
                usage: Some(claude_core::message::Usage {
                    input_tokens: 200_000,
                    output_tokens: 5_000,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                }),
            },
        ));
    }

    // Should now trigger (200K + 5K tokens >> 1000 threshold)
    assert!(engine.should_auto_compact().await);
}
