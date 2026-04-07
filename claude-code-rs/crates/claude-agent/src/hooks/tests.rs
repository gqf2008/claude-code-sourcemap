//! Tests for the hook system.

use super::*;
use super::execution::{get_cached_regex, interpret_output, tool_matches};
use claude_core::config::{HookCommandDef, HookRule, HooksConfig};

// ── HookEvent ────────────────────────────────────────────────────────

#[test]
fn test_event_as_str_roundtrip() {
    let events = [
        HookEvent::PreToolUse, HookEvent::PostToolUse,
        HookEvent::PostToolUseFailure, HookEvent::Stop,
        HookEvent::StopFailure, HookEvent::UserPromptSubmit,
        HookEvent::SessionStart, HookEvent::SessionEnd,
        HookEvent::Setup, HookEvent::PreCompact,
        HookEvent::PostCompact, HookEvent::SubagentStart,
        HookEvent::SubagentStop, HookEvent::Notification,
        HookEvent::PostSampling, HookEvent::PermissionRequest,
        HookEvent::PermissionDenied, HookEvent::InstructionsLoaded,
        HookEvent::CwdChanged, HookEvent::FileChanged,
        HookEvent::ConfigChange, HookEvent::TaskCreated,
        HookEvent::TaskCompleted,
    ];
    // All 23 events have unique string representations
    let strs: Vec<&str> = events.iter().map(|e| e.as_str()).collect();
    assert_eq!(strs.len(), 23);
    let unique: std::collections::HashSet<_> = strs.iter().collect();
    assert_eq!(unique.len(), 23, "All event names should be unique");
}

#[test]
fn test_event_as_str_known_values() {
    assert_eq!(HookEvent::PreToolUse.as_str(), "PreToolUse");
    assert_eq!(HookEvent::Stop.as_str(), "Stop");
    assert_eq!(HookEvent::UserPromptSubmit.as_str(), "UserPromptSubmit");
    assert_eq!(HookEvent::TaskCompleted.as_str(), "TaskCompleted");
}

// ── tool_matches ─────────────────────────────────────────────────────

#[test]
fn test_tool_matches_none_matches_all() {
    assert!(tool_matches(&None, "Bash"));
    assert!(tool_matches(&None, "FileRead"));
    assert!(tool_matches(&None, "anything"));
}

#[test]
fn test_tool_matches_empty_and_wildcard() {
    assert!(tool_matches(&Some("".into()), "Bash"));
    assert!(tool_matches(&Some("*".into()), "Bash"));
}

#[test]
fn test_tool_matches_exact() {
    assert!(tool_matches(&Some("Bash".into()), "Bash"));
    assert!(!tool_matches(&Some("Bash".into()), "FileRead"));
    assert!(!tool_matches(&Some("Bash".into()), "bash")); // case-sensitive
}

#[test]
fn test_tool_matches_regex_pipe() {
    assert!(tool_matches(&Some("Bash|FileRead".into()), "Bash"));
    assert!(tool_matches(&Some("Bash|FileRead".into()), "FileRead"));
    assert!(!tool_matches(&Some("Bash|FileRead".into()), "Grep"));
}

#[test]
fn test_tool_matches_regex_pattern() {
    assert!(tool_matches(&Some("File.*".into()), "FileRead"));
    assert!(tool_matches(&Some("File.*".into()), "FileEdit"));
    assert!(tool_matches(&Some("File.*".into()), "FileWrite"));
    assert!(!tool_matches(&Some("File.*".into()), "Bash"));
}

#[test]
fn test_tool_matches_regex_anchors() {
    assert!(tool_matches(&Some("^Bash$".into()), "Bash"));
    assert!(!tool_matches(&Some("^Bash$".into()), "BashTool"));
}

#[test]
fn test_tool_matches_invalid_regex_returns_false() {
    // Unbalanced brackets — invalid regex
    assert!(!tool_matches(&Some("[invalid".into()), "anything"));
}

// ── interpret_output ─────────────────────────────────────────────────

#[test]
fn test_interpret_exit0_empty_stdout() {
    let d = interpret_output(HookEvent::PreToolUse, 0, String::new());
    assert!(matches!(d, HookDecision::Continue));
}

