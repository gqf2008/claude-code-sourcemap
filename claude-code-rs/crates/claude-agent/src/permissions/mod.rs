//! Permission checking — rule-based + interactive TUI for tool authorization.

pub mod helpers;
pub mod tui;
#[cfg(test)]
mod tests;

use claude_core::permissions::{
    PermissionBehavior, PermissionDestination, PermissionMode, PermissionResponse,
    PermissionResult, PermissionRule,
};
use claude_core::tool::{Tool, ToolCategory};
use serde_json::Value;

use helpers::{build_permission_suggestions, input_matches_pattern};

/// Checks tool permissions against configured rules, mode, and session state.
///
/// Combines static rules (from settings files), the active permission mode,
/// and a per-session "always allow" cache to decide whether a tool call
/// should be allowed, denied, or prompted interactively.
pub struct PermissionChecker {
    rules: Vec<PermissionRule>,
    mode: PermissionMode,
    /// Tracks tools the user has permanently allowed during this session.
    pub(crate) session_allowed: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl PermissionChecker {
    pub fn new(mode: PermissionMode, rules: Vec<PermissionRule>) -> Self {
        Self {
            rules,
            mode,
            session_allowed: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    pub async fn check(&self, tool: &dyn Tool, input: &Value, runtime_mode: Option<PermissionMode>) -> PermissionResult {
        let mode = runtime_mode.unwrap_or(self.mode);
        if mode == PermissionMode::BypassAll || mode == PermissionMode::DontAsk {
            return PermissionResult::allow();
        }
        if mode == PermissionMode::Plan && !tool.is_read_only() {
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
        if mode == PermissionMode::AcceptEdits
            && tool.category() == ToolCategory::FileSystem
        {
            return PermissionResult::allow();
        }

        // Build suggestions based on tool type
        let suggestions = build_permission_suggestions(tool, input);
        PermissionResult::ask_with_suggestions(format!("Allow {} ?", tool.name()), suggestions)
    }

    /// Interactive terminal permission prompt with arrow-key navigation.
    /// Delegates to [`tui::prompt_user`].
    pub fn prompt_user(
        tool_name: &str,
        description: &str,
        suggestions: &[claude_core::permissions::PermissionSuggestion],
    ) -> PermissionResponse {
        tui::prompt_user(tool_name, description, suggestions)
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
