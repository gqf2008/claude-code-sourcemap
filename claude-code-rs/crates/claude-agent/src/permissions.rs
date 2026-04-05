use claude_core::permissions::{PermissionBehavior, PermissionMode, PermissionResult, PermissionRule};
use claude_core::tool::Tool;
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
        if self.mode == PermissionMode::BypassAll {
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
        for rule in &self.rules {
            if rule.tool_name == tool.name() || rule.tool_name == "*" {
                // If rule has a pattern, check if input matches
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
        }

        if tool.is_read_only() {
            return PermissionResult::allow();
        }

        // AcceptEdits mode: auto-allow edit tools
        if self.mode == PermissionMode::AcceptEdits {
            let edit_tools = ["Edit", "FileEdit", "Write", "FileWrite", "MultiEdit", "NotebookEdit"];
            if edit_tools.contains(&tool.name()) {
                return PermissionResult::allow();
            }
        }

        PermissionResult::ask(format!("Allow {} ?", tool.name()))
    }

    /// Interactive terminal permission prompt.
    /// Returns (allowed, always) — where `always` means user chose to allow permanently.
    pub fn prompt_user(tool_name: &str, description: &str) -> (bool, bool) {
        print!(
            "\n\x1b[33m⚠  {} wants to: {}\n   Allow? [y/N/a(lways)]: \x1b[0m",
            tool_name, description
        );
        io::stdout().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        match input.trim().to_lowercase().as_str() {
            "y" | "yes" => (true, false),
            "a" | "always" => (true, true),
            _ => (false, false),
        }
    }

    /// Mark a tool as always-allowed for this session.
    pub fn session_allow(&self, tool_name: &str) {
        if let Ok(mut allowed) = self.session_allowed.lock() {
            allowed.insert(tool_name.to_string());
        }
    }
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
