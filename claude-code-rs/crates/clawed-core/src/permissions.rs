use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    BypassAll,
    Plan,
    /// "Don't ask" mode — auto-allow everything without prompts.
    DontAsk,
    /// Auto-approve mode: safe tools are allowed immediately, unsafe tools
    /// pass through a classifier (local fast-path + optional remote LLM).
    Auto,
}

// ── Auto-mode configuration ─────────────────────────────────────────────────

/// User-configurable rules for auto-approve mode (from settings.json `autoMode`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AutoModeConfig {
    /// Glob patterns for commands/tools that should always be allowed.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Glob patterns for commands that should be soft-denied (prompt user).
    #[serde(default)]
    pub soft_deny: Vec<String>,
    /// Free-text environment context injected into the classifier prompt.
    #[serde(default)]
    pub environment: Option<String>,
}

/// Result of the auto-mode classifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoModeDecision {
    /// Action is safe to execute without confirmation.
    Allow,
    /// Action should be blocked; optional reason string.
    Block(Option<String>),
    /// Classifier unavailable (API error / timeout).
    Unavailable,
}

// ── Safe tool allowlist ─────────────────────────────────────────────────────

/// Tools that are intrinsically safe and should be auto-approved in Auto mode
/// without consulting the classifier. Mirrors TS `SAFE_YOLO_ALLOWLISTED_TOOLS`.
pub const SAFE_AUTO_TOOLS: &[&str] = &[
    // Read-only file operations
    "FileReadTool",
    "GrepTool",
    "GlobTool",
    "LSTool",
    // Task management (metadata-only)
    "TodoWriteTool",
    "TaskCreateTool",
    "TaskGetTool",
    "TaskUpdateTool",
    "TaskListTool",
    "TaskStopTool",
    "TaskOutputTool",
    // User interaction / plan mode
    "AskUserQuestionTool",
    // Swarm coordination
    "TeamCreateTool",
    "TeamDeleteTool",
    "SendMessageTool",
    // MCP read resources
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
];

/// Check if a tool name is on the intrinsic safe allowlist.
pub fn is_safe_auto_tool(name: &str) -> bool {
    SAFE_AUTO_TOOLS.contains(&name)
}

// ── Denial tracking ─────────────────────────────────────────────────────────

/// Tracks consecutive and total denials for auto-mode fallback.
#[derive(Debug, Clone, Default)]
pub struct DenialState {
    pub consecutive_denials: u32,
    pub total_denials: u32,
    pub total_approvals: u32,
}

/// Maximum consecutive denials before falling back to manual prompting.
pub const MAX_CONSECUTIVE_DENIALS: u32 = 5;

impl DenialState {
    pub fn record_denial(&mut self) {
        self.consecutive_denials += 1;
        self.total_denials += 1;
    }

    pub fn record_approval(&mut self) {
        self.consecutive_denials = 0;
        self.total_approvals += 1;
    }

    /// Whether we should fall back to manual prompting.
    pub fn should_fallback(&self) -> bool {
        self.consecutive_denials >= MAX_CONSECUTIVE_DENIALS
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

/// Where a permission rule should be persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionDestination {
    /// In-memory only (lost on restart).
    Session,
    /// `.claude/settings.local.json` (gitignored).
    LocalSettings,
    /// `.claude/settings.json` (shared per project).
    ProjectSettings,
    /// `~/.claude/settings.json` (global user).
    UserSettings,
}

/// Classification of how the permission was resolved (for telemetry / logging).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionClassification {
    /// User allowed temporarily (this invocation only).
    UserTemporary,
    /// User created a permanent rule.
    UserPermanent,
    /// User rejected.
    UserReject,
    /// Resolved by a pre-configured rule.
    RuleMatch,
    /// Resolved by mode (BypassAll, Plan, AcceptEdits).
    ModeMatch,
}

/// A suggested permission update the UI can offer to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionSuggestion {
    pub label: String,
    pub rule: PermissionRule,
    pub destination: PermissionDestination,
}

#[derive(Debug, Clone)]
pub struct PermissionResult {
    pub behavior: PermissionBehavior,
    pub reason: Option<String>,
    /// Suggested rule updates the UI can offer when behavior == Ask.
    pub suggestions: Vec<PermissionSuggestion>,
    /// Optional modified input (e.g., after path expansion).
    pub updated_input: Option<serde_json::Value>,
    /// How the decision was classified (set after user responds).
    pub classification: Option<PermissionClassification>,
}