#[test]
fn test_interpret_exit0_plain_text_injection_event() {
    // UserPromptSubmit with plain text → AppendContext
    let d = interpret_output(HookEvent::UserPromptSubmit, 0, "extra context".into());
    assert!(matches!(d, HookDecision::AppendContext { text } if text == "extra context"));
}

#[test]
fn test_interpret_exit0_plain_text_non_injection_event() {
    // PreToolUse with plain text → Continue (not an injection event)
    let d = interpret_output(HookEvent::PreToolUse, 0, "some text".into());
    assert!(matches!(d, HookDecision::Continue));
}

#[test]
fn test_interpret_exit0_json_block() {
    let json = r#"{"decision":"block","reason":"security policy"}"#;
    let d = interpret_output(HookEvent::PreToolUse, 0, json.into());
    assert!(matches!(d, HookDecision::Block { reason } if reason == "security policy"));
}

#[test]
fn test_interpret_exit0_json_modify() {
    let json = r#"{"decision":"modify","input":{"file":"new.txt"}}"#;
    let d = interpret_output(HookEvent::PreToolUse, 0, json.into());
    match d {
        HookDecision::ModifyInput { new_input } => {
            assert_eq!(new_input["file"], "new.txt");
        }
        _ => panic!("expected ModifyInput"),
    }
}

#[test]
fn test_interpret_exit0_json_approve() {
    let json = r#"{"decision":"approve"}"#;
    let d = interpret_output(HookEvent::PreToolUse, 0, json.into());
    assert!(matches!(d, HookDecision::Continue));
}

#[test]
fn test_interpret_exit0_json_continue() {
    let json = r#"{"decision":"continue"}"#;
    let d = interpret_output(HookEvent::PreToolUse, 0, json.into());
    assert!(matches!(d, HookDecision::Continue));
}

#[test]
fn test_interpret_exit2_stop_event() {
    let d = interpret_output(HookEvent::Stop, 2, "keep going".into());
    assert!(matches!(d, HookDecision::FeedbackAndContinue { feedback } if feedback == "keep going"));
}

#[test]
fn test_interpret_exit2_stop_empty() {
    let d = interpret_output(HookEvent::Stop, 2, String::new());
    assert!(matches!(d, HookDecision::FeedbackAndContinue { feedback } if feedback == "Continue."));
}

#[test]
fn test_interpret_exit2_precompact_blocks() {
    let d = interpret_output(HookEvent::PreCompact, 2, "not now".into());
    assert!(matches!(d, HookDecision::Block { reason } if reason == "not now"));
}

#[test]
fn test_interpret_nonzero_blocks() {
    let d = interpret_output(HookEvent::PreToolUse, 1, "denied".into());
    assert!(matches!(d, HookDecision::Block { reason } if reason == "denied"));
}

#[test]
fn test_interpret_nonzero_empty_reason() {
    let d = interpret_output(HookEvent::PreToolUse, 1, String::new());
    assert!(matches!(d, HookDecision::Block { reason } if reason.contains("code 1")));
}

#[test]
fn test_interpret_fire_and_forget_events() {
    // StopFailure, Notification, SessionEnd, PostCompact always Continue
    for event in [HookEvent::StopFailure, HookEvent::Notification, HookEvent::SessionEnd, HookEvent::PostCompact] {
        let d = interpret_output(event, 1, "error".into());
        assert!(matches!(d, HookDecision::Continue), "event {:?} should Continue", event.as_str());
    }
}

#[test]
fn test_interpret_injection_events_list() {
    // All 4 injection events should get AppendContext with exit 0 + text
    for event in [HookEvent::UserPromptSubmit, HookEvent::SessionStart, HookEvent::SubagentStart, HookEvent::PreCompact] {
        let d = interpret_output(event, 0, "ctx".into());
        assert!(matches!(d, HookDecision::AppendContext { .. }), "event {:?} should AppendContext", event.as_str());
    }
}

