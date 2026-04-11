use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use clawed_core::permissions::PermissionMode;
use clawed_core::message::Message;

/// Per-model usage statistics.
#[derive(Debug, Clone, Default)]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub api_calls: u32,
    pub cost_usd: f64,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub model: String,
    pub permission_mode: PermissionMode,
    /// Stashed mode before entering plan mode, restored on exit.
    pub pre_plan_mode: Option<PermissionMode>,
    pub verbose: bool,
    pub messages: Vec<Message>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub turn_count: u32,
    /// Cumulative error tracking for diagnostics and circuit breaking.
    pub error_counts: HashMap<String, u32>,
    pub total_errors: u32,
    pub total_cache_read_tokens: u64,
    pub total_cache_creation_tokens: u64,
    /// Per-model usage breakdown.
    pub model_usage: HashMap<String, ModelUsage>,
    /// Current working directory (may change during session).
    pub cwd: Option<std::path::PathBuf>,
    /// Lines added/removed during this session.
    pub total_lines_added: u64,
    pub total_lines_removed: u64,
    /// Cumulative timing metrics (milliseconds).
    pub total_api_duration_ms: u64,
    pub total_tool_duration_ms: u64,
    /// Whether context was reloaded via /reload-context.
    pub context_reloaded: bool,
    /// Cached CLAUDE.md content (refreshed by /reload-context).
    pub claude_md_content: String,
}

impl AppState {
    /// Record an error by category (e.g., "rate_limit", "overloaded", "timeout").
    pub fn record_error(&mut self, category: &str) {
        *self.error_counts.entry(category.to_string()).or_insert(0) += 1;
        self.total_errors += 1;
    }

    /// Record token usage for a specific model.
    pub fn record_model_usage(
        &mut self,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_creation: u64,
        cost_usd: f64,
    ) {
        let entry = self.model_usage.entry(model.to_string()).or_default();
        entry.input_tokens += input_tokens;
        entry.output_tokens += output_tokens;
        entry.cache_read_tokens += cache_read;
        entry.cache_creation_tokens += cache_creation;
        entry.api_calls += 1;
        entry.cost_usd += cost_usd;
    }

    /// Record token usage with automatic cost calculation based on model pricing.
    pub fn record_usage_auto_cost(
        &mut self,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_creation: u64,
    ) {
        let cost = clawed_core::model::estimate_cost(
            model,
            input_tokens,
            output_tokens,
            cache_read,
            cache_creation,
        );
        self.record_model_usage(model, input_tokens, output_tokens, cache_read, cache_creation, cost);
    }

    /// Get total estimated cost across all models.
    pub fn total_cost(&self) -> f64 {
        self.model_usage.values().map(|u| u.cost_usd).sum()
    }

    /// Get a formatted cost summary string.
    pub fn cost_summary(&self) -> String {
        clawed_core::model::format_cost(self.total_cost())
    }

    /// Record line change statistics.
    pub fn record_line_changes(&mut self, added: u64, removed: u64) {
        self.total_lines_added += added;
        self.total_lines_removed += removed;
    }

    /// Enter plan mode, stashing the current mode for later restoration.
    pub fn enter_plan_mode(&mut self) {
        if self.permission_mode != PermissionMode::Plan {
            self.pre_plan_mode = Some(self.permission_mode);
            self.permission_mode = PermissionMode::Plan;
        }
    }

    /// Exit plan mode, restoring the previously stashed mode.
    /// Returns the restored mode.
    pub fn exit_plan_mode(&mut self) -> PermissionMode {
        let restore = self.pre_plan_mode.take().unwrap_or(PermissionMode::Default);
        self.permission_mode = restore;
        restore
    }

