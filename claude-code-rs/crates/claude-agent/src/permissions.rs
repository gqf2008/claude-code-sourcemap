use claude_core::permissions::{
    PermissionBehavior, PermissionDestination, PermissionMode, PermissionResponse,
    PermissionResult, PermissionRule, PermissionSuggestion,
};
use claude_core::tool::{Tool, ToolCategory};
use serde_json::Value;
use std::io;

pub struct PermissionChecker {
    rules: Vec<PermissionRule>,
    mode: PermissionMode,
    /// Tracks tools the user has permanently allowed during this session.
    session_allowed: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl PermissionChecker {
    pub fn new(mode: PermissionMode, rules: Vec<PermissionRule>) -> Self {
        Self {
            rules,
            mode,
            session_allowed: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    pub async fn check(&self, tool: &dyn Tool, input: &Value) -> PermissionResult {
        if self.mode == PermissionMode::BypassAll || self.mode == PermissionMode::DontAsk {
            return PermissionResult::allow();
        }
        if self.mode == PermissionMode::Plan && !tool.is_read_only() {
            return PermissionResult::deny("Plan mode: writes not allowed".into());
        }

        // Check session-level "always allow" cache
        if let Ok(allowed) = self.session_allowed.lock() {
            if allowed.contains(tool.name()) {
                return PermissionResult::allow();
            }
        }

        // Check configured rules (with optional pattern matching)
        let tool_cat = format!("category:{}", tool.category());
        for rule in &self.rules {
            let name_matches = rule.tool_name == tool.name()
                || rule.tool_name == "*"
                || rule.tool_name == tool_cat;
            if !name_matches {
                continue;
            }
            if let Some(ref pattern) = rule.pattern {
                if !input_matches_pattern(input, pattern) {
                    continue;
                }
            }
            match rule.behavior {
                PermissionBehavior::Allow => return PermissionResult::allow(),
                PermissionBehavior::Deny => {
                    return PermissionResult::deny(format!("'{}' denied by rule", tool.name()));
                }
                PermissionBehavior::Ask => {}
            }
        }

        if tool.is_read_only() {
            return PermissionResult::allow();
        }

        // AcceptEdits mode: auto-allow filesystem edit tools by category
        if self.mode == PermissionMode::AcceptEdits
            && tool.category() == ToolCategory::FileSystem {
                return PermissionResult::allow();
            }

        // Build suggestions based on tool type
        let suggestions = build_permission_suggestions(tool, input);
        PermissionResult::ask_with_suggestions(
            format!("Allow {} ?", tool.name()),
            suggestions,
        )
    }

    /// Interactive terminal permission prompt with arrow-key navigation.
    /// Returns a `PermissionResponse` with the user's choice.
    pub fn prompt_user(tool_name: &str, description: &str, suggestions: &[PermissionSuggestion]) -> PermissionResponse {
        use crossterm::{
            cursor, execute,
            event::{self, Event, KeyCode, KeyModifiers},
            style::{Attribute, Color, Print, SetAttribute, SetForegroundColor, ResetColor},
            terminal::{self, ClearType},
        };
        use std::io::{stdout, Write};

        // Build options list
        let mut options: Vec<(String, PermissionResponse)> = Vec::new();
        options.push(("Allow once".into(), PermissionResponse::allow_once()));
        options.push(("Allow always (this session)".into(), PermissionResponse::allow_always()));
        for (i, s) in suggestions.iter().enumerate() {
            options.push((
                s.label.clone(),
                PermissionResponse {
                    allowed: true,
                    persist: true,
                    feedback: None,
                    selected_suggestion: Some(i),
                    destination: Some(s.destination),
                },
            ));
        }
        options.push(("Deny".into(), PermissionResponse::deny()));

        // Print header
        let mut out = stdout();
        let _ = execute!(
            out,
            Print("\n"),
            SetForegroundColor(Color::Yellow),
            SetAttribute(Attribute::Bold),
            Print(format!("⚠  {} ", tool_name)),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::Yellow),
            Print(format!("wants to: {}", description)),
            ResetColor,
            Print("\n\n"),
        );

        // If not a terminal, fall back to simple stdin
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let _ = execute!(out, Print("   Allow? [y/N]: "));
            let _ = out.flush();
            let mut input = String::new();
            io::stdin().read_line(&mut input).ok();
            return match input.trim().to_lowercase().as_str() {
                "y" | "yes" => PermissionResponse::allow_once(),
                _ => PermissionResponse::deny(),
            };
        }

        // Interactive arrow-key selection
        let mut selected: usize = 0;
        let _ = terminal::enable_raw_mode();

        let result = loop {
            // Render options
            let _ = execute!(out, cursor::MoveToColumn(0));
            for (i, (label, _)) in options.iter().enumerate() {
                let _ = execute!(out, terminal::Clear(ClearType::CurrentLine));
                if i == selected {
                    let _ = execute!(
                        out,
                        SetForegroundColor(Color::Cyan),
                        SetAttribute(Attribute::Bold),
                        Print(format!("  ❯ {}", label)),
                        SetAttribute(Attribute::Reset),
                        ResetColor,
                    );
                } else {
                    let _ = execute!(
                        out,
                        SetForegroundColor(Color::DarkGrey),
                        Print(format!("    {}", label)),
                        ResetColor,
                    );
                }
                let _ = execute!(out, Print("\n"));
            }
            let _ = execute!(
                out,
                SetForegroundColor(Color::DarkGrey),
                Print("\n  ↑↓ navigate · Enter select · n deny · y allow"),
                ResetColor,
            );
            let _ = out.flush();

            // Wait for key event
            if let Ok(Event::Key(key)) = event::read() {
                match key.code {
                    KeyCode::Up => {
                        if selected > 0 { selected -= 1; }
                    }
                    KeyCode::Down => {
                        if selected < options.len().saturating_sub(1) { selected += 1; }
                    }
                    KeyCode::Enter => {
                        break options[selected].1.clone();
                    }
                    KeyCode::Char('y') => {
                        break PermissionResponse::allow_once();
                    }
                    KeyCode::Char('a') => {
                        break PermissionResponse::allow_always();
                    }
                    KeyCode::Char('n') | KeyCode::Esc => {
                        break PermissionResponse::deny();
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break PermissionResponse::deny();
                    }
                    _ => {}
                }
            }

            // Move cursor up to re-render (options + hint line)
            let lines_to_clear = options.len() + 2;
            let _ = execute!(out, cursor::MoveUp(lines_to_clear as u16));
        };

        let _ = terminal::disable_raw_mode();
        // Clear the menu after selection
        let _ = execute!(out, Print("\n"));

        result
    }

    /// Mark a tool as always-allowed for this session.
    pub fn session_allow(&self, tool_name: &str) {
        if let Ok(mut allowed) = self.session_allowed.lock() {
            allowed.insert(tool_name.to_string());
        }
    }

    /// Apply a permission response, updating session state and optionally persisting.
    pub fn apply_response(
        &self,
        tool_name: &str,
        response: &PermissionResponse,
        result: &PermissionResult,
        cwd: &std::path::Path,
    ) {
        if response.allowed && response.persist {
            if let Some(idx) = response.selected_suggestion {
                if let Some(suggestion) = result.suggestions.get(idx) {
                    // Map PermissionDestination to SettingsSource for persistence
                    match suggestion.destination {
                        PermissionDestination::Session => {
                            if let Ok(mut allowed) = self.session_allowed.lock() {
                                allowed.insert(suggestion.rule.tool_name.clone());
                            }
                        }
                        PermissionDestination::LocalSettings => {
                            let _ = claude_core::config::Settings::add_permission_rule(
                                suggestion.rule.clone(),
                                claude_core::config::SettingsSource::Local,
                                cwd,
                            );
                            // Also add to session for immediate effect
                            if let Ok(mut allowed) = self.session_allowed.lock() {
                                allowed.insert(suggestion.rule.tool_name.clone());
                            }
                        }
                        PermissionDestination::ProjectSettings => {
                            let _ = claude_core::config::Settings::add_permission_rule(
                                suggestion.rule.clone(),
                                claude_core::config::SettingsSource::Project,
                                cwd,
                            );
                            if let Ok(mut allowed) = self.session_allowed.lock() {
                                allowed.insert(suggestion.rule.tool_name.clone());
                            }
                        }
                        PermissionDestination::UserSettings => {
                            let _ = claude_core::config::Settings::add_permission_rule(
                                suggestion.rule.clone(),
                                claude_core::config::SettingsSource::User,
                                cwd,
                            );
                            if let Ok(mut allowed) = self.session_allowed.lock() {
                                allowed.insert(suggestion.rule.tool_name.clone());
                            }
                        }
                    }
                }
            } else {
                // Generic "always allow" for this session
                self.session_allow(tool_name);
            }
        }
    }
}

/// Build permission suggestions based on tool type and input.
fn build_permission_suggestions(tool: &dyn Tool, input: &Value) -> Vec<PermissionSuggestion> {
    let mut suggestions = Vec::new();

    match tool.category() {
        ToolCategory::Shell => {
            // Suggest allowing by command prefix
            if let Some(cmd) = input["command"].as_str() {
                let prefix = cmd.split_whitespace().next().unwrap_or(cmd);
                suggestions.push(PermissionSuggestion {
                    label: format!("Allow commands starting with `{}`", prefix),
                    rule: PermissionRule {
                        tool_name: tool.name().to_string(),
                        pattern: Some(format!("{}*", prefix)),
                        behavior: PermissionBehavior::Allow,
                    },
                    destination: PermissionDestination::Session,
                });
            }
        }
        ToolCategory::FileSystem => {
            // Suggest allowing by directory
            if let Some(path) = input["file_path"].as_str().or(input["path"].as_str()) {
                if let Some(dir) = std::path::Path::new(path).parent() {
                    suggestions.push(PermissionSuggestion {
                        label: format!("Allow writes in `{}/`", dir.display()),
                        rule: PermissionRule {
                            tool_name: tool.name().to_string(),
                            pattern: Some(format!("{}/*", dir.display())),
                            behavior: PermissionBehavior::Allow,
                        },
                        destination: PermissionDestination::Session,
                    });
                }
            }
        }
        _ => {}
    }

    // Always offer "allow this tool for session"
    suggestions.push(PermissionSuggestion {
        label: format!("Allow `{}` for this session", tool.name()),
        rule: PermissionRule {
            tool_name: tool.name().to_string(),
            pattern: None,
            behavior: PermissionBehavior::Allow,
        },
        destination: PermissionDestination::Session,
    });

    suggestions
}

/// Check if a tool's input matches a pattern string.
/// Pattern is matched against the JSON-serialized command/path field.
fn input_matches_pattern(input: &Value, pattern: &str) -> bool {
    // Try common fields: command, file_path, path, pattern
    for key in &["command", "file_path", "path", "pattern", "subcommand"] {
        if let Some(val) = input[*key].as_str() {
            if val.contains(pattern) || glob_match(val, pattern) {
                return true;
            }
        }
    }
    false
}

/// Simple glob matching (supports * and ?).
fn glob_match(text: &str, pattern: &str) -> bool {
    if !pattern.contains('*') && !pattern.contains('?') {
        return text == pattern;
    }
    // Escape all regex special characters, then convert glob wildcards
    let mut regex_str = String::with_capacity(pattern.len() * 2);
    for ch in pattern.chars() {
        match ch {
            '*' => regex_str.push_str(".*"),
            '?' => regex_str.push('.'),
            '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                regex_str.push('\\');
                regex_str.push(ch);
            }
            _ => regex_str.push(ch),
        }
    }
    regex::Regex::new(&format!("^{}$", regex_str))
        .map(|re| re.is_match(text))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Mock tool for testing ────────────────────────────────────────