#[test]
fn test_interpret_modify_without_input_no_panic() {
    // Previously this would panic with .expect("input checked above")
    // Now it gracefully falls through when "modify" has no input
    let json = r#"{"decision":"modify"}"#;
    let d = interpret_output(HookEvent::PreToolUse, 0, json.into());
    // Should NOT panic; falls through to Continue for non-injection events
    assert!(matches!(d, HookDecision::Continue), "expected Continue, got {:?}", d);
}

// ── HookRegistry ─────────────────────────────────────────────────────

fn make_rule(matcher: Option<&str>, command: &str) -> HookRule {
    HookRule {
        matcher: matcher.map(|s| s.to_string()),
        hooks: vec![HookCommandDef {
            hook_type: "command".into(),
            command: command.into(),
            timeout_ms: Some(1000),
        }],
    }
}

#[test]
fn test_registry_empty_has_no_hooks() {
    let reg = HookRegistry::new();
    assert!(!reg.has_hooks(HookEvent::PreToolUse));
    assert!(!reg.has_hooks(HookEvent::Stop));
}

#[test]
fn test_registry_from_config_routes_events() {
    let mut config = HooksConfig::default();
    config.pre_tool_use.push(make_rule(Some("Bash"), "echo pre"));
    config.stop.push(make_rule(None, "echo stop"));

    let reg = HookRegistry::from_config(config, "/tmp", "test-session");
    assert!(reg.has_hooks(HookEvent::PreToolUse));
    assert!(reg.has_hooks(HookEvent::Stop));
    assert!(!reg.has_hooks(HookEvent::SessionStart));
}

#[test]
fn test_registry_rules_for_all_events() {
    // Ensure rules_for handles all 23 events without panic
    let reg = HookRegistry::new();
    let events = [
        HookEvent::PreToolUse, HookEvent::PostToolUse,
        HookEvent::PostToolUseFailure, HookEvent::Stop,
        HookEvent::StopFailure, HookEvent::UserPromptSubmit,
        HookEvent::SessionStart, HookEvent::SessionEnd,
        HookEvent::Setup, HookEvent::PreCompact,
        HookEvent::PostCompact, HookEvent::SubagentStart,
        HookEvent::SubagentStop, HookEvent::Notification,
        HookEvent::PostSampling, HookEvent::PermissionRequest,
        HookEvent::PermissionDenied, HookEvent::InstructionsLoaded,
        HookEvent::CwdChanged, HookEvent::FileChanged,
        HookEvent::ConfigChange, HookEvent::TaskCreated,
        HookEvent::TaskCompleted,
    ];
    for event in events {
        assert!(reg.rules_for(event).is_empty());
    }
}

// ── Context builders ─────────────────────────────────────────────────

#[test]
fn test_tool_ctx_fields() {
    let reg = HookRegistry::from_config(HooksConfig::default(), "/project", "sess-123");
    let ctx = reg.tool_ctx(
        HookEvent::PreToolUse,
        "Bash",
        Some(serde_json::json!({"command": "ls"})),
        None,
        None,
    );
    assert_eq!(ctx.event, "PreToolUse");
    assert_eq!(ctx.tool_name.as_deref(), Some("Bash"));
    assert_eq!(ctx.tool_input.unwrap()["command"], "ls");
    assert!(ctx.tool_output.is_none());
    assert_eq!(ctx.session_id, "sess-123");
}

#[test]
fn test_tool_failure_ctx() {
    let reg = HookRegistry::from_config(HooksConfig::default(), "/project", "sess-1");
    let ctx = reg.tool_failure_ctx("Edit", None, "file not found");
    assert_eq!(ctx.event, "PostToolUseFailure");
    assert_eq!(ctx.tool_error, Some(true));
    assert_eq!(ctx.error.as_deref(), Some("file not found"));
}

#[test]
fn test_prompt_ctx() {
    let reg = HookRegistry::from_config(HooksConfig::default(), "/project", "sess-1");
    let ctx = reg.prompt_ctx(HookEvent::UserPromptSubmit, Some("Hello".into()));
    assert_eq!(ctx.event, "UserPromptSubmit");
    assert_eq!(ctx.prompt.as_deref(), Some("Hello"));
    assert!(ctx.tool_name.is_none());
}

