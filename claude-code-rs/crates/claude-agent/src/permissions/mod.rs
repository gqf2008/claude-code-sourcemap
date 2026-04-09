//! Permission checking — rule-based + interactive TUI for tool authorization.

pub mod auto_classifier;
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
use std::sync::Arc;

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
    /// Optional API client for remote auto-classifier side-queries.
    classifier_client: Option<Arc<claude_api::client::ApiClient>>,
    /// Recent tool call history for classifier transcript (tool_name, projected_input).
    recent_tools: std::sync::Mutex<Vec<(String, Value)>>,
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
            classifier_client: None,
            recent_tools: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Attach an API client for the remote auto-classifier.
    pub fn with_classifier_client(mut self, client: Arc<claude_api::client::ApiClient>) -> Self {
        self.classifier_client = Some(client);
        self
    }

    /// Record a tool call in the recent history (for classifier transcript).
    pub fn record_tool_call(&self, tool_name: &str, classifier_input: Value) {
        if let Ok(mut history) = self.recent_tools.lock() {
            history.push((tool_name.to_string(), classifier_input));
            // Keep only last 20 entries to bound memory
            if history.len() > 20 {
                let drain_count = history.len() - 20;
                history.drain(..drain_count);
            }
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
    /// 4. Web tools → auto-allow safe ones
    /// 5. Remote classifier side-query (if API client available)
    /// 6. Fall through to interactive prompt
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
        if tool.category() == ToolCategory::Web
            && (tool.name() == "WebFetchTool" || tool.name() == "WebSearchTool")
        {
            return PermissionResult::allow();
        }

        // Stage 5: Remote classifier side-query
        if let Some(ref client) = self.classifier_client {
            let classifier_input = tool.to_auto_classifier_input(input);
            let recent = self.recent_tools.lock()
                .map(|h| h.clone())
                .unwrap_or_default();

            match auto_classifier::classify(
                client,
                &recent,
                tool.name(),
                &classifier_input,
                None,
            ).await {
                Ok(Some(decision)) => {
                    if decision.should_block {
                        self.record_denial();
                        let reason = decision.reason.unwrap_or_else(|| "Classifier blocked".into());
                        return PermissionResult::deny(format!(
                            "Auto-mode classifier (S{}): {}",
                            decision.stage, reason
                        ));
                    }
                    // Classifier approved
                    self.record_auto_approval();
                    return PermissionResult::allow();
                }
                Ok(None) => {
                    // Unparseable response — fall through to interactive
                    tracing::warn!("Auto-classifier returned unparseable response, falling through");
                }
                Err(e) => {
                    // API error — fall through to interactive
                    tracing::warn!(error = %e, "Auto-classifier failed, falling through");
                }
            }
        }

        // Stage 6: Fall through to interactive prompt for unresolved tools.
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