    struct MockTool {
        name: &'static str,
        category: ToolCategory,
        read_only: bool,
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str { self.name }
        fn description(&self) -> &str { "mock tool" }
        fn category(&self) -> ToolCategory { self.category }
        fn is_read_only(&self) -> bool { self.read_only }
        fn input_schema(&self) -> Value { json!({}) }
        async fn call(&self, _input: Value, _ctx: &claude_core::tool::ToolContext) -> anyhow::Result<claude_core::tool::ToolResult> {
            Ok(claude_core::tool::ToolResult::text("ok"))
        }
    }

    fn shell_tool() -> MockTool {
        MockTool { name: "Bash", category: ToolCategory::Shell, read_only: false }
    }
    fn read_tool() -> MockTool {
        MockTool { name: "Read", category: ToolCategory::FileSystem, read_only: true }
    }
    fn write_tool() -> MockTool {
        MockTool { name: "FileWrite", category: ToolCategory::FileSystem, read_only: false }
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
        // Dots in patterns are escaped, not regex wildcards
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
        let result = checker.check(&shell_tool(), &json!({})).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_plan_mode_blocks_writes() {
        let checker = PermissionChecker::new(PermissionMode::Plan, vec![]);
        let result = checker.check(&write_tool(), &json!({})).await;
        assert_eq!(result.behavior, PermissionBehavior::Deny);
    }

    #[tokio::test]
    async fn test_check_plan_mode_allows_reads() {
        let checker = PermissionChecker::new(PermissionMode::Plan, vec![]);
        let result = checker.check(&read_tool(), &json!({})).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_read_only_auto_allowed() {
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        let result = checker.check(&read_tool(), &json!({})).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_write_tool_asks() {
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        let result = checker.check(&write_tool(), &json!({})).await;
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    #[tokio::test]
    async fn test_check_accept_edits_allows_filesystem() {
        let checker = PermissionChecker::new(PermissionMode::AcceptEdits, vec![]);
        let result = checker.check(&write_tool(), &json!({})).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn test_check_accept_edits_asks_shell() {
        let checker = PermissionChecker::new(PermissionMode::AcceptEdits, vec![]);
        let result = checker.check(&shell_tool(), &json!({})).await;
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    #[tokio::test]
    async fn test_check_rule_allow() {
        let rules = vec![PermissionRule {
            tool_name: "Bash".into(),
            pattern: None,
            behavior: PermissionBehavior::Allow,
        }];
        let checker = PermissionChecker::new(PermissionMode::Default, rules);
        let result = checker.check(&shell_tool(), &json!({})).await;
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
        let result = checker.check(&shell_tool(), &json!({})).await;
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
        let result = checker.check(&shell_tool(), &json!({"command": "git status"})).await;
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
        // Pattern doesn't match → rule skipped → falls through to Ask
        let result = checker.check(&shell_tool(), &json!({"command": "rm -rf /"})).await;
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
        let result = checker.check(&shell_tool(), &json!({})).await;
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
        let result = checker.check(&shell_tool(), &json!({})).await;
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    // ── session_allow ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_session_allow_persists() {
        let checker = PermissionChecker::new(PermissionMode::Default, vec![]);
        // First check: write tool should Ask
        let r1 = checker.check(&shell_tool(), &json!({})).await;
        assert_eq!(r1.behavior, PermissionBehavior::Ask);

        // Mark as session-allowed
        checker.session_allow("Bash");

        // Second check: should now be allowed
        let r2 = checker.check(&shell_tool(), &json!({})).await;
        assert_eq!(r2.behavior, PermissionBehavior::Allow);
    }

    // ── build_permission_suggestions ─────────────────────────────────

    #[test]
    fn test_suggestions_shell_tool() {
        let tool = shell_tool();
        let input = json!({"command": "git push origin main"});
        let suggestions = build_permission_suggestions(&tool, &input);
        // Should have: command prefix + always allow
        assert!(suggestions.len() >= 2);
        assert!(suggestions[0].label.contains("git"));
    }

    #[test]
    fn test_suggestions_filesystem_tool() {
        let tool = write_tool();
        let input = json!({"file_path": "src/main.rs"});
        let suggestions = build_permission_suggestions(&tool, &input);
        // Should have: directory pattern + always allow
        assert!(suggestions.len() >= 2);
        assert!(suggestions[0].label.contains("src"));
    }

    #[test]
    fn test_suggestions_always_has_session_allow() {
        let tool = MockTool { name: "CustomTool", category: ToolCategory::Agent, read_only: false };
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
            selected_suggestion: None, // no suggestion selected → generic session allow
            destination: None,
        };
        checker.apply_response("Bash", &response, &result, std::path::Path::new("."));
        // When no suggestion is selected, falls back to session_allow(tool_name)
        let allowed = checker.session_allowed.lock().unwrap();
        assert!(allowed.contains("Bash"));
    }
}
