use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use claude_core::permissions::PermissionMode;
use claude_core::message::Message;

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
        let cost = claude_core::model::estimate_cost(
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
        claude_core::model::format_cost(self.total_cost())
    }

    /// Record line change statistics.
    pub fn record_line_changes(&mut self, added: u64, removed: u64) {
        self.total_lines_added += added;
        self.total_lines_removed += removed;
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            permission_mode: PermissionMode::Default,
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
}
