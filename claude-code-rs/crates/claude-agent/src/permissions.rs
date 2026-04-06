use claude_core::permissions::{
    PermissionBehavior, PermissionDestination, PermissionMode, PermissionResponse,
    PermissionResult, PermissionRule, PermissionSuggestion,
};
use claude_core::tool::{Tool, ToolCategory};
use serde_json::Value;
use std::io::{self, Write};

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

    /// Interactive terminal permission prompt with rich options.
    /// Returns a `PermissionResponse` with the user's choice.
    pub fn prompt_user(tool_name: &str, description: &str, suggestions: &[PermissionSuggestion]) -> PermissionResponse {
        print!(
            "\n\x1b[33m⚠  {} wants to: {}\x1b[0m\n",
            tool_name, description
        );

        // Show suggestions if available
        if !suggestions.is_empty() {
            println!("  Suggested permission rules:");
            for (i, s) in suggestions.iter().enumerate() {
                println!("    \x1b[36m{})\x1b[0m {}", i + 1, s.label);
            }
        }

        print!(
            "   \x1b[33mAllow? [y/N/a(lways){}]: \x1b[0m",
            if !suggestions.is_empty() { "/1..N(suggestion)" } else { "" }
        );
        io::stdout().flush().ok();

        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        let trimmed = input.trim().to_lowercase();

        match trimmed.as_str() {
            "y" | "yes" => PermissionResponse::allow_once(),
            "a" | "always" => PermissionResponse::allow_always(),
            "n" | "no" | "" => PermissionResponse::deny(),
            other => {
                // Try parsing as suggestion index
                if let Ok(idx) = other.parse::<usize>() {
                    if idx >= 1 && idx <= suggestions.len() {
                        PermissionResponse {
                            allowed: true,
                            persist: true,
                            feedback: None,
                            selected_suggestion: Some(idx - 1),
                            destination: Some(suggestions[idx - 1].destination),
                        }
                    } else {
                        PermissionResponse::deny()
                    }
                } else {
                    PermissionResponse::deny()
                }
            }
        }
    }

    /// Mark a tool as always-allowed for this session.
    pub fn session_allow(&self, tool_name: &str) {
        if let Ok(mut allowed) = self.session_allowed.lock() {
            allowed.insert(tool_name.to_string());
        }
    }

    /// Apply a permission response, updating session state as needed.
    pub fn apply_response(&self, tool_name: &str, response: &PermissionResponse, result: &PermissionResult) {
        if response.allowed && response.persist {
            if let Some(idx) = response.selected_suggestion {
                // A suggestion was selected — in a full impl we'd write to the
                // appropriate settings file. For now, apply as session rule.
                if let Some(suggestion) = result.suggestions.get(idx) {
                    if let Ok(mut allowed) = self.session_allowed.lock() {
                        allowed.insert(suggestion.rule.tool_name.clone());
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