    /// Create a SessionSnapshot from the current state.
    pub fn to_session_snapshot(
        &self,
        session_id: &str,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> clawed_core::session::SessionSnapshot {
        use clawed_core::session::{SessionModelUsage, SessionSnapshot};

        let now = chrono::Utc::now();
        SessionSnapshot {
            id: session_id.to_string(),
            title: clawed_core::session::title_from_messages(&self.messages),
            model: self.model.clone(),
            cwd: self.cwd.as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            created_at,
            updated_at: now,
            turn_count: self.turn_count,
            input_tokens: self.total_input_tokens,
            output_tokens: self.total_output_tokens,
            model_usage: self.model_usage.iter().map(|(k, v)| {
                (k.clone(), SessionModelUsage {
                    input_tokens: v.input_tokens,
                    output_tokens: v.output_tokens,
                    cache_read_tokens: v.cache_read_tokens,
                    cache_creation_tokens: v.cache_creation_tokens,
                    api_calls: v.api_calls,
                    cost_usd: v.cost_usd,
                })
            }).collect(),
            total_cost_usd: self.total_cost(),
            messages: self.messages.clone(),
            git_branch: None,
            custom_title: None,
            ai_title: None,
            summary: None,
            last_prompt: self.last_user_prompt(),
        }
    }

    /// Restore state from a session snapshot (for --resume).
    pub fn restore_from_snapshot(&mut self, snap: &clawed_core::session::SessionSnapshot) {
        self.model = snap.model.clone();
        self.messages = snap.messages.clone();
        self.turn_count = snap.turn_count;
        self.total_input_tokens = snap.input_tokens;
        self.total_output_tokens = snap.output_tokens;

        // Restore per-model usage
        self.model_usage = snap.model_usage.iter().map(|(k, v)| {
            (k.clone(), ModelUsage {
                input_tokens: v.input_tokens,
                output_tokens: v.output_tokens,
                cache_read_tokens: v.cache_read_tokens,
                cache_creation_tokens: v.cache_creation_tokens,
                api_calls: v.api_calls,
                cost_usd: v.cost_usd,
            })
        }).collect();

        if !snap.cwd.is_empty() {
            self.cwd = Some(std::path::PathBuf::from(&snap.cwd));
        }
    }

    /// Extract the last user prompt (truncated to 200 chars) from messages.
    fn last_user_prompt(&self) -> Option<String> {
        for msg in self.messages.iter().rev() {
            if let clawed_core::message::Message::User(u) = msg {
                for block in &u.content {
                    if let clawed_core::message::ContentBlock::Text { text } = block {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            let display: String = trimmed.chars().take(200).collect();
                            return Some(display);
                        }
                    }
                }
            }
        }
        None
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            permission_mode: PermissionMode::Default,
            pre_plan_mode: None,
            verbose: false,
            messages: Vec::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            turn_count: 0,
            error_counts: HashMap::new(),
            total_errors: 0,
            total_cache_read_tokens: 0,
            total_cache_creation_tokens: 0,
            model_usage: HashMap::new(),
            cwd: None,
            total_lines_added: 0,
            total_lines_removed: 0,
            total_api_duration_ms: 0,
            total_tool_duration_ms: 0,
            context_reloaded: false,
            claude_md_content: String::new(),
        }
    }
}

pub type SharedState = Arc<RwLock<AppState>>;

pub fn new_shared_state() -> SharedState {
    Arc::new(RwLock::new(AppState::default()))
}