#[test]
fn test_compact_ctx() {
    let reg = HookRegistry::from_config(HooksConfig::default(), "/project", "sess-1");
    let ctx = reg.compact_ctx(HookEvent::PreCompact, "auto", None);
    assert_eq!(ctx.trigger.as_deref(), Some("auto"));
    assert!(ctx.summary.is_none());

    let ctx2 = reg.compact_ctx(HookEvent::PostCompact, "manual", Some("Summary...".into()));
    assert_eq!(ctx2.summary.as_deref(), Some("Summary..."));
}

#[test]
fn test_permission_ctx() {
    let reg = HookRegistry::from_config(HooksConfig::default(), "/project", "sess-1");
    let ctx = reg.permission_ctx(
        HookEvent::PermissionDenied,
        "Bash",
        &serde_json::json!({"command": "rm -rf /"}),
        "blocked by policy",
    );
    assert_eq!(ctx.event, "PermissionDenied");
    assert_eq!(ctx.tool_name.as_deref(), Some("Bash"));
    assert_eq!(ctx.error.as_deref(), Some("blocked by policy"));
}

#[test]
fn test_task_ctx() {
    let reg = HookRegistry::from_config(HooksConfig::default(), "/project", "sess-1");
    let ctx = reg.task_ctx(HookEvent::TaskCreated, "fix bug #42", None);
    assert_eq!(ctx.event, "TaskCreated");
    assert_eq!(ctx.tool_input.unwrap()["task"], "fix bug #42");
}

#[test]
fn test_context_serialization() {
    let reg = HookRegistry::from_config(HooksConfig::default(), "/project", "sess-1");
    let ctx = reg.tool_ctx(HookEvent::PreToolUse, "Bash", None, None, None);
    let json = serde_json::to_string(&ctx).unwrap();
    assert!(json.contains("PreToolUse"));
    assert!(json.contains("Bash"));
    // None fields should be skipped
    assert!(!json.contains("tool_output"));
    assert!(!json.contains("trigger"));
}

// ── Regex cache ──────────────────────────────────────────────────────

#[test]
fn test_regex_cache_returns_same_result() {
    let re1 = get_cached_regex("Bash|File.*");
    let re2 = get_cached_regex("Bash|File.*");
    assert!(re1.is_some());
    assert!(re2.is_some());
    assert!(re1.unwrap().is_match("Bash"));
}

#[test]
fn test_regex_cache_invalid_returns_none() {
    assert!(get_cached_regex("[invalid").is_none());
}

// ── Async run tests ──────────────────────────────────────────────────

#[tokio::test]
async fn test_run_no_hooks_returns_continue() {
    let reg = HookRegistry::new();
    let ctx = reg.tool_ctx(HookEvent::PreToolUse, "Bash", None, None, None);
    let decision = reg.run(HookEvent::PreToolUse, ctx).await;
    assert!(matches!(decision, HookDecision::Continue));
}

#[tokio::test]
async fn test_run_matcher_filters_tool_name() {
    let mut config = HooksConfig::default();
    // Only matches "Edit" — should not fire for "Bash"
    config.pre_tool_use.push(make_rule(Some("Edit"), "echo blocked"));
    let reg = HookRegistry::from_config(config, ".", "test");
    let ctx = reg.tool_ctx(HookEvent::PreToolUse, "Bash", None, None, None);
    let decision = reg.run(HookEvent::PreToolUse, ctx).await;
    assert!(matches!(decision, HookDecision::Continue));
}

#[tokio::test]
async fn test_run_echo_hook_returns_continue() {
    // `echo hello` exits 0 with non-empty stdout, but PreToolUse is not an injection event
    let mut config = HooksConfig::default();
    config.pre_tool_use.push(make_rule(None, "echo hello"));
    let reg = HookRegistry::from_config(config, ".", "test");
    let ctx = reg.tool_ctx(HookEvent::PreToolUse, "Bash", None, None, None);
    let decision = reg.run(HookEvent::PreToolUse, ctx).await;
    assert!(matches!(decision, HookDecision::Continue));
}