impl PermissionResult {
    pub fn allow() -> Self {
        Self {
            behavior: PermissionBehavior::Allow,
            reason: None,
            suggestions: Vec::new(),
            updated_input: None,
            classification: Some(PermissionClassification::ModeMatch),
        }
    }
    pub fn deny(reason: String) -> Self {
        Self {
            behavior: PermissionBehavior::Deny,
            reason: Some(reason),
            suggestions: Vec::new(),
            updated_input: None,
            classification: Some(PermissionClassification::RuleMatch),
        }
    }
    pub fn ask(reason: String) -> Self {
        Self {
            behavior: PermissionBehavior::Ask,
            reason: Some(reason),
            suggestions: Vec::new(),
            updated_input: None,
            classification: None,
        }
    }
    /// Create an "ask" result with suggested permission rules.
    pub fn ask_with_suggestions(reason: String, suggestions: Vec<PermissionSuggestion>) -> Self {
        Self {
            behavior: PermissionBehavior::Ask,
            reason: Some(reason),
            suggestions,
            updated_input: None,
            classification: None,
        }
    }
}

/// The user's response to a permission prompt.
#[derive(Debug, Clone)]
pub struct PermissionResponse {
    /// Whether to allow the operation.
    pub allowed: bool,
    /// If true, persist the rule (via suggestion or "always allow").
    pub persist: bool,
    /// Optional feedback text from the user.
    pub feedback: Option<String>,
    /// Which suggestion was selected (index into PermissionResult::suggestions).
    pub selected_suggestion: Option<usize>,
    /// Override destination for the rule.
    pub destination: Option<PermissionDestination>,
}

