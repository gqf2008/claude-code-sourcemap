use claude_core::permissions::{
    PermissionBehavior, PermissionDestination, PermissionRule, PermissionSuggestion,
};
use claude_core::tool::{Tool, ToolCategory};
use serde_json::Value;

/// Build permission suggestions based on tool type and input.
pub fn build_permission_suggestions(tool: &dyn Tool, input: &Value) -> Vec<PermissionSuggestion> {
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
pub fn input_matches_pattern(input: &Value, pattern: &str) -> bool {
    for key in &["command", "file_path", "path", "pattern", "subcommand"] {
        if let Some(val) = input[*key].as_str() {
            if val.contains(pattern) || glob_match(val, pattern) {
                return true;
            }
        }
    }
    false
}

/// Simple glob matching (supports `*` and `?`).
pub fn glob_match(text: &str, pattern: &str) -> bool {
    if !pattern.contains('*') && !pattern.contains('?') {
        return text == pattern;
    }
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