pub fn new_shared_state_with_model(model: String) -> SharedState {
    Arc::new(RwLock::new(AppState {
        model,
        ..AppState::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_model_usage_accumulates() {
        let mut state = AppState::default();
        state.record_model_usage("claude-sonnet-4-20250514", 1000, 500, 200, 100, 0.005);
        state.record_model_usage("claude-sonnet-4-20250514", 2000, 1000, 400, 200, 0.010);
        state.record_model_usage("claude-haiku-3-5-20241022", 500, 250, 100, 50, 0.001);

        assert_eq!(state.model_usage.len(), 2);

        let sonnet = &state.model_usage["claude-sonnet-4-20250514"];
        assert_eq!(sonnet.input_tokens, 3000);
        assert_eq!(sonnet.output_tokens, 1500);
        assert_eq!(sonnet.api_calls, 2);
        assert!((sonnet.cost_usd - 0.015).abs() < 1e-6);

        let haiku = &state.model_usage["claude-haiku-3-5-20241022"];
        assert_eq!(haiku.input_tokens, 500);
        assert_eq!(haiku.api_calls, 1);
    }

    #[test]
    fn test_record_line_changes() {
        let mut state = AppState::default();
        state.record_line_changes(50, 20);
        state.record_line_changes(30, 10);
        assert_eq!(state.total_lines_added, 80);
        assert_eq!(state.total_lines_removed, 30);
    }

    #[test]
    fn test_record_error() {
        let mut state = AppState::default();
        state.record_error("rate_limit");
        state.record_error("rate_limit");
        state.record_error("overloaded");
        assert_eq!(state.total_errors, 3);
        assert_eq!(state.error_counts["rate_limit"], 2);
        assert_eq!(state.error_counts["overloaded"], 1);
    }

    #[test]
    fn test_record_usage_auto_cost() {
        let mut state = AppState::default();
        // Sonnet: 10K input @ $3/MTok = $0.03, 2K output @ $15/MTok = $0.03
        state.record_usage_auto_cost("claude-sonnet-4", 10_000, 2_000, 0, 0);
        let cost = state.total_cost();
        assert!(cost > 0.05 && cost < 0.07, "expected ~0.06, got {cost}");
    }

    #[test]
    fn test_total_cost_multi_model() {
        let mut state = AppState::default();
        state.record_usage_auto_cost("claude-sonnet-4", 10_000, 2_000, 0, 0);
        state.record_usage_auto_cost("claude-haiku-4-5", 10_000, 2_000, 0, 0);
        let cost = state.total_cost();
        // Sonnet: 10K*$3/M + 2K*$15/M = $0.06
        // Haiku 4.5: 10K*$1/M + 2K*$5/M = $0.02 → total ~$0.08
        assert!(cost > 0.07 && cost < 0.09, "expected ~0.08, got {cost}");
    }

    #[test]
    fn test_cost_summary_formatting() {
        let mut state = AppState::default();
        state.record_usage_auto_cost("claude-sonnet-4", 100_000, 50_000, 0, 0);
        let summary = state.cost_summary();
        assert!(summary.starts_with('$'));
    }

    // ── to_session_snapshot / restore_from_snapshot ──────────────────────

    #[test]
    fn test_to_session_snapshot() {
        use clawed_core::message::{ContentBlock, Message, UserMessage};

        let mut state = AppState::default();
        state.model = "claude-sonnet-4".to_string();
        state.turn_count = 5;
        state.total_input_tokens = 1000;
        state.total_output_tokens = 500;
        state.messages.push(Message::User(UserMessage {
            uuid: "u1".to_string(),
            content: vec![ContentBlock::Text { text: "Hello Rust".to_string() }],
        }));
        state.cwd = Some(std::path::PathBuf::from("/project"));

        let created = chrono::Utc::now();
        let snap = state.to_session_snapshot("test-id", created);
        assert_eq!(snap.id, "test-id");
        assert_eq!(snap.model, "claude-sonnet-4");
        assert_eq!(snap.turn_count, 5);
        assert_eq!(snap.input_tokens, 1000);
        assert_eq!(snap.messages.len(), 1);
        assert_eq!(snap.title, "Hello Rust");
        assert_eq!(snap.cwd, "/project");
        assert_eq!(snap.last_prompt.as_deref(), Some("Hello Rust"));
    }

    #[test]
    fn test_restore_from_snapshot() {
        use clawed_core::session::SessionSnapshot;

        let snap = SessionSnapshot {
            id: "s1".to_string(),
            title: "Restored".to_string(),
            model: "claude-haiku-4-5".to_string(),
            cwd: "/restored".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            turn_count: 10,
            input_tokens: 5000,
            output_tokens: 2000,
            model_usage: std::collections::HashMap::new(),
            total_cost_usd: 0.1,
            messages: vec![],
            git_branch: Some("main".to_string()),
            custom_title: None,
            ai_title: None,
            summary: None,
            last_prompt: None,
        };

        let mut state = AppState::default();
        state.restore_from_snapshot(&snap);
        assert_eq!(state.model, "claude-haiku-4-5");
        assert_eq!(state.turn_count, 10);
        assert_eq!(state.total_input_tokens, 5000);
        assert_eq!(state.cwd.as_ref().unwrap().to_str().unwrap(), "/restored");
    }

    #[test]
    fn test_last_user_prompt() {
        use clawed_core::message::{ContentBlock, Message, UserMessage, AssistantMessage};

        let mut state = AppState::default();
        state.messages = vec![
            Message::User(UserMessage {
                uuid: "u1".to_string(),
                content: vec![ContentBlock::Text { text: "first question".to_string() }],
            }),
            Message::Assistant(AssistantMessage {
                uuid: "a1".to_string(),
                content: vec![ContentBlock::Text { text: "answer".to_string() }],
                stop_reason: None,
                usage: None,
            }),
            Message::User(UserMessage {
                uuid: "u2".to_string(),
                content: vec![ContentBlock::Text { text: "second question".to_string() }],
            }),
        ];

        assert_eq!(state.last_user_prompt().as_deref(), Some("second question"));
    }

    #[test]
    fn test_last_user_prompt_empty() {
        let state = AppState::default();
        assert!(state.last_user_prompt().is_none());
    }

    // ── Plan mode transitions ───────────────────────────────────────────

    #[test]
    fn test_enter_plan_mode_from_default() {
        let mut state = AppState::default();
        assert_eq!(state.permission_mode, PermissionMode::Default);
        state.enter_plan_mode();
        assert_eq!(state.permission_mode, PermissionMode::Plan);
        assert_eq!(state.pre_plan_mode, Some(PermissionMode::Default));
    }

    #[test]
    fn test_enter_plan_mode_from_auto() {
        let mut state = AppState::default();
        state.permission_mode = PermissionMode::Auto;
        state.enter_plan_mode();
        assert_eq!(state.permission_mode, PermissionMode::Plan);
        assert_eq!(state.pre_plan_mode, Some(PermissionMode::Auto));
    }

    #[test]
    fn test_enter_plan_mode_idempotent() {
        let mut state = AppState::default();
        state.enter_plan_mode();
        // Entering again while already in plan mode should not overwrite pre_plan_mode
        state.enter_plan_mode();
        assert_eq!(state.permission_mode, PermissionMode::Plan);
        assert_eq!(state.pre_plan_mode, Some(PermissionMode::Default));
    }

    #[test]
    fn test_exit_plan_mode_restores_default() {
        let mut state = AppState::default();
        state.enter_plan_mode();
        let restored = state.exit_plan_mode();
        assert_eq!(restored, PermissionMode::Default);
        assert_eq!(state.permission_mode, PermissionMode::Default);
        assert_eq!(state.pre_plan_mode, None);
    }

    #[test]
    fn test_exit_plan_mode_restores_auto() {
        let mut state = AppState::default();
        state.permission_mode = PermissionMode::Auto;
        state.enter_plan_mode();
        let restored = state.exit_plan_mode();
        assert_eq!(restored, PermissionMode::Auto);
        assert_eq!(state.permission_mode, PermissionMode::Auto);
    }

    #[test]
    fn test_exit_plan_mode_without_enter() {
        let mut state = AppState::default();
        // Exit without enter should fall back to Default
        let restored = state.exit_plan_mode();
        assert_eq!(restored, PermissionMode::Default);
    }
}