impl PermissionResponse {
    pub fn allow_once() -> Self {
        Self { allowed: true, persist: false, feedback: None, selected_suggestion: None, destination: None }
    }
    pub fn allow_always() -> Self {
        Self { allowed: true, persist: true, feedback: None, selected_suggestion: None, destination: Some(PermissionDestination::Session) }
    }
    pub fn deny() -> Self {
        Self { allowed: false, persist: false, feedback: None, selected_suggestion: None, destination: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool_name: String,
    pub pattern: Option<String>,
    pub behavior: PermissionBehavior,
}

/// Wildcard matcher supporting `*` glob (e.g. `*.rs`, `foo*bar`, `*`).
pub fn matches_wildcard(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == value;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    let n = parts.len();

    // Prefix must match the start of value
    if !parts[0].is_empty() && !value.starts_with(parts[0]) {
        return false;
    }
    // Suffix must match the end of value
    if !parts[n - 1].is_empty() && !value.ends_with(parts[n - 1]) {
        return false;
    }

    let start = parts[0].len();
    let end = value.len().saturating_sub(parts[n - 1].len());
    if start > end {
        return false; // prefix and suffix overlap
    }

    // Match middle segments left-to-right
    let mut pos = start;
    for part in &parts[1..n - 1] {
        if part.is_empty() {
            continue;
        }
        match value[pos..end].find(part) {
            Some(idx) => pos += idx + part.len(),
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_result_allow() {
        let r = PermissionResult::allow();
        assert!(matches!(r.behavior, PermissionBehavior::Allow));
        assert!(r.suggestions.is_empty());
        assert_eq!(r.classification, Some(PermissionClassification::ModeMatch));
    }

    #[test]
    fn test_permission_result_deny() {
        let r = PermissionResult::deny("rule match".into());
        assert!(matches!(r.behavior, PermissionBehavior::Deny));
        assert_eq!(r.reason.as_deref(), Some("rule match"));
    }

    #[test]
    fn test_permission_result_ask_with_suggestions() {
        let suggestions = vec![
            PermissionSuggestion {
                label: "Allow npm*".into(),
                rule: PermissionRule {
                    tool_name: "Bash".into(),
                    pattern: Some("npm*".into()),
                    behavior: PermissionBehavior::Allow,
                },
                destination: PermissionDestination::Session,
            },
        ];
        let r = PermissionResult::ask_with_suggestions("Allow Bash?".into(), suggestions);
        assert!(matches!(r.behavior, PermissionBehavior::Ask));
        assert_eq!(r.suggestions.len(), 1);
        assert_eq!(r.suggestions[0].label, "Allow npm*");
    }

    #[test]
    fn test_permission_response_factories() {
        let r = PermissionResponse::allow_once();
        assert!(r.allowed);
        assert!(!r.persist);

        let r = PermissionResponse::allow_always();
        assert!(r.allowed);
        assert!(r.persist);
        assert_eq!(r.destination, Some(PermissionDestination::Session));

        let r = PermissionResponse::deny();
        assert!(!r.allowed);
    }

    #[test]
    fn test_wildcard_matcher() {
        assert!(matches_wildcard("*", "anything"));
        assert!(matches_wildcard("npm*", "npm install"));
        assert!(!matches_wildcard("npm*", "yarn install"));
        assert!(matches_wildcard("*.rs", "main.rs"));
        assert!(!matches_wildcard("*.rs", "main.ts"));
        assert!(matches_wildcard("src/**/test*", "src/**/test_foo"));
    }

    #[test]
    fn test_permission_mode_dont_ask() {
        // DontAsk should be a distinct mode
        assert_ne!(PermissionMode::DontAsk, PermissionMode::BypassAll);
        assert_ne!(PermissionMode::DontAsk, PermissionMode::Default);
    }

    #[test]
    fn test_permission_mode_auto() {
        assert_ne!(PermissionMode::Auto, PermissionMode::BypassAll);
        assert_ne!(PermissionMode::Auto, PermissionMode::AcceptEdits);
        assert_ne!(PermissionMode::Auto, PermissionMode::DontAsk);
    }

    #[test]
    fn test_safe_auto_tools_contains_expected() {
        assert!(is_safe_auto_tool("FileReadTool"));
        assert!(is_safe_auto_tool("GrepTool"));
        assert!(is_safe_auto_tool("GlobTool"));
        assert!(is_safe_auto_tool("LSTool"));
        assert!(is_safe_auto_tool("TodoWriteTool"));
        assert!(is_safe_auto_tool("TaskCreateTool"));
        assert!(is_safe_auto_tool("AskUserQuestionTool"));
        assert!(is_safe_auto_tool("TeamCreateTool"));
        // Not safe:
        assert!(!is_safe_auto_tool("BashTool"));
        assert!(!is_safe_auto_tool("FileEditTool"));
        assert!(!is_safe_auto_tool("FileWriteTool"));
        assert!(!is_safe_auto_tool("WebFetchTool"));
    }

    #[test]
    fn test_denial_state_tracking() {
        let mut state = DenialState::default();
        assert_eq!(state.consecutive_denials, 0);
        assert!(!state.should_fallback());

        state.record_denial();
        state.record_denial();
        assert_eq!(state.consecutive_denials, 2);
        assert_eq!(state.total_denials, 2);
        assert!(!state.should_fallback());

        state.record_approval();
        assert_eq!(state.consecutive_denials, 0);
        assert_eq!(state.total_approvals, 1);
        assert_eq!(state.total_denials, 2);

        // Hit the limit
        for _ in 0..MAX_CONSECUTIVE_DENIALS {
            state.record_denial();
        }
        assert!(state.should_fallback());
    }

    #[test]
    fn test_auto_mode_config_deserialize() {
        let json = r#"{"allow": ["git*", "npm*"], "soft_deny": ["rm*"], "environment": "CI server"}"#;
        let config: AutoModeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.allow, vec!["git*", "npm*"]);
        assert_eq!(config.soft_deny, vec!["rm*"]);
        assert_eq!(config.environment.as_deref(), Some("CI server"));
    }

    #[test]
    fn test_auto_mode_config_deserialize_empty() {
        let json = r#"{}"#;
        let config: AutoModeConfig = serde_json::from_str(json).unwrap();
        assert!(config.allow.is_empty());
        assert!(config.soft_deny.is_empty());
        assert!(config.environment.is_none());
    }

    #[test]
    fn test_auto_mode_decision_variants() {
        assert_eq!(AutoModeDecision::Allow, AutoModeDecision::Allow);
        assert_ne!(AutoModeDecision::Allow, AutoModeDecision::Block(None));
        assert_ne!(AutoModeDecision::Allow, AutoModeDecision::Unavailable);
    }
}
