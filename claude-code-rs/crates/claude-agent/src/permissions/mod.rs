//! Permission checking — rule-based + interactive TUI for tool authorization.

pub mod helpers;
pub mod tui;
#[cfg(test)]
mod tests;

use claude_core::permissions::{
    DenialState, PermissionBehavior, PermissionDestination,
    PermissionMode, PermissionResponse, PermissionResult, PermissionRule,
    is_safe_auto_tool,
};
use claude_core::bash_classifier;
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
    /// Auto-mode denial tracking for fallback to manual prompting.
    denial_state: std::sync::Mutex<DenialState>,
}

impl PermissionChecker {
    pub fn new(mode: PermissionMode, rules: Vec<PermissionRule>) -> Self {
        // In AcceptEdits mode, strip dangerous permission rules that would
        // bypass security (e.g., python:*, eval:*, sudo:*)
        let effective_rules = if mode == PermissionMode::AcceptEdits {
            let (safe, _stripped) = bash_classifier::strip_dangerous_rules(&rules);
            safe
        } else {
            rules
        };
        Self {
            rules: effective_rules,
            mode,
            session_allowed: std::sync::Mutex::new(std::collections::HashSet::new()),
            denial_state: std::sync::Mutex::new(DenialState::default()),
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

        // ── Auto mode: multi-stage decision ─────────────────────────────
        if mode == PermissionMode::Auto {
            return self.check_auto_mode(tool, input).await;
        }

        // AcceptEdits mode: auto-allow filesystem edit tools by category
        if mode == PermissionMode::AcceptEdits
            && tool.category() == ToolCategory::FileSystem
        {
            return PermissionResult::allow();
        }

        // AcceptEdits mode: auto-approve safe shell commands via risk classifier
        if mode == PermissionMode::AcceptEdits
            && tool.category() == ToolCategory::Shell
        {
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                let classification = bash_classifier::classify(cmd);
                if classification.risk.auto_approvable() {
                    return PermissionResult::allow();
                }
            }
        }

        // Build suggestions based on tool type
        let suggestions = build_permission_suggestions(tool, input);
        let prompt_msg = if tool.category() == ToolCategory::Shell {
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                let classification = bash_classifier::classify(cmd);
                format!("Allow {} ({})? [risk: {}]", tool.name(), cmd, classification.risk.label())
            } else {
                format!("Allow {} ?", tool.name())
            }
        } else {
            format!("Allow {} ?", tool.name())
        };
        PermissionResult::ask_with_suggestions(prompt_msg, suggestions)
    }

    /// Auto-mode permission decision pipeline:
    /// 1. Safe tool allowlist → auto-allow
    /// 2. AcceptEdits fast-path simulation → auto-allow
    /// 3. Bash classifier for shell commands → auto-allow/block
    /// 4. Fall through to classifier (or prompt if classifier unavailable)
    async fn check_auto_mode(&self, tool: &dyn Tool, input: &Value) -> PermissionResult {
        // Check denial fallback first
        if let Ok(state) = self.denial_state.lock() {
            if state.should_fallback() {
                let suggestions = build_permission_suggestions(tool, input);
                return PermissionResult::ask_with_suggestions(
                    format!("Auto-mode fallback: too many denials. Allow {}?", tool.name()),
                    suggestions,
                );
            }
        }

        // Stage 1: Safe tool allowlist (intrinsically safe, no classifier needed)
        if is_safe_auto_tool(tool.name()) {
            return PermissionResult::allow();
        }

        // Stage 2: AcceptEdits fast-path — file system tools auto-approved
        if tool.category() == ToolCategory::FileSystem {
            return PermissionResult::allow();
        }

        // Stage 3: Shell commands — use bash risk classifier
        if tool.category() == ToolCategory::Shell {
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                let classification = bash_classifier::classify(cmd);
                if classification.risk.auto_approvable() {
                    return PermissionResult::allow();
                }
                // High-risk shell commands are blocked in auto-mode
                if classification.risk.always_ask() {
                    self.record_denial();
                    return PermissionResult::deny(format!(
                        "Auto-mode blocked: {} (risk: {})",
                        cmd, classification.risk.label()
                    ));
                }
                // Medium-risk (Network) — could go to classifier, for now prompt
            }
        }

        // Stage 4: Web tools — auto-approve fetch but block if dangerous
        if tool.category() == ToolCategory::Web {
            // WebFetch is generally safe for reading; allow in auto-mode
            if tool.name() == "WebFetchTool" || tool.name() == "WebSearchTool" {
                return PermissionResult::allow();
            }
        }

        // Stage 5: Future remote classifier would go here.
        // For now, fall through to interactive prompt for unresolved tools.
        let suggestions = build_permission_suggestions(tool, input);
        let prompt_msg = if tool.category() == ToolCategory::Shell {
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                let classification = bash_classifier::classify(cmd);
                format!("Auto-mode: Allow {} ({})? [risk: {}]", tool.name(), cmd, classification.risk.label())
            } else {
                format!("Auto-mode: Allow {}?", tool.name())
            }
        } else {
            format!("Auto-mode: Allow {}?", tool.name())
        };
        PermissionResult::ask_with_suggestions(prompt_msg, suggestions)
    }

    /// Record a denial in the auto-mode denial tracker.
    fn record_denial(&self) {
        if let Ok(mut state) = self.denial_state.lock() {
            state.record_denial();
        }
    }

    /// Record an approval in the auto-mode denial tracker.
    pub fn record_auto_approval(&self) {
        if let Ok(mut state) = self.denial_state.lock() {
            state.record_approval();
        }
    }

    /// Get the current denial state (for testing/diagnostics).
    pub fn denial_state(&self) -> DenialState {
        self.denial_state.lock().map(|s| s.clone()).unwrap_or_default()
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
