use crate::permissions::helpers::*;
use crate::permissions::PermissionChecker;
use claude_core::permissions::*;
use claude_core::tool::{Tool, ToolCategory};
use serde_json::{json, Value};

    // ── Mock tool for testing ────────────────────────────────────────

    struct MockTool {
        name: &'static str,
        category: ToolCategory,
        read_only: bool,
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "mock tool"
        }
        fn category(&self) -> ToolCategory {
            self.category
        }
        fn is_read_only(&self) -> bool {
            self.read_only
        }
        fn input_schema(&self) -> Value {
            json!({})
        }
        async fn call(
            &self,
            _input: Value,
            _ctx: &claude_core::tool::ToolContext,
        ) -> anyhow::Result<claude_core::tool::ToolResult> {
            Ok(claude_core::tool::ToolResult::text("ok"))
        }
    }

    fn shell_tool() -> MockTool {
        MockTool {
            name: "Bash",
            category: ToolCategory::Shell,
            read_only: false,
        }
    }
    fn read_tool() -> MockTool {
        MockTool {
            name: "Read",
            category: ToolCategory::FileSystem,
            read_only: true,
        }
    }
    fn write_tool() -> MockTool {
        MockTool {
            name: "FileWrite",
            category: ToolCategory::FileSystem,
            read_only: false,
        }
    }

    // ── glob_match ───────────────────────────────────────────────────

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match("ls", "ls"));
        assert!(!glob_match("ls", "cat"));
    }

    #[test]
    fn test_glob_match_wildcard_star() {
        assert!(glob_match("git status", "git*"));
        assert!(glob_match("git commit -m 'msg'", "git*"));
        assert!(!glob_match("cargo build", "git*"));
    }

    #[test]
    fn test_glob_match_wildcard_question() {
        assert!(glob_match("cat", "c?t"));
        assert!(!glob_match("cart", "c?t"));
    }

    #[test]
    fn test_glob_match_path_pattern() {
        assert!(glob_match("src/main.rs", "src/*"));
        assert!(glob_match("src/utils/helper.rs", "src/*"));
        assert!(!glob_match("tests/main.rs", "src/*"));
    }

    #[test]
    fn test_glob_match_special_chars_escaped() {
        assert!(glob_match("file.rs", "file.rs"));
        assert!(!glob_match("filexrs", "file.rs"));
    }

    // ── input_matches_pattern ────────────────────────────────────────

    #[test]
    fn test_input_matches_command_field() {
        let input = json!({"command": "git status"});
        assert!(input_matches_pattern(&input, "git"));
        assert!(!input_matches_pattern(&input, "cargo"));
    }

    #[test]
    fn test_input_matches_file_path_field() {
        let input = json!({"file_path": "src/main.rs"});
        assert!(input_matches_pattern(&input, "src/main.rs"));
        assert!(input_matches_pattern(&input, "src"));
    }

    #[test]
    fn test_input_matches_glob_pattern() {
        let input = json!({"command": "npm install"});
        assert!(input_matches_pattern(&input, "npm*"));
    }

    #[test]
    fn test_input_matches_no_relevant_fields() {
        let input = json!({"something": "else"});
        assert!(!input_matches_pattern(&input, "anything"));
    }

    // ── PermissionChecker::check ─────────────────────────────────────

    #[tokio::test]
    async fn test_check_bypass_mode() {
        let checker = PermissionChecker::new(PermissionMode::BypassAll, vec![]);
        let result = checker.check(&shell_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_plan_mode_blocks_writes() {
        let checker = PermissionChecker::new(PermissionMode::Plan, vec![]);
        let result = checker.check(&write_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Deny);
    }

    #[tokio::test]
    async fn test_check_plan_mode_allows_reads() {
        let checker = PermissionChecker::new(PermissionMode::Plan, vec![]);
        let result = checker.check(&read_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_read_only_auto_allowed() {
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        let result = checker.check(&read_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_write_tool_asks() {
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        let result = checker.check(&write_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    #[tokio::test]
    async fn test_check_accept_edits_allows_filesystem() {
        let checker = PermissionChecker::new(PermissionMode::AcceptEdits, vec![]);
        let result = checker.check(&write_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_accept_edits_asks_shell() {
        let checker = PermissionChecker::new(PermissionMode::AcceptEdits, vec![]);
        let result = checker.check(&shell_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    // ── runtime mode override ───────────────────────────────────────

    #[tokio::test]
    async fn test_runtime_mode_overrides_initial() {
        // Checker created with Default (would ask for shell), but runtime bypass overrides
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        let result = checker.check(&shell_tool(), &json!({}), Some(PermissionMode::BypassAll)).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_runtime_plan_overrides_initial_default() {
        // Checker created with Default, but runtime Plan blocks writes
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        let result = checker.check(&write_tool(), &json!({}), Some(PermissionMode::Plan)).await;
        assert_eq!(result.behavior, PermissionBehavior::Deny);
    }

    #[tokio::test]
    async fn test_runtime_none_uses_initial_mode() {
        // None runtime mode → falls back to checker's initial mode
        let checker = PermissionChecker::new(PermissionMode::BypassAll, vec![]);
        let result = checker.check(&shell_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_rule_allow() {
        let rules = vec![PermissionRule {
            tool_name: "Bash".into(),
            pattern: None,
            behavior: PermissionBehavior::Allow,
        }];
        let checker = PermissionChecker::new(PermissionMode::Default, rules);
        let result = checker.check(&shell_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_rule_deny() {
        let rules = vec![PermissionRule {
            tool_name: "Bash".into(),
            pattern: None,
            behavior: PermissionBehavior::Deny,
        }];
        let checker = PermissionChecker::new(PermissionMode::Default, rules);
        let result = checker.check(&shell_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Deny);
    }

    #[tokio::test]
    async fn test_check_rule_with_pattern_match() {
        let rules = vec![PermissionRule {
            tool_name: "Bash".into(),
            pattern: Some("git*".into()),
            behavior: PermissionBehavior::Allow,
        }];
        let checker = PermissionChecker::new(PermissionMode::Default, rules);
        let result = checker
            .check(&shell_tool(), &json!({"command": "git status"}), None)
            .await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_rule_with_pattern_no_match() {
        let rules = vec![PermissionRule {
            tool_name: "Bash".into(),
            pattern: Some("git*".into()),
            behavior: PermissionBehavior::Allow,
        }];
        let checker = PermissionChecker::new(PermissionMode::Default, rules);
        let result = checker
            .check(&shell_tool(), &json!({"command": "rm -rf /"}), None)
            .await;
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    #[tokio::test]
    async fn test_check_wildcard_rule() {
        let rules = vec![PermissionRule {
            tool_name: "*".into(),
            pattern: None,
            behavior: PermissionBehavior::Allow,
        }];
        let checker = PermissionChecker::new(PermissionMode::Default, rules);
        let result = checker.check(&shell_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_category_rule() {
        let rules = vec![PermissionRule {
            tool_name: "category:shell".into(),
            pattern: None,
            behavior: PermissionBehavior::Allow,
        }];
        let checker = PermissionChecker::new(PermissionMode::Default, rules);
        let result = checker.check(&shell_tool(), &json!({}), None).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    // ── session_allow ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_session_allow_persists() {
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        let r1 = checker.check(&shell_tool(), &json!({}), None).await;
        assert_eq!(r1.behavior, PermissionBehavior::Ask);

        checker.session_allow("Bash");

        let r2 = checker.check(&shell_tool(), &json!({}), None).await;
        assert_eq!(r2.behavior, PermissionBehavior::Allow);
    }

    // ── build_permission_suggestions ─────────────────────────────────

    #[test]
    fn test_suggestions_shell_tool() {
        let tool = shell_tool();
        let input = json!({"command": "git push origin main"});
        let suggestions = build_permission_suggestions(&tool, &input);
        assert!(suggestions.len() >= 2);
        assert!(suggestions[0].label.contains("git"));
    }

    #[test]
    fn test_suggestions_filesystem_tool() {
        let tool = write_tool();
        let input = json!({"file_path": "src/main.rs"});
        let suggestions = build_permission_suggestions(&tool, &input);
        assert!(suggestions.len() >= 2);
        assert!(suggestions[0].label.contains("src"));
    }

    #[test]
    fn test_suggestions_always_has_session_allow() {
        let tool = MockTool {
            name: "CustomTool",
            category: ToolCategory::Agent,
            read_only: false,
        };
        let suggestions = build_permission_suggestions(&tool, &json!({}));
        assert!(!suggestions.is_empty());
        let last = suggestions.last().unwrap();
        assert!(last.label.contains("CustomTool"));
        assert!(last.label.contains("session"));
    }

    // ── apply_response ──────────────────────────────────────────────────

    #[test]
    fn test_apply_response_session_adds_to_session_allowed() {
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        let result = PermissionResult {
            behavior: PermissionBehavior::Ask,
            reason: None,
            suggestions: vec![PermissionSuggestion {
                label: "Allow Bash (session)".into(),
                rule: PermissionRule {
                    tool_name: "Bash".into(),
                    pattern: None,
                    behavior: PermissionBehavior::Allow,
                },
                destination: PermissionDestination::Session,
            }],
            updated_input: None,
            classification: None,
        };
        let response = PermissionResponse {
            allowed: true,
            persist: true,
            feedback: None,
            selected_suggestion: Some(0),
            destination: None,
        };
        checker.apply_response("Bash", &response, &result, std::path::Path::new("."));
        let allowed = checker.session_allowed.lock().unwrap();
        assert!(allowed.contains("Bash"));
    }

    #[test]
    fn test_apply_response_not_persisted_when_not_allowed() {
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        let result = PermissionResult {
            behavior: PermissionBehavior::Ask,
            reason: None,
            suggestions: vec![PermissionSuggestion {
                label: "Allow Bash".into(),
                rule: PermissionRule {
                    tool_name: "Bash".into(),
                    pattern: None,
                    behavior: PermissionBehavior::Allow,
                },
                destination: PermissionDestination::Session,
            }],
            updated_input: None,
            classification: None,
        };
        let response = PermissionResponse {
            allowed: false,
            persist: true,
            feedback: None,
            selected_suggestion: Some(0),
            destination: None,
        };
        checker.apply_response("Bash", &response, &result, std::path::Path::new("."));
        let allowed = checker.session_allowed.lock().unwrap();
        assert!(!allowed.contains("Bash"));
    }

    #[test]
    fn test_apply_response_no_suggestion_falls_back_to_session() {
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        let result = PermissionResult {
            behavior: PermissionBehavior::Ask,
            reason: None,
            suggestions: vec![PermissionSuggestion {
                label: "Allow Bash".into(),
                rule: PermissionRule {
                    tool_name: "Bash".into(),
                    pattern: None,
                    behavior: PermissionBehavior::Allow,
                },
                destination: PermissionDestination::Session,
            }],
            updated_input: None,
            classification: None,
        };
        let response = PermissionResponse {
            allowed: true,
            persist: true,
            feedback: None,
            selected_suggestion: None,
            destination: None,
        };
        checker.apply_response("Bash", &response, &result, std::path::Path::new("."));
        let allowed = checker.session_allowed.lock().unwrap();
        assert!(allowed.contains("Bash"));
    }
